#![allow(dead_code)] // Task 1 contracts are consumed by the following lifecycle tasks.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Provider {
    Claude,
    Codex,
    /// ACP providers (`acp_providers`). Offered only when gated on.
    Cursor,
    Gemini,
    OpenCode,
}

impl Provider {
    /// Every provider, in display order.
    pub(crate) const ALL: [Provider; 5] = [
        Provider::Claude,
        Provider::Codex,
        Provider::Cursor,
        Provider::Gemini,
        Provider::OpenCode,
    ];

    /// The provider for a stored/wire name, if this build knows it.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "cursor" => Some(Self::Cursor),
            "gemini" => Some(Self::Gemini),
            "opencode" => Some(Self::OpenCode),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Gemini => "gemini",
            Self::OpenCode => "opencode",
        }
    }

    /// Whether the product may offer this provider right now: Claude and Codex
    /// always; an ACP provider only when `NEEV_AGENT_ACP_PROVIDERS` names it.
    pub(crate) fn offered(self) -> bool {
        match self {
            Self::Claude | Self::Codex => true,
            _ => super::acp_providers::hosted_enabled(self.name()),
        }
    }

    /// The offered provider for a request-supplied name. An ACP provider that
    /// is not gated on is `None` — the same answer as an unknown name.
    pub(crate) fn parse_offered(value: &str) -> Option<Self> {
        Self::parse(value).filter(|provider| provider.offered())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    ReconnectRequired,
    Disconnecting,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentError {
    Auth(crate::neev::error::AuthError),
    NotFound,
    InvalidInput,
    Conflict,
    RateLimited,
    Unavailable,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ConnectionView {
    pub(crate) provider: Provider,
    pub(crate) state: ConnectionState,
    pub(crate) account_label: Option<String>,
    pub(crate) checked_at: Option<i64>,
    pub(crate) error_code: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LoginTicket {
    pub(crate) connection_id: String,
    pub(crate) provider: Provider,
    pub(crate) generation: i64,
    pub(crate) org_id: i32,
    pub(crate) user_id: i32,
    pub(crate) session_id: String,
    pub(crate) attempt_id: String,
    pub(crate) operation_id: String,
    pub(crate) lease_owner: String,
    pub(crate) expires_at: i64,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum BeginOutcome {
    Pending(LoginTicket),
    Settled(ConnectionView),
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct MutationReceipt {
    pub(crate) operation_id: String,
    pub(crate) generation: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttemptView {
    pub(crate) state: String,
    pub(crate) expires_at: i64,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct VerifiedSubscription {
    pub(crate) account_label: String,
    pub(crate) storage_handle: String,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum CleanupTarget {
    Cancel { attempt_id: String },
    Disconnect,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CleanupTicket {
    pub(crate) operation_id: String,
    pub(crate) connection_id: String,
    pub(crate) provider: Provider,
    pub(crate) org_id: i32,
    pub(crate) user_id: i32,
    pub(crate) target: CleanupTarget,
    pub(crate) target_generation: i64,
    pub(crate) result_generation: i64,
    pub(crate) lease_owner: String,
    pub(crate) lease_expires_at: i64,
}
