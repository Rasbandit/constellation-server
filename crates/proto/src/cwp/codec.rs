//! Reading transport messages (doc 01 D1.10 rules 1 and 3). A message is one frame or a JSON
//! array of frames. A frame that does not decode still gets its answer: when its `cid` is
//! readable the server answers with a `result` for it, otherwise with an `error` frame.
use super::frame::{ClientFrame, CmdResult, ErrorBody, ServerFrame};
use super::names::{CmdId, StatePath, TypeName};
use super::serde_util::in_range;
use crate::ErrorCode;
use alloc::{boxed::Box, string::ToString, vec, vec::Vec};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// A frame that could not be decoded, with what the receiver answers it with. An `error`
/// frame's `ref` names the frame it answers: its `cid` when it had a readable one, else its `op`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadFrame {
    /// The `cid` was readable: answer with this `result`.
    Answer(Box<CmdResult>),
    /// No usable `cid`: answer with this `error` frame.
    Error(Box<ErrorBody>),
}

/// Ops whose frames carry a `cid` and are answered by a `result`.
const ANSWERED: &[&str] = &["cmd", "get", "watch", "unwatch"];

/// Which field made a frame fail, so the answer names the right error.
fn classify(raw: &Value) -> ErrorCode {
    let bad_type =
        raw.get("type").and_then(Value::as_str).is_some_and(|t| TypeName::new(t).is_err());
    let path_ok = |p: &Value| p.as_str().is_some_and(|p| StatePath::new(p).is_ok());
    let bad_path = raw.get("path").is_some_and(|p| !path_ok(p))
        || raw.get("paths").and_then(Value::as_array).is_some_and(|ps| !ps.iter().all(path_ok));
    if bad_type {
        ErrorCode::TypeUnknown
    } else if bad_path {
        ErrorCode::PathUnknown
    } else {
        ErrorCode::DataInvalid
    }
}

fn answer(raw: &Value, code: ErrorCode, reason: &str) -> BadFrame {
    let op = raw.get("op").and_then(Value::as_str);
    let body = ErrorBody::from(code).with_detail(json!({ "reason": reason }));
    let cid = raw.get("cid").and_then(Value::as_str).and_then(|c| CmdId::new(c).ok());
    match (op, cid) {
        (Some(op), Some(cid)) if ANSWERED.contains(&op) => {
            BadFrame::Answer(Box::new(CmdResult::error(cid, body)))
        }
        (_, Some(cid)) => BadFrame::Error(Box::new(body.with_ref(cid.as_str()))),
        (Some(op), None) => BadFrame::Error(Box::new(body.with_ref(op))),
        (None, _) => BadFrame::Error(Box::new(body)),
    }
}

fn frames<T: DeserializeOwned>(text: &str) -> Vec<Result<T, BadFrame>> {
    let msg: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            let body = ErrorBody::from(ErrorCode::DataInvalid)
                .with_detail(json!({ "reason": e.to_string() }));
            return vec![Err(BadFrame::Error(Box::new(body)))];
        }
    };
    let items = match msg {
        Value::Array(items) => items,
        one => vec![one],
    };
    items
        .into_iter()
        .map(|raw| {
            if !in_range(&raw) {
                return Err(answer(&raw, ErrorCode::DataInvalid, "integer outside the CWP range"));
            }
            T::deserialize(&raw).map_err(|e| answer(&raw, classify(&raw), &e.to_string()))
        })
        .collect()
}

/// Reads one client message into its frames, in order.
#[must_use]
pub fn decode_client(text: &str) -> Vec<Result<ClientFrame, BadFrame>> {
    frames(text)
}

/// Reads one server message into its frames, in order. A client ignores frames it cannot read.
#[must_use]
pub fn decode_server(text: &str) -> Vec<Result<ServerFrame, BadFrame>> {
    frames(text)
}
