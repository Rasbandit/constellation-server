//! Error registry (doc 01 D1.7). Numeric codes are PROVISIONAL until the registry (D-66) assigns them;
//! the range per area is normative, the number inside it is not. Append-only once published.
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Registered error codes (initial set from doc 01 D1.7).
pub enum ErrorCode {
    /// `capability.missing`
    CapabilityMissing,
    /// `type.unknown`: a command type that is not registered or not enabled.
    TypeUnknown,
    /// `data.invalid`: a payload that does not match its type's schema.
    DataInvalid,
    /// `auth.token_revoked`
    AuthTokenRevoked,
    /// `auth.step_up_required`
    AuthStepUpRequired,
    /// `room.rolled_back`
    RoomRolledBack,
    /// `resume.too_old`
    ResumeTooOld,
    /// `room.unavailable`
    RoomUnavailable,
}

impl ErrorCode {
    /// Every registered code, for registry tests and docs generation.
    pub const ALL: [Self; 8] = [
        Self::CapabilityMissing,
        Self::TypeUnknown,
        Self::DataInvalid,
        Self::AuthTokenRevoked,
        Self::AuthStepUpRequired,
        Self::RoomRolledBack,
        Self::ResumeTooOld,
        Self::RoomUnavailable,
    ];

    /// Numeric code (provisional, see module docs).
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::CapabilityMissing => 1001,
            Self::TypeUnknown => 1002,
            Self::DataInvalid => 1003,
            Self::AuthTokenRevoked => 2012,
            Self::AuthStepUpRequired => 2013,
            Self::RoomRolledBack => 3001,
            Self::ResumeTooOld => 3002,
            Self::RoomUnavailable => 5001,
        }
    }

    /// Normative dotted key; clients localize from this.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::CapabilityMissing => "capability.missing",
            Self::TypeUnknown => "type.unknown",
            Self::DataInvalid => "data.invalid",
            Self::AuthTokenRevoked => "auth.token_revoked",
            Self::AuthStepUpRequired => "auth.step_up_required",
            Self::RoomRolledBack => "room.rolled_back",
            Self::ResumeTooOld => "resume.too_old",
            Self::RoomUnavailable => "room.unavailable",
        }
    }

    /// Retry hint the server must send with this error.
    #[must_use]
    pub const fn retry(self) -> Retry {
        match self {
            Self::RoomUnavailable => Retry::Backoff,
            Self::CapabilityMissing
            | Self::TypeUnknown
            | Self::DataInvalid
            | Self::AuthTokenRevoked
            | Self::AuthStepUpRequired
            | Self::RoomRolledBack
            | Self::ResumeTooOld => Retry::Never,
        }
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
        assert_eq!(codes.len(), ErrorCode::ALL.len());
        assert_eq!(keys.len(), ErrorCode::ALL.len());
        for k in keys {
            assert!(
                k.contains('.')
                    && k.bytes().all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_')
            );
        }
    }

    #[test]
    fn code_is_in_its_area_range() {
        // 1xxx framing, 2xxx auth, 3xxx room, 5xxx server fault (doc 01 D1.7)
        for e in ErrorCode::ALL {
            let area = match e.key().split('.').next().unwrap() {
                "capability" | "type" | "data" => 1,
                "auth" => 2,
                "room" | "resume" if e == ErrorCode::RoomUnavailable => 5,
                "room" | "resume" => 3,
                other => panic!("unmapped area {other}"),
            };
            assert_eq!(e.code() / 1000, area, "{}", e.key());
        }
    }

    #[test]
    fn retry_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Retry::Backoff).unwrap(), "\"backoff\"");
    }
}
