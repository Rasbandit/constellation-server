extern crate std;

use super::*;
use crate::{ErrorCode, Seq};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    string::{String, ToString},
    vec::Vec,
};
use serde_json::{Value, json};

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
        "org.cwp.x/Bad",
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
fn extension_ids_need_two_dotted_words() {
    assert!(ExtId::new("org.cwp.deathlink").is_ok(), "standard id");
    assert!(ExtId::new("x.mine").is_ok(), "experimental id");
    assert!(ExtId::new("deathlink").is_err(), "one word is a core name, not an extension");
}

#[test]
fn state_paths_allow_percent_encoded_keys_and_refuse_empty_segments() {
    let p = StatePath::new("kv/legacy/Energy%20Link").unwrap();
    assert_eq!(p.root(), "kv");
    for bad in ["", "slot//status", "/slot", "slot/", "slot/3 status", "kv/a?b"] {
        assert!(StatePath::new(bad).is_err(), "{bad:?} should be refused");
    }
    assert_eq!(StatePath::new("a".repeat(STATE_PATH_MAX + 1)), Err(NameError::Length));
}

#[test]
fn command_ids_are_one_to_sixty_four_bytes() {
    assert!(CmdId::new("x".repeat(CMD_ID_MAX)).is_ok(), "64 bytes is the limit");
    assert_eq!(CmdId::new("x".repeat(CMD_ID_MAX + 1)), Err(NameError::Length));
    assert_eq!(CmdId::new(""), Err(NameError::Length));
}

#[test]
fn a_bad_name_inside_a_frame_fails_the_whole_frame() {
    let f = r#"{"op":"cmd","cid":"c","type":"Not A Type","data":{}}"#;
    let e = serde_json::from_str::<ClientFrame>(f).unwrap_err().to_string();
    assert!(e.contains("TypeName"), "error names the field type: {e}");
}

// ---- frames ------------------------------------------------------------------------------

#[test]
fn a_minimal_hello_wants_every_item_and_no_slot_data() {
    let f: ClientFrame = serde_json::from_str(r#"{"op":"hello","proto":[1],"token":"t"}"#).unwrap();
    let ClientFrame::Hello(h) = f else { panic!("not a hello") };
    assert_eq!(
        h.want,
        Want {
            items: WantItems { from_others: true, own: true, starting: true },
            slot_data: false
        }
    );
    assert!(h.exts.is_empty() && h.resume.is_none(), "optional fields default to empty");
}

#[test]
fn a_cmd_without_data_carries_an_empty_object() {
    let f: ClientFrame = serde_json::from_str(r#"{"op":"cmd","cid":"c","type":"goal"}"#).unwrap();
    let ClientFrame::Cmd(c) = f else { panic!("not a cmd") };
    assert_eq!(c.data, json!({}));
    assert_eq!(CoreCmd::decode(&c).unwrap(), CoreCmd::Goal(Goal {}));
}

#[test]
fn unknown_frames_are_refused_and_unknown_fields_are_ignored() {
    assert!(serde_json::from_str::<ClientFrame>(r#"{"op":"teleport"}"#).is_err(), "no such op");
    assert!(
        serde_json::from_str::<ClientFrame>(r#"{"op":"ack","applied":3,"later":true}"#).is_ok(),
        "D1.8: ignore unknown fields"
    );
}

#[test]
fn a_result_is_ok_or_error_with_its_cid() {
    let ok = ServerFrame::Result(CmdResult {
        cid: CmdId::new("c").unwrap(),
        outcome: Outcome::Ok(json!({})),
    });
    assert_eq!(serde_json::to_value(&ok).unwrap(), json!({"op":"result","cid":"c","ok":{}}));
    let err = ServerFrame::Result(CmdResult {
        cid: CmdId::new("c").unwrap(),
        outcome: Outcome::Error(ErrorCode::TypeUnknown.into()),
    });
    assert_eq!(
        serde_json::to_value(&err).unwrap(),
        json!({"op":"result","cid":"c","error":{"code":1002,"key":"type.unknown","retry":"never","detail":{}}})
    );
}

#[test]
fn decode_tells_extensions_unknowns_and_bad_payloads_apart() {
    let cmd = |ty: &str, data: Value| Cmd {
        cid: CmdId::new("c").unwrap(),
        ty: TypeName::new(ty).unwrap(),
        data,
    };
    assert!(matches!(
        CoreCmd::decode(&cmd("org.cwp.deathlink/death", json!({}))),
        Err(DecodeError::Extension)
    ));
    assert!(matches!(CoreCmd::decode(&cmd("teleport", json!({}))), Err(DecodeError::Unknown)));
    assert!(
        matches!(CoreCmd::decode(&cmd("item.grant", json!({}))), Err(DecodeError::Unknown)),
        "an event name is not a command"
    );
    assert!(matches!(
        CoreCmd::decode(&cmd("check", json!({"location":"x"}))),
        Err(DecodeError::Data(_))
    ));
    assert_eq!(
        CoreCmd::decode(&cmd("check", json!({"location":-9}))).unwrap(),
        CoreCmd::Check(Check { location: -9 })
    );
}

// ---- registry ----------------------------------------------------------------------------

#[test]
fn registry_names_are_unique_core_names_and_events_declare_delivery() {
    let mut seen = BTreeSet::new();
    for t in CORE_TYPES {
        assert!(seen.insert(t.name), "{} registered twice", t.name);
        let n = TypeName::new(t.name).unwrap();
        assert!(n.extension().is_none(), "{} is a core type", t.name);
        assert_eq!(
            t.delivery.is_some(),
            t.kind == Kind::Event,
            "{}: events and only events have a delivery class",
            t.name
        );
        assert!(!t.scopes.is_empty(), "{} has no scope", t.name);
        assert_eq!(lookup(t.name), Some(t));
    }
}

/// The payload schema of each registered type. Panics for a type with no payload, so a new
/// registry row without its payload type fails here.
fn data_schema(name: &str) -> Value {
    let s = match name {
        "check" => schemars::schema_for!(Check),
        "goal" => schemars::schema_for!(Goal),
        "location.checked" => schemars::schema_for!(LocationChecked),
        "item.grant" => schemars::schema_for!(ItemGrant),
        "slot.goal" => schemars::schema_for!(SlotGoal),
        "state.changed" => schemars::schema_for!(StateChanged),
        other => panic!("registered type {other} has no payload type in data_schema"),
    };
    serde_json::to_value(s).unwrap()
}

#[test]
fn every_template_placeholder_is_a_payload_field_or_the_slot() {
    for t in CORE_TYPES {
        let Some(tpl) = t.template else { continue };
        let schema = data_schema(t.name);
        let fields: BTreeSet<&str> = schema["properties"]
            .as_object()
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default();
        for part in tpl.split('{').skip(1) {
            let field = part.split('}').next().unwrap();
            assert!(
                field == "slot" || fields.contains(field),
                "{}: placeholder {{{field}}} is not a field",
                t.name
            );
        }
    }
}

// ---- schema snapshot ---------------------------------------------------------------------

fn schema_document() -> Value {
    let types: BTreeMap<&str, Value> = CORE_TYPES
        .iter()
        .map(|t| {
            let mut entry = serde_json::to_value(t).unwrap();
            entry["data"] = data_schema(t.name);
            (t.name, entry)
        })
        .collect();
    let errors: Vec<Value> = ErrorCode::ALL
        .iter()
        .map(|e| json!({"code": e.code(), "key": e.key(), "retry": e.retry()}))
        .collect();
    json!({
        "title": "CWP v1 (design doc 01 D1.10, proposal D-123). Generated by `cargo test -p proto`; do not edit.",
        "client_frame": serde_json::to_value(schemars::schema_for!(ClientFrame)).unwrap(),
        "server_frame": serde_json::to_value(schemars::schema_for!(ServerFrame)).unwrap(),
        "types": types,
        "errors": errors,
    })
}

#[test]
fn the_committed_schema_matches_the_types() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/cwp-v1.json");
    let generated = serde_json::to_string_pretty(&schema_document()).unwrap() + "\n";
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    if committed != generated {
        let new = path.with_extension("json.new");
        std::fs::write(&new, &generated).unwrap();
        panic!(
            "the CWP schema changed. Review {} and, if the change is intended, move it over {}",
            new.display(),
            path.display()
        );
    }
}

// ---- golden transcripts ------------------------------------------------------------------

const GOLDEN: &[(&str, &str)] = &[
    ("check-item-ack", include_str!("../../golden/check-item-ack.jsonl")),
    ("resume-get-watch", include_str!("../../golden/resume-get-watch.jsonl")),
    ("refusals", include_str!("../../golden/refusals.jsonl")),
];

/// Replays one transcript and checks the protocol rules a server and a client must both keep.
fn check_transcript(name: &str, text: &str) {
    let mut open_cids: Vec<String> = Vec::new();
    let mut enabled: BTreeSet<String> = BTreeSet::new();
    let mut last_seq = Seq(0);
    for (n, line) in text.lines().enumerate() {
        let at = format!("{name}:{}", n.saturating_add(1));
        let line: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("{at}: {e}"));
        let raw = line["frame"].clone();
        match line["dir"].as_str() {
            Some("c2s") => {
                let f: ClientFrame =
                    serde_json::from_value(raw.clone()).unwrap_or_else(|e| panic!("{at}: {e}"));
                assert_eq!(
                    serde_json::to_value(&f).unwrap(),
                    with_defaults(&raw, &f),
                    "{at}: round trip"
                );
                match &f {
                    ClientFrame::Cmd(c) => {
                        open_cids.push(c.cid.to_string());
                        if let Some(ext) = c.ty.extension() {
                            assert!(enabled.contains(ext), "{at}: {ext} was not enabled");
                        }
                    }
                    ClientFrame::Get(g) => open_cids.push(g.cid.to_string()),
                    ClientFrame::Watch(w) => open_cids.push(w.cid.to_string()),
                    _ => {}
                }
            }
            Some("s2c") => {
                let f: ServerFrame =
                    serde_json::from_value(raw.clone()).unwrap_or_else(|e| panic!("{at}: {e}"));
                assert_eq!(
                    serde_json::to_value(&f).unwrap(),
                    with_defaults(&raw, &f),
                    "{at}: round trip"
                );
                match &f {
                    ServerFrame::Welcome(w) => {
                        enabled = w.exts.keys().map(ToString::to_string).collect();
                    }
                    ServerFrame::Result(r) => {
                        let i = open_cids.iter().position(|c| *c == r.cid.as_str());
                        assert!(i.is_some(), "{at}: result for {} answers nothing open", r.cid);
                        open_cids.remove(i.unwrap());
                        if let Outcome::Error(e) = &r.outcome {
                            assert_registered(&at, e);
                        }
                    }
                    ServerFrame::Error(e) => assert_registered(&at, e),
                    ServerFrame::Event(ev) => {
                        if let Some(ext) = ev.ty.extension() {
                            assert!(
                                enabled.contains(ext),
                                "{at}: event of {ext}, which was not enabled"
                            );
                            assert!(
                                ev.seq.is_none(),
                                "{at}: extension events in these transcripts are relays"
                            );
                            continue;
                        }
                        let decoded =
                            CoreEvent::decode(ev).unwrap_or_else(|e| panic!("{at}: {e:?}"));
                        let info = lookup(decoded.type_name()).unwrap();
                        match (info.delivery, ev.seq) {
                            (Some(Delivery::Durable), Some(s)) => {
                                assert!(s > last_seq, "{at}: seq must increase");
                                last_seq = s;
                            }
                            (Some(Delivery::Ephemeral), None) => {}
                            (d, s) => panic!("{at}: delivery {d:?} with seq {s:?}"),
                        }
                    }
                    ServerFrame::Ping | ServerFrame::Pong => {}
                }
            }
            other => panic!("{at}: dir {other:?}"),
        }
    }
    assert!(open_cids.is_empty(), "{name}: unanswered {open_cids:?}");
}

/// The transcript may leave out fields with defaults; compare against the frame re-encoded
/// from its own decoded form, field by field, so a dropped or renamed field still fails.
fn with_defaults(raw: &Value, decoded: &impl serde::Serialize) -> Value {
    let full = serde_json::to_value(decoded).unwrap();
    let mut out = full.clone();
    if let (Some(raw), Some(out)) = (raw.as_object(), out.as_object_mut()) {
        for (k, v) in raw {
            assert_eq!(full.get(k), Some(v), "field {k} did not survive a round trip");
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

fn assert_registered(at: &str, e: &ErrorBody) {
    let code = ErrorCode::ALL.iter().find(|c| c.key() == e.key);
    let code = code.unwrap_or_else(|| panic!("{at}: unregistered error {}", e.key));
    assert_eq!(
        (code.code(), code.retry()),
        (e.code, e.retry),
        "{at}: {} sent with the wrong code or retry",
        e.key
    );
}

#[test]
fn golden_transcripts_parse_round_trip_and_keep_the_protocol_rules() {
    for (name, text) in GOLDEN {
        check_transcript(name, text);
    }
}

#[test]
fn a_transcript_with_an_unanswered_command_fails() {
    let bad = r#"{"dir":"c2s","frame":{"op":"cmd","cid":"c-1","type":"goal"}}"#;
    let r = std::panic::catch_unwind(|| check_transcript("bad", bad));
    assert!(r.is_err(), "an open cid at the end must fail the transcript");
}
