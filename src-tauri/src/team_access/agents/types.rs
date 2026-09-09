use chrono::{DateTime, Utc};

use crate::team_access::core::TeamAuthError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    ReconnectRequired,
    Disconnecting,
    Unavailable,
}

impl ConnectionState {
    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "disconnected" => Some(Self::Disconnected),
            "connecting" => Some(Self::Connecting),
            "connected" => Some(Self::Connected),
            "reconnect_required" => Some(Self::ReconnectRequired),
            "disconnecting" => Some(Self::Disconnecting),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentError {
    Auth(TeamAuthError),
    NotFound,
    InvalidInput,
    Conflict,
    RateLimited,
    Unavailable,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ConnectionView {
    pub(crate) provider: Provider,
    pub(crate) state: ConnectionState,
    pub(crate) account_label: Option<String>,
    pub(crate) checked_at: Option<DateTime<Utc>>,
    pub(crate) error_code: Option<String>,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct LoginTicket {
    pub(crate) connection_id: String,
    pub(crate) provider: Provider,
    pub(crate) generation: i64,
    pub(crate) org_id: i32,
    pub(crate) user_id: i32,
    pub(crate) attempt_id: String,
    pub(crate) operation_id: String,
    pub(crate) lease_owner: String,
    pub(crate) expires_at: DateTime<Utc>,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct VerifiedSubscription {
    pub(crate) account_label: String,
    pub(crate) storage_handle: String,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct CleanupTicket {
    pub(crate) operation_id: String,
    pub(crate) connection_id: String,
    pub(crate) provider: Provider,
    pub(crate) org_id: i32,
    pub(crate) user_id: i32,
    pub(crate) target_generation: i64,
    pub(crate) result_generation: i64,
    pub(crate) lease_owner: String,
    pub(crate) lease_expires_at: DateTime<Utc>,
}

pub(super) fn valid_account_label(value: &str) -> bool {
    let characters = value.chars().count();
    (1..=160).contains(&characters)
        && !value
            .chars()
            .any(|character| character.is_control() || is_bidi_formatting(character))
}

pub(super) fn valid_storage_handle(value: &str) -> bool {
    value.len() == 32
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        && uuid::Uuid::parse_str(value).is_ok()
}

fn is_bidi_formatting(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct MutationReceipt {
    pub(crate) operation_id: String,
    pub(crate) generation: i64,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) enum BeginOutcome {
    Pending(LoginTicket),
    Settled(ConnectionView),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AttemptView {
    pub(crate) state: String,
    pub(crate) expires_at: DateTime<Utc>,
}
