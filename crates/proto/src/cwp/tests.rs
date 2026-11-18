extern crate std;

use super::*;
use crate::{ErrorCode, ItemSeq, Retry, Seq, SlotNo};
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use serde_json::{Value, json};

fn cid(s: &str) -> CmdId {
    CmdId::new(s).unwrap()
}

fn one_client(text: &str) -> Result<ClientFrame, BadFrame> {
    let mut v = decode_client(text);
    assert_eq!(v.len(), 1, "one frame expected");
    v.remove(0)
}

fn one_server(text: &str) -> Result<ServerFrame, BadFrame> {
    let mut v = decode_server(text);
    assert_eq!(v.len(), 1, "one frame expected");
    v.remove(0)
}

// ---- names -------------------------------------------------------------------------------

#[test]
fn type_names_follow_the_core_and_extension_grammar() {
    for ok in ["check", "item.grant", "a1_b.c2", "org.cwp.deathlink/death", "x.mine/ping.pong"] {
        assert!(TypeName::new(ok).is_ok(), "{ok} should be accepted");
    }
    for bad in [
        "",
        "Check",
        "1check",
        "item..grant",
        "item.",
        "/death",
        "cwp/death",
        "org.cwp/",
        "a/b/c",
        "check\n",
    ] {
        assert!(TypeName::new(bad).is_err(), "{bad:?} should be refused");
    }
    assert_eq!(
        TypeName::new("org.cwp.deathlink/death").unwrap().extension(),
        Some("org.cwp.deathlink")
    );
    assert_eq!(TypeName::new("item.grant").unwrap().extension(), None);
}

#[test]
fn command_ids_are_one_to_sixty_four_visible_ascii_characters() {
    assert!(CmdId::new("x".repeat(CMD_ID_MAX)).is_ok(), "64 is the limit");
    assert_eq!(CmdId::new("x".repeat(CMD_ID_MAX.saturating_add(1))), Err(NameError::Length));
    assert_eq!(CmdId::new(""), Err(NameError::Length));
    for bad in ["a b", "a\nb", "\0", "na\u{ef}ve"] {
        assert_eq!(CmdId::new(bad), Err(NameError::Syntax), "{bad:?}");
    }
}

#[test]
fn state_paths_refuse_dot_segments_and_non_canonical_escapes() {
    assert!(StatePath::new("kv/legacy/Energy%20Link").is_ok(), "a needed escape");
    for bad in [
        "",
        "slot//status",
        "/slot",
        "slot/",
        "slot/3 status",
        "kv/a?b",
        "slot/3/../4",
        "./room/info",
        "kv/%zz",
        "kv/%2f",
        "kv/%41",
        "kv/%2E",
    ] {
        assert!(StatePath::new(bad).is_err(), "{bad:?} should be refused");
    }
    assert!(
        StatePath::new("kv/a%2Fb").is_ok(),
        "an encoded slash is one key byte, not a separator"
    );
    assert_eq!(
        StatePath::new("a".repeat(STATE_PATH_MAX.saturating_add(1))),
        Err(NameError::Length)
    );
}

#[test]
fn game_ids_checksums_and_item_uids_have_their_grammars() {
    assert!(GameId::new("dev.example.mosscave").is_ok(), "reverse domain");
    for bad in ["mosscave", "Moss Cave", "dev..x", "dev.example."] {
        assert!(GameId::new(bad).is_err(), "{bad:?}");
    }
    assert!(Checksum::new(format!("sha256:{}", "a".repeat(64))).is_ok(), "64 hex digits");
    assert!(Checksum::new("sha256:5f1c0a").is_err(), "too short");
    assert!(ItemUid::new("iu_7QK2M4XH7QK2M4XH7QK2M4XH7Q").is_ok(), "26 Crockford base32 digits");
    assert!(ItemUid::new("iu_7QK2M4XH").is_err(), "too short for 128 bits");
    assert!(ItemUid::new("iu_7QK2M4XH7QK2M4XH7QK2M4XI").is_err(), "I is not Crockford");
    assert!(
        ItemUid::new("iu_8QK2M4XH7QK2M4XH7QK2M4XH7Q").is_err(),
        "a 128-bit value starts 0 to 7"
    );
}

/// Every string over a small alphabet, up to `len` pieces.
fn strings(alphabet: &[&str], len: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut frontier = vec![String::new()];
    for _ in 0..len {
        let mut next = Vec::new();
        for s in &frontier {
            for a in alphabet {
                next.push(format!("{s}{a}"));
            }
        }
        out.extend(next.iter().cloned());
        frontier = next;
    }
    out
}

fn agrees(name: &str, pattern: &str, max: usize, check: impl Fn(&str) -> bool, cases: &[String]) {
    let re = regex::Regex::new(pattern).unwrap();
    for s in cases {
        let by_schema = re.is_match(s) && !s.is_empty() && s.chars().count() <= max;
        assert_eq!(by_schema, check(s), "{name}: the schema and the Rust check disagree on {s:?}");
    }
}

#[test]
fn schema_patterns_accept_exactly_what_the_rust_checks_accept() {
    let path_alpha = [
        "a", "Z", "0", ".", "_", "~", "-", "/", "%", "%20", "%2E", "%2F", "%41", "%7E", "%zz", " ",
        "\n", "\u{e9}",
    ];
    agrees(
        "StatePath",
        StatePath::PATTERN,
        StatePath::MAX,
        |s| StatePath::new(s).is_ok(),
        &strings(&path_alpha, 4),
    );
    let name_alpha = ["a", "z", "0", "_", ".", "/", "A", "-", "\n", " "];
    let names = strings(&name_alpha, 5);
    agrees("TypeName", TypeName::PATTERN, TypeName::MAX, |s| TypeName::new(s).is_ok(), &names);
    agrees("ExtId", ExtId::PATTERN, ExtId::MAX, |s| ExtId::new(s).is_ok(), &names);
    agrees(
        "StreamName",
        StreamName::PATTERN,
        StreamName::MAX,
        |s| StreamName::new(s).is_ok(),
        &names,
    );
    agrees("GameId", GameId::PATTERN, GameId::MAX, |s| GameId::new(s).is_ok(), &names);
    let cid_alpha = ["a", "!", "~", " ", "\n", "\0", "\u{e9}", "\u{7f}"];
    agrees("CmdId", CmdId::PATTERN, CmdId::MAX, |s| CmdId::new(s).is_ok(), &strings(&cid_alpha, 3));
}

#[test]
fn secrets_never_print_their_value() {
    let f =
        one_client(r#"{"op":"hello","proto":[1],"token":"cst1_SECRET","client":{"kind":"game"}}"#)
            .unwrap();
    assert!(!format!("{f:?}").contains("SECRET"), "Debug leaks the token");
    let s = Secret::new("cst1_SECRET").unwrap();
    assert!(!s.to_string().contains("SECRET"), "Display leaks the token");
    assert_eq!(
        serde_json::to_value(&s).unwrap(),
        json!("cst1_SECRET"),
        "the wire still carries it"
    );
}

#[test]
fn names_can_be_looked_up_by_str() {
    let mut m = BTreeMap::new();
    m.insert(ExtId::new("org.cwp.deathlink").unwrap(), 1);
    let ty = TypeName::new("org.cwp.deathlink/death").unwrap();
    assert_eq!(m.get(ty.extension().unwrap()), Some(&1), "Borrow<str> lookup");
    assert_eq!("check".parse::<TypeName>().unwrap().as_str(), "check");
}

// ---- integers ----------------------------------------------------------------------------

#[test]
fn integers_above_two_to_the_53_are_refused_both_ways() {
    let ok = format!(r#"{{"op":"ack","applied":{MAX_SAFE}}}"#);
    assert!(one_client(&ok).is_ok(), "2^53 - 1 is allowed");
    let big = format!(r#"{{"op":"ack","applied":{}}}"#, MAX_SAFE.saturating_add(1));
    assert!(one_client(&big).is_err(), "2^53 is refused");
    let frame = ClientFrame::Ack(Ack { applied: ItemSeq(MAX_SAFE.saturating_add(1)) });
    assert!(serde_json::to_string(&frame).is_err(), "and never sent");
    let neg = Check { location: -MAX_SAFE_I64 };
    assert!(serde_json::to_value(neg).is_ok(), "the negative bound is allowed");
    assert!(
        serde_json::from_value::<Check>(json!({"location": i64::MIN})).is_err(),
        "below it is not"
    );
}

// ---- frames ------------------------------------------------------------------------------

#[test]
fn a_minimal_hello_wants_every_item_and_presents_one_credential() {
    let ClientFrame::Hello(h) =
        one_client(r#"{"op":"hello","proto":[1],"token":"t","client":{"kind":"game"}}"#).unwrap()
    else {
        panic!("not a hello")
    };
    assert_eq!(h.want.items, WantItems { from_others: true, own: true, starting: true });
    assert!(
        !h.want.slot_data && h.exts.is_empty() && h.resume.is_none(),
        "optional fields default"
    );
    assert!(matches!(h.credential(), Some(Credential::Token(_))));
    let both = one_client(
        r#"{"op":"hello","proto":[1],"token":"t","ticket":"u","client":{"kind":"game"}}"#,
    )
    .unwrap();
    let ClientFrame::Hello(both) = both else { panic!("not a hello") };
    assert_eq!(both.credential(), None, "token and ticket together are refused");
    assert!(one_client(r#"{"op":"hello","proto":[1],"token":"t"}"#).is_err(), "client is required");
}

#[test]
fn unknown_ops_decode_as_unknown_and_unknown_fields_are_ignored() {
    assert_eq!(one_client(r#"{"op":"teleport","to":"moon"}"#).unwrap(), ClientFrame::Unknown);
    assert_eq!(one_server(r#"{"op":"moving"}"#).unwrap(), ServerFrame::Unknown);
    assert!(
        one_client(r#"{"op":"ack","applied":3,"later":true}"#).is_ok(),
        "D1.8: ignore unknown fields"
    );
}

#[test]
fn a_result_has_exactly_one_of_ok_and_error_whatever_the_key_order() {
    let e = r#"{"code":1002,"key":"type.unknown","retry":"never"}"#;
    for both in [
        format!(r#"{{"op":"result","cid":"c","upto":0,"ok":{{}},"error":{e}}}"#),
        format!(r#"{{"op":"result","cid":"c","upto":0,"error":{e},"ok":{{}}}}"#),
    ] {
        assert!(one_server(&both).is_err(), "both keys: {both}");
    }
    assert!(one_server(r#"{"op":"result","cid":"c","upto":0}"#).is_err(), "neither key");
    let ServerFrame::Result(r) =
        one_server(r#"{"op":"result","cid":"c","upto":0,"ok":null}"#).unwrap()
    else {
        panic!("not a result")
    };
    assert_eq!(r.outcome, Outcome::Ok(Value::Null), "ok: null is a value");
    let ok = ServerFrame::Result(CmdResult::ok(cid("c"), Seq(4), json!({})));
    assert_eq!(
        serde_json::to_value(&ok).unwrap(),
        json!({"op":"result","cid":"c","upto":4,"ok":{}})
    );
    let err = ServerFrame::Result(CmdResult::error(cid("c"), Seq(4), ErrorCode::TypeUnknown));
    assert_eq!(
        serde_json::to_value(&err).unwrap(),
        json!({"op":"result","cid":"c","upto":4,"error":{"code":1002,"key":"type.unknown","retry":"never","detail":{}}})
    );
    assert!(one_server(r#"{"op":"result","cid":"c","ok":{}}"#).is_err(), "upto is required");
}

#[test]
fn clients_ping_with_presence_and_servers_pong() {
    assert_eq!(one_client(r#"{"op":"ping"}"#).unwrap(), ClientFrame::Ping(Beat::default()));
    let ClientFrame::Ping(b) = one_client(r#"{"op":"ping","n":7,"p":"menu"}"#).unwrap() else {
        panic!("not a ping")
    };
    assert_eq!((b.n, b.p.as_ref().map(StreamName::as_str)), (Some(7), Some("menu")));
    assert!(
        matches!(one_server(r#"{"op":"pong","n":7}"#).unwrap(), ServerFrame::Pong(_)),
        "pong is the server's"
    );
    assert_eq!(
        one_client(r#"{"op":"pong"}"#).unwrap(),
        ClientFrame::Unknown,
        "clients never send pong"
    );
    assert_eq!(
        one_server(r#"{"op":"ping"}"#).unwrap(),
        ServerFrame::Unknown,
        "servers never send ping"
    );
}

#[test]
fn values_a_newer_peer_adds_are_tolerated() {
    let r = one_server(
        r#"{"op":"result","cid":"c","upto":0,"error":{"code":6104,"key":"lobby.stale_ready","retry":"immediate"}}"#,
    );
    assert!(r.is_ok(), "doc 09's immediate retry decodes");
    let r = one_server(
        r#"{"op":"result","cid":"c","upto":0,"error":{"code":6999,"key":"x.new","retry":"later"}}"#,
    )
    .unwrap();
    let ServerFrame::Result(CmdResult { outcome: Outcome::Error(e), .. }) = r else {
        panic!("an error result")
    };
    assert_eq!(e.retry, Retry::Other, "an unknown retry hint reads as Other");
    let caller: Caller =
        serde_json::from_value(json!({"kind":"device","scopes":["admin"],"roles":["streamer"]}))
            .unwrap();
    assert_eq!(
        (caller.kind, caller.scopes.clone(), caller.roles.clone()),
        (CallerKind::Other, vec![Scope::Other], vec![RoomRole::Other])
    );
    let none = Allow { scopes: &[Scope::Play], roles: &[RoomRole::Owner], step_up: false };
    assert!(!none.permits(&caller), "unknown scopes and roles grant nothing");
    let mut g = sample("item.grant");
    g["source"] = json!("gift");
    let ev = EventFrame {
        seq: Some(Seq(1)),
        eseq: None,
        ty: TypeName::new("item.grant").unwrap(),
        slot: Some(SlotNo(1)),
        t: None,
        prev_hash: None,
        data: g,
    };
    let CoreEvent::ItemGrant(g) = CoreEvent::decode(&ev).unwrap() else { panic!("a grant") };
    assert_eq!(g.source, Source::Other, "an unknown source still delivers the item");
    let w: ResumeOutcome = serde_json::from_value(json!({"status":"moved"})).unwrap();
    assert_eq!(w.status, ResumeStatus::Other);
}

#[test]
fn error_bodies_carry_a_ref_and_know_their_code() {
    let e = ErrorBody::from(ErrorCode::OpUnknown).with_ref("teleport");
    assert_eq!(
        serde_json::to_value(&e).unwrap(),
        json!({"code":1004,"key":"op.unknown","retry":"never","ref":"teleport","detail":{}})
    );
    assert_eq!(e.registered(), Some(ErrorCode::OpUnknown));
}

// ---- codec -------------------------------------------------------------------------------

fn answer_key(f: Result<ClientFrame, BadFrame>) -> (Option<String>, String) {
    match f {
        Err(BadFrame::Answer { cid, error }) => (Some(cid.to_string()), error.key),
        Err(BadFrame::Error(e)) => (None, e.key),
        Ok(f) => panic!("decoded: {f:?}"),
    }
}

#[test]
fn a_bad_field_still_gets_a_result_for_its_cid() {
    let cases = [
        (r#"{"op":"cmd","cid":"c-9","type":"Teleport"}"#, "type.unknown"),
        (r#"{"op":"get","cid":"c-9","path":"slot/1/../x"}"#, "path.unknown"),
        (r#"{"op":"watch","cid":"c-9","paths":["room/info","bad path"]}"#, "path.unknown"),
        (r#"{"op":"get","cid":"c-9","path":"room/info","limit":-1}"#, "data.invalid"),
    ];
    for (text, key) in cases {
        assert_eq!(answer_key(one_client(text)), (Some("c-9".into()), key.into()), "{text}");
    }
    assert_eq!(
        answer_key(one_client(r#"{"op":"ack","applied":-1}"#)),
        (None, "data.invalid".into()),
        "no cid: an error frame"
    );
    let Err(BadFrame::Error(e)) = one_client(r#"{"op":"ack","applied":-1}"#) else {
        panic!("an error frame")
    };
    assert_eq!(e.reference.as_deref(), Some("ack"), "the error names the op it answers");
    let Err(BadFrame::Error(e)) = one_client(r#"{"op":"ack","cid":"a-1","applied":-1}"#) else {
        panic!("an error frame")
    };
    assert_eq!(e.reference.as_deref(), Some("a-1"), "or its cid, when it has one");
    let big = r#"{"op":"cmd","cid":"c-9","type":"org.cwp.x/y","data":{"n":18446744073709551615}}"#;
    assert_eq!(
        answer_key(one_client(big)),
        (Some("c-9".into()), "data.invalid".into()),
        "free-form data is range-checked"
    );
    let deep = r#"{"op":"cmd","cid":"c-9","type":"org.cwp.x/y","data":{"a":[{"f":1e300}]}}"#;
    assert_eq!(
        answer_key(one_client(deep)),
        (Some("c-9".into()), "data.invalid".into()),
        "however deep"
    );
    assert!(
        one_client(r#"{"op":"cmd","cid":"c-9","type":"org.cwp.x/y","data":{"f":2.5}}"#).is_ok(),
        "fractions are fine"
    );
    assert_eq!(answer_key(one_client("{nope")), (None, "data.invalid".into()), "not JSON");
}

#[test]
fn a_message_may_carry_an_array_of_frames() {
    let v = decode_client(
        r#"[{"op":"ping"},{"op":"ack","applied":2},{"op":"cmd","cid":"x","type":"BAD"}]"#,
    );
    assert_eq!(v.len(), 3);
    assert!(v[0].is_ok() && v[1].is_ok() && v[2].is_err(), "each frame decodes on its own");
}

// ---- registry ----------------------------------------------------------------------------

#[test]
fn registry_rows_are_unique_and_well_formed() {
    let mut seen = BTreeSet::new();
    for t in CORE_TYPES {
        assert!(seen.insert(t.name), "{} registered twice", t.name);
        assert!(TypeName::new(t.name).unwrap().extension().is_none(), "{} is a core type", t.name);
        let event = t.kind == Kind::Event;
        assert_eq!(
            t.delivery.is_some(),
            event,
            "{}: events and only events have a delivery class",
            t.name
        );
        assert_eq!(
            t.audience.is_some(),
            event,
            "{}: events and only events have an audience",
            t.name
        );
        assert_eq!(
            t.allow.is_some(),
            !event,
            "{}: commands and only commands have an allow rule",
            t.name
        );
        assert!(t.max_bytes > 0, "{} has no size limit", t.name);
        assert_eq!(lookup(t.name), Some(t));
    }
    assert_eq!(lookup("no.such"), None);
    for e in COMMON_CMD_ERRORS {
        assert!(ErrorCode::ALL.contains(e), "{} is registered", e.key());
    }
}

/// A sample `data` for every registered type; a new row without a sample fails here.
fn sample(name: &str) -> Value {
    match name {
        "check" | "location.checked" => json!({"location": 104}),
        "goal" | "slot.goal" => json!({}),
        "status.set" => json!({"client_state": "ready"}),
        "session.update" => json!({"exts": {}}),
        "item.grant" => {
            json!({"source":"check","from":1,"location":104,"item":7,"item_seq":1,"item_uid":"iu_7QK2M4XH7QK2M4XH7QK2M4XH7Q","flags":["trap"]})
        }
        "room.epoch_change" => json!({"from": 1, "to": 2, "recovered_head": 40}),
        "state.changed" => json!({"path": "room/info", "value": {}, "ver": 3}),
        other => panic!("no sample for registered type {other}"),
    }
}

#[test]
fn every_registered_type_decodes_and_encodes_back() {
    for t in CORE_TYPES {
        let ty = TypeName::new(t.name).unwrap();
        let data = sample(t.name);
        match t.kind {
            Kind::Cmd => {
                let c = Cmd { cid: cid("c"), ty, data };
                let decoded = CoreCmd::decode(&c).unwrap_or_else(|e| panic!("{}: {e}", t.name));
                assert_eq!(decoded.type_name(), t.name);
                assert_eq!(decoded.to_cmd(cid("c")).unwrap(), c, "{} round trip", t.name);
            }
            Kind::Event => {
                let (seq, eseq, at) = match t.delivery {
                    Some(Delivery::Durable) => (Some(Seq(5)), None, Delivered::Durable(Seq(5))),
                    _ => (None, Some(1), Delivered::Ephemeral(1)),
                };
                let ev = EventFrame {
                    seq,
                    eseq,
                    ty,
                    slot: Some(SlotNo(1)),
                    t: None,
                    prev_hash: None,
                    data,
                };
                let decoded = CoreEvent::decode(&ev).unwrap_or_else(|e| panic!("{}: {e}", t.name));
                assert_eq!(decoded.type_name(), t.name);
                assert_eq!(
                    decoded.to_frame(at, Some(SlotNo(1)), None).unwrap(),
                    ev,
                    "{} round trip",
                    t.name
                );
            }
        }
    }
}

#[test]
fn decode_tells_extensions_unknowns_and_bad_payloads_apart() {
    let cmd = |ty: &str, data: Value| Cmd { cid: cid("c"), ty: TypeName::new(ty).unwrap(), data };
    let key = |r: Result<CoreCmd, DecodeError>| r.unwrap_err().error_code().map(ErrorCode::key);
    assert_eq!(key(CoreCmd::decode(&cmd("org.cwp.deathlink/death", json!({})))), None, "extension");
    assert_eq!(key(CoreCmd::decode(&cmd("teleport", json!({})))), Some("type.unknown"));
    assert_eq!(
        key(CoreCmd::decode(&cmd("item.grant", json!({})))),
        Some("type.unknown"),
        "an event name is not a command"
    );
    assert_eq!(
        key(CoreCmd::decode(&cmd("check", json!([104])))),
        Some("data.invalid"),
        "arrays are not payloads"
    );
    assert_eq!(
        key(CoreCmd::decode(&cmd("goal", json!([])))),
        Some("data.invalid"),
        "not even for empty payloads"
    );
    assert_eq!(key(CoreCmd::decode(&cmd("check", json!({"location": "x"})))), Some("data.invalid"));
    assert_eq!(
        CoreCmd::decode(&cmd("check", json!({"location": -9}))).unwrap(),
        CoreCmd::Check(Check { location: -9 })
    );
}

#[test]
fn event_decode_checks_the_frame_rules_of_the_type() {
    let base = |ty: &str, data: Value| EventFrame {
        seq: Some(Seq(1)),
        eseq: None,
        ty: TypeName::new(ty).unwrap(),
        slot: Some(SlotNo(1)),
        t: None,
        prev_hash: None,
        data,
    };
    let grant = sample("item.grant");
    assert!(
        CoreEvent::decode(&EventFrame { slot: None, ..base("item.grant", grant.clone()) }).is_err(),
        "a grant names its receiver"
    );
    assert!(
        CoreEvent::decode(&EventFrame {
            seq: None,
            eseq: Some(1),
            ..base("item.grant", grant.clone())
        })
        .is_err(),
        "grants are durable"
    );
    assert!(
        CoreEvent::decode(&base("state.changed", sample("state.changed"))).is_err(),
        "state.changed is ephemeral"
    );
    let mut start = grant.clone();
    start["source"] = json!("start");
    assert!(
        CoreEvent::decode(&base("item.grant", start.clone())).is_err(),
        "a starting item names no location"
    );
    start.as_object_mut().unwrap().remove("from");
    start.as_object_mut().unwrap().remove("location");
    assert!(CoreEvent::decode(&base("item.grant", start)).is_ok(), "now it is consistent");
    let mut far = grant.clone();
    far["location"] = json!(i64::MAX);
    assert!(CoreEvent::decode(&base("item.grant", far)).is_err(), "location within the CWP range");
    let mut flag = grant;
    flag["flags"] = json!(["shiny"]);
    let CoreEvent::ItemGrant(g) = CoreEvent::decode(&base("item.grant", flag)).unwrap() else {
        panic!("a grant")
    };
    assert_eq!(g.flags, vec![ItemFlag::Other], "unknown flags are kept as Other");
}

/// Placeholders of a template: `(field, kind, slot field)`.
fn placeholders(text: &str) -> Vec<(String, String, Option<String>)> {
    text.split('{')
        .skip(1)
        .map(|p| {
            let inner = p.split('}').next().unwrap();
            let (field, rest) = inner.split_once(':').unwrap_or_else(|| panic!("{inner}: no kind"));
            let (kind, at) = match rest.split_once('@') {
                Some((k, a)) => (k.to_owned(), Some(a.to_owned())),
                None => (rest.to_owned(), None),
            };
            (field.to_owned(), kind, at)
        })
        .collect()
}

/// Whether a template condition holds for `data` on a frame about `slot`.
fn holds(when: &str, data: &Value, slot: u32) -> bool {
    let get = |f: &str| if f == "slot" { Some(json!(slot)) } else { data.get(f).cloned() };
    if let Some((a, b)) = when.split_once('=') {
        return get(a).is_some() && get(a) == get(b);
    }
    let (field, lit) = when.split_once(':').unwrap_or_else(|| panic!("condition {when}"));
    get(field) == Some(json!(lit))
}

fn check_template(name: &str, data: &Value, text: &str) {
    let has = |f: &str| f == "slot" || data.get(f).is_some();
    for (field, kind, at) in placeholders(text) {
        assert!(has(&field), "{name}: {{{field}}} is missing in {data}");
        assert!(
            ["slot", "item", "location", "text"].contains(&kind.as_str()),
            "{name}: kind {kind}"
        );
        let needs_game = kind == "item" || kind == "location";
        assert_eq!(at.is_some(), needs_game, "{name}: {field} needs @slotfield exactly for ids");
        if let Some(at) = at {
            assert!(has(&at), "{name}: @{at} is not a field");
        }
    }
}

#[test]
fn templates_render_for_every_source_and_use_known_fields() {
    for t in CORE_TYPES {
        if let Some(last) = t.templates.last() {
            assert!(last.when.is_none(), "{}: the last variant must always apply", t.name);
        }
        let mut cases = vec![sample(t.name)];
        if t.name == "item.grant" {
            for source in ["release", "collect", "link", "start", "admin"] {
                let mut d = sample(t.name);
                d["source"] = json!(source);
                if source == "start" || source == "admin" {
                    d.as_object_mut().unwrap().remove("from");
                    d.as_object_mut().unwrap().remove("location");
                }
                cases.push(d);
            }
            let mut own = sample(t.name);
            own["from"] = json!(1);
            cases.push(own);
        }
        for data in &cases {
            if let Some(v) = t.templates.iter().find(|v| v.when.is_none_or(|w| holds(w, data, 1))) {
                check_template(t.name, data, v.text);
            }
        }
    }
}

// ---- paths -------------------------------------------------------------------------------

#[test]
fn core_paths_parse_on_segments_and_every_one_has_a_row() {
    let p = |s: &str| CorePath::parse(&StatePath::new(s).unwrap());
    assert_eq!(p("slot/3/status"), Ok(CorePath::Slot(SlotNo(3), SlotPart::Status)));
    assert_eq!(p("kv/slot.2/x"), Ok(CorePath::Kv(KvScope::Slot(SlotNo(2)), "x".into())));
    assert_eq!(
        p("ext/org.cwp.lobby/board/host"),
        Ok(CorePath::Ext(ExtId::new("org.cwp.lobby").unwrap(), "board/host".into()))
    );
    for unknown in [
        "slot/03/status",
        "slot/x/status",
        "slot/3",
        "slot/3/secrets",
        "room",
        "game/Moss/names",
        "kv/other/x",
        "kv/slot.03/x",
        "ext/org.cwp.lobby",
        "rooms/info",
    ] {
        assert_eq!(p(unknown), Err(UnknownPath), "{unknown}");
    }
    let samples = [
        "room/info",
        "room/slots",
        "slot/1/status",
        "slot/1/locations",
        "slot/1/inventory",
        "slot/1/slot_data",
        "slot/1/hints",
        "game/dev.example.mosscave/names",
        "game/dev.example.mosscave/groups",
        "kv/room/k",
        "ext/x.mine/a",
    ];
    let reached: BTreeSet<&str> =
        samples.iter().map(|s| p(s).unwrap().info().unwrap().pattern).collect();
    let all: BTreeSet<&str> = CORE_PATHS.iter().map(|i| i.pattern).collect();
    assert_eq!(reached, all, "every registered pattern parses, and every parse has a row");
    assert_eq!(p("slot/4/hints").unwrap().subject(), Some(SlotNo(4)));
    assert_eq!(p("room/info").unwrap().subject(), None);
}

// ---- schema snapshot ---------------------------------------------------------------------

#[derive(schemars::JsonSchema)]
struct Empty {}

fn schema_document() -> Value {
    let mut g = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let mut types = BTreeMap::new();
    for t in CORE_TYPES {
        let data = match t.name {
            "check" => g.subschema_for::<Check>(),
            "goal" => g.subschema_for::<Goal>(),
            "status.set" => g.subschema_for::<StatusSet>(),
            "session.update" => g.subschema_for::<SessionUpdate>(),
            "location.checked" => g.subschema_for::<LocationChecked>(),
            "item.grant" => g.subschema_for::<ItemGrant>(),
            "slot.goal" => g.subschema_for::<SlotGoal>(),
            "room.epoch_change" => g.subschema_for::<EpochChange>(),
            "state.changed" => g.subschema_for::<StateChanged>(),
            other => panic!("registered type {other} has no payload schema here"),
        };
        let mut entry = serde_json::to_value(t).unwrap();
        entry["data"] = serde_json::to_value(data).unwrap();
        if t.kind == Kind::Cmd {
            let result = match t.name {
                "check" => g.subschema_for::<CheckOk>(),
                "session.update" => g.subschema_for::<SessionUpdateOk>(),
                _ => g.subschema_for::<Empty>(),
            };
            entry["result"] = serde_json::to_value(result).unwrap();
        }
        types.insert(t.name, entry);
    }
    let client = serde_json::to_value(g.subschema_for::<ClientFrame>()).unwrap();
    let server = serde_json::to_value(g.subschema_for::<ServerFrame>()).unwrap();
    let get_ok = serde_json::to_value(g.subschema_for::<GetOk>()).unwrap();
    let watch_ok = serde_json::to_value(g.subschema_for::<WatchOk>()).unwrap();
    let errors: Vec<Value> = ErrorCode::ALL
        .iter()
        .map(|e| json!({"code": e.code(), "key": e.key(), "retry": e.retry()}))
        .collect();
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "CWP v1 (design doc 01 D1.10, proposal D-123). Generated by `cargo test -p proto`; do not edit.",
        "client_frame": client,
        "server_frame": server,
        "results": {"get": get_ok, "watch": watch_ok},
        "types": types,
        "paths": CORE_PATHS,
        "errors": errors,
        "limits": {"cmd_id_max": CMD_ID_MAX, "name_max": NAME_MAX, "state_path_max": STATE_PATH_MAX, "max_safe_integer": MAX_SAFE},
        "$defs": open_enums(g.take_definitions(true)),
    })
}

/// Catch-all variants (`unknown` ops, `other` values) accept, in the schema as in Rust, any
/// string that is not one of the known values: the catch-all branch's `const` becomes
/// `not: {enum: [known values]}`, so the branches stay mutually exclusive (`oneOf`) and a
/// malformed known frame is not accepted as "unknown".
fn open_enums(mut defs: serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    fn tag(branch: &Value) -> Option<&Value> {
        branch.get("const").or_else(|| branch.pointer("/properties/op/const"))
    }
    let is_open = |v: &Value| matches!(v.as_str(), Some("unknown" | "other"));
    for def in defs.values_mut() {
        let Some(Value::Array(branches)) = def.get_mut("oneOf") else { continue };
        let known: Vec<Value> =
            branches.iter().filter_map(tag).filter(|v| !is_open(v)).cloned().collect();
        for b in branches.iter_mut() {
            if !tag(b).is_some_and(is_open) {
                continue;
            }
            let open = json!({"type": "string", "not": {"enum": known.clone()}});
            if b.get("const").is_some() {
                let obj = b.as_object_mut().unwrap();
                obj.remove("const");
                obj.insert("not".into(), json!({"enum": known.clone()}));
            } else if let Some(op) = b.pointer_mut("/properties/op") {
                *op = open;
            }
        }
    }
    defs
}

#[test]
fn the_committed_schema_matches_the_types() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/cwp-v1.json");
    let generated = schema_document();
    let committed: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    if committed != generated {
        let new = std::env::temp_dir().join("cwp-v1.json.new");
        let text = serde_json::to_string_pretty(&generated).unwrap() + "\n";
        let wrote = std::fs::write(&new, text).is_ok();
        panic!(
            "the CWP schema changed. Review {} (written: {wrote}) and, if the change is intended, copy it over {}",
            new.display(),
            path.display()
        );
    }
}

// ---- golden transcripts ------------------------------------------------------------------

/// What a frame that expects a `result` must be answered with.
#[derive(Debug, Clone)]
enum Expect {
    Ok(CoreCmd),
    Err(&'static str),
    Same(Box<CmdResult>),
    Any,
    Get(StatePath),
    Watch(Vec<StatePath>),
    Unwatch(Vec<StatePath>),
}

/// An open request: what it expects, and the fingerprint of a `cmd` (for dedupe).
#[derive(Debug, Clone)]
struct Open {
    expect: Expect,
    fingerprint: Option<Value>,
}

/// Error keys that end a session (rule 13).
const ENDING: &[&str] = &["room.moving", "slot.in_use", "auth.token_revoked", "room.kicked"];
/// Error frames that only answer a client frame; anything else is unsolicited.
const ANSWERING: &[&str] = &["op.unknown", "ack.invalid"];

#[derive(Default)]
struct Session {
    hello: Option<Box<Hello>>,
    welcome: Option<Box<Welcome>>,
    ended: bool,
    pending: Vec<ClientFrame>,
    enabled: BTreeSet<String>,
    open: BTreeMap<String, Open>,
    done: BTreeMap<String, (Value, CmdResult)>,
    watched: BTreeMap<String, u64>,
    epoch: u64,
    head: u64,
    last_seq: u64,
    replay: Option<(u64, u64)>,
    replay_done: bool,
    eseq: u64,
    applied: u64,
    own_item_seq: u64,
    granted: BTreeMap<String, u64>,
    last_ack: u64,
    paging: bool,
    paged_max: u64,
    expect_error: Vec<&'static str>,
    pongs: Vec<Option<u64>>,
}

/// Every field of `raw` survives in `full` (a raw `null`, `{}` or `[]` matches an absent field,
/// since defaults are left out when encoding).
fn covers(full: &Value, raw: &Value, at: &str) -> Result<(), String> {
    let empty = |v: &Value| {
        v.is_null()
            || v.as_object().is_some_and(serde_json::Map::is_empty)
            || v.as_array().is_some_and(Vec::is_empty)
    };
    match (raw, full) {
        (Value::Object(r), Value::Object(f)) => {
            for (k, v) in r {
                match f.get(k) {
                    Some(fv) => covers(fv, v, &format!("{at}.{k}"))?,
                    None if empty(v) => {}
                    None => return Err(format!("{at}.{k} did not survive a round trip")),
                }
            }
            Ok(())
        }
        (Value::Array(r), Value::Array(f)) if r.len() == f.len() => r
            .iter()
            .zip(f)
            .enumerate()
            .try_for_each(|(i, (rv, fv))| covers(fv, rv, &format!("{at}[{i}]"))),
        _ if raw == full => Ok(()),
        _ => Err(format!("{at}: {raw} became {full}")),
    }
}

fn registered(e: &ErrorBody) -> Result<(), String> {
    let code = e.registered().ok_or_else(|| format!("unregistered error {}", e.key))?;
    if (code.code(), code.retry()) != (e.code, e.retry) {
        return Err(format!("{} sent with the wrong code or retry", e.key));
    }
    Ok(())
}

impl Session {
    fn caller(&self) -> Result<&Caller, String> {
        self.welcome.as_ref().map(|w| &w.caller).ok_or_else(|| "no welcome yet".into())
    }

    fn wants(&self) -> Want {
        self.hello.as_ref().map(|h| h.want.clone()).unwrap_or_default()
    }

    fn client(&mut self, f: &ClientFrame) -> Result<(), String> {
        if self.ended {
            return Err("frame after the session ended".into());
        }
        match (f, self.hello.is_some()) {
            (ClientFrame::Hello(h), false) => {
                self.hello = Some(h.clone());
                return Ok(());
            }
            (ClientFrame::Hello(_), true) => return Err("a second hello".into()),
            (_, false) => return Err("the first client frame must be hello".into()),
            _ => {}
        }
        if self.welcome.is_none() {
            self.pending.push(f.clone());
            return Ok(());
        }
        self.handle_client(f)
    }

    fn handle_client(&mut self, f: &ClientFrame) -> Result<(), String> {
        match f {
            ClientFrame::Cmd(c) => {
                let fp = json!([c.ty.as_str(), c.data]);
                let expect = match self.done.get(c.cid.as_str()) {
                    Some((prev, r)) if *prev == fp => Expect::Same(Box::new(r.clone())),
                    Some(_) => Expect::Err("cid.conflict"),
                    None => self.expect_cmd(c)?,
                };
                let keep = !matches!(expect, Expect::Err("cid.conflict") | Expect::Same(_));
                self.open_cid(c.cid.as_str(), expect, keep.then_some(fp))
            }
            ClientFrame::Get(g) => {
                let e = self.expect_read(&g.path, Expect::Get(g.path.clone()))?;
                self.open_cid(g.cid.as_str(), e, None)
            }
            ClientFrame::Watch(w) => {
                let mut e = Expect::Watch(w.paths.clone());
                for p in &w.paths {
                    if let Expect::Err(k) = self.expect_read(p, Expect::Any)? {
                        e = Expect::Err(k);
                    }
                }
                self.open_cid(w.cid.as_str(), e, None)
            }
            ClientFrame::Unwatch(w) => {
                self.open_cid(w.cid.as_str(), Expect::Unwatch(w.paths.clone()), None)
            }
            ClientFrame::Ack(a) => {
                let no_slot = self.caller()?.slot.is_none();
                let delivered = self.own_item_seq.max(self.paged_max);
                if no_slot || a.applied.0 > delivered {
                    self.expect_error.push("ack.invalid");
                } else {
                    // An ack below the stored cursor lowers it: the game reloaded an older save.
                    self.last_ack = a.applied.0;
                }
                Ok(())
            }
            ClientFrame::Ping(b) => {
                self.pongs.push(b.n);
                Ok(())
            }
            ClientFrame::Unknown => {
                self.expect_error.push("op.unknown");
                Ok(())
            }
            ClientFrame::Hello(_) => Ok(()),
        }
    }

    fn open_cid(
        &mut self,
        c: &str,
        expect: Expect,
        fingerprint: Option<Value>,
    ) -> Result<(), String> {
        if self.open.insert(c.into(), Open { expect, fingerprint }).is_some() {
            return Err(format!("cid {c} is already open"));
        }
        Ok(())
    }

    fn expect_cmd(&self, c: &Cmd) -> Result<Expect, String> {
        if let Some(ext) = c.ty.extension() {
            return Ok(if self.enabled.contains(ext) {
                Expect::Any
            } else {
                Expect::Err("ext.not_enabled")
            });
        }
        match CoreCmd::decode(c) {
            Err(e) => Ok(Expect::Err(e.error_code().map_or("type.unknown", ErrorCode::key))),
            Ok(core) => {
                let allow = lookup(core.type_name())
                    .and_then(|t| t.allow)
                    .ok_or("a command without an allow rule")?;
                if !allow.permits(self.caller()?) {
                    return Ok(Expect::Err("auth.scope_insufficient"));
                }
                Ok(Expect::Ok(core))
            }
        }
    }

    fn expect_read(&self, path: &StatePath, ok: Expect) -> Result<Expect, String> {
        let Ok(parsed) = CorePath::parse(path) else { return Ok(Expect::Err("path.unknown")) };
        let caller = self.caller()?;
        let read = parsed.info().ok_or("a parsed path without a row")?.read;
        let own = parsed.subject().is_some() && parsed.subject() == caller.slot;
        let privileged = !caller.roles.is_empty();
        let allowed = match read {
            Read::Room | Read::Extension => true,
            Read::Subject | Read::SubjectOrSpoiler | Read::SubjectOrPublic => own || privileged,
        };
        Ok(if allowed { ok } else { Expect::Err("path.forbidden") })
    }

    fn server(&mut self, f: &ServerFrame) -> Result<(), String> {
        if self.ended {
            return Err("frame after the session ended".into());
        }
        if self.welcome.is_none() {
            return match f {
                ServerFrame::Welcome(w) => self.welcome(w),
                ServerFrame::Error(e) => {
                    self.ended = true;
                    registered(e)
                }
                _ => Err("only welcome or error may come before welcome".into()),
            };
        }
        match f {
            ServerFrame::Welcome(_) => Err("a second welcome".into()),
            ServerFrame::Result(r) => self.result(r),
            ServerFrame::Event(ev) => self.event(ev),
            ServerFrame::Error(e) => {
                registered(e)?;
                if ANSWERING.contains(&e.key.as_str()) {
                    if self.expect_error.first() != Some(&e.key.as_str()) {
                        return Err(format!("an unsolicited {}", e.key));
                    }
                    self.expect_error.remove(0);
                }
                if ENDING.contains(&e.key.as_str()) {
                    self.ended = true;
                }
                Ok(())
            }
            ServerFrame::Pong(b) => {
                if self.pongs.is_empty() {
                    return Err("pong without a ping".into());
                }
                let n = self.pongs.remove(0);
                if n == b.n { Ok(()) } else { Err(format!("pong must echo the nonce {n:?}")) }
            }
            ServerFrame::Unknown => Ok(()),
        }
    }

    fn welcome(&mut self, w: &Welcome) -> Result<(), String> {
        let h = self.hello.as_ref().ok_or("welcome before hello")?;
        if h.credential().is_none() {
            return Err("a welcome for a hello without exactly one credential".into());
        }
        if !h.proto.contains(&w.proto.major) {
            return Err(format!("welcome picked proto {} that hello did not offer", w.proto.major));
        }
        for (id, choice) in &w.exts {
            let offered =
                h.exts.get(id).ok_or_else(|| format!("extension {id} was not offered"))?;
            if !offered.v.contains(&choice.v) {
                return Err(format!("extension {id} major {} was not offered", choice.v));
            }
        }
        let c = &w.caller;
        match c.kind {
            CallerKind::Slot if c.slot.is_none() => {
                return Err("a slot caller without a slot".into());
            }
            CallerKind::Spectator if c.slot.is_some() || c.scopes.contains(&Scope::Play) => {
                return Err("a spectator with a slot or the play scope".into());
            }
            _ => {}
        }
        if let (Some(s), Some(info)) = (c.slot, &w.slot)
            && s != info.index
        {
            return Err("caller.slot and slot.index differ".into());
        }
        let after = h.resume.map_or(0, |r| r.after.0);
        if h.resume.is_some_and(|r| r.after.0 > 0 && r.epoch.is_none()) {
            return Err("a resume after 0 needs its epoch".into());
        }
        if w.resume.status == ResumeStatus::Ok && w.head.0 < after {
            return Err("a head below the resume cursor needs new_timeline".into());
        }
        match (w.resume.status, &w.replay, &w.resume.epoch_change) {
            (ResumeStatus::TooOld, Some(_), _) => return Err("too_old comes with no replay".into()),
            (ResumeStatus::NewTimeline, _, None) => {
                return Err("new_timeline needs resume.epoch_change".into());
            }
            _ => {}
        }
        if let Some(r) = w.replay {
            if r.from > r.to {
                return Err("replay range is reversed".into());
            }
            if w.head < r.to {
                return Err("replay range outside the log".into());
            }
            // From after + 1, or earlier: items above the applied cursor (doc 02 D3) or a new
            // timeline's recovered head (rule 4).
            if let Some(ec) = &w.resume.epoch_change
                && r.from.0 != ec.recovered_head.0.saturating_add(1)
            {
                return Err("a new timeline replays from recovered_head + 1".into());
            }
            self.replay = Some((r.from.0, r.to.0));
        } else {
            self.replay_done = true;
        }
        self.enabled = w.exts.keys().map(ToString::to_string).collect();
        self.epoch = w.epoch.0;
        self.head = w.head.0;
        self.last_seq =
            w.replay.map_or_else(|| after.max(w.head.0), |r| r.from.0.saturating_sub(1));
        self.paging = w.resume.status == ResumeStatus::TooOld;
        self.applied = w.applied.0;
        self.own_item_seq = w.applied.0;
        self.last_ack = w.applied.0;
        self.welcome = Some(Box::new(w.clone()));
        for f in core::mem::take(&mut self.pending) {
            self.handle_client(&f)?;
        }
        Ok(())
    }

    fn result(&mut self, r: &CmdResult) -> Result<(), String> {
        let c = r.cid.as_str();
        let open =
            self.open.remove(c).ok_or_else(|| format!("result for {c} answers nothing open"))?;
        if let Outcome::Error(e) = &r.outcome {
            registered(e)?;
        }
        let repeat = matches!(open.expect, Expect::Same(_));
        if r.epoch.is_some_and(|e| e.0 > self.epoch || (!repeat && e.0 != self.epoch)) {
            return Err(format!("{c}: result epoch differs from the session's"));
        }
        if r.upto.0 != self.last_seq {
            return Err(format!("{c}: upto must be the last seq sent to the session"));
        }
        // A retryable refusal (rate, pause, unavailable) may answer any command; it is never
        // kept for dedupe, so the retry with the same cid gets a fresh answer (rule 3).
        if let Outcome::Error(e) = &r.outcome
            && e.retry != Retry::Never
        {
            return Ok(());
        }
        match (&open.expect, &r.outcome) {
            (Expect::Err(k), Outcome::Error(e)) if e.key == *k => {}
            (Expect::Err(k), _) => return Err(format!("{c}: expected error {k}")),
            (Expect::Same(prev), _) => {
                if (prev.seq, prev.epoch, &prev.outcome) != (r.seq, r.epoch, &r.outcome) {
                    return Err(format!("{c}: a repeated cid must get the same result"));
                }
            }
            (Expect::Ok(core), Outcome::Ok(v)) => self.ok_result(c, core, v, r)?,
            (Expect::Get(p), Outcome::Ok(v)) => {
                let ok: GetOk = serde_json::from_value(v.clone())
                    .map_err(|e| format!("{c}: get result: {e}"))?;
                if ok.path != *p {
                    return Err(format!("{c}: get answered for another path"));
                }
                self.inventory_page(p, &ok)?;
            }
            (Expect::Watch(ps), Outcome::Ok(v)) => {
                let ok: WatchOk = serde_json::from_value(v.clone())
                    .map_err(|e| format!("{c}: watch result: {e}"))?;
                if ok.paths.iter().map(|s| &s.path).ne(ps.iter()) {
                    return Err(format!("{c}: watch must return each path, in order"));
                }
                for s in ok.paths {
                    self.watched.insert(s.path.to_string(), s.ver.0);
                }
            }
            (Expect::Unwatch(ps), Outcome::Ok(_)) => {
                for p in ps {
                    self.watched.remove(p.as_str());
                }
            }
            (Expect::Any, _) => {}
            (_, Outcome::Error(e)) => return Err(format!("{c}: expected ok, got {}", e.key)),
        }
        if let Some(fp) = open.fingerprint {
            self.done.insert(c.into(), (fp, r.clone()));
        }
        Ok(())
    }

    fn ok_result(
        &mut self,
        c: &str,
        core: &CoreCmd,
        v: &Value,
        r: &CmdResult,
    ) -> Result<(), String> {
        match core {
            CoreCmd::Check(_) => {
                let ok: CheckOk = serde_json::from_value(v.clone())
                    .map_err(|e| format!("{c}: check result: {e}"))?;
                let own = self.caller()?.slot;
                for item in ok.items.iter().filter(|i| Some(i.to) == own) {
                    if !self.granted.contains_key(item.item_uid.as_str()) {
                        return Err(format!(
                            "{c}: result came before its events ({})",
                            item.item_uid
                        ));
                    }
                }
                if !ok.items.is_empty() && (r.seq.is_none() || r.epoch.is_none()) {
                    return Err(format!("{c}: a committing result carries seq and epoch"));
                }
            }
            CoreCmd::Goal(_) | CoreCmd::StatusSet(_) if !v.is_object() => {
                return Err(format!("{c}: {} result must be an object", core.type_name()));
            }
            CoreCmd::Goal(_) | CoreCmd::StatusSet(_) => {}
            CoreCmd::SessionUpdate(_) => {
                let ok: SessionUpdateOk = serde_json::from_value(v.clone())
                    .map_err(|e| format!("{c}: session.update result: {e}"))?;
                self.enabled = ok.exts.keys().map(ToString::to_string).collect();
            }
        }
        Ok(())
    }

    fn event(&mut self, ev: &EventFrame) -> Result<(), String> {
        let caller = self.caller()?.clone();
        if let Some(ext) = ev.ty.extension() {
            if !self.enabled.contains(ext) {
                return Err(format!("event of {ext}, which is not enabled"));
            }
            if ev.seq.is_none() && ev.slot.is_some() && ev.slot == caller.slot {
                return Err("a relay was echoed to its sender".into());
            }
        } else {
            self.core_event(ev, &caller)?;
        }
        if let Some(s) = ev.seq.map(|s| s.0) {
            if s <= self.last_seq {
                return Err(format!("seq must increase: {s} after {}", self.last_seq));
            }
            match self.replay {
                Some((from, to)) if !self.replay_done => {
                    if s < from || s > to {
                        return Err(format!("seq {s} outside the replay {from}..={to}"));
                    }
                    self.replay_done = s == to;
                    self.last_seq = if self.replay_done { self.head.max(s) } else { s };
                }
                _ => self.last_seq = s,
            }
        } else {
            if !self.replay_done {
                return Err("an ephemeral event during replay".into());
            }
            let e = ev.eseq.ok_or("an ephemeral event without eseq")?;
            if e != self.eseq.saturating_add(1) {
                return Err(format!("eseq {e} after {}", self.eseq));
            }
            self.eseq = e;
        }
        Ok(())
    }

    fn core_event(&mut self, ev: &EventFrame, caller: &Caller) -> Result<(), String> {
        let decoded = CoreEvent::decode(ev).map_err(|e| format!("{}: {e}", ev.ty))?;
        let info = lookup(decoded.type_name()).ok_or("unregistered")?;
        let streams = self.wants().streams;
        let in_stream = info.stream.is_some_and(|s| streams.iter().any(|w| w.as_str() == s));
        match info.audience {
            Some(Audience::Subject)
                if ev.slot != caller.slot && caller.roles.is_empty() && !in_stream =>
            {
                return Err(format!("{}: audience is the subject, slot {:?}", ev.ty, ev.slot));
            }
            _ => {}
        }
        match &decoded {
            CoreEvent::StateChanged(s) => {
                let last = self
                    .watched
                    .get(s.path.as_str())
                    .ok_or_else(|| format!("state.changed for {}, which is not watched", s.path))?;
                if s.ver.0 <= *last {
                    return Err(format!("{}: ver {} after {last}", s.path, s.ver.0));
                }
                self.watched.insert(s.path.to_string(), s.ver.0);
            }
            CoreEvent::EpochChange(e) => self.epoch = e.to.0,
            CoreEvent::ItemGrant(g) if ev.slot == caller.slot => self.own_grant(g, caller)?,
            _ => {}
        }
        Ok(())
    }

    fn own_grant(&mut self, g: &ItemGrant, caller: &Caller) -> Result<(), String> {
        let want = self.wants().items;
        let own_world = g.from.is_some() && g.from == caller.slot;
        let wanted = match g.source {
            Source::Start => want.starting,
            _ if own_world => want.own,
            _ => want.from_others,
        };
        if !wanted {
            return Err(format!("an item the session did not want (source {:?})", g.source));
        }
        self.deliver(g)?;
        let n = g.item_seq.0;
        let filtered = !(want.own && want.from_others && want.starting);
        if self.paging || filtered {
            // Live grants may run ahead of an open inventory read, and filtered grants leave
            // gaps (rule 7): no continuity check, only the high-water mark.
            self.paged_max = self.paged_max.max(n);
        } else if n > self.applied {
            if n != self.own_item_seq.saturating_add(1) {
                return Err(format!("item_seq {n} after {}", self.own_item_seq));
            }
            self.own_item_seq = n;
        }
        Ok(())
    }

    /// Records a grant by `item_uid`; the same uid never comes with another `item_seq`.
    fn deliver(&mut self, g: &ItemGrant) -> Result<(), String> {
        let uid = g.item_uid.to_string();
        match self.granted.insert(uid.clone(), g.item_seq.0) {
            Some(prev) if prev != g.item_seq.0 => {
                Err(format!("{uid} came with item_seq {prev} and {}", g.item_seq.0))
            }
            _ => Ok(()),
        }
    }

    /// An inventory page counts as delivery (rule 10); the last page ends a `too_old` read.
    fn inventory_page(&mut self, path: &StatePath, ok: &GetOk) -> Result<(), String> {
        let own = self.caller()?.slot;
        let Ok(CorePath::Slot(n, SlotPart::Inventory)) = CorePath::parse(path) else {
            return Ok(());
        };
        if Some(n) != own {
            return Ok(());
        }
        let items: Vec<ItemGrant> = serde_json::from_value(ok.value.clone())
            .map_err(|e| format!("an inventory page holds item.grant data: {e}"))?;
        for g in &items {
            self.deliver(g)?;
            self.paged_max = self.paged_max.max(g.item_seq.0);
        }
        if ok.next.is_none() {
            self.paging = false;
            self.own_item_seq = self.own_item_seq.max(self.paged_max);
        }
        Ok(())
    }

    fn finish(&self) -> Result<(), String> {
        if !self.open.is_empty() || !self.pending.is_empty() {
            return Err(format!("unanswered {:?}", self.open.keys().collect::<Vec<_>>()));
        }
        if !self.expect_error.is_empty() {
            return Err(format!("expected error {:?} never came", self.expect_error));
        }
        if !self.pongs.is_empty() {
            return Err("ping never answered".into());
        }
        if !self.replay_done && self.welcome.is_some() {
            return Err("replay never completed".into());
        }
        Ok(())
    }
}

/// Replays one transcript against the protocol rules of doc 01 D1.10.
fn check_transcript(text: &str) -> Result<(), String> {
    let mut s = Session::default();
    for (n, line) in text.lines().enumerate() {
        let at = n.saturating_add(1);
        let line: Value = serde_json::from_str(line).map_err(|e| format!("line {at}: {e}"))?;
        let raw = line["frame"].clone();
        let message = raw.to_string();
        let raws: Vec<Value> = match &raw {
            Value::Array(v) => v.clone(),
            one => vec![one.clone()],
        };
        let step = |r: Result<(), String>| r.map_err(|e| format!("line {at}: {e}"));
        match line["dir"].as_str() {
            Some("c2s") => {
                for (f, raw) in decode_client(&message).into_iter().zip(&raws) {
                    let f = f.map_err(|e| format!("line {at}: {e:?}"))?;
                    if f != ClientFrame::Unknown {
                        step(covers(&serde_json::to_value(&f).unwrap(), raw, "frame"))?;
                    }
                    step(s.client(&f))?;
                }
            }
            Some("s2c") => {
                for (f, raw) in decode_server(&message).into_iter().zip(&raws) {
                    let f = f.map_err(|e| format!("line {at}: {e:?}"))?;
                    step(covers(&serde_json::to_value(&f).unwrap(), raw, "frame"))?;
                    step(s.server(&f))?;
                }
            }
            other => return Err(format!("line {at}: dir {other:?}")),
        }
    }
    s.finish()
}

const GOLDEN: &[(&str, &str)] = &[
    ("check-item-ack", include_str!("../../golden/check-item-ack.jsonl")),
    ("resume-get-watch", include_str!("../../golden/resume-get-watch.jsonl")),
    ("refusals", include_str!("../../golden/refusals.jsonl")),
    ("spectator", include_str!("../../golden/spectator.jsonl")),
    ("too-old", include_str!("../../golden/too-old.jsonl")),
];

#[test]
fn golden_transcripts_parse_round_trip_and_keep_the_protocol_rules() {
    for (name, text) in GOLDEN {
        if let Err(e) = check_transcript(text) {
            panic!("{name}: {e}");
        }
    }
}

const U1: &str = "iu_7QK2M4XH7QK2M4XH7QK2M4XH7Q";
const U2: &str = "iu_4C9R1T0WBC9R1T0WBC9R1T0WBC";

/// Sets or (with `null`) removes the top-level keys of `patch` in `base`.
fn patched(mut base: Value, patch: &Value) -> Value {
    if let (Some(b), Some(p)) = (base.as_object_mut(), patch.as_object()) {
        for (k, v) in p {
            if v.is_null() {
                b.remove(k);
            } else {
                b.insert(k.clone(), v.clone());
            }
        }
    }
    base
}

fn line(dir: &str, frame: &Value) -> String {
    json!({ "dir": dir, "frame": frame }).to_string()
}

fn hello(patch: &Value) -> String {
    let base = json!({"op":"hello","proto":[1],"token":"t","client":{"kind":"game"},"exts":{"org.cwp.deathlink":{"v":[1]}}});
    line("c2s", &patched(base, patch))
}

fn welcome(patch: &Value) -> String {
    let base = json!({"op":"welcome","proto":{"major":1,"minor":0},"session":"s",
        "caller":{"kind":"slot","slot":1,"scopes":["play"],"roles":[]},
        "limits":{"max_frame_bytes":65536,"hb_ms":30000,"hb_slack":2.5,"cmds_per_sec":20,"max_watches":64,"max_paths_per_watch":16},
        "epoch":1,"head":1,"applied":0,"resume":{"status":"ok"},"server_time_ms":1});
    line("s2c", &patched(base, patch))
}

fn c2s(frame: &Value) -> String {
    line("c2s", frame)
}

fn s2c(frame: &Value) -> String {
    line("s2c", frame)
}

fn session(lines: &[String]) -> String {
    let mut all = vec![hello(&json!({})), welcome(&json!({}))];
    all.extend(lines.iter().cloned());
    all.join("\n")
}

fn grant(seq: u64, item_seq: u64, uid: &str, slot: u32) -> String {
    s2c(&json!({"op":"event","seq":seq,"type":"item.grant","slot":slot,
        "data":{"source":"check","from":1,"location":5,"item":7,"item_seq":item_seq,"item_uid":uid,"flags":[]}}))
}

fn cmd(cid: &str, ty: &str, data: &Value) -> String {
    c2s(&json!({"op":"cmd","cid":cid,"type":ty,"data":data}))
}

fn ok(cid: &str, extra: &Value, v: &Value) -> String {
    s2c(&patched(json!({"op":"result","cid":cid,"upto":1,"ok":v}), extra))
}

fn err(cid: &str, code: u16, key: &str) -> String {
    s2c(&json!({"op":"result","cid":cid,"upto":1,"error":{"code":code,"key":key,"retry":"never"}}))
}

fn relay(eseq: u64, slot: u32) -> String {
    s2c(&json!({"op":"event","eseq":eseq,"type":"org.cwp.deathlink/death","slot":slot,"data":{}}))
}

/// One inventory entry: the `data` of an `item.grant`.
fn inv(item_seq: u64, uid: &str) -> Value {
    json!({"source":"check","from":2,"location":9,"item":3,"item_seq":item_seq,"item_uid":uid,"flags":[]})
}

fn sent(uid: &str) -> Value {
    json!({"items":[{"to":1,"item":7,"item_uid":uid}]})
}

/// Wrong transcripts, each breaking one rule, with the message the checker must give.
#[expect(clippy::too_many_lines, reason = "one row per protocol rule")]
fn wrong_transcripts() -> Vec<(&'static str, String, &'static str)> {
    let none = json!({});
    let dl = json!({"exts":{"org.cwp.deathlink":{"v":1}}});
    let check = |cid: &str, loc: i64| cmd(cid, "check", &json!({ "location": loc }));
    let commit = |seq: u64, upto: u64| json!({"seq":seq,"epoch":1,"upto":upto});
    let rows = vec![
        ("no hello first", vec![c2s(&json!({"op":"ping"}))], "first client frame must be hello"),
        ("event before welcome", vec![hello(&none), grant(2, 1, U1, 1)], "before welcome"),
        (
            "proto not offered",
            vec![hello(&none), welcome(&json!({"proto":{"major":2,"minor":0}}))],
            "did not offer",
        ),
        (
            "extension not offered",
            vec![hello(&json!({"exts":null})), welcome(&dl)],
            "was not offered",
        ),
        (
            "two credentials",
            vec![hello(&json!({"ticket":"u"})), welcome(&none)],
            "exactly one credential",
        ),
        (
            "spectator with play",
            vec![
                hello(&none),
                welcome(&json!({"caller":{"kind":"spectator","scopes":["play"],"roles":[]}})),
            ],
            "spectator",
        ),
        (
            "too_old with replay",
            vec![
                hello(&none),
                welcome(
                    &json!({"head":7,"replay":{"from":5,"to":7},"resume":{"status":"too_old"}}),
                ),
            ],
            "too_old",
        ),
        (
            "new_timeline alone",
            vec![hello(&none), welcome(&json!({"resume":{"status":"new_timeline"}}))],
            "epoch_change",
        ),
        (
            "bad data answered ok",
            vec![check("c", 0).replace("0}", "\"x\"}"), ok("c", &none, &none)],
            "expected error data.invalid",
        ),
        (
            "observer check answered ok",
            vec![
                hello(&none),
                welcome(&json!({"caller":{"kind":"spectator","scopes":["observe"],"roles":[]}})),
                check("c", 1),
                ok("c", &none, &json!({"items":[]})),
            ],
            "expected error auth.scope_insufficient",
        ),
        (
            "unknown type answered ok",
            vec![cmd("c", "teleport", &none), ok("c", &none, &none)],
            "expected error type.unknown",
        ),
        (
            "extension not enabled answered ok",
            vec![cmd("c", "org.cwp.deathlink/death", &none), ok("c", &none, &none)],
            "expected error ext.not_enabled",
        ),
        (
            "conflicting cid answered ok",
            vec![
                check("c", 1),
                ok("c", &none, &json!({"items":[]})),
                check("c", 2),
                ok("c", &none, &json!({"items":[]})),
            ],
            "expected error cid.conflict",
        ),
        (
            "repeat answered differently",
            vec![
                cmd("c", "goal", &none),
                ok("c", &none, &none),
                cmd("c", "goal", &none),
                ok("c", &none, &json!({"again":true})),
            ],
            "same result",
        ),
        (
            "repeat with another seq",
            vec![
                cmd("c", "goal", &none),
                s2c(&json!({"op":"event","seq":2,"type":"slot.goal","slot":1,"data":{}})),
                ok("c", &commit(2, 2), &none),
                cmd("c", "goal", &none),
                ok("c", &commit(3, 2), &none),
            ],
            "same result",
        ),
        ("seq going back", vec![grant(2, 1, U1, 1), grant(1, 2, U2, 1)], "seq must increase"),
        (
            "seq at the resume cursor",
            vec![
                hello(&json!({"resume":{"after":5,"epoch":1}})),
                welcome(&json!({"head":5})),
                grant(5, 1, U1, 1),
            ],
            "seq must increase",
        ),
        (
            "seq at or below head without replay",
            vec![hello(&none), welcome(&json!({"head":9})), grant(2, 1, U1, 1)],
            "seq must increase",
        ),
        (
            "seq outside the replay",
            vec![
                hello(&none),
                welcome(&json!({"head":7,"replay":{"from":5,"to":7}})),
                grant(8, 1, U1, 1),
            ],
            "outside the replay",
        ),
        (
            "replay never completed",
            vec![hello(&none), welcome(&json!({"head":2,"replay":{"from":1,"to":2}}))],
            "replay never completed",
        ),
        (
            "ephemeral during replay",
            vec![
                hello(&none),
                welcome(
                    &json!({"head":2,"replay":{"from":1,"to":2},"exts":{"org.cwp.deathlink":{"v":1}}}),
                ),
                relay(1, 2),
                grant(2, 1, U1, 1),
            ],
            "during replay",
        ),
        ("eseq gap", vec![hello(&none), welcome(&dl), relay(2, 2)], "eseq 2 after 0"),
        (
            "relay echoed to its sender",
            vec![hello(&none), welcome(&dl), relay(1, 1)],
            "echoed to its sender",
        ),
        (
            "state.changed unwatched",
            vec![s2c(
                &json!({"op":"event","eseq":1,"type":"state.changed","data":{"path":"room/info","value":{},"ver":2}}),
            )],
            "not watched",
        ),
        (
            "state.changed going back",
            vec![
                c2s(&json!({"op":"watch","cid":"w","paths":["room/info"]})),
                ok("w", &none, &json!({"paths":[{"path":"room/info","value":{},"ver":9}]})),
                s2c(
                    &json!({"op":"event","eseq":1,"type":"state.changed","data":{"path":"room/info","value":{},"ver":3}}),
                ),
            ],
            "ver 3 after 9",
        ),
        ("grant for another slot", vec![grant(2, 1, U1, 2)], "audience is the subject"),
        ("item_seq gap", vec![grant(2, 1, U1, 1), grant(3, 3, U2, 1)], "item_seq 3 after 1"),
        (
            "gap after welcome.applied",
            vec![hello(&none), welcome(&json!({"applied":6})), grant(2, 8, U1, 1)],
            "item_seq 8 after 6",
        ),
        (
            "own-world item not wanted",
            vec![
                hello(&json!({"want":{"items":{"own":false}}})),
                welcome(&none),
                grant(2, 1, U1, 1),
            ],
            "did not want",
        ),
        (
            "ack beyond delivery accepted",
            vec![c2s(&json!({"op":"ack","applied":4}))],
            "ack.invalid",
        ),
        (
            "unsolicited ack.invalid",
            vec![
                grant(2, 1, U1, 1),
                c2s(&json!({"op":"ack","applied":1})),
                s2c(&json!({"op":"error","code":3051,"key":"ack.invalid","retry":"never"})),
            ],
            "unsolicited",
        ),
        (
            "result before its grant",
            vec![check("loc5", 5), ok("loc5", &commit(2, 1), &sent(U1)), grant(2, 1, U1, 1)],
            "before its events",
        ),
        (
            "two checks in flight",
            vec![
                check("a", 5),
                check("b", 6),
                grant(2, 1, U1, 1),
                ok("a", &commit(2, 2), &sent(U1)),
                ok("b", &commit(3, 2), &sent(U2)),
            ],
            "before its events",
        ),
        (
            "committing result without seq",
            vec![check("a", 5), grant(2, 1, U1, 1), ok("a", &json!({"upto":2}), &sent(U1))],
            "carries seq and epoch",
        ),
        (
            "result in another epoch",
            vec![
                check("a", 5),
                grant(2, 1, U1, 1),
                ok("a", &json!({"seq":2,"epoch":2,"upto":2}), &sent(U1)),
            ],
            "epoch differs",
        ),
        (
            "wrong upto",
            vec![check("a", 5), grant(2, 1, U1, 1), ok("a", &commit(2, 1), &sent(U1))],
            "upto must be",
        ),
        (
            "session.update answered with a number",
            vec![cmd("su", "session.update", &json!({"exts":{}})), ok("su", &none, &json!(5))],
            "session.update result",
        ),
        (
            "goal answered with a number",
            vec![cmd("g", "goal", &none), ok("g", &none, &json!(5))],
            "must be an object",
        ),
        (
            "events after a kick",
            vec![
                s2c(&json!({"op":"error","code":3060,"key":"room.kicked","retry":"never"})),
                grant(2, 1, U1, 1),
            ],
            "after the session ended",
        ),
        ("unanswered command", vec![cmd("c", "goal", &none)], "unanswered"),
        ("unknown op not refused", vec![c2s(&json!({"op":"teleport"}))], "op.unknown"),
        (
            "get answered for another path",
            vec![
                c2s(&json!({"op":"get","cid":"g","path":"room/info"})),
                ok("g", &none, &json!({"path":"room/slots","value":[],"ver":0})),
            ],
            "another path",
        ),
        (
            "error with the wrong code",
            vec![cmd("c", "teleport", &none), err("c", 1003, "type.unknown")],
            "wrong code",
        ),
        (
            "pong without the nonce",
            vec![c2s(&json!({"op":"ping","n":3})), s2c(&json!({"op":"pong"}))],
            "echo the nonce",
        ),
        ("result for nothing", vec![ok("x", &none, &none)], "answers nothing open"),
        (
            "durable event without seq",
            vec![grant(2, 1, U1, 1).replace("\"seq\":2", "\"eseq\":1")],
            "delivery class",
        ),
        (
            "forbidden read answered",
            vec![
                c2s(&json!({"op":"get","cid":"g","path":"slot/2/inventory"})),
                ok("g", &none, &json!({"path":"slot/2/inventory","value":[],"ver":0})),
            ],
            "expected error path.forbidden",
        ),
        (
            "resume without its epoch",
            vec![hello(&json!({"resume":{"after":5}})), welcome(&json!({"head":5}))],
            "needs its epoch",
        ),
        (
            "head below the cursor called ok",
            vec![hello(&json!({"resume":{"after":5,"epoch":1}})), welcome(&json!({"head":3}))],
            "needs new_timeline",
        ),
        (
            "new timeline replayed from the old cursor",
            vec![
                hello(&json!({"resume":{"after":5,"epoch":1}})),
                welcome(
                    &json!({"epoch":2,"head":6,"replay":{"from":6,"to":6},"resume":{"status":"new_timeline","epoch_change":{"from":1,"to":2,"recovered_head":3}}}),
                ),
            ],
            "recovered_head + 1",
        ),
        (
            "one uid with two item_seqs",
            vec![grant(2, 1, U1, 1), grant(3, 2, U1, 1)],
            "came with item_seq",
        ),
        (
            "ack beyond the inventory",
            vec![
                hello(&json!({"resume":{"after":2,"epoch":1}})),
                welcome(&json!({"head":9,"resume":{"status":"too_old"}})),
                c2s(&json!({"op":"get","cid":"inv","path":"slot/1/inventory"})),
                ok(
                    "inv",
                    &json!({"upto":9}),
                    &json!({"path":"slot/1/inventory","value":[inv(1, U1)],"ver":9}),
                ),
                c2s(&json!({"op":"ack","applied":2})),
            ],
            "ack.invalid",
        ),
    ];
    rows.into_iter()
        .map(|(name, lines, why)| {
            let starts_with_hello = lines.first().is_some_and(|l| l.contains(r#""op":"hello""#));
            let text = if starts_with_hello || name == "no hello first" {
                lines.join("\n")
            } else {
                session(&lines)
            };
            (name, text, why)
        })
        .collect()
}

#[test]
fn every_wrong_transcript_is_caught_with_its_reason() {
    for (name, text, why) in wrong_transcripts() {
        match check_transcript(&text) {
            Ok(()) => panic!("{name}: the checker passed a wrong transcript"),
            Err(e) => {
                assert!(e.contains(why), "{name}: expected a failure about {why:?}, got {e:?}");
            }
        }
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "one row per edge case")]
fn correct_edge_cases_pass_the_checker() {
    let none = json!({});
    let cases = [
        (
            "re-ack of welcome.applied",
            vec![
                hello(&none),
                welcome(&json!({"applied":3})),
                c2s(&json!({"op":"ack","applied":3})),
            ],
        ),
        (
            "extension enabled mid-session",
            vec![
                hello(&none),
                welcome(&none),
                cmd("su", "session.update", &json!({"exts":{"org.cwp.deathlink":{"v":[1]}}})),
                ok("su", &none, &json!({"exts":{"org.cwp.deathlink":{"v":1,"minor":0}}})),
                cmd("d", "org.cwp.deathlink/death", &none),
                ok("d", &none, &none),
            ],
        ),
        (
            "command pipelined before welcome",
            vec![
                hello(&none),
                cmd("st", "status.set", &json!({"client_state":"ready"})),
                welcome(&none),
                ok("st", &none, &none),
            ],
        ),
        (
            "the room.items stream sees every grant",
            vec![
                hello(&json!({"want":{"streams":["room.items"]}})),
                welcome(&none),
                grant(2, 1, U1, 2),
            ],
        ),
        (
            "a replayed item at or below applied",
            vec![
                hello(&json!({"resume":{"after":1,"epoch":1}})),
                welcome(&json!({"head":3,"applied":2,"replay":{"from":2,"to":3}})),
                grant(2, 2, U1, 1),
                grant(3, 3, U2, 1),
            ],
        ),
        (
            "a retryable refusal, then the retry succeeds",
            vec![
                hello(&none),
                welcome(&none),
                cmd("loc5", "check", &json!({"location":5})),
                s2c(
                    &json!({"op":"result","cid":"loc5","upto":1,"error":{"code":4001,"key":"rate.limited","retry":"after","after_ms":500}}),
                ),
                cmd("loc5", "check", &json!({"location":5})),
                grant(2, 1, U1, 1),
                ok("loc5", &json!({"seq":2,"epoch":1,"upto":2}), &sent(U1)),
            ],
        ),
        (
            "too_old, an inventory page, then its ack",
            vec![
                hello(&json!({"resume":{"after":2,"epoch":1}})),
                welcome(&json!({"head":9,"resume":{"status":"too_old"}})),
                c2s(&json!({"op":"get","cid":"inv","path":"slot/1/inventory"})),
                ok(
                    "inv",
                    &json!({"upto":9}),
                    &json!({"path":"slot/1/inventory","value":[inv(1, U1)],"ver":9}),
                ),
                c2s(&json!({"op":"ack","applied":1})),
            ],
        ),
        (
            "a new timeline replays from its recovered head",
            vec![
                hello(&json!({"resume":{"after":5,"epoch":1}})),
                welcome(
                    &json!({"epoch":2,"head":6,"replay":{"from":4,"to":6},"resume":{"status":"new_timeline","epoch_change":{"from":1,"to":2,"recovered_head":3}}}),
                ),
                s2c(
                    &json!({"op":"event","seq":4,"type":"room.epoch_change","data":{"from":1,"to":2,"recovered_head":3}}),
                ),
                grant(6, 1, U1, 1),
            ],
        ),
        (
            "an ack below the stored cursor (an older save)",
            vec![
                hello(&none),
                welcome(&json!({"applied":3})),
                c2s(&json!({"op":"ack","applied":2})),
            ],
        ),
        (
            "filtered items leave gaps",
            vec![
                hello(&json!({"want":{"items":{"own":false}}})),
                welcome(&none),
                s2c(
                    &json!({"op":"event","seq":2,"type":"item.grant","slot":1,"data":{"source":"check","from":2,"location":5,"item":7,"item_seq":2,"item_uid":U1,"flags":[]}}),
                ),
            ],
        ),
    ];
    for (name, lines) in cases {
        if let Err(e) = check_transcript(&lines.join("\n")) {
            panic!("{name}: {e}");
        }
    }
}

#[test]
fn transcript_errors_must_match_the_registry() {
    assert_eq!(ErrorCode::RoomMoving.retry(), Retry::After);
    assert!(
        registered(&ErrorBody::from(ErrorCode::RoomMoving)).is_ok(),
        "a registered error passes"
    );
    let mut wrong = ErrorBody::from(ErrorCode::RoomMoving);
    wrong.retry = Retry::Never;
    assert!(registered(&wrong).is_err(), "a wrong retry fails");
}
