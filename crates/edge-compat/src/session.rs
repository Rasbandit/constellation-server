//! One connection's state machine. Input: text frames from the client and events the room
//! publishes. Output: text frames to send. The socket loop (`socket.rs`) owns the IO.

use crate::limit::Bucket;
use crate::wire::{
    self, Bounce, CLIENT_GOAL, Connect, ConnectUpdate, Get, GetDataPackage, Locations, NetVersion,
    NetworkItem, NetworkPlayer, NetworkSlot, Permissions, PrintJson, Say, ServerPacket,
    StatusUpdate, TextPart,
};
use crate::{CompatRoom, NO_GAME_MIN_VERSION, NO_GAME_TAGS, PROTOCOL_VERSION, check_cid};
use proto::{Command, Event, EventKind, Seq, SlotNo};
use runtime::{Published, Subscription};
use seed_bundle::{Slot, SlotKind};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
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
enum Fanout {
    /// A `PrintJSON` for every connected client (unless it asked for `NoText`).
    Print { print: PrintJson },
    /// A `Bounced` for the clients that match the targets.
    Bounce { team: u32, targets: Bounce, packet: Map<String, Value> },
}

/// Which items a client wants (section `items_handling flags`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Handling {
    /// Any items at all (bit 0).
    any: bool,
    /// Items found in the client's own world too (bit 1).
    own: bool,
}

impl Handling {
    fn parse(v: &Value) -> Option<Self> {
        // Only the three documented flags count; higher bits are ignored, as the reference
        // server tests bits rather than ranges (MultiServer.py, `items_handling` setter).
        let bits = v.as_u64()? & 0b111;
        let any = bits & 0b001 != 0;
        // Bits 1 and 2 need bit 0.
        if !any && bits != 0 {
            return None;
        }
        // Bit 2 (starting inventory) is accepted; seeds we import carry none yet.
        Some(Self { any, own: bits & 0b010 != 0 })
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
    checked: BTreeSet<i64>,
    goals: BTreeSet<u32>,
    live: mpsc::Receiver<Arc<Published>>,
    /// The last room `seq` folded into this view: where a resync resumes.
    cursor: Seq,
}

impl Joined {
    fn no_text(&self) -> bool {
        self.tags.contains("NoText")
    }

    fn no_locations(&self) -> bool {
        NO_GAME_TAGS.iter().any(|t| self.tags.contains(*t))
    }

    fn wanted(&self) -> Vec<NetworkItem> {
        self.items.iter().filter(|i| self.handling.wants(self.slot, i)).copied().collect()
    }
}

/// One client connection.
#[derive(Debug)]
pub struct Session {
    room: Arc<CompatRoom>,
    joined: Option<Joined>,
    /// Chat and bounce share one bucket (they share the fan-out path).
    fanout: Bucket,
    datapackage: Bucket,
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
    /// A new connection to `room`.
    #[must_use]
    pub fn new(room: Arc<CompatRoom>) -> Self {
        let l = room.limits;
        let now = Instant::now();
        Self {
            fanout: Bucket::new(l.fanout_burst, l.fanout_refill_milli_per_sec, now),
            datapackage: Bucket::new(l.datapackage_burst, l.datapackage_refill_milli_per_sec, now),
            room,
            joined: None,
        }
    }

    /// One token from `bucket`, or the `InvalidPacket` that says the client is over its rate.
    fn rate(bucket: &mut Bucket, cmd: &str) -> Result<(), Step> {
        if bucket.take(Instant::now()) {
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
                Self::rate(&mut self.datapackage, cmd)?;
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
            "Get" => self.get(p),
            "ConnectUpdate" => self.connect_update(parse(cmd, p)?),
            "Set" | "SetNotify" | "CreateHints" | "UpdateHint" => Err(Step::from(invalid(
                "cmd",
                Some(cmd),
                format!("{cmd} is not supported by this server yet"),
            ))),
            _ => Err(Step::from(invalid("cmd", Some(cmd), format!("unknown command {cmd:?}")))),
        }
    }

    async fn connect(&mut self, c: Connect) -> Result<Vec<String>, Step> {
        let room = Arc::clone(&self.room);
        let (errors, ok) = refusals(&room, &c);
        let Some((team, slot, handling)) = ok.filter(|_| errors.is_empty()) else {
            return Ok(vec![wire::frame(&[ServerPacket::ConnectionRefused { errors }])]);
        };
        let previous = self.joined.as_ref().map(|j| (j.team, j.slot));
        let Subscription { history, live } =
            self.room.room.subscribe(Seq(0)).await.map_err(|_| Step::Closed)?;
        let mut j = Joined {
            team,
            slot: slot.id,
            tags: c.tags.iter().cloned().collect(),
            handling,
            sent: 0,
            items: Vec::new(),
            checked: BTreeSet::new(),
            goals: BTreeSet::new(),
            live,
            cursor: Seq(0),
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
                self.fan(old, &Fanout::Print { print: part_print(old_team, old) }).await?;
            }
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
            Published::Events(evs) => events_packets(&room, j, evs),
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
                    Ok(Fanout::Print { .. }) => Vec::new(),
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
        let packets = events_packets(&room, j, &history);
        Ok(if packets.is_empty() { Vec::new() } else { vec![wire::frame(&packets)] })
    }

    /// The client is gone: tell the others.
    pub async fn leave(&mut self) {
        let Some(j) = self.joined.take() else { return };
        let print = part_print(j.team, j.slot);
        let body = serde_json::to_value(Fanout::Print { print }).unwrap_or(Value::Null);
        // The room may already be gone; nobody is left to tell then.
        drop(self.room.room.ext(SlotNo(j.slot), Arc::new(body)).await);
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
            let out = self.room.room.command(Command::Goal { slot: SlotNo(slot) });
            out.await.map_err(|_| Step::Closed)?.map_err(|_| Step::Closed)?;
        }
        // Other statuses are accepted and not stored yet (docs/compat-status.md).
        Ok(Vec::new())
    }

    async fn say(&mut self, req: Say) -> Result<Vec<String>, Step> {
        if req.text.len() > self.room.limits.chat_max_bytes {
            return Err(Step::from(invalid("arguments", Some("Say"), "Say: text too long")));
        }
        Self::rate(&mut self.fanout, "Say")?;
        let j = self.joined.as_ref().ok_or(Step::Closed)?;
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

    async fn bounce(&mut self, p: Value) -> Result<Vec<String>, Step> {
        if p.to_string().len() > self.room.limits.bounce_max_bytes {
            return Err(Step::from(invalid(
                "arguments",
                Some("Bounce"),
                "Bounce: packet too large",
            )));
        }
        Self::rate(&mut self.fanout, "Bounce")?;
        let j = self.joined.as_ref().ok_or(Step::Closed)?;
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

    fn get(&self, p: Value) -> Result<Vec<String>, Step> {
        let Value::Object(mut extra) = p else {
            return Err(Step::from(invalid("arguments", Some("Get"), "Get: not an object")));
        };
        let req: Get = parse("Get", Value::Object(extra.clone()))?;
        extra.remove("cmd");
        extra.remove("keys");
        let keys = req.keys.into_iter().map(|k| {
            let v = self.read_key(&k);
            (k, v)
        });
        let packet = ServerPacket::Retrieved { keys: keys.collect(), extra };
        Ok(vec![wire::frame(&[packet])])
    }

    /// The read-only special keys we can answer; everything else is `null` (no data storage).
    fn read_key(&self, key: &str) -> Value {
        let j = self.joined.as_ref();
        let b = &self.room.bundle;
        if let Some(rest) = key.strip_prefix("_read_client_status_") {
            let slot = rest.split_once('_').and_then(|(_, s)| s.parse::<u32>().ok());
            let goal = slot.is_some_and(|s| j.is_some_and(|j| j.goals.contains(&s)));
            return if goal { json!(CLIENT_GOAL) } else { json!(0) };
        }
        if key.starts_with("_read_hints_") {
            return json!([]); // no hints yet
        }
        if key == "_read_race_mode" {
            return json!(b.race_mode);
        }
        if let Some(rest) = key.strip_prefix("_read_slot_data_") {
            let slot = rest.parse::<u32>().ok().and_then(|s| b.slot(s));
            return slot.map_or(Value::Null, |s| s.slot_data.clone());
        }
        Value::Null
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
        EventKind::Goal => {
            j.goals.insert(slot);
            None
        }
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
/// checks, one `ReceivedItems` for its new items (continuing its index), and the prints.
fn events_packets(room: &CompatRoom, j: &mut Joined, evs: &[Event]) -> Vec<ServerPacket> {
    let (mut checked, mut items, mut prints) = (Vec::new(), Vec::new(), Vec::new());
    for e in evs {
        let slot = e.slot.map_or(0, |s| s.0);
        let was = j.checked.len();
        let item = fold(room, j, e);
        match (e.kind, item) {
            (EventKind::Checked { location, .. }, _) if j.checked.len() != was => {
                checked.push(location);
            }
            (EventKind::Item { .. }, Some(it)) => {
                if slot == j.slot && j.handling.wants(j.slot, &it) {
                    items.push(it);
                }
                if !j.no_text() {
                    prints.push(item_send(slot, it));
                }
            }
            (EventKind::Goal, _) if !j.no_text() => prints.push(goal_print(j.team, slot)),
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
    // so their logs read the same with us (compatibility, not a copy of their code).
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
