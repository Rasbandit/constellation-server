//! One connection's state machine. Input: text frames from the client and events the room
//! publishes. Output: text frames to send. The socket loop (`socket.rs`) owns the IO.

use crate::limit::RateTable;
use crate::presence::{Changed, Who};
use crate::storage;
use crate::wire::{
    self, Bounce, CLIENT_GOAL, Connect, ConnectUpdate, Get, GetDataPackage, Locations, NetVersion,
    NetworkItem, NetworkPlayer, NetworkSlot, Permissions, PrintJson, Say, ServerPacket,
    StatusUpdate, TextPart,
};
use crate::{CompatRoom, NO_GAME_MIN_VERSION, NO_GAME_TAGS, PROTOCOL_VERSION, check_cid};
use proto::{Command, ErrorCode, Event, EventKind, Seq, SlotNo};
use room_core::{CoreError, RESERVED_KEY_PREFIX, Reply};
use runtime::{Note, Published, Subscription};
use seed_bundle::{Slot, SlotKind};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

/// The session must end: the room is gone or the client fell too far behind. The client
/// reconnects and resyncs from the protocol's item index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

/// Fan-out bodies this edge puts on the room's non-durable channel. The room never reads them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "fan", rename_all = "snake_case")]
pub(crate) enum Fanout {
    /// A `PrintJSON` for every connected client (unless it asked for `NoText`).
    Print { print: PrintJson },
    /// A `Bounced` for the clients that match the targets.
    Bounce { team: u32, targets: Bounce, packet: Map<String, Value> },
    /// A read-only key changed: a `SetReply` for the clients watching it.
    KeyChanged { key: String, value: Value },
}

/// Which items a client wants (section `items_handling flags`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Handling {
    /// Any items at all (bit 0).
    any: bool,
    /// Items found in the client's own world too (bit 1).
    own: bool,
    /// The starting inventory, before every received item (bit 2).
    start: bool,
}

impl Handling {
    fn parse(v: &Value) -> Option<Self> {
        // The reference server reads this as an integer bit set (MultiServer.py, the
        // `items_handling` setter): only the three documented flags count, higher bits are
        // ignored, a negative number is its two's complement bits (-1 is every flag), and a
        // boolean is the integer 0 or 1. Anything else is refused.
        let raw = match v {
            Value::Bool(b) => u64::from(*b),
            _ => v.as_u64().or_else(|| v.as_i64().map(|i| u64::from_le_bytes(i.to_le_bytes())))?,
        };
        let bits = raw & 0b111;
        let any = bits & 0b001 != 0;
        // Bits 1 and 2 need bit 0.
        if !any && bits != 0 {
            return None;
        }
        Some(Self { any, own: bits & 0b010 != 0, start: bits & 0b100 != 0 })
    }

    const fn wants(self, me: u32, item: &NetworkItem) -> bool {
        self.any && (self.own || item.player != me)
    }
}

#[derive(Debug)]
struct Joined {
    team: u32,
    slot: u32,
    tags: BTreeSet<String>,
    handling: Handling,
    /// Items sent to the client so far (positions in the filtered list).
    sent: usize,
    /// Every item the slot received, in order, `player` being the finder.
    items: Vec<NetworkItem>,
    /// The slot's starting inventory. A client that asks for it (bit 2) gets these first, so
    /// its index space is `start` then `items`; any other client indexes `items` from 0
    /// (reference server: start items lead the list it indexes, `MultiServer.py:1146-1168`).
    start: Vec<NetworkItem>,
    checked: BTreeSet<i64>,
    live: mpsc::Receiver<Arc<Published>>,
    /// The last room `seq` folded into this view: where a resync resumes.
    cursor: Seq,
    /// Keys this connection asked to be told about (`SetNotify`). Not durable; kept across a
    /// re-`Connect` on the same socket, lost with the socket.
    watch: BTreeSet<String>,
    /// This connection's own key writes not yet seen in the stream: already answered from the
    /// room's reply, so the published copy is skipped (exactly one `SetReply` per write).
    own_writes: BTreeSet<Seq>,
}

impl Joined {
    fn no_text(&self) -> bool {
        self.tags.contains("NoText")
    }

    fn no_locations(&self) -> bool {
        NO_GAME_TAGS.iter().any(|t| self.tags.contains(*t))
    }

    fn wanted(&self) -> Vec<NetworkItem> {
        let start = if self.handling.start { self.start.as_slice() } else { &[] };
        let received = self.items.iter().filter(|i| self.handling.wants(self.slot, i));
        start.iter().chain(received).copied().collect()
    }
}

/// One client connection.
#[derive(Debug)]
pub struct Session {
    room: Arc<CompatRoom>,
    joined: Option<Joined>,
    /// The client address the room's per-address rate limits are kept by.
    peer: IpAddr,
}

fn invalid(kind: &'static str, cmd: Option<&str>, text: impl Into<String>) -> ServerPacket {
    ServerPacket::InvalidPacket { kind, original_cmd: cmd.map(str::to_owned), text: text.into() }
}

fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

fn parse<T: serde::de::DeserializeOwned>(cmd: &str, v: Value) -> Result<T, Step> {
    serde_json::from_value(v)
        .map_err(|e| Step::from(invalid("arguments", Some(cmd), format!("{cmd}: {e}"))))
}

impl Session {
    /// A new connection to `room` from `peer` (the client address rate limits are kept by).
    #[must_use]
    pub const fn with_peer(room: Arc<CompatRoom>, peer: IpAddr) -> Self {
        Self { room, joined: None, peer }
    }

    /// A new connection to `room` from loopback.
    #[must_use]
    pub const fn new(room: Arc<CompatRoom>) -> Self {
        Self::with_peer(room, IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
    }

    /// A token from `table` for `key`, or the `InvalidPacket` that says the client is over its
    /// rate.
    fn rate<K: std::hash::Hash + Eq>(table: &RateTable<K>, key: K, cmd: &str) -> Result<(), Step> {
        if table.take(key, Instant::now()) {
            Ok(())
        } else {
            Err(Step::from(invalid(
                "arguments",
                Some(cmd),
                format!("{cmd}: rate limited, slow down"),
            )))
        }
    }

    /// Whether a `Connect` has succeeded on this socket.
    #[must_use]
    pub const fn is_joined(&self) -> bool {
        self.joined.is_some()
    }

    /// The live stream, once connected; the socket loop waits on it.
    pub const fn live(&mut self) -> Option<&mut mpsc::Receiver<Arc<Published>>> {
        match &mut self.joined {
            Some(j) => Some(&mut j.live),
            None => None,
        }
    }

    /// The first frame: `RoomInfo` (section "Archipelago Connection Handshake", step 2).
    #[must_use]
    pub fn hello(&self) -> String {
        let b = &self.room.bundle;
        let o = &b.server_options;
        let g = b.generator_version;
        let (major, minor, build) = PROTOCOL_VERSION;
        wire::frame(&[ServerPacket::RoomInfo {
            version: NetVersion::new(major, minor, build),
            generator_version: NetVersion::new(g.major, g.minor, g.build),
            tags: Vec::new(),
            password: self.room.password.is_some(),
            // Release, collect and remaining are not implemented yet, so they are advertised as
            // disabled whatever the seed chose (docs/compat-status.md).
            permissions: Permissions { release: 0, collect: 0, remaining: 0 },
            hint_cost: o.hint_cost,
            location_check_points: o.location_check_points,
            games: b.games.keys().cloned().collect(),
            datapackage_checksums: b
                .games
                .iter()
                .map(|(n, g)| (n.clone(), g.checksum.clone()))
                .collect(),
            seed_name: b.seed_name.clone(),
            time: unix_now(),
        }])
    }

    /// Handle one text frame from the client.
    ///
    /// # Errors
    /// [`Closed`] when the room is gone.
    pub async fn on_text(&mut self, text: &str) -> Result<Vec<String>, Closed> {
        let Ok(Value::Array(packets)) = serde_json::from_str::<Value>(text) else {
            let p = invalid("cmd", None, "a frame must be a JSON list of commands");
            return Ok(vec![wire::frame(&[p])]);
        };
        let mut out = Vec::new();
        for p in packets {
            let cmd = p.get("cmd").and_then(Value::as_str).map(str::to_owned);
            let Some(cmd) = cmd else {
                out.push(wire::frame(&[invalid("cmd", None, "a command needs a string cmd")]));
                continue;
            };
            match self.command(&cmd, p).await {
                Ok(frames) => out.extend(frames),
                Err(Step::Reply(packet)) => out.push(wire::frame(&[*packet])),
                Err(Step::Closed) => return Err(Closed),
            }
        }
        Ok(out)
    }

    async fn command(&mut self, cmd: &str, p: Value) -> Result<Vec<String>, Step> {
        match cmd {
            "Connect" => self.connect(parse(cmd, p)?).await,
            "GetDataPackage" => {
                let req = parse::<GetDataPackage>(cmd, p)?;
                Self::rate(&self.room.datapackage, self.peer, cmd)?;
                Ok(vec![self.data_package(&req)])
            }
            _ if self.joined.is_none() => {
                Err(Step::from(invalid("cmd", Some(cmd), format!("{cmd}: connect first"))))
            }
            "Sync" => Ok(self.full_items().into_iter().collect()),
            "LocationChecks" => self.location_checks(parse(cmd, p)?).await,
            "LocationScouts" => Ok(vec![self.scout(&parse(cmd, p)?)?]),
            "StatusUpdate" => self.status(&parse(cmd, p)?).await,
            "Say" => self.say(parse(cmd, p)?).await,
            "Bounce" => self.bounce(p).await,
            "Get" => self.get(p).await,
            "Set" => self.set(p).await,
            "SetNotify" => self.set_notify(&p),
            "ConnectUpdate" => self.connect_update(parse(cmd, p)?),
            "CreateHints" | "UpdateHint" => Err(Step::from(invalid(
                "cmd",
                Some(cmd),
                format!("{cmd} is not supported by this server yet"),
            ))),
            _ => Err(Step::from(invalid("cmd", Some(cmd), format!("unknown command {cmd:?}")))),
        }
    }

    async fn connect(&mut self, c: Connect) -> Result<Vec<String>, Step> {
        let room = Arc::clone(&self.room);
        Self::rate(&room.connects, self.peer, "Connect")?;
        let (errors, ok) = refusals(&room, &c);
        let Some((team, slot, handling)) = ok.filter(|_| errors.is_empty()) else {
            return Ok(vec![wire::frame(&[ServerPacket::ConnectionRefused { errors }])]);
        };
        // Leave the old subscription first, so the room can prune its queue.
        let (previous, watch) = match self.joined.take() {
            Some(j) => (Some((j.team, j.slot)), j.watch),
            None => (None, BTreeSet::new()),
        };
        let Subscription { history, live } =
            self.room.room.subscribe(Seq(0)).await.map_err(|_| Step::Closed)?;
        let mut j = Joined {
            team,
            slot: slot.id,
            tags: c.tags.iter().cloned().collect(),
            handling,
            sent: 0,
            items: Vec::new(),
            start: slot
                .start_inventory
                .iter()
                .map(|&item| NetworkItem { item, location: START_LOCATION, player: 0, flags: 0 })
                .collect(),
            checked: BTreeSet::new(),
            live,
            cursor: Seq(0),
            watch,
            own_writes: BTreeSet::new(),
        };
        for e in &history {
            fold(&self.room, &mut j, e);
        }
        let connected = self.connected(&j, slot, c.slot_data);
        let wanted = j.wanted();
        j.sent = wanted.len();
        self.joined = Some(j);
        let mut packets = vec![connected];
        if !wanted.is_empty() {
            packets.push(ServerPacket::ReceivedItems { index: 0, items: wanted });
        }
        // A re-Connect to the same slot announces nothing; to another slot it leaves the old one.
        if previous.is_none_or(|(_, old)| old != slot.id) {
            if let Some((old_team, old)) = previous {
                self.presence_part(old_team, old).await?;
                self.fan(old, &Fanout::Print { print: part_print(old_team, old) }).await?;
            }
            let who = self.who(team, slot.id).await?;
            self.room.presence.join(who, async |c| self.publish_change(slot.id, c).await).await;
            let print = PrintJson {
                kind: "Join".to_owned(),
                data: json!([TextPart::player(slot.id), TextPart::text(" joined the room.")]),
                team: Some(team),
                slot: Some(slot.id),
                tags: Some(c.tags),
                ..PrintJson::default()
            };
            self.fan(slot.id, &Fanout::Print { print }).await?;
        }
        Ok(vec![wire::frame(&packets)])
    }

    fn connected(&self, j: &Joined, slot: &Slot, with_data: bool) -> ServerPacket {
        let b = &self.room.bundle;
        ServerPacket::Connected {
            team: j.team,
            slot: j.slot,
            players: b
                .slots
                .iter()
                .filter(|s| s.kind == SlotKind::Player)
                .map(|s| NetworkPlayer {
                    team: s.team,
                    slot: s.id,
                    alias: s.name.clone(),
                    name: s.name.clone(),
                })
                .collect(),
            missing_locations: slot
                .locations
                .iter()
                .map(|p| p.location)
                .filter(|l| !j.checked.contains(l))
                .collect(),
            checked_locations: j.checked.iter().copied().collect(),
            slot_data: with_data.then(|| slot.slot_data.clone()),
            slot_info: b
                .slots
                .iter()
                .map(|s| {
                    let kind = match s.kind {
                        SlotKind::Spectator => 0,
                        SlotKind::Player => 1,
                        SlotKind::Group => 2,
                    };
                    let info = NetworkSlot {
                        name: s.name.clone(),
                        game: s.game.clone(),
                        kind,
                        group_members: s.group_members.clone(),
                    };
                    (s.id.to_string(), info)
                })
                .collect(),
            hint_points: hint_points(&self.room, j),
        }
    }

    /// Handle one published room message.
    ///
    /// # Errors
    /// Never today; kept so the socket loop treats every input alike.
    pub fn on_published(&mut self, p: &Published) -> Result<Vec<String>, Closed> {
        let room = Arc::clone(&self.room);
        let Some(j) = self.joined.as_mut() else { return Ok(Vec::new()) };
        let packets = match p {
            Published::Events { events, notes } => events_packets(&room, j, events, notes),
            Published::Ext { body, .. } => {
                match serde_json::from_value::<Fanout>((**body).clone()) {
                    Ok(Fanout::Print { print }) if !j.no_text() => {
                        vec![ServerPacket::PrintJson(print)]
                    }
                    Ok(Fanout::Bounce { team, targets, packet }) => {
                        if bounce_matches(&room, j, team, &targets) {
                            return Ok(vec![Value::Array(vec![Value::Object(packet)]).to_string()]);
                        }
                        Vec::new()
                    }
                    Ok(Fanout::KeyChanged { key, value }) if j.watch.contains(&key) => {
                        vec![ServerPacket::SetReply(storage::read_only_reply(&key, value))]
                    }
                    Ok(Fanout::Print { .. } | Fanout::KeyChanged { .. }) => Vec::new(),
                    Err(e) => {
                        tracing::error!(error = %e, "unreadable fan-out body");
                        Vec::new()
                    }
                }
            }
        };
        Ok(if packets.is_empty() { Vec::new() } else { vec![wire::frame(&packets)] })
    }

    /// The room dropped this session's queue because it fell behind (doc 11 R3): resubscribe
    /// from the last event applied and send what was missed, with the right item indexes.
    ///
    /// # Errors
    /// [`Closed`] when the room is gone.
    pub async fn resync(&mut self) -> Result<Vec<String>, Closed> {
        let room = Arc::clone(&self.room);
        let Some(j) = self.joined.as_mut() else { return Ok(Vec::new()) };
        let Subscription { history, live } =
            room.room.subscribe(j.cursor).await.map_err(|_| Closed)?;
        j.live = live;
        // The log has no notes: watchers get `null` for `original_value` and no pass-through
        // fields for the writes they missed (docs/protocol-plan.md 1.4).
        let packets = events_packets(&room, j, &history, &[]);
        Ok(if packets.is_empty() { Vec::new() } else { vec![wire::frame(&packets)] })
    }

    /// The client is gone: tell the others.
    pub async fn leave(&mut self) {
        let Some(j) = self.joined.take() else { return };
        // The room may already be gone; nobody is left to tell then.
        drop(self.presence_part(j.team, j.slot).await);
        let print = part_print(j.team, j.slot);
        let body = serde_json::to_value(Fanout::Print { print }).unwrap_or(Value::Null);
        // The room may already be gone; nobody is left to tell then.
        drop(self.room.room.ext(SlotNo(j.slot), Arc::new(body)).await);
    }

    /// A presence change for `slot`, final when its goal is reached in the room or it is not a
    /// player (`storage::done`).
    async fn who(&self, team: u32, slot: u32) -> Result<Who, Step> {
        let goal = self.room.room.read(move |s| s.has_goal(SlotNo(slot))).await;
        let goal = goal.map_err(|_| Step::Closed)?;
        let done = self.room.bundle.slot(slot).is_some_and(|s| storage::done(s, goal));
        Ok(Who { team, slot, done })
    }

    async fn presence_part(&self, team: u32, slot: u32) -> Result<(), Step> {
        let who = self.who(team, slot).await?;
        self.room.presence.part(who, async |c| self.publish_change(slot, c).await).await;
        Ok(())
    }

    /// Tell the watchers of a read-only key its new value. The room may be gone; then nobody
    /// is left to tell.
    async fn publish_change(&self, slot: u32, c: Changed) {
        let f = Fanout::KeyChanged { key: c.key, value: Value::from(c.value) };
        drop(self.fan(slot, &f).await);
    }

    async fn fan(&self, from: u32, f: &Fanout) -> Result<(), Step> {
        let body = serde_json::to_value(f).map_err(|_| Step::Closed)?;
        self.room.room.ext(SlotNo(from), Arc::new(body)).await.map_err(|_| Step::Closed)
    }

    fn data_package(&self, req: &GetDataPackage) -> String {
        let wanted = |name: &String| req.games.as_ref().is_none_or(|g| g.contains(name));
        let games: Vec<String> = self
            .room
            .packages
            .iter()
            .filter(|(name, _)| wanted(name))
            .map(|(name, json)| format!("{}:{json}", Value::String(name.clone())))
            .collect();
        format!("[{{\"cmd\":\"DataPackage\",\"data\":{{\"games\":{{{}}}}}}}]", games.join(","))
    }

    fn full_items(&mut self) -> Option<String> {
        let j = self.joined.as_mut()?;
        let items = j.wanted();
        j.sent = items.len();
        // Nothing to send (no items yet, or a client that takes none): no empty packet.
        (!items.is_empty()).then(|| wire::frame(&[ServerPacket::ReceivedItems { index: 0, items }]))
    }

    async fn location_checks(&self, req: Locations) -> Result<Vec<String>, Step> {
        let j = self.joined.as_ref().ok_or(Step::Closed)?;
        if j.no_locations() {
            return Err(Step::from(invalid(
                "cmd",
                Some("LocationChecks"),
                "trackers and text clients cannot check locations",
            )));
        }
        let slot = SlotNo(j.slot);
        let fresh: BTreeSet<i64> =
            req.locations.into_iter().filter(|l| !j.checked.contains(l)).collect();
        let cmds = fresh
            .into_iter()
            .map(|location| Command::Check { slot, cid: check_cid(location), location })
            .collect();
        // Unknown locations are refused by the room and ignored here, as the protocol allows.
        self.room.room.commands(cmds).await.map_err(|_| Step::Closed)?;
        Ok(Vec::new())
    }

    /// One answer per requested id, in request order; an id outside the slot's world refuses
    /// the whole packet.
    fn scout(&self, req: &Locations) -> Result<String, Step> {
        let slot = self.joined.as_ref().and_then(|j| self.room.bundle.slot(j.slot));
        let slot = slot.ok_or(Step::Closed)?;
        let world: BTreeMap<i64, _> = slot.locations.iter().map(|p| (p.location, p)).collect();
        let locations = req
            .locations
            .iter()
            .map(|l| {
                let p = world.get(l).ok_or_else(|| {
                    let text = format!("LocationScouts: location {l} is not in this world");
                    Step::from(invalid("arguments", Some("LocationScouts"), text))
                })?;
                Ok(NetworkItem {
                    item: p.item,
                    location: p.location,
                    player: p.receiver,
                    flags: p.flags,
                })
            })
            .collect::<Result<_, Step>>()?;
        Ok(wire::frame(&[ServerPacket::LocationInfo { locations }]))
    }

    async fn status(&self, req: &StatusUpdate) -> Result<Vec<String>, Step> {
        let j = self.joined.as_ref().ok_or(Step::Closed)?;
        let slot = j.slot;
        if req.status == CLIENT_GOAL && j.no_locations() {
            return Err(Step::from(invalid(
                "arguments",
                Some("StatusUpdate"),
                "trackers and text clients cannot reach a goal",
            )));
        }
        if req.status == CLIENT_GOAL {
            // Watchers learn it from the durable `Goal` event.
            let room = &self.room.room;
            let goal = async || match room.command(Command::Goal { slot: SlotNo(slot) }).await {
                Ok(Ok(_)) => Ok(()),
                _ => Err(Step::Closed),
            };
            self.room.presence.reach_goal(slot, goal).await?;
            return Ok(Vec::new());
        }
        // Any other value is kept as the slot's status until the goal, like the reference
        // (`MultiServer.py:2203-2210`); not durable.
        let who = self.who(j.team, slot).await?;
        let status = req.status;
        self.room.presence.set(who, status, async |c| self.publish_change(slot, c).await).await;
        Ok(Vec::new())
    }

    async fn say(&self, req: Say) -> Result<Vec<String>, Step> {
        if req.text.len() > self.room.limits.chat_max_bytes {
            return Err(Step::from(invalid("arguments", Some("Say"), "Say: text too long")));
        }
        let j = self.joined.as_ref().ok_or(Step::Closed)?;
        Self::rate(&self.room.fanout, j.slot, "Say")?;
        if req.text.starts_with('!') {
            let print = PrintJson {
                kind: "CommandResult".to_owned(),
                data: json!([TextPart::text("Commands are not supported by this server yet.")]),
                ..PrintJson::default()
            };
            return Ok(vec![wire::frame(&[ServerPacket::PrintJson(print)])]);
        }
        let name = self.room.bundle.slot(j.slot).map_or("", |s| s.name.as_str());
        let print = PrintJson {
            kind: "Chat".to_owned(),
            data: json!([TextPart::text(format!("{name}: {}", req.text))]),
            team: Some(j.team),
            slot: Some(j.slot),
            message: Some(req.text),
            ..PrintJson::default()
        };
        self.fan(j.slot, &Fanout::Print { print }).await?;
        Ok(Vec::new())
    }

    async fn bounce(&self, p: Value) -> Result<Vec<String>, Step> {
        if p.to_string().len() > self.room.limits.bounce_max_bytes {
            return Err(Step::from(invalid(
                "arguments",
                Some("Bounce"),
                "Bounce: packet too large",
            )));
        }
        let j = self.joined.as_ref().ok_or(Step::Closed)?;
        Self::rate(&self.room.fanout, j.slot, "Bounce")?;
        let targets: Bounce = parse("Bounce", p.clone())?;
        if !matches!(targets.operator.as_deref(), None | Some("legacy" | "or" | "and")) {
            return Err(Step::from(invalid(
                "arguments",
                Some("Bounce"),
                "Bounce: unknown operator",
            )));
        }
        let Value::Object(mut packet) = p else {
            return Err(Step::from(invalid("arguments", Some("Bounce"), "Bounce: not an object")));
        };
        packet.insert("cmd".to_owned(), Value::String("Bounced".to_owned()));
        self.fan(j.slot, &Fanout::Bounce { team: j.team, targets, packet }).await?;
        Ok(Vec::new())
    }

    async fn get(&self, p: Value) -> Result<Vec<String>, Step> {
        let Value::Object(mut extra) = p else {
            return Err(Step::from(invalid("arguments", Some("Get"), "Get: not an object")));
        };
        let req: Get = parse("Get", Value::Object(extra.clone()))?;
        self.key_count("Get", req.keys.len())?;
        extra.remove("cmd");
        extra.remove("keys");
        let room = &self.room;
        // One read of the room for the stored keys and the goals of the status keys asked for.
        let stored: Vec<String> =
            req.keys.iter().filter(|k| !k.starts_with(RESERVED_KEY_PREFIX)).cloned().collect();
        let status: Vec<u32> =
            req.keys.iter().filter_map(|k| storage::status_slot(room, k)).map(|s| s.id).collect();
        let (values, goals) = room
            .room
            .read(move |s| {
                let values: BTreeMap<String, Value> = stored
                    .into_iter()
                    .map(|k| {
                        let v = s.kv_get(&k).cloned().unwrap_or(Value::Null);
                        (k, v)
                    })
                    .collect();
                let goals: BTreeSet<u32> =
                    status.into_iter().filter(|&n| s.has_goal(SlotNo(n))).collect();
                (values, goals)
            })
            .await
            .map_err(|_| Step::Closed)?;
        let mut keys = Map::new();
        for k in req.keys {
            let v = if let Some(v) = values.get(&k) {
                v.clone()
            } else if let Some(v) = storage::read_only(room, &k) {
                v
            } else if let Some(slot) = storage::status_slot(room, &k) {
                let presence = room.presence.status(slot.id).await;
                Value::from(storage::status_value(slot, goals.contains(&slot.id), presence))
            } else {
                Value::Null
            };
            keys.insert(k, v);
        }
        Ok(vec![wire::frame(&[ServerPacket::Retrieved { keys, extra }])])
    }

    /// Refuse a `Get` or `SetNotify` with more keys than `Limits::kv_max_keys_per_request`.
    fn key_count(&self, cmd: &str, n: usize) -> Result<(), Step> {
        if n > self.room.limits.kv_max_keys_per_request {
            return Err(Step::from(invalid(
                "arguments",
                Some(cmd),
                format!("{cmd}: too many keys"),
            )));
        }
        Ok(())
    }

    async fn set(&mut self, p: Value) -> Result<Vec<String>, Step> {
        let Value::Object(packet) = p else {
            return Err(Step::from(invalid("arguments", Some("Set"), "Set: not an object")));
        };
        let j = self.joined.as_ref().ok_or(Step::Closed)?;
        let slot = j.slot;
        let req = storage::parse_set(slot, &packet)
            .map_err(|text| Step::from(invalid("arguments", Some("Set"), text)))?;
        Self::rate(&self.room.kv_sets, slot, "Set")?;
        if !self.room.kv_bytes.ready(slot, Instant::now()) {
            let what = "this slot wrote too many bytes; slow down";
            let text = format!("Set: {}", storage::registered(ErrorCode::RoomWriteRate, what));
            return Err(Step::from(invalid("arguments", Some("Set"), text)));
        }
        let key = req.write.key.clone();
        let packet = Arc::new(Value::Object(packet));
        let cmd = Command::KvSet(Box::new(req.write));
        let out = self.room.room.noted_commands(vec![(cmd, Some(Arc::clone(&packet)))]);
        let out = out.await.map_err(|_| Step::Closed)?.pop().ok_or(Step::Closed)?;
        let done = match out {
            Ok(Reply::KvSet(done)) => done,
            Err(CoreError::Kv(r)) => {
                return Err(Step::from(invalid(
                    "arguments",
                    Some("Set"),
                    storage::refusal_text(r),
                )));
            }
            Ok(_) | Err(_) => return Err(Step::Closed),
        };
        let j = self.joined.as_mut().ok_or(Step::Closed)?;
        // The published copy of this write reaches this session later; it is skipped then.
        j.own_writes.insert(done.seq);
        // The room measured the logged bytes (`room_core` `entry_bytes`); charge them as is.
        self.room.kv_bytes.charge(slot, done.bytes, Instant::now());
        if !(req.want_reply || j.watch.contains(&key)) {
            return Ok(Vec::new());
        }
        let reply =
            storage::set_reply(Some(&packet), &key, &done.value, Some(&done.original), slot);
        Ok(vec![wire::frame(&[ServerPacket::SetReply(reply)])])
    }

    /// Watch keys for this connection; nothing is sent back (reference `MultiServer.py:2306-2312`).
    fn set_notify(&mut self, p: &Value) -> Result<Vec<String>, Step> {
        let req: Get = parse("SetNotify", p.clone())?;
        self.key_count("SetNotify", req.keys.len())?;
        let max = self.room.limits.kv_max_watched_keys;
        let j = self.joined.as_mut().ok_or(Step::Closed)?;
        let new = req.keys.iter().filter(|k| !j.watch.contains(*k)).collect::<BTreeSet<_>>().len();
        if j.watch.len().saturating_add(new) > max {
            let text = "SetNotify: too many watched keys on this connection";
            return Err(Step::from(invalid("arguments", Some("SetNotify"), text)));
        }
        j.watch.extend(req.keys);
        Ok(Vec::new())
    }

    fn connect_update(&mut self, req: ConnectUpdate) -> Result<Vec<String>, Step> {
        let j = self.joined.as_mut().ok_or(Step::Closed)?;
        // Validate everything before changing anything.
        let handling = match req.items_handling.as_ref().map(Handling::parse) {
            Some(None) => {
                return Err(Step::from(invalid(
                    "arguments",
                    Some("ConnectUpdate"),
                    "bad items_handling",
                )));
            }
            Some(Some(h)) => Some(h),
            None => None,
        };
        if let Some(tags) = req.tags {
            j.tags = tags.into_iter().collect();
        }
        let Some(h) = handling else { return Ok(Vec::new()) };
        if h == j.handling {
            return Ok(Vec::new());
        }
        j.handling = h;
        Ok(self.full_items().into_iter().collect())
    }
}

/// Location id of a start item: the generic game's "Server" location (protocol doc, "Data
/// Package Contents"); its player is 0, the server.
const START_LOCATION: i64 = -2;

/// Why a command stopped early.
#[derive(Debug)]
enum Step {
    /// Send this one packet back.
    Reply(Box<ServerPacket>),
    /// End the session.
    Closed,
}

impl From<ServerPacket> for Step {
    fn from(p: ServerPacket) -> Self {
        Self::Reply(Box::new(p))
    }
}

/// Every reason to refuse a `Connect` (section `ConnectionRefused`), and the slot when the name
/// was found.
fn refusals<'a>(
    room: &'a CompatRoom,
    c: &Connect,
) -> (Vec<&'static str>, Option<(u32, &'a Slot, Handling)>) {
    let mut errors = Vec::new();
    if room.password.as_ref().is_some_and(|p| c.password.as_ref() != Some(p)) {
        errors.push("InvalidPassword");
    }
    let b = &room.bundle;
    let found = b.connect_names.get(&c.name).and_then(|&(team, id)| Some((team, b.slot(id)?)));
    let Some((team, slot)) = found else {
        errors.push("InvalidSlot");
        return (errors, None);
    };
    let no_game = c.game.is_empty() && c.tags.iter().any(|t| NO_GAME_TAGS.contains(&t.as_str()));
    if !no_game && c.game != slot.game {
        errors.push("InvalidGame");
    }
    let min = slot.minimum_client_version;
    // A client without a game is held to the server-wide minimum, not the world's.
    let min = if no_game { NO_GAME_MIN_VERSION } else { (min.major, min.minor, min.build) };
    if c.version.tuple() < min {
        errors.push("IncompatibleVersion");
    }
    let handling = Handling::parse(&c.items_handling);
    if handling.is_none() {
        errors.push("InvalidItemsHandling");
    }
    (errors, handling.map(|h| (team, slot, h)))
}

/// Points per check times checks; no hints are spent yet.
fn hint_points(room: &CompatRoom, j: &Joined) -> i64 {
    let per = i64::from(room.bundle.server_options.location_check_points);
    per.saturating_mul(i64::try_from(j.checked.len()).unwrap_or(i64::MAX))
}

/// Apply a durable event to a session's view of the room. Returns the item an `Item` event
/// carries, for whoever it went to.
fn fold(room: &CompatRoom, j: &mut Joined, e: &Event) -> Option<NetworkItem> {
    j.cursor = j.cursor.max(e.seq);
    let slot = e.slot?.0;
    match e.kind {
        EventKind::Checked { location, .. } => {
            if slot == j.slot {
                j.checked.insert(location);
            }
            None
        }
        // The goal and stored values are read from the room when asked for.
        EventKind::Goal | EventKind::KvSet { .. } => None,
        EventKind::Item { from, location, item, .. } => {
            let it =
                NetworkItem { item, location, player: from.0, flags: room.flags(from.0, location) };
            if slot == j.slot {
                j.items.push(it);
            }
            Some(it)
        }
    }
}

/// What one batch of durable events means for this client: one `RoomUpdate` for its new
/// checks, one `ReceivedItems` for its new items (continuing its index), the prints, and a
/// `SetReply` for each watched key written (with the write's pass-through fields from `notes`).
fn events_packets(
    room: &CompatRoom,
    j: &mut Joined,
    evs: &[Event],
    notes: &[Note],
) -> Vec<ServerPacket> {
    let (mut checked, mut items, mut prints) = (Vec::new(), Vec::new(), Vec::new());
    let mut replies = Vec::new();
    for e in evs {
        let slot = e.slot.map_or(0, |s| s.0);
        let was = j.checked.len();
        let item = fold(room, j, e);
        match (&e.kind, item) {
            (EventKind::KvSet { key, value }, _) => {
                if !j.own_writes.remove(&e.seq) && j.watch.contains(key) {
                    let note = notes.iter().find(|n| n.seq == e.seq);
                    let original = note.and_then(|n| match &n.reply {
                        Reply::KvSet(done) => Some(&done.original),
                        _ => None,
                    });
                    let request = note.map(|n| &*n.extra);
                    let reply = storage::set_reply(request, key, value, original, slot);
                    replies.push(ServerPacket::SetReply(reply));
                }
            }
            (&EventKind::Checked { location, .. }, _) if j.checked.len() != was => {
                checked.push(location);
            }
            (&EventKind::Item { .. }, Some(it)) => {
                if slot == j.slot && j.handling.wants(j.slot, &it) {
                    items.push(it);
                }
                if !j.no_text() {
                    prints.push(item_send(slot, it));
                }
            }
            (EventKind::Goal, _) => {
                if !j.no_text() {
                    prints.push(goal_print(j.team, slot));
                }
                let team = room.bundle.slot(slot).map_or(0, |s| s.team);
                let key = storage::status_key(team, slot);
                if j.watch.contains(&key) {
                    let reply = storage::read_only_reply(&key, Value::from(CLIENT_GOAL));
                    replies.push(ServerPacket::SetReply(reply));
                }
            }
            _ => {}
        }
    }
    let mut packets = Vec::new();
    if !checked.is_empty() {
        let hint_points = hint_points(room, j);
        packets.push(ServerPacket::RoomUpdate { checked_locations: checked, hint_points });
    }
    if !items.is_empty() {
        let index = j.sent;
        j.sent = j.sent.saturating_add(items.len());
        packets.push(ServerPacket::ReceivedItems { index, items });
    }
    packets.extend(prints);
    packets.extend(replies);
    packets
}

fn part_print(team: u32, slot: u32) -> PrintJson {
    PrintJson {
        kind: "Part".to_owned(),
        data: json!([TextPart::player(slot), TextPart::text(" left the room.")]),
        team: Some(team),
        slot: Some(slot),
        ..PrintJson::default()
    }
}

fn item_send(receiver: u32, it: NetworkItem) -> ServerPacket {
    // Deliberately the same part layout Archipelago clients already display for an item send,
    // for compatibility (reference behaviour: `json_format_send_event` in MultiServer.py).
    let mut data = vec![TextPart::player(it.player)];
    if it.player == receiver {
        data.extend([TextPart::text(" found their "), TextPart::item(it.item, receiver, it.flags)]);
    } else {
        data.extend([
            TextPart::text(" sent "),
            TextPart::item(it.item, receiver, it.flags),
            TextPart::text(" to "),
            TextPart::player(receiver),
        ]);
    }
    data.extend([
        TextPart::text(" ("),
        TextPart::location(it.location, it.player),
        TextPart::text(")"),
    ]);
    let data = json!(data);
    ServerPacket::PrintJson(PrintJson {
        kind: "ItemSend".to_owned(),
        data,
        receiving: Some(receiver),
        item: Some(it),
        ..PrintJson::default()
    })
}

fn goal_print(team: u32, slot: u32) -> ServerPacket {
    ServerPacket::PrintJson(PrintJson {
        kind: "Goal".to_owned(),
        data: json!([TextPart::player(slot), TextPart::text(" reached their goal.")]),
        team: Some(team),
        slot: Some(slot),
        ..PrintJson::default()
    })
}

/// Section `Bounce`: `or` and `and` over the conditions given; `legacy` is
/// `teams and (games or slots or tags)`. A missing team list means the sender's team.
fn bounce_matches(room: &CompatRoom, j: &Joined, sender_team: u32, b: &Bounce) -> bool {
    let game = room.bundle.slot(j.slot).map(|s| s.game.as_str());
    let teams = b.teams.clone().unwrap_or_else(|| vec![sender_team]);
    let team_ok = teams.contains(&j.team);
    let others = [
        b.games.as_ref().map(|g| game.is_some_and(|me| g.iter().any(|x| x == me))),
        b.slots.as_ref().map(|s| s.contains(&j.slot)),
        b.tags.as_ref().map(|t| t.iter().any(|x| j.tags.contains(x))),
    ];
    let given = others.iter().flatten().copied();
    match b.operator.as_deref() {
        Some("or") => team_ok || given.clone().any(|m| m),
        Some("and") => team_ok && given.clone().all(|m| m),
        _ => team_ok && given.clone().any(|m| m),
    }
}
