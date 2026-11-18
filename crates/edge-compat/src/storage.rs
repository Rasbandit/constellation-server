//! Data storage on the wire (protocol doc, sections "Get", "Set", "`SetNotify`", "`SetReply`";
//! docs/protocol-plan.md section 1): parsing a `Set` into a room-core key write, building
//! `SetReply`, and the read-only `_read_` keys. The room owns the stored values; this module only
//! translates.
#![allow(
    clippy::redundant_pub_crate,
    reason = "rustc's unreachable_pub (denied) needs crate visibility in this private module"
)]

use crate::CompatRoom;
use crate::wire::CLIENT_GOAL;
use proto::{KvOp, KvWrite, SlotNo};
use room_core::{KvRefusal, RESERVED_KEY_PREFIX};
use seed_bundle::{Slot, SlotKind};
use serde_json::{Map, Value, json};

/// The wire name of each operation (protocol doc, "`DataStorageOperation`").
fn op_named(name: &str) -> Option<KvOp> {
    Some(match name {
        "replace" => KvOp::Replace,
        "default" => KvOp::Default,
        "add" => KvOp::Add,
        "mul" => KvOp::Mul,
        "pow" => KvOp::Pow,
        "mod" => KvOp::Mod,
        "floor" => KvOp::Floor,
        "ceil" => KvOp::Ceil,
        "max" => KvOp::Max,
        "min" => KvOp::Min,
        "and" => KvOp::And,
        "or" => KvOp::Or,
        "xor" => KvOp::Xor,
        "left_shift" => KvOp::LeftShift,
        "right_shift" => KvOp::RightShift,
        "remove" => KvOp::Remove,
        "pop" => KvOp::Pop,
        "update" => KvOp::Update,
        _ => return None,
    })
}

/// A `Set` packet: the room-core write, and whether the sender asked for a reply.
#[derive(Debug)]
pub(crate) struct SetRequest {
    pub(crate) write: KvWrite,
    pub(crate) want_reply: bool,
}

/// Python truthiness, which the reference applies to `want_reply` (`MultiServer.py:2301`).
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Parse a `Set`. `Err` is the text of the `InvalidPacket`.
///
/// Deviation, docs/compat-status.md: `default`, `floor` and `ceil` may leave out `value`, as the
/// protocol doc says they ignore it (the reference reads it for every operation).
pub(crate) fn parse_set(slot: u32, p: &Map<String, Value>) -> Result<SetRequest, String> {
    let key = p.get("key").and_then(Value::as_str).ok_or("Set: key must be a string")?;
    let ops =
        p.get("operations").and_then(Value::as_array).ok_or("Set: operations must be a list")?;
    let ops = ops
        .iter()
        .map(|o| {
            let name = o.get("operation").and_then(Value::as_str);
            let op = name.and_then(op_named).ok_or_else(|| {
                format!("Set: unknown operation {}", o.get("operation").unwrap_or(&Value::Null))
            })?;
            let value = match (o.get("value"), op) {
                (Some(v), _) => v.clone(),
                (None, KvOp::Default | KvOp::Floor | KvOp::Ceil) => Value::Null,
                (None, _) => return Err(format!("Set: operation {name:?} needs a value")),
            };
            Ok((op, value))
        })
        .collect::<Result<_, String>>()?;
    Ok(SetRequest {
        write: KvWrite {
            slot: SlotNo(slot),
            key: key.to_owned(),
            default: p.get("default").cloned(),
            ops,
        },
        want_reply: p.get("want_reply").is_some_and(truthy),
    })
}

/// Why a write was refused, for the `InvalidPacket` text.
pub(crate) fn refusal_text(r: KvRefusal) -> String {
    let what = match r {
        KvRefusal::ReservedKey => format!("keys starting with {RESERVED_KEY_PREFIX} are read only"),
        KvRefusal::KeyTooLong => "the key is too long".to_owned(),
        KvRefusal::TooManyOps => "too many operations".to_owned(),
        KvRefusal::TooDeep => "a value is nested too deeply".to_owned(),
        KvRefusal::ValueTooLarge => "the value would be too large".to_owned(),
        KvRefusal::RoomFull => "the room's data storage is full".to_owned(),
        KvRefusal::Type(op) => format!("{op:?} does not apply to these values"),
        KvRefusal::ZeroDivision(op) => format!("{op:?} divides by zero"),
        KvRefusal::Overflow(op) => format!("{op:?} gives a number out of range"),
        KvRefusal::Range(op) => format!("{op:?} with an index or shift out of range"),
    };
    format!("Set: {what}; nothing was stored")
}

/// The `SetReply` body (without `cmd`, which the packet type adds) for a write: every field of
/// the `Set` packet (the reference passes the request through, `MultiServer.py:2290-2303`), then
/// `value`, `original_value` (`null` when unknown) and `slot`.
pub(crate) fn set_reply(
    request: Option<&Value>,
    key: &str,
    value: &Value,
    original: Option<&Value>,
    slot: u32,
) -> Map<String, Value> {
    let mut m = match request {
        Some(Value::Object(o)) => o.clone(),
        _ => Map::new(),
    };
    m.remove("cmd");
    m.insert("key".to_owned(), json!(key));
    m.insert("value".to_owned(), value.clone());
    m.insert("original_value".to_owned(), original.cloned().unwrap_or(Value::Null));
    m.insert("slot".to_owned(), json!(slot));
    m
}

/// The `SetReply` body for a read-only key: only `key` and `value` (`MultiServer.py:966, 972`).
pub(crate) fn read_only_reply(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("key".to_owned(), json!(key));
    m.insert("value".to_owned(), value);
    m
}

/// The read-only key of a slot's client status.
pub(crate) fn status_key(team: u32, slot: u32) -> String {
    format!("{RESERVED_KEY_PREFIX}client_status_{team}_{slot}")
}

/// The slot a `<team>_<slot>` key suffix names, written exactly as the reference forms it (so
/// `0_01` names nothing).
fn slot_of<'a>(room: &'a CompatRoom, rest: &str) -> Option<&'a Slot> {
    let (team, slot) = rest.split_once('_')?;
    let s = room.bundle.slot(slot.parse().ok()?)?;
    (team == s.team.to_string() && slot == s.id.to_string()).then_some(s)
}

/// Whether a slot's client status is final (30): its goal is reached, or it is not a player.
/// The reference sets spectators and groups to the goal at load (`MultiServer.py:594-598`) and
/// never changes a goal (`update_client_status`, `MultiServer.py:2316-2317`).
pub(crate) fn done(s: &Slot, goal: bool) -> bool {
    goal || s.kind != SlotKind::Player
}

/// A slot's client status: 30 when [`done`], else what its connections set.
pub(crate) fn status_value(s: &Slot, goal: bool, presence: i64) -> i64 {
    if done(s, goal) { CLIENT_GOAL } else { presence }
}

/// What a `_read_` key needs from outside the bundle: the slot whose client status it names.
pub(crate) fn status_slot<'a>(room: &'a CompatRoom, key: &str) -> Option<&'a Slot> {
    slot_of(room, key.strip_prefix(RESERVED_KEY_PREFIX)?.strip_prefix("client_status_")?)
}

/// The value of a read-only key from the bundle alone, or `None` for the client status keys
/// (the caller fills those). Unknown `_read_` keys are `null`, as in the reference.
pub(crate) fn read_only(room: &CompatRoom, key: &str) -> Option<Value> {
    let b = &room.bundle;
    let rest = key.strip_prefix(RESERVED_KEY_PREFIX).unwrap_or_default();
    let group = |games: &str, pick: fn(&seed_bundle::GameData) -> Value| {
        b.games.get(games).map_or(Value::Null, pick)
    };
    Some(if let Some(r) = rest.strip_prefix("hints_") {
        // No hints yet: every slot's list is empty.
        slot_of(room, r).map_or(Value::Null, |_| json!([]))
    } else if let Some(r) = rest.strip_prefix("slot_data_") {
        let slot = r.parse::<u32>().ok().and_then(|s| b.slot(s)).filter(|s| r == s.id.to_string());
        slot.map_or(Value::Null, |s| s.slot_data.clone())
    } else if let Some(game) = rest.strip_prefix("item_name_groups_") {
        group(game, |g| json!(g.item_name_groups))
    } else if let Some(game) = rest.strip_prefix("location_name_groups_") {
        group(game, |g| json!(g.location_name_groups))
    } else if rest == "race_mode" {
        json!(b.race_mode)
    } else if status_slot(room, key).is_some() {
        return None;
    } else {
        Value::Null
    })
}
