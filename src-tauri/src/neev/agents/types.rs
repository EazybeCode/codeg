#![allow(dead_code)] // Task 1 contracts are consumed by the following lifecycle tasks.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Provider {
    Claude,
    Codex,
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
pub(crate) struct CleanupTicket {
    pub(crate) operation_id: String,
    pub(crate) connection_id: String,
    pub(crate) provider: Provider,
    pub(crate) org_id: i32,
    pub(crate) user_id: i32,
    pub(crate) target_generation: i64,
    pub(crate) result_generation: i64,
    pub(crate) lease_owner: String,
    pub(crate) lease_expires_at: i64,
}
