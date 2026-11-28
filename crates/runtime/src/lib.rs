//! The room actor host (doc 05 7.3 and 8.1, D-99: one process, an in-process seam).
//!
//! One tokio task owns one room's [`RoomState`]. Edges talk to it only through a [`RoomHandle`]
//! over a bounded mailbox. The loop: take a batch of commands, run `room_core::apply` on each,
//! append all their events to the [`LogStore`] in one fenced write, mark them durable, publish
//! them, and only then release the replies (the hold rule on `room_core::Applied`). Nothing a
//! client sees can be erased by a crash.
//!
//! Deviations from doc 05 8.1, all for the first milestone: the batch is one mailbox message
//! (no cross-room group commit window), there are no timers (cursor flush, snapshot, eviction),
//! no supervisor beyond "the task ended, so every handle reports `Unavailable`", and acks are not
//! written to the store.

use config::Limits;
use log::{AppendBatch, AppendError, LogStore};
use proto::{Command, Epoch, Event, RoomId, Seq, SlotNo, Timestamp};
use room_core::{CoreError, Ctx, Reply, RoomState, apply};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// What the room publishes to every subscriber, in log order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Published {
    /// One batch's durable log events, in order (one queue entry however large the batch),
    /// with the notes the edges attached to the batch's commands.
    Events {
        /// The events.
        events: Arc<[Event]>,
        /// Non-durable notes about some of these events (the archived protocol plan section 0.5).
        /// A subscriber that resyncs from the log gets the events without them.
        notes: Arc<[Note]>,
    },
    /// A message one session asked to fan out (chat, bounce). Not durable; each edge decides who
    /// gets it. The body is opaque to the room.
    Ext {
        /// The sending slot.
        from: SlotNo,
        /// Edge-defined payload.
        body: Arc<serde_json::Value>,
    },
}

/// What an edge attached to one command, published with the command's events.
///
/// Facts that are not state ([`RoomHandle::noted_commands`]): a request's pass-through fields,
/// and the command's reply (for example a key's value before a write).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// The command's last event.
    pub seq: Seq,
    /// The command's reply.
    pub reply: Reply,
    /// What the edge attached, opaque to the room.
    pub extra: Arc<serde_json::Value>,
}

/// A read-only query, run on the room task against its state ([`RoomHandle::read`]).
struct ReadFn(Box<dyn FnOnce(&RoomState) + Send>);

impl core::fmt::Debug for ReadFn {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ReadFn")
    }
}

/// A subscriber's view: the durable events after its cursor, then the live stream from right
/// after the last one, with no gap and no overlap.
///
/// Doc 11 R3: each subscriber has its own bounded queue (`Limits::session_queue` entries). A
/// subscriber whose queue is full when events are published is dropped: its `live` ends, and it
/// resubscribes from the last `seq` it applied, so it loses nothing and the room never waits for
/// it. Non-durable messages (`Ext`, chat) are simply skipped for a full queue.
#[derive(Debug)]
pub struct Subscription {
    /// Durable events with `seq` above the cursor, up to the head at subscribe time.
    pub history: Vec<Event>,
    /// Everything published after `history`. `None` from `recv` means: resubscribe from your
    /// cursor (you fell behind), or the room stopped (then `subscribe` fails).
    pub live: mpsc::Receiver<Arc<Published>>,
}

/// Why the room could not serve a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomError {
    /// The room task has stopped (fenced, store failure or panic). Retry after the room reloads.
    Unavailable,
}

impl core::fmt::Display for RoomError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("room unavailable")
    }
}

impl std::error::Error for RoomError {}

/// One command's outcome inside a batch.
pub type Outcome = Result<Reply, CoreError>;

/// A command and the note to publish with its events, if any.
pub type Noted = (Command, Option<Arc<serde_json::Value>>);

#[derive(Debug)]
enum RoomMsg {
    Commands { cmds: Vec<Noted>, reply: oneshot::Sender<Vec<Outcome>> },
    Read(ReadFn),
    Subscribe { after: Seq, reply: oneshot::Sender<Result<Subscription, RoomError>> },
    Ext { from: SlotNo, body: Arc<serde_json::Value>, done: oneshot::Sender<()> },
    Subscribers { reply: oneshot::Sender<usize> },
}

/// Why a room task ended (the value of its `JoinHandle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// A newer owner holds the room (the store fenced an append). Do not restart this owner:
    /// it would fence the new one in turn.
    Fenced {
        /// The store's current epoch.
        current: Epoch,
    },
    /// The log failed or refused a write, or a read failed. Restart and reload from the log.
    Failed,
    /// Every handle was dropped.
    Closed,
}

/// A room's mailbox before its task runs: lets the host build what needs a [`RoomHandle`]
/// (and can fail) before it claims the room.
#[derive(Debug)]
pub struct Mailbox {
    rx: mpsc::Receiver<RoomMsg>,
}

impl Mailbox {
    /// Start the room task on this mailbox. `epoch` must be the owner epoch the store holds
    /// for `room`. The task's value says why it stopped.
    #[must_use]
    pub fn spawn<S: LogStore>(
        self,
        room: RoomId,
        epoch: Epoch,
        state: RoomState,
        store: Arc<S>,
        limits: Limits,
    ) -> JoinHandle<Stopped> {
        let actor = Actor { room, epoch, state, store, limits, subs: Vec::new() };
        tokio::spawn(actor.run(self.rx))
    }
}

/// The edge's handle on a room. Cheap to clone.
#[derive(Debug, Clone)]
pub struct RoomHandle {
    tx: mpsc::Sender<RoomMsg>,
}

impl RoomHandle {
    /// A handle and the mailbox it sends to; the room runs once [`Mailbox::spawn`] is called.
    #[must_use]
    pub fn mailbox(limits: &Limits) -> (Self, Mailbox) {
        let (tx, rx) = mpsc::channel(limits.room_mailbox.max(1));
        (Self { tx }, Mailbox { rx })
    }

    /// Start the room task. `epoch` must be the owner epoch the store holds for `room`.
    #[must_use]
    pub fn spawn<S: LogStore>(
        room: RoomId,
        epoch: Epoch,
        state: RoomState,
        store: Arc<S>,
        limits: Limits,
    ) -> (Self, JoinHandle<Stopped>) {
        let (handle, mailbox) = Self::mailbox(&limits);
        (handle, mailbox.spawn(room, epoch, state, store, limits))
    }

    /// Apply commands in order as one durable batch. Each gets its own outcome; the whole reply
    /// arrives only after the batch's events are durable and published.
    ///
    /// # Errors
    /// [`RoomError::Unavailable`] when the room task has stopped.
    pub async fn commands(&self, cmds: Vec<Command>) -> Result<Vec<Outcome>, RoomError> {
        self.noted_commands(cmds.into_iter().map(|c| (c, None)).collect()).await
    }

    /// As [`RoomHandle::commands`], each command with an optional note that is published with
    /// its events when it succeeds and emits any ([`Note`]).
    ///
    /// # Errors
    /// [`RoomError::Unavailable`] when the room task has stopped.
    pub async fn noted_commands(&self, cmds: Vec<Noted>) -> Result<Vec<Outcome>, RoomError> {
        let (reply, rx) = oneshot::channel();
        self.send(RoomMsg::Commands { cmds, reply }).await?;
        rx.await.map_err(|_| RoomError::Unavailable)
    }

    /// Run a read-only query on the room state, after every message sent before it. Nothing is
    /// written. The answer can reach the caller before events published earlier have left its
    /// queue (the archived protocol plan section 0.4).
    ///
    /// # Errors
    /// [`RoomError::Unavailable`] when the room task has stopped.
    pub async fn read<T: Send + 'static>(
        &self,
        query: impl FnOnce(&RoomState) -> T + Send + 'static,
    ) -> Result<T, RoomError> {
        let (tx, rx) = oneshot::channel();
        // A caller that went away does not stop the room.
        let run = move |s: &RoomState| drop(tx.send(query(s)));
        self.send(RoomMsg::Read(ReadFn(Box::new(run)))).await?;
        rx.await.map_err(|_| RoomError::Unavailable)
    }

    /// Apply one command (see [`RoomHandle::commands`]).
    ///
    /// # Errors
    /// [`RoomError::Unavailable`] when the room task has stopped.
    pub async fn command(&self, cmd: Command) -> Result<Outcome, RoomError> {
        self.commands(vec![cmd]).await?.pop().ok_or(RoomError::Unavailable)
    }

    /// Join the room's stream after `after` (doc 05 7.3 `Subscribe`; `Seq(0)` for everything).
    ///
    /// # Errors
    /// [`RoomError::Unavailable`] when the room task has stopped, the log cannot be read, or
    /// `after` is beyond the durable head.
    pub async fn subscribe(&self, after: Seq) -> Result<Subscription, RoomError> {
        let (reply, rx) = oneshot::channel();
        self.send(RoomMsg::Subscribe { after, reply }).await?;
        rx.await.map_err(|_| RoomError::Unavailable)?
    }

    /// Fan a non-durable message out to every subscriber, ordered with the events. Returns once
    /// it is published.
    ///
    /// # Errors
    /// [`RoomError::Unavailable`] when the room task has stopped.
    pub async fn ext(&self, from: SlotNo, body: Arc<serde_json::Value>) -> Result<(), RoomError> {
        let (done, rx) = oneshot::channel();
        self.send(RoomMsg::Ext { from, body, done }).await?;
        rx.await.map_err(|_| RoomError::Unavailable)
    }

    /// How many subscriber queues the room holds (a metric, doc 11 R8).
    ///
    /// # Errors
    /// [`RoomError::Unavailable`] when the room task has stopped.
    pub async fn subscribers(&self) -> Result<usize, RoomError> {
        let (reply, rx) = oneshot::channel();
        self.send(RoomMsg::Subscribers { reply }).await?;
        rx.await.map_err(|_| RoomError::Unavailable)
    }

    /// Whether the room task is still running (a liveness probe, `/healthz`).
    #[must_use]
    pub fn is_running(&self) -> bool {
        !self.tx.is_closed()
    }

    /// Resolves once the room task has stopped (fenced, store failure or panic), so the host
    /// can exit and let its supervisor restart it from the log.
    pub async fn stopped(&self) {
        self.tx.closed().await;
    }

    async fn send(&self, msg: RoomMsg) -> Result<(), RoomError> {
        self.tx.send(msg).await.map_err(|_| RoomError::Unavailable)
    }
}

struct Actor<S> {
    room: RoomId,
    epoch: Epoch,
    state: RoomState,
    store: Arc<S>,
    limits: Limits,
    /// One bounded queue per subscriber.
    subs: Vec<mpsc::Sender<Arc<Published>>>,
}

fn now() -> Timestamp {
    Timestamp(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()))
}

impl<S: LogStore> Actor<S> {
    async fn run(mut self, mut rx: mpsc::Receiver<RoomMsg>) -> Stopped {
        while let Some(msg) = rx.recv().await {
            let ok = match msg {
                RoomMsg::Commands { cmds, reply } => match self.batch(cmds).await {
                    Ok(outcomes) => {
                        // A caller that went away does not stop the room.
                        drop(reply.send(outcomes));
                        Ok(())
                    }
                    Err(why) => Err(why),
                },
                RoomMsg::Subscribe { after, reply } => {
                    // A cursor beyond the head is the caller's mistake: refuse it, keep serving.
                    if after > self.state.durable() {
                        drop(reply.send(Err(RoomError::Unavailable)));
                        continue;
                    }
                    let sub = self.subscribe(after).await;
                    let ok = if sub.is_ok() { Ok(()) } else { Err(Stopped::Failed) };
                    drop(reply.send(sub));
                    ok
                }
                RoomMsg::Read(ReadFn(run)) => {
                    run(&self.state);
                    Ok(())
                }
                RoomMsg::Subscribers { reply } => {
                    let _ = reply.send(self.subs.len());
                    Ok(())
                }
                RoomMsg::Ext { from, body, done } => {
                    let msg = Arc::new(Published::Ext { from, body });
                    // A full queue skips the message (chat may be dropped, doc 11 R3); a closed
                    // one is a subscriber that left.
                    self.subs.retain(|tx| {
                        !matches!(tx.try_send(Arc::clone(&msg)), Err(TrySendError::Closed(_)))
                    });
                    let _ = done.send(());
                    Ok(())
                }
            };
            if let Err(why) = ok {
                // The in-memory state may now be ahead of the log: stop rather than serve it.
                tracing::error!(
                    room = self.room.0,
                    ?why,
                    "room stopped: the log refused or failed"
                );
                return why;
            }
        }
        Stopped::Closed
    }

    /// Apply, append once, mark durable, publish, then reply. `Err` means the room must stop.
    async fn batch(&mut self, cmds: Vec<Noted>) -> Result<Vec<Outcome>, Stopped> {
        let ctx = Ctx { now: now(), limits: &self.limits, powf: f64::powf };
        let mut events = Vec::new();
        let mut notes = Vec::new();
        let mut outcomes = Vec::with_capacity(cmds.len());
        let mut wait = Seq(0);
        for (cmd, extra) in cmds {
            match apply(&mut self.state, &ctx, cmd) {
                Ok(a) => {
                    wait = wait.max(a.depends_on.unwrap_or_default());
                    if let (Some(extra), Some(last)) = (extra, a.events.last()) {
                        notes.push(Note { seq: last.seq, reply: a.reply.clone(), extra });
                    }
                    events.extend(a.events);
                    outcomes.push(Ok(a.reply));
                }
                Err(e) => outcomes.push(Err(e)),
            }
        }
        if !events.is_empty() {
            let batch =
                AppendBatch { room: self.room, epoch: self.epoch, events: &events, cursors: &[] };
            if let Err(e) = self.store.append(batch).await {
                tracing::error!(room = self.room.0, error = ?e, "append failed");
                return Err(match e {
                    AppendError::Fenced { current } => Stopped::Fenced { current },
                    AppendError::Unavailable | AppendError::Corrupt(_) => Stopped::Failed,
                });
            }
            if self.state.mark_durable(self.state.head()).is_err() {
                return Err(Stopped::Failed);
            }
        }
        // Hold rule: every reply rests on something durable by now; if not, a bug lost a write.
        if wait > self.state.durable() {
            tracing::error!(room = self.room.0, wait = wait.0, "reply depends on a lost write");
            return Err(Stopped::Failed);
        }
        if !events.is_empty() {
            self.publish(&Arc::new(Published::Events {
                events: events.into(),
                notes: notes.into(),
            }));
        }
        Ok(outcomes)
    }

    /// Durable events only: a subscriber whose queue is full is dropped and resyncs from its
    /// cursor, so events are never skipped for it.
    fn publish(&mut self, msg: &Arc<Published>) {
        let room = self.room.0;
        self.subs.retain(|tx| match tx.try_send(Arc::clone(msg)) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                tracing::debug!(room, "subscriber fell behind; it resyncs from its cursor");
                false
            }
            Err(TrySendError::Closed(_)) => false,
        });
    }

    async fn subscribe(&mut self, after: Seq) -> Result<Subscription, RoomError> {
        let durable = self.state.durable();
        // shortcut: a first subscribe (after = 0) replays the whole log, O(log); upgrade to a
        // snapshot plus tail when rooms get long or reconnect storms show up in spike S2.
        let history = self
            .store
            .read_tail(self.room, after, usize::MAX)
            .await
            .map_err(|_| RoomError::Unavailable)?;
        if history.last().map_or(after, |e| e.seq) != durable {
            return Err(RoomError::Unavailable);
        }
        let (tx, live) = mpsc::channel(self.limits.session_queue.max(1));
        // Drop queues whose subscriber left, so a quiet room's reconnects cannot pile them up.
        self.subs.retain(|t| !t.is_closed());
        self.subs.push(tx);
        Ok(Subscription { history, live })
    }
}

#[cfg(test)]
mod tests;
