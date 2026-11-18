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
    assert!(ItemUid::new("iu_7QK2M4XH").is_ok(), "Crockford base32");
    assert!(ItemUid::new("iu_7QK2M4XI").is_err(), "I is not Crockford");
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
        format!(r#"{{"op":"result","cid":"c","ok":{{}},"error":{e}}}"#),
        format!(r#"{{"op":"result","cid":"c","error":{e},"ok":{{}}}}"#),
    ] {
        assert!(one_server(&both).is_err(), "both keys: {both}");
    }
    assert!(one_server(r#"{"op":"result","cid":"c"}"#).is_err(), "neither key");
    let ServerFrame::Result(r) = one_server(r#"{"op":"result","cid":"c","ok":null}"#).unwrap()
    else {
        panic!("not a result")
    };
    assert_eq!(r.outcome, Outcome::Ok(Value::Null), "ok: null is a value");
    let ok = ServerFrame::Result(CmdResult::ok(cid("c"), json!({})));
    assert_eq!(serde_json::to_value(&ok).unwrap(), json!({"op":"result","cid":"c","ok":{}}));
    let err = ServerFrame::Result(CmdResult::error(cid("c"), ErrorCode::TypeUnknown));
    assert_eq!(
        serde_json::to_value(&err).unwrap(),
        json!({"op":"result","cid":"c","error":{"code":1002,"key":"type.unknown","retry":"never","detail":{}}})
    );
}

#[test]
fn ping_and_pong_carry_an_optional_nonce() {
    assert_eq!(one_client(r#"{"op":"ping"}"#).unwrap(), ClientFrame::Ping(Beat::default()));
    let ClientFrame::Pong(b) = one_client(r#"{"op":"pong","n":7,"p":"menu"}"#).unwrap() else {
        panic!("not a pong")
    };
    assert_eq!((b.n, b.p.as_ref().map(StreamName::as_str)), (Some(7), Some("menu")));
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
        Err(BadFrame::Answer(r)) => match r.outcome {
            Outcome::Error(e) => (Some(r.cid.to_string()), e.key),
            Outcome::Ok(_) => panic!("an answer is an error"),
        },
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
            json!({"source":"check","from":1,"location":104,"item":7,"item_seq":1,"item_uid":"iu_7QK2M4XH","flags":["trap"]})
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

#[test]
fn templates_use_known_fields_kinds_and_conditions() {
    for t in CORE_TYPES {
        let data = sample(t.name);
        let has = |f: &str| f == "slot" || data.get(f).is_some();
        for v in t.templates {
            if let Some(when) = v.when {
                let (a, b) = when.split_once('=').unwrap();
                assert!(has(a) && has(b), "{}: condition {when}", t.name);
            }
            for (field, kind, at) in placeholders(v.text) {
                assert!(has(&field), "{}: {{{field}}} is not a field", t.name);
                assert!(
                    ["slot", "item", "location", "text"].contains(&kind.as_str()),
                    "{}: kind {kind}",
                    t.name
                );
                let needs_game = kind == "item" || kind == "location";
                assert_eq!(
                    at.is_some(),
                    needs_game,
                    "{}: {field} needs @slotfield exactly for ids",
                    t.name
                );
                if let Some(at) = at {
                    assert!(has(&at), "{}: @{at} is not a field", t.name);
                }
            }
        }
        if let Some(last) = t.templates.last() {
            assert!(last.when.is_none(), "{}: the last variant must always apply", t.name);
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
        "$defs": g.take_definitions(true),
    })
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
    Ok(Option<CoreCmd>),
    Err(&'static str),
    Same(Outcome),
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

#[derive(Default)]
struct Session {
    hello: Option<Box<Hello>>,
    caller: Option<Caller>,
    ended: bool,
    enabled: BTreeSet<String>,
    open: BTreeMap<String, Open>,
    done: BTreeMap<String, (Value, Outcome)>,
    watched: BTreeSet<String>,
    last_seq: u64,
    replay_to: Option<u64>,
    replay_done: bool,
    eseq: u64,
    own_item_seq: u64,
    grants_since: Vec<i64>,
    last_ack: u64,
    expect_error: Vec<&'static str>,
    expect_pong: Vec<Option<u64>>,
}

/// Every field of `raw` survives in `full` (a raw `null`, `{}` or `[]` matches an absent field,
/// since defaults are left out when encoding).
fn covers(full: &Value, raw: &Value, at: &str) -> Result<(), String> {
    match (raw, full) {
        (Value::Object(r), Value::Object(f)) => {
            for (k, v) in r {
                match f.get(k) {
                    Some(fv) => covers(fv, v, &format!("{at}.{k}"))?,
                    None if v.is_null()
                        || v.as_object().is_some_and(serde_json::Map::is_empty)
                        || v.as_array().is_some_and(Vec::is_empty) => {}
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
        self.caller.as_ref().ok_or_else(|| "no welcome yet".into())
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
        match f {
            ClientFrame::Cmd(c) => {
                let fp = json!([c.ty.as_str(), c.data]);
                let expect = match self.done.get(c.cid.as_str()) {
                    Some((prev, outcome)) if *prev == fp => Expect::Same(outcome.clone()),
                    Some(_) => Expect::Err("cid.conflict"),
                    None => self.expect_cmd(c)?,
                };
                let keep = !matches!(expect, Expect::Err("cid.conflict"));
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
                if no_slot || a.applied.0 > self.own_item_seq || a.applied.0 < self.last_ack {
                    self.expect_error.push("ack.invalid");
                } else {
                    self.last_ack = a.applied.0;
                }
                Ok(())
            }
            ClientFrame::Ping(b) => {
                self.expect_pong.push(b.n);
                Ok(())
            }
            ClientFrame::Unknown => {
                self.expect_error.push("op.unknown");
                Ok(())
            }
            ClientFrame::Hello(_) | ClientFrame::Pong(_) => Ok(()),
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

    fn expect_cmd(&mut self, c: &Cmd) -> Result<Expect, String> {
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
                if matches!(core, CoreCmd::Check(_)) {
                    self.grants_since.clear();
                }
                Ok(Expect::Ok(Some(core)))
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
        if self.caller.is_none() {
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
                if !self.expect_error.is_empty() {
                    let want = self.expect_error.remove(0);
                    if e.key != want {
                        return Err(format!("expected error {want}, got {}", e.key));
                    }
                }
                Ok(())
            }
            ServerFrame::Pong(b) => {
                match (!self.expect_pong.is_empty()).then(|| self.expect_pong.remove(0)) {
                    Some(n) if n == b.n => Ok(()),
                    Some(n) => Err(format!("pong must echo the nonce {n:?}")),
                    None => Err("pong without a ping".into()),
                }
            }
            ServerFrame::Ping(_) | ServerFrame::Unknown => Ok(()),
        }
    }

    fn welcome(&mut self, w: &Welcome) -> Result<(), String> {
        let h = self.hello.as_ref().ok_or("welcome before hello")?;
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
        if let (Some(s), Some(info)) = (w.caller.slot, &w.slot)
            && s != info.index
        {
            return Err("caller.slot and slot.index differ".into());
        }
        self.enabled = w.exts.keys().map(ToString::to_string).collect();
        self.last_seq = h.resume.map_or(0, |r| r.after.0);
        match w.replay {
            Some(r) if r.from > r.to => {
                return Err("replay range is reversed".into());
            }
            Some(r) if r.to > w.head => {
                return Err("replay range outside the log".into());
            }
            Some(r) if r.from.0 <= self.last_seq => {
                return Err("replay starts at or before the resume cursor".into());
            }
            Some(r) => self.replay_to = Some(r.to.0),
            None => self.replay_done = true,
        }
        self.caller = Some(w.caller.clone());
        Ok(())
    }

    fn result(&mut self, r: &CmdResult) -> Result<(), String> {
        let c = r.cid.as_str();
        let open =
            self.open.remove(c).ok_or_else(|| format!("result for {c} answers nothing open"))?;
        if let Outcome::Error(e) = &r.outcome {
            registered(e)?;
        }
        match (&open.expect, &r.outcome) {
            (Expect::Err(k), Outcome::Error(e)) if e.key == *k => {}
            (Expect::Err(k), _) => return Err(format!("{c}: expected error {k}")),
            (Expect::Same(o), got) if o == got => {}
            (Expect::Same(_), _) => {
                return Err(format!("{c}: a repeated cid must get the same result"));
            }
            (Expect::Ok(core), Outcome::Ok(v)) => {
                if let Some(CoreCmd::Check(_)) = core {
                    let ok: CheckOk = serde_json::from_value(v.clone())
                        .map_err(|e| format!("{c}: check result: {e}"))?;
                    let own = self.caller()?.slot;
                    for item in ok.items.iter().filter(|i| Some(i.to) == own) {
                        if !self.grants_since.contains(&item.item) {
                            return Err(format!(
                                "{c}: result came before its events (item {})",
                                item.item
                            ));
                        }
                    }
                }
            }
            (Expect::Get(p), Outcome::Ok(v)) => {
                let ok: GetOk = serde_json::from_value(v.clone())
                    .map_err(|e| format!("{c}: get result: {e}"))?;
                if ok.path != *p {
                    return Err(format!("{c}: get answered for another path"));
                }
            }
            (Expect::Watch(ps), Outcome::Ok(v)) => {
                let ok: WatchOk = serde_json::from_value(v.clone())
                    .map_err(|e| format!("{c}: watch result: {e}"))?;
                if ok.paths.iter().map(|s| &s.path).ne(ps.iter()) {
                    return Err(format!("{c}: watch must return each path, in order"));
                }
                self.watched.extend(ps.iter().map(ToString::to_string));
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
            self.done.insert(c.into(), (fp, r.outcome.clone()));
        }
        Ok(())
    }

    fn event(&mut self, ev: &EventFrame) -> Result<(), String> {
        if let Some(ext) = ev.ty.extension() {
            if !self.enabled.contains(ext) {
                return Err(format!("event of {ext}, which is not enabled"));
            }
        } else {
            let decoded = CoreEvent::decode(ev).map_err(|e| format!("{}: {e}", ev.ty))?;
            let info = lookup(decoded.type_name()).ok_or("unregistered")?;
            let caller = self.caller()?.clone();
            match info.audience {
                Some(Audience::Subject) if ev.slot != caller.slot && caller.roles.is_empty() => {
                    return Err(format!("{}: audience is the subject, slot {:?}", ev.ty, ev.slot));
                }
                Some(Audience::Path) => {
                    if let CoreEvent::StateChanged(s) = &decoded
                        && !self.watched.contains(s.path.as_str())
                    {
                        return Err(format!("state.changed for {}, which is not watched", s.path));
                    }
                }
                _ => {}
            }
            if let CoreEvent::ItemGrant(g) = &decoded
                && ev.slot == caller.slot
            {
                if self.own_item_seq != 0 && g.item_seq.0 != self.own_item_seq.saturating_add(1) {
                    return Err(format!("item_seq {} after {}", g.item_seq.0, self.own_item_seq));
                }
                self.own_item_seq = g.item_seq.0;
                self.grants_since.push(g.item);
            }
        }
        if let Some(s) = ev.seq.map(|s| s.0) {
            if s <= self.last_seq {
                return Err(format!("seq must increase: {s} after {}", self.last_seq));
            }
            if let Some(to) = self.replay_to
                && !self.replay_done
            {
                if s > to {
                    return Err(format!("replay must end at {to}"));
                }
                self.replay_done = s == to;
            }
            self.last_seq = s;
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

    fn finish(&self) -> Result<(), String> {
        if !self.open.is_empty() {
            return Err(format!("unanswered {:?}", self.open.keys().collect::<Vec<_>>()));
        }
        if !self.expect_error.is_empty() {
            return Err(format!("expected error {:?} never came", self.expect_error));
        }
        if !self.expect_pong.is_empty() {
            return Err("ping never answered".into());
        }
        if !self.replay_done && self.caller.is_some() {
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
];

#[test]
fn golden_transcripts_parse_round_trip_and_keep_the_protocol_rules() {
    for (name, text) in GOLDEN {
        if let Err(e) = check_transcript(text) {
            panic!("{name}: {e}");
        }
    }
}

const HELLO: &str = r#"{"dir":"c2s","frame":{"op":"hello","proto":[1],"token":"t","client":{"kind":"game"},"exts":{"org.cwp.deathlink":{"v":[1]}}}}"#;
const WELCOME: &str = r#"{"dir":"s2c","frame":{"op":"welcome","proto":{"major":1,"minor":0},"session":"s","caller":{"kind":"slot","slot":1,"scopes":["play"],"roles":[]},"limits":{"max_frame_bytes":65536,"hb_ms":30000,"hb_slack":2.5,"cmds_per_sec":20,"max_watches":64,"max_paths_per_watch":16},"epoch":1,"head":9,"resume":{"status":"ok"},"server_time_ms":1}}"#;
const GRANT: &str = r#"{"op":"event","seq":2,"type":"item.grant","slot":1,"data":{"source":"check","from":1,"location":5,"item":7,"item_seq":1,"item_uid":"iu_7QK2M4XH","flags":[]}}"#;

fn c2s(frame: &str) -> String {
    format!(r#"{{"dir":"c2s","frame":{frame}}}"#)
}

fn s2c(frame: &str) -> String {
    format!(r#"{{"dir":"s2c","frame":{frame}}}"#)
}

fn with_deathlink(welcome: &str) -> String {
    welcome.replace(r#""epoch":1"#, r#""exts":{"org.cwp.deathlink":{"v":1}},"epoch":1"#)
}

fn session(lines: &[String]) -> String {
    let mut all = vec![HELLO.to_owned(), WELCOME.to_owned()];
    all.extend(lines.iter().cloned());
    all.join("\n")
}

/// Wrong transcripts, each breaking one rule, with the message the checker must give.
#[expect(clippy::too_many_lines, reason = "one row per protocol rule")]
fn wrong_transcripts() -> Vec<(&'static str, String, &'static str)> {
    let cmd = |cid: &str, ty: &str, data: &str| {
        c2s(&format!(r#"{{"op":"cmd","cid":"{cid}","type":"{ty}","data":{data}}}"#))
    };
    let ok = |cid: &str, v: &str| s2c(&format!(r#"{{"op":"result","cid":"{cid}","ok":{v}}}"#));
    let err = |cid: &str, code: u16, key: &str| {
        s2c(&format!(
            r#"{{"op":"result","cid":"{cid}","error":{{"code":{code},"key":"{key}","retry":"never"}}}}"#
        ))
    };
    let observer = WELCOME.replace(
        r#""kind":"slot","slot":1,"scopes":["play"]"#,
        r#""kind":"spectator","scopes":["observe"]"#,
    );
    let replaying =
        with_deathlink(WELCOME).replace(r#""resume""#, r#""replay":{"from":1,"to":2},"resume""#);
    vec![
        ("no hello first", c2s(r#"{"op":"ping"}"#), "first client frame must be hello"),
        ("event before welcome", [HELLO.to_owned(), s2c(GRANT)].join("\n"), "before welcome"),
        (
            "proto not offered",
            [HELLO.to_owned(), WELCOME.replace(r#""major":1"#, r#""major":2"#)].join("\n"),
            "did not offer",
        ),
        (
            "extension not offered",
            [HELLO.replace(r#""org.cwp.deathlink":{"v":[1]}"#, ""), with_deathlink(WELCOME)]
                .join("\n"),
            "was not offered",
        ),
        (
            "bad data answered ok",
            session(&[cmd("c", "check", r#"{"location":"x"}"#), ok("c", "{}")]),
            "expected error data.invalid",
        ),
        (
            "observer check answered ok",
            [
                HELLO.to_owned(),
                observer,
                cmd("c", "check", r#"{"location":1}"#),
                ok("c", r#"{"items":[]}"#),
            ]
            .join("\n"),
            "expected error auth.scope_insufficient",
        ),
        (
            "unknown type answered ok",
            session(&[cmd("c", "teleport", "{}"), ok("c", "{}")]),
            "expected error type.unknown",
        ),
        (
            "extension not enabled answered ok",
            session(&[cmd("c", "org.cwp.deathlink/death", "{}"), ok("c", "{}")]),
            "expected error ext.not_enabled",
        ),
        (
            "conflicting cid answered ok",
            session(&[
                cmd("c", "check", r#"{"location":1}"#),
                ok("c", r#"{"items":[]}"#),
                cmd("c", "check", r#"{"location":2}"#),
                ok("c", r#"{"items":[]}"#),
            ]),
            "expected error cid.conflict",
        ),
        (
            "repeated cid answered differently",
            session(&[
                cmd("c", "goal", "{}"),
                ok("c", "{}"),
                cmd("c", "goal", "{}"),
                ok("c", r#"{"again":true}"#),
            ]),
            "same result",
        ),
        (
            "seq going back",
            session(&[
                s2c(GRANT),
                s2c(&GRANT
                    .replace(r#""seq":2"#, r#""seq":1"#)
                    .replace(r#""item_seq":1"#, r#""item_seq":2"#)),
            ]),
            "seq must increase",
        ),
        (
            "seq at the resume cursor",
            [
                HELLO.replace(r#""client""#, r#""resume":{"after":5},"client""#),
                WELCOME.to_owned(),
                s2c(&GRANT.replace(r#""seq":2"#, r#""seq":5"#)),
            ]
            .join("\n"),
            "seq must increase",
        ),
        (
            "ephemeral during replay",
            [
                HELLO.to_owned(),
                replaying,
                s2c(r#"{"op":"event","eseq":1,"type":"org.cwp.deathlink/death","data":{}}"#),
                s2c(GRANT),
            ]
            .join("\n"),
            "during replay",
        ),
        (
            "eseq gap",
            [
                HELLO.to_owned(),
                with_deathlink(WELCOME),
                s2c(r#"{"op":"event","eseq":2,"type":"org.cwp.deathlink/death","data":{}}"#),
            ]
            .join("\n"),
            "eseq 2 after 0",
        ),
        (
            "state.changed for an unwatched path",
            session(&[s2c(
                r#"{"op":"event","eseq":1,"type":"state.changed","data":{"path":"room/info","value":{},"ver":1}}"#,
            )]),
            "not watched",
        ),
        (
            "grant for another slot",
            session(&[s2c(&GRANT.replace(r#""slot":1"#, r#""slot":2"#))]),
            "audience is the subject",
        ),
        (
            "item_seq gap",
            session(&[
                s2c(GRANT),
                s2c(&GRANT
                    .replace(r#""seq":2"#, r#""seq":3"#)
                    .replace(r#""item_seq":1"#, r#""item_seq":3"#)),
            ]),
            "item_seq 3 after 1",
        ),
        (
            "ack beyond delivery accepted",
            session(&[c2s(r#"{"op":"ack","applied":4}"#)]),
            "ack.invalid",
        ),
        (
            "result before its grant",
            session(&[
                cmd("loc5", "check", r#"{"location":5}"#),
                ok("loc5", r#"{"items":[{"to":1,"item":7}]}"#),
                s2c(GRANT),
            ]),
            "result came before its events",
        ),
        ("unanswered command", session(&[cmd("c", "goal", "{}")]), "unanswered"),
        ("unknown op not refused", session(&[c2s(r#"{"op":"teleport"}"#)]), "op.unknown"),
        (
            "get answered for another path",
            session(&[
                c2s(r#"{"op":"get","cid":"g","path":"room/info"}"#),
                ok("g", r#"{"path":"room/slots","value":[],"ver":0}"#),
            ]),
            "another path",
        ),
        (
            "error with the wrong code",
            session(&[cmd("c", "teleport", "{}"), err("c", 1003, "type.unknown")]),
            "wrong code",
        ),
        (
            "pong without the nonce",
            session(&[c2s(r#"{"op":"ping","n":3}"#), s2c(r#"{"op":"pong"}"#)]),
            "echo the nonce",
        ),
        ("result for nothing", session(&[ok("x", "{}")]), "answers nothing open"),
        (
            "durable event without seq",
            session(&[s2c(&GRANT.replace(r#""seq":2"#, r#""eseq":1"#))]),
            "delivery class",
        ),
        (
            "forbidden read answered",
            session(&[
                c2s(r#"{"op":"get","cid":"g","path":"slot/2/inventory"}"#),
                ok("g", r#"{"path":"slot/2/inventory","value":[],"ver":0}"#),
            ]),
            "expected error path.forbidden",
        ),
        (
            "replay never completed",
            [
                HELLO.to_owned(),
                with_deathlink(WELCOME)
                    .replace(r#""resume""#, r#""replay":{"from":1,"to":2},"resume""#),
            ]
            .join("\n"),
            "replay never completed",
        ),
    ]
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
