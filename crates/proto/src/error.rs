//! Error registry (doc 01 D1.7). Numeric codes are PROVISIONAL until the registry (D-66) assigns
//! them; the range per area is normative, the number inside it is not. Append-only once
//! published. Numbers follow doc 09 section 4.7 where that table already assigns one.
use serde::{Deserialize, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(schemars::JsonSchema))]
/// Retry hint carried by every error frame (doc 01 D1.7).
pub enum Retry {
    /// Retrying will not help.
    Never,
    /// Retry with exponential backoff and full jitter (doc 01 D1.7).
    Backoff,
    /// Retry after the `after_ms` the server sent.
    After,
}

macro_rules! registry {
    ($($(#[$doc:meta])* $v:ident = $code:literal, $key:literal, $retry:ident;)*) => {
        /// Registered error codes (doc 01 D1.7, D1.10; doc 09 section 4.7).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum ErrorCode {
            $($(#[$doc])* $v,)*
        }

        impl ErrorCode {
            /// Every registered code, for registry tests and docs generation.
            pub const ALL: &'static [Self] = &[$(Self::$v,)*];

            /// Numeric code (provisional, see module docs).
            #[must_use]
            pub const fn code(self) -> u16 {
                match self { $(Self::$v => $code,)* }
            }

            /// Normative dotted key; clients localize from this.
            #[must_use]
            pub const fn key(self) -> &'static str {
                match self { $(Self::$v => $key,)* }
            }

            /// Retry hint the server must send with this error.
            #[must_use]
            pub const fn retry(self) -> Retry {
                match self { $(Self::$v => Retry::$retry,)* }
            }
        }
    };
}

registry! {
    /// A capability the frame needs was not negotiated.
    CapabilityMissing = 1001, "capability.missing", Never;
    /// A command type that is not registered.
    TypeUnknown = 1002, "type.unknown", Never;
    /// A payload or frame field that does not match its schema.
    DataInvalid = 1003, "data.invalid", Never;
    /// A frame `op` the server does not know; the session stays open.
    OpUnknown = 1004, "op.unknown", Never;
    /// A type of an extension the session did not enable.
    ExtNotEnabled = 1005, "ext.not_enabled", Never;
    /// A state path that is not registered.
    PathUnknown = 1006, "path.unknown", Never;
    /// No protocol major in common.
    ProtoUnsupported = 1010, "proto.unsupported", Never;
    /// The credential is not valid for this room.
    AuthTokenUnknown = 2011, "auth.token_unknown", Never;
    /// The credential was revoked.
    AuthTokenRevoked = 2012, "auth.token_revoked", Never;
    /// The action needs a fresh confirmation (doc 10 step-up).
    AuthStepUpRequired = 2013, "auth.step_up_required", Never;
    /// The caller's scopes or room roles do not allow this command.
    AuthScopeInsufficient = 2021, "auth.scope_insufficient", Never;
    /// The caller may not read this path.
    PathForbidden = 2022, "path.forbidden", Never;
    /// No such room.
    RoomNotFound = 3001, "room.not_found", Never;
    /// Another session holds this seat (doc 09 section 11.2).
    SlotInUse = 3011, "slot.in_use", After;
    /// The room was restored to an older state than the client saw.
    RoomRolledBack = 3040, "room.rolled_back", Never;
    /// The resume cursor is older than the retained window.
    ResumeTooOld = 3041, "resume.too_old", Never;
    /// A repeated `cid` with a different payload (doc 01 D1.10 rule 3).
    CidConflict = 3050, "cid.conflict", Never;
    /// An `ack` with no slot, or beyond the last delivered `item_seq`.
    AckInvalid = 3051, "ack.invalid", Never;
    /// A room role removed this session.
    RoomKicked = 3060, "room.kicked", Never;
    /// Too many frames or commands.
    RateLimited = 4001, "rate.limited", After;
    /// Too many watches or paths for this session.
    WatchLimit = 4002, "watch.limit", Never;
    /// A frame above `limits.max_frame_bytes`.
    FrameTooLarge = 4003, "frame.too_large", Never;
    /// The room cannot serve right now.
    RoomUnavailable = 5001, "room.unavailable", Backoff;
    /// The room is moving to another node; reconnect after `after_ms`.
    RoomMoving = 5002, "room.moving", After;
}

impl ErrorCode {
    /// The registered code with this key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|e| e.key() == key)
    }

    /// The registered code with this number.
    #[must_use]
    pub fn from_code(code: u16) -> Option<Self> {
        Self::ALL.iter().copied().find(|e| e.code() == code)
    }
}

/// Serialized as its key, for the registry document.
impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::collections::BTreeSet;

    #[test]
    fn codes_and_keys_are_unique_and_keys_are_dotted_lowercase() {
        let codes: BTreeSet<_> = ErrorCode::ALL.iter().map(|e| e.code()).collect();
        let keys: BTreeSet<_> = ErrorCode::ALL.iter().map(|e| e.key()).collect();
        assert_eq!(codes.len(), ErrorCode::ALL.len(), "duplicate code");
        assert_eq!(keys.len(), ErrorCode::ALL.len(), "duplicate key");
        for k in keys {
            assert!(
                k.contains('.')
                    && k.bytes().all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'),
                "{k} is not a dotted lower-case key"
            );
        }
    }

    #[test]
    fn code_is_in_its_area_range() {
        // 1xxx framing, 2xxx auth and permission, 3xxx room and slot state, 4xxx limits,
        // 5xxx server fault (doc 01 D1.7).
        for &e in ErrorCode::ALL {
            let area = match e.key().split('.').next().unwrap() {
                "path" if e == ErrorCode::PathForbidden => 2,
                "capability" | "type" | "data" | "op" | "ext" | "proto" | "path" => 1,
                "auth" => 2,
                "room" if matches!(e, ErrorCode::RoomUnavailable | ErrorCode::RoomMoving) => 5,
                "room" | "slot" | "resume" | "cid" | "ack" => 3,
                "rate" | "watch" | "frame" => 4,
                other => panic!("unmapped area {other}"),
            };
            assert_eq!(e.code() / 1000, area, "{}", e.key());
        }
    }

    #[test]
    fn codes_agree_with_doc_09_where_it_assigns_one() {
        for (code, key) in [
            (1010, "proto.unsupported"),
            (2011, "auth.token_unknown"),
            (2012, "auth.token_revoked"),
            (2021, "auth.scope_insufficient"),
            (3001, "room.not_found"),
            (3011, "slot.in_use"),
            (4001, "rate.limited"),
        ] {
            assert_eq!(ErrorCode::from_key(key).map(ErrorCode::code), Some(code), "{key}");
        }
    }

    #[test]
    fn lookups_by_key_and_code_round_trip() {
        for &e in ErrorCode::ALL {
            assert_eq!(ErrorCode::from_key(e.key()), Some(e));
            assert_eq!(ErrorCode::from_code(e.code()), Some(e));
        }
        assert_eq!(ErrorCode::from_key("no.such"), None);
        assert_eq!(ErrorCode::from_code(9999), None);
    }

    #[test]
    fn retry_serializes_lowercase_and_codes_as_keys() {
        assert_eq!(serde_json::to_string(&Retry::Backoff).unwrap(), "\"backoff\"");
        assert_eq!(serde_json::to_string(&ErrorCode::CidConflict).unwrap(), "\"cid.conflict\"");
    }
}
