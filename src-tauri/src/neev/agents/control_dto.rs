use serde::de::{DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

const REQUEST_MAX: usize = 8192;
const RESPONSE_MAX: usize = 16384;
const MAX_DEPTH: usize = 16;
const MAX_MEMBERS: usize = 32;
const MAX_KEY_BYTES: usize = 64;
const MAX_STRING_BYTES: usize = 1024;
const MAX_ARRAY: usize = 32;
const I64_MAX: i64 = 9_223_372_036_854_775_807;
const DEVICE_URL: &str = "https://auth.openai.com/codex/device";
/// Verified against the pinned Claude 2.1.267 binary in an offline probe. The
/// legacy `claude.ai` origin is deliberately NOT accepted.
const CLAUDE_AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DtoError {
    Invalid,
    Correlation,
    Sequence,
    State,
    Expired,
}

impl fmt::Display for DtoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "invalid",
            Self::Correlation => "correlation",
            Self::Sequence => "sequence",
            Self::State => "state",
            Self::Expired => "expired",
        })
    }
}

impl std::error::Error for DtoError {}

fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Id(String);

impl Id {
    pub(crate) fn new(value: String) -> Result<Self, DtoError> {
        if valid_id(&value) {
            Ok(Self(value))
        } else {
            Err(DtoError::Invalid)
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SessionId(String);

impl SessionId {
    pub(crate) fn new(value: String) -> Result<Self, DtoError> {
        let bytes = value.as_bytes();
        let valid = bytes.len() == 36
            && bytes[8] == b'-'
            && bytes[13] == b'-'
            && bytes[18] == b'-'
            && bytes[23] == b'-'
            && bytes[14] == b'4'
            && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
            && bytes.iter().enumerate().all(|(index, byte)| {
                matches!(index, 8 | 13 | 18 | 23)
                    || byte.is_ascii_digit()
                    || (b'a'..=b'f').contains(byte)
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(DtoError::Invalid)
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Counter(i64);

impl Counter {
    pub(crate) fn new(value: String) -> Result<Self, DtoError> {
        if value.is_empty()
            || value.len() > 19
            || value.starts_with('0')
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(DtoError::Invalid);
        }
        let parsed = value.parse::<i64>().map_err(|_| DtoError::Invalid)?;
        if parsed < 1 {
            return Err(DtoError::Invalid);
        }
        Ok(Self(parsed))
    }

    pub(crate) fn get(&self) -> i64 {
        self.0
    }
}

impl Serialize for Counter {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Counter {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

macro_rules! integer_newtype {
    ($name:ident, $minimum:expr, $maximum:expr) => {
        #[derive(Clone, PartialEq, Eq, Serialize)]
        pub(crate) struct $name(i64);

        impl $name {
            pub(crate) fn new(value: i64) -> Result<Self, DtoError> {
                if ($minimum..=$maximum).contains(&value) {
                    Ok(Self(value))
                } else {
                    Err(DtoError::Invalid)
                }
            }

            pub(crate) fn get(&self) -> i64 {
                self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = u64::deserialize(deserializer)?;
                let value = i64::try_from(value).map_err(serde::de::Error::custom)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

integer_newtype!(OwnerId, 1, 2_147_483_647);
integer_newtype!(UnixSec, 0, 253_402_300_799);
integer_newtype!(RetryMs, 250, 30_000);

fn allowed_label_character(character: char) -> bool {
    let code = character as u32;
    !(code <= 0x1f
        || (0x7f..=0x9f).contains(&code)
        || code == 0x061c
        || matches!(code, 0x200e | 0x200f)
        || (0x202a..=0x202e).contains(&code)
        || (0x2066..=0x2069).contains(&code))
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Label(String);

impl Label {
    pub(crate) fn new(value: String) -> Result<Self, DtoError> {
        let count = value.chars().count();
        if (1..=160).contains(&count)
            && value.len() <= 640
            && value.chars().all(allowed_label_character)
        {
            Ok(Self(value))
        } else {
            Err(DtoError::Invalid)
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Label {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UserCode(String);

impl UserCode {
    pub(crate) fn new(value: String) -> Result<Self, DtoError> {
        if (1..=64).contains(&value.len())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
        {
            Ok(Self(value))
        } else {
            Err(DtoError::Invalid)
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for UserCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

macro_rules! wire_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub(crate) enum $name {
            $(
                #[serde(rename = $wire)]
                $variant,
            )+
        }

        impl $name {
            pub(crate) fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }

            fn from_wire(value: &str) -> Result<Self, DtoError> {
                match value {
                    $($wire => Ok(Self::$variant),)+
                    _ => Err(DtoError::Invalid),
                }
            }
        }
    };
}

// Both providers Neev can connect. Claude was previously absent here, so a
// Claude sign-in could not even be *expressed* on the control plane; the run
// side (`agents/execution.rs`) has always supported both.
wire_enum!(Provider { Codex => "codex", Claude => "claude" });
wire_enum!(Op {
    StartLogin => "start_login",
    ObserveLogin => "observe_login",
    SettleLogin => "settle_login",
    CheckConnection => "check_connection",
    CleanupConnection => "cleanup_connection",
    ReconcilePage => "reconcile_page",
    // Delivers the authorization code a Claude-style login asks the user to
    // paste back. Codex device logins never use it.
    SubmitCode => "submit_code",
});
wire_enum!(Outcome { Ok => "ok", Error => "error" });
wire_enum!(ErrorCode {
    NotFound => "not_found",
    Conflict => "conflict",
    Stale => "stale",
    Expired => "expired",
    Busy => "busy",
    Unavailable => "unavailable",
});
wire_enum!(Disposition { Accepted => "accepted", Replayed => "replayed" });
wire_enum!(LoginFailure {
    ProviderUnavailable => "provider_unavailable",
    ReauthRequired => "reauth_required",
    UnsupportedAccount => "unsupported_account",
});
wire_enum!(CheckFailure {
    ProviderUnavailable => "provider_unavailable",
    Interrupted => "interrupted",
    Expired => "expired",
});
wire_enum!(CleanupPhase {
    Queued => "queued",
    Canceling => "canceling",
    LoggingOut => "logging_out",
    Stopping => "stopping",
    Disposing => "disposing",
});
wire_enum!(LogoutResult {
    Confirmed => "confirmed",
    NotConfirmed => "not_confirmed",
    NotNeeded => "not_needed",
});
wire_enum!(RegistryPhase {
    AllocationPending => "allocation_pending",
    PendingLogin => "pending_login",
    Prepared => "prepared",
    Committed => "committed",
    CleanupPending => "cleanup_pending",
    Quarantined => "quarantined",
    Disposed => "disposed",
});

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scope {
    pub(crate) org_id: OwnerId,
    pub(crate) user_id: OwnerId,
    pub(crate) provider: Provider,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Coordinator {
    pub(crate) epoch: Counter,
    pub(crate) boot_id: Id,
}

impl Coordinator {
    pub(crate) fn new(epoch: Counter, boot_id: Id) -> Self {
        Self { epoch, boot_id }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginBinding {
    pub(crate) scope: Scope,
    pub(crate) connection_id: Id,
    pub(crate) generation: Counter,
    pub(crate) session_id: SessionId,
    pub(crate) attempt_id: Id,
    pub(crate) operation_id: Id,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginTicketDto {
    pub(crate) binding: LoginBinding,
    pub(crate) lease_owner: Id,
    pub(crate) expires_at: UnixSec,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConnectionBinding {
    pub(crate) scope: Scope,
    pub(crate) connection_id: Id,
    pub(crate) generation: Counter,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckTicketDto {
    pub(crate) binding: ConnectionBinding,
    pub(crate) session_id: SessionId,
    pub(crate) storage_handle: Id,
    pub(crate) check_id: Id,
    pub(crate) expires_at: UnixSec,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CompletedConnectKind {
    #[serde(rename = "completed_connect")]
    CompletedConnect,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommittedLoginWitnessDto {
    pub(crate) kind: CompletedConnectKind,
    pub(crate) binding: LoginBinding,
    pub(crate) storage_handle: Id,
    pub(crate) completed_at: UnixSec,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CleanupTarget {
    Cancel { attempt_id: Id },
    Disconnect,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CleanupTicketDto {
    pub(crate) scope: Scope,
    pub(crate) connection_id: Id,
    pub(crate) operation_id: Id,
    pub(crate) target: CleanupTarget,
    pub(crate) target_generation: Counter,
    pub(crate) result_generation: Counter,
    pub(crate) lease_owner: Id,
    pub(crate) lease_expires_at: UnixSec,
}

impl CleanupTicketDto {
    fn validate(&self) -> Result<(), DtoError> {
        let target = self.target_generation.get();
        if target == I64_MAX || self.result_generation.get() != target + 1 {
            return Err(DtoError::Invalid);
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartLoginBody {
    pub(crate) ticket: LoginTicketDto,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObserveLoginBody {
    pub(crate) binding: LoginBinding,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubmitCodeBody {
    pub(crate) binding: LoginBinding,
    pub(crate) code: AuthorizationCode,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SettleLoginBody {
    pub(crate) receipt_id: Id,
    pub(crate) witness: CommittedLoginWitnessDto,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckConnectionBody {
    pub(crate) ticket: CheckTicketDto,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CleanupConnectionBody {
    pub(crate) ticket: CleanupTicketDto,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReconcilePageRequest {
    pub(crate) snapshot_revision: Option<Counter>,
    pub(crate) after_entry_id: Option<Id>,
    pub(crate) limit: u8,
}

impl ReconcilePageRequest {
    fn validate(&self) -> Result<(), DtoError> {
        if self.snapshot_revision.is_some() != self.after_entry_id.is_some()
            || !(1..=32).contains(&self.limit)
        {
            return Err(DtoError::Invalid);
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum RequestBody {
    StartLogin(StartLoginBody),
    ObserveLogin(ObserveLoginBody),
    SettleLogin(SettleLoginBody),
    CheckConnection(CheckConnectionBody),
    CleanupConnection(CleanupConnectionBody),
    ReconcilePage(ReconcilePageRequest),
    SubmitCode(SubmitCodeBody),
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) coordinator: Coordinator,
    pub(crate) sequence: Counter,
    pub(crate) request_id: Id,
    pub(crate) body: RequestBody,
}

impl Request {
    pub(crate) fn op(&self) -> Op {
        match self.body {
            RequestBody::StartLogin(_) => Op::StartLogin,
            RequestBody::ObserveLogin(_) => Op::ObserveLogin,
            RequestBody::SettleLogin(_) => Op::SettleLogin,
            RequestBody::CheckConnection(_) => Op::CheckConnection,
            RequestBody::CleanupConnection(_) => Op::CleanupConnection,
            RequestBody::ReconcilePage(_) => Op::ReconcilePage,
            RequestBody::SubmitCode(_) => Op::SubmitCode,
        }
    }

    pub(crate) fn login_ticket(&self) -> Option<&LoginTicketDto> {
        match &self.body {
            RequestBody::StartLogin(body) => Some(&body.ticket),
            _ => None,
        }
    }

    pub(crate) fn login_binding(&self) -> Option<&LoginBinding> {
        match &self.body {
            RequestBody::ObserveLogin(body) => Some(&body.binding),
            RequestBody::SubmitCode(body) => Some(&body.binding),
            _ => None,
        }
    }

    pub(crate) fn cleanup_ticket(&self) -> Option<&CleanupTicketDto> {
        match &self.body {
            RequestBody::CleanupConnection(body) => Some(&body.ticket),
            _ => None,
        }
    }

    fn validate(&self) -> Result<(), DtoError> {
        match &self.body {
            RequestBody::CleanupConnection(body) => body.ticket.validate(),
            RequestBody::ReconcilePage(body) => body.validate(),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartLoginResult {
    pub(crate) disposition: Disposition,
    pub(crate) poll_after_ms: RetryMs,
}

/// What the user must do to approve a sign-in — **provider-shaped**, because
/// the two providers genuinely differ.
///
/// Codex runs a device flow (a user code entered at a fixed verification URL).
/// Claude 2.1.267 does not: `auth login` prints an authorize URL and then reads
/// an authorization code from stdin ("Paste code here if prompted"). Modelling
/// Claude as a device flow would mean inventing a `user_code` that does not
/// exist, so the two are separate variants and each URL is allowlisted to its
/// own provider's origin.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LoginChallenge {
    /// Codex: show `user_code`, the user enters it at `verification_url`, and
    /// the supervisor polls. Neev needs no further input.
    Device {
        verification_url: String,
        user_code: UserCode,
        expires_at: UnixSec,
    },
    /// Claude: the user opens `verification_url`, approves, and pastes the
    /// resulting authorization code back — delivered by [`Op::SubmitCode`].
    AuthorizationCode {
        verification_url: String,
        expires_at: UnixSec,
    },
}

impl LoginChallenge {
    pub(crate) fn verification_url(&self) -> &str {
        match self {
            Self::Device {
                verification_url, ..
            }
            | Self::AuthorizationCode {
                verification_url, ..
            } => verification_url,
        }
    }

    pub(crate) fn expires_at(&self) -> &UnixSec {
        match self {
            Self::Device { expires_at, .. } | Self::AuthorizationCode { expires_at, .. } => {
                expires_at
            }
        }
    }

    pub(crate) fn user_code(&self) -> Option<&UserCode> {
        match self {
            Self::Device { user_code, .. } => Some(user_code),
            Self::AuthorizationCode { .. } => None,
        }
    }

    /// A challenge may only ever point at a known provider origin. A supervisor
    /// that returned some other URL would be redirecting the user somewhere we
    /// did not intend, so the whole exchange is refused.
    fn validate(&self) -> Result<(), DtoError> {
        let acceptable = match self {
            Self::Device {
                verification_url, ..
            } => verification_url == DEVICE_URL,
            Self::AuthorizationCode {
                verification_url, ..
            } => verification_url.starts_with(CLAUDE_AUTHORIZE_URL),
        };
        if acceptable {
            Ok(())
        } else {
            Err(DtoError::Invalid)
        }
    }
}

/// An authorization code pasted back by the user.
///
/// Deliberately has no `Debug`/`Display`: it is a short-lived bearer secret and
/// must never reach a log, an error message or a panic payload.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AuthorizationCode(String);

impl AuthorizationCode {
    pub(crate) fn new(value: String) -> Result<Self, DtoError> {
        // Bounded, single-line, printable: whatever the provider issues is a
        // compact token, and anything else is a paste accident or an attack.
        if (1..=512).contains(&value.len())
            && !value.chars().any(|ch| ch.is_control() || ch.is_whitespace())
        {
            Ok(Self(value))
        } else {
            Err(DtoError::Invalid)
        }
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for AuthorizationCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum LoginSnapshot {
    Queued {
        expires_at: UnixSec,
        poll_after_ms: RetryMs,
    },
    Starting {
        expires_at: UnixSec,
        poll_after_ms: RetryMs,
    },
    AwaitingUser {
        expires_at: UnixSec,
        poll_after_ms: RetryMs,
        challenge: LoginChallenge,
    },
    Verifying {
        expires_at: UnixSec,
        poll_after_ms: RetryMs,
    },
    Prepared {
        receipt_id: Id,
        storage_handle: Id,
        account_label: Option<Label>,
    },
    Committed {
        receipt_id: Id,
        storage_handle: Id,
    },
    Failed {
        reason: LoginFailure,
    },
    Expired,
    Canceled,
    Interrupted,
}

impl LoginSnapshot {
    fn validate(&self) -> Result<(), DtoError> {
        if let Self::AwaitingUser { challenge, .. } = self {
            challenge.validate()?;
        }
        Ok(())
    }

    fn tag(&self) -> &'static str {
        match self {
            Self::Queued { .. } => "queued",
            Self::Starting { .. } => "starting",
            Self::AwaitingUser { .. } => "awaiting_user",
            Self::Verifying { .. } => "verifying",
            Self::Prepared { .. } => "prepared",
            Self::Committed { .. } => "committed",
            Self::Failed { .. } => "failed",
            Self::Expired => "expired",
            Self::Canceled => "canceled",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CommittedState {
    #[serde(rename = "committed")]
    Committed,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SettleLoginResult {
    pub(crate) state: CommittedState,
    pub(crate) receipt_id: Id,
    pub(crate) storage_handle: Id,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum AccountStatus {
    Supported { account_label: Label },
    Disconnected,
    Unsupported,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum CheckResult {
    Queued {
        check_id: Id,
        poll_after_ms: RetryMs,
    },
    Checking {
        check_id: Id,
        poll_after_ms: RetryMs,
    },
    Finished {
        check_id: Id,
        observed_at: UnixSec,
        account: AccountStatus,
    },
    Unavailable {
        check_id: Id,
        reason: CheckFailure,
    },
}

impl CheckResult {
    fn check_id(&self) -> &Id {
        match self {
            Self::Queued { check_id, .. }
            | Self::Checking { check_id, .. }
            | Self::Finished { check_id, .. }
            | Self::Unavailable { check_id, .. } => check_id,
        }
    }

    fn tag(&self) -> &'static str {
        match self {
            Self::Queued { .. } => "queued",
            Self::Checking { .. } => "checking",
            Self::Finished { .. } => "finished",
            Self::Unavailable { .. } => "unavailable",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum CleanupResult {
    Pending {
        phase: CleanupPhase,
        poll_after_ms: RetryMs,
    },
    Completed {
        provider_logout: LogoutResult,
    },
    Failed {
        reason: CleanupFailedReason,
        retry_after_ms: RetryMs,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CleanupFailedReason {
    #[serde(rename = "cleanup_failed")]
    CleanupFailed,
}

impl CleanupResult {
    fn tag(&self) -> &'static str {
        match self {
            Self::Pending { .. } => "pending",
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReconcileEntry {
    pub(crate) entry_id: Id,
    pub(crate) binding: ConnectionBinding,
    pub(crate) attempt_id: Id,
    pub(crate) operation_id: Id,
    pub(crate) phase: RegistryPhase,
    pub(crate) storage_handle: Option<Id>,
    pub(crate) receipt_id: Option<Id>,
    pub(crate) cleanup_operation_id: Option<Id>,
}

impl ReconcileEntry {
    fn validate(&self) -> Result<(), DtoError> {
        let handle = self.storage_handle.is_some();
        let receipt = self.receipt_id.is_some();
        let cleanup = self.cleanup_operation_id.is_some();
        let valid = match self.phase {
            RegistryPhase::AllocationPending => !handle && !receipt && !cleanup,
            RegistryPhase::PendingLogin => handle && !receipt && !cleanup,
            RegistryPhase::Prepared | RegistryPhase::Committed => handle && receipt && !cleanup,
            RegistryPhase::CleanupPending => cleanup && (!receipt || handle),
            RegistryPhase::Quarantined | RegistryPhase::Disposed => !receipt || handle,
        };
        if valid {
            Ok(())
        } else {
            Err(DtoError::Invalid)
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReconcilePage {
    pub(crate) snapshot_revision: Counter,
    pub(crate) entries: Vec<ReconcileEntry>,
    pub(crate) next_after_entry_id: Option<Id>,
}

impl ReconcilePage {
    fn validate(&self) -> Result<(), DtoError> {
        if self.entries.len() > MAX_ARRAY {
            return Err(DtoError::Invalid);
        }
        for entry in &self.entries {
            entry.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum ResultBody {
    StartLogin(StartLoginResult),
    ObserveLogin(LoginSnapshot),
    SettleLogin(SettleLoginResult),
    CheckConnection(CheckResult),
    CleanupConnection(CleanupResult),
    ReconcilePage(ReconcilePage),
    /// Submitting a code advances the same login state machine, so it answers
    /// with the same snapshot `observe_login` would return.
    SubmitCode(LoginSnapshot),
}

impl ResultBody {
    fn op(&self) -> Op {
        match self {
            Self::StartLogin(_) => Op::StartLogin,
            Self::ObserveLogin(_) => Op::ObserveLogin,
            Self::SettleLogin(_) => Op::SettleLogin,
            Self::CheckConnection(_) => Op::CheckConnection,
            Self::CleanupConnection(_) => Op::CleanupConnection,
            Self::SubmitCode(_) => Op::SubmitCode,
            Self::ReconcilePage(_) => Op::ReconcilePage,
        }
    }

    fn validate(&self) -> Result<(), DtoError> {
        match self {
            Self::ObserveLogin(snapshot) => snapshot.validate(),
            Self::ReconcilePage(page) => page.validate(),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppError {
    pub(crate) code: ErrorCode,
    pub(crate) retry_after_ms: Option<RetryMs>,
}

impl AppError {
    fn validate(&self) -> Result<(), DtoError> {
        let requires = matches!(self.code, ErrorCode::Busy | ErrorCode::Unavailable);
        if requires == self.retry_after_ms.is_some() {
            Ok(())
        } else {
            Err(DtoError::Invalid)
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Response {
    pub(crate) coordinator: Coordinator,
    pub(crate) supervisor_boot_id: Id,
    pub(crate) sequence: Counter,
    pub(crate) request_id: Id,
    pub(crate) operation: Op,
    pub(crate) outcome: Outcome,
    pub(crate) body: Option<ResultBody>,
    pub(crate) error: Option<AppError>,
}

impl Response {
    pub(crate) fn op(&self) -> Op {
        self.operation
    }

    pub(crate) fn outcome(&self) -> Outcome {
        self.outcome
    }

    pub(crate) fn tag(&self) -> &'static str {
        if let Some(error) = &self.error {
            return error.code.as_str();
        }
        match self.body.as_ref().expect("validated response body") {
            ResultBody::StartLogin(result) => result.disposition.as_str(),
            ResultBody::ObserveLogin(result) => result.tag(),
            ResultBody::SettleLogin(_) => "committed",
            ResultBody::CheckConnection(result) => result.tag(),
            ResultBody::CleanupConnection(result) => result.tag(),
            ResultBody::ReconcilePage(_) => "page",
            ResultBody::SubmitCode(result) => result.tag(),
        }
    }

    pub(crate) fn login_snapshot(&self) -> Option<&LoginSnapshot> {
        match self.body.as_ref() {
            Some(ResultBody::ObserveLogin(snapshot)) => Some(snapshot),
            _ => None,
        }
    }

    fn validate(&self) -> Result<(), DtoError> {
        match (&self.body, &self.error, self.outcome) {
            (Some(body), None, Outcome::Ok) if body.op() == self.operation => body.validate(),
            (None, Some(error), Outcome::Error) => error.validate(),
            _ => Err(DtoError::Invalid),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LoginKey(LoginTicketDto);

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CleanupKey {
    scope: Scope,
    connection_id: Id,
    operation_id: Id,
    target: CleanupTarget,
    target_generation: Counter,
    result_generation: Counter,
}

pub(crate) fn login_semantic_key(ticket: &LoginTicketDto) -> LoginKey {
    LoginKey(ticket.clone())
}

pub(crate) fn cleanup_semantic_key(ticket: &CleanupTicketDto) -> CleanupKey {
    CleanupKey {
        scope: ticket.scope.clone(),
        connection_id: ticket.connection_id.clone(),
        operation_id: ticket.operation_id.clone(),
        target: ticket.target.clone(),
        target_generation: ticket.target_generation.clone(),
        result_generation: ticket.result_generation.clone(),
    }
}

#[derive(Clone, PartialEq, Eq)]
enum JsonNode {
    Null,
    Bool(bool),
    Unsigned(u64),
    String(String),
    Array(Vec<JsonNode>),
    Object(Vec<(String, JsonNode)>),
}

impl Serialize for JsonNode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_none(),
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Unsigned(value) => serializer.serialize_u64(*value),
            Self::String(value) => serializer.serialize_str(value),
            Self::Array(values) => values.serialize(serializer),
            Self::Object(values) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(values.len()))?;
                for (key, value) in values {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = JsonNode;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded JSON")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(JsonNode::Null)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(JsonNode::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(JsonNode::Bool(value))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(JsonNode::Unsigned(value))
    }

    fn visit_i64<E: serde::de::Error>(self, _value: i64) -> Result<Self::Value, E> {
        Err(E::custom(DtoError::Invalid))
    }

    fn visit_f64<E: serde::de::Error>(self, _value: f64) -> Result<Self::Value, E> {
        Err(E::custom(DtoError::Invalid))
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        self.visit_string(value.to_owned())
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
        if value.len() > MAX_STRING_BYTES {
            return Err(E::custom(DtoError::Invalid));
        }
        Ok(JsonNode::String(value))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<JsonNode>()? {
            if values.len() == MAX_ARRAY {
                return Err(serde::de::Error::custom(DtoError::Invalid));
            }
            values.push(value);
        }
        Ok(JsonNode::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.len() == MAX_MEMBERS
                || key.len() > MAX_KEY_BYTES
                || !key.is_ascii()
                || values.iter().any(|(existing, _)| existing == &key)
            {
                return Err(serde::de::Error::custom(DtoError::Invalid));
            }
            let value = map.next_value::<JsonNode>()?;
            values.push((key, value));
        }
        Ok(JsonNode::Object(values))
    }
}

impl<'de> Deserialize<'de> for JsonNode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

impl JsonNode {
    fn object(&self) -> Result<&[(String, JsonNode)], DtoError> {
        match self {
            Self::Object(value) => Ok(value),
            _ => Err(DtoError::Invalid),
        }
    }

    fn string(&self) -> Result<&str, DtoError> {
        match self {
            Self::String(value) => Ok(value),
            _ => Err(DtoError::Invalid),
        }
    }
}

fn field<'a>(object: &'a [(String, JsonNode)], name: &str) -> Result<&'a JsonNode, DtoError> {
    object
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
        .ok_or(DtoError::Invalid)
}

fn exact_keys(object: &[(String, JsonNode)], names: &[&str]) -> Result<(), DtoError> {
    if object.len() == names.len()
        && names
            .iter()
            .all(|name| object.iter().any(|(key, _)| key == name))
    {
        Ok(())
    } else {
        Err(DtoError::Invalid)
    }
}

fn scan_json(raw: &[u8], maximum: usize) -> Result<(), DtoError> {
    if raw.is_empty() || raw.len() > maximum || raw.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(DtoError::Invalid);
    }
    let text = std::str::from_utf8(raw).map_err(|_| DtoError::Invalid)?;
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth = depth.checked_add(1).ok_or(DtoError::Invalid)?;
                if depth > MAX_DEPTH {
                    return Err(DtoError::Invalid);
                }
            }
            b'}' | b']' => {
                depth = depth.checked_sub(1).ok_or(DtoError::Invalid)?;
            }
            b'-' => return Err(DtoError::Invalid),
            b'0'..=b'9' => {
                let mut end = index + 1;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end < bytes.len() && matches!(bytes[end], b'.' | b'e' | b'E') {
                    return Err(DtoError::Invalid);
                }
                index = end;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    if depth != 0 || in_string {
        return Err(DtoError::Invalid);
    }
    Ok(())
}

fn parse_tree(raw: &[u8], maximum: usize) -> Result<JsonNode, DtoError> {
    scan_json(raw, maximum)?;
    let mut deserializer = serde_json::Deserializer::from_slice(raw);
    let value = JsonNode::deserialize(&mut deserializer).map_err(|_| DtoError::Invalid)?;
    deserializer.end().map_err(|_| DtoError::Invalid)?;
    if !matches!(value, JsonNode::Object(_)) {
        return Err(DtoError::Invalid);
    }
    Ok(value)
}

#[cfg(test)]
pub(crate) fn validate_bounded_json_for_test(raw: &[u8], maximum: usize) -> Result<(), DtoError> {
    parse_tree(raw, maximum).map(|_| ())
}

fn encode_json<T: Serialize>(value: &T, maximum: usize) -> Result<Vec<u8>, DtoError> {
    let encoded = serde_json::to_vec(value).map_err(|_| DtoError::Invalid)?;
    parse_tree(&encoded, maximum)?;
    Ok(encoded)
}

#[cfg(test)]
pub(crate) fn encode_response_bound_control_for_test(
    final_string_size: usize,
) -> Result<Vec<u8>, DtoError> {
    #[derive(Serialize)]
    struct BoundControl {
        value: Vec<String>,
    }

    let mut value = vec!["s".repeat(MAX_STRING_BYTES); 15];
    value.push("s".repeat(final_string_size));
    encode_json(&BoundControl { value }, RESPONSE_MAX)
}

fn parse_as<T: DeserializeOwned>(tree: &JsonNode) -> Result<T, DtoError> {
    let encoded = serde_json::to_vec(tree).map_err(|_| DtoError::Invalid)?;
    serde_json::from_slice(&encoded).map_err(|_| DtoError::Invalid)
}

fn schema_scope(value: &JsonNode) -> Result<(), DtoError> {
    exact_keys(value.object()?, &["org_id", "user_id", "provider"])
}

fn schema_coordinator(value: &JsonNode) -> Result<(), DtoError> {
    exact_keys(value.object()?, &["epoch", "boot_id"])
}

fn schema_login_binding(value: &JsonNode) -> Result<(), DtoError> {
    let object = value.object()?;
    exact_keys(
        object,
        &[
            "scope",
            "connection_id",
            "generation",
            "session_id",
            "attempt_id",
            "operation_id",
        ],
    )?;
    schema_scope(field(object, "scope")?)
}

fn schema_connection_binding(value: &JsonNode) -> Result<(), DtoError> {
    let object = value.object()?;
    exact_keys(object, &["scope", "connection_id", "generation"])?;
    schema_scope(field(object, "scope")?)
}

fn schema_login_ticket(value: &JsonNode) -> Result<(), DtoError> {
    let object = value.object()?;
    exact_keys(object, &["binding", "lease_owner", "expires_at"])?;
    schema_login_binding(field(object, "binding")?)
}

fn schema_check_ticket(value: &JsonNode) -> Result<(), DtoError> {
    let object = value.object()?;
    exact_keys(
        object,
        &[
            "binding",
            "session_id",
            "storage_handle",
            "check_id",
            "expires_at",
        ],
    )?;
    schema_connection_binding(field(object, "binding")?)
}

fn schema_cleanup_ticket(value: &JsonNode) -> Result<(), DtoError> {
    let object = value.object()?;
    exact_keys(
        object,
        &[
            "scope",
            "connection_id",
            "operation_id",
            "target",
            "target_generation",
            "result_generation",
            "lease_owner",
            "lease_expires_at",
        ],
    )?;
    schema_scope(field(object, "scope")?)?;
    let target = field(object, "target")?.object()?;
    match field(target, "kind")?.string()? {
        "cancel" => exact_keys(target, &["kind", "attempt_id"]),
        "disconnect" => exact_keys(target, &["kind"]),
        _ => Err(DtoError::Invalid),
    }
}

fn schema_request(tree: &JsonNode) -> Result<Op, DtoError> {
    let root = tree.object()?;
    exact_keys(
        root,
        &["v", "coordinator", "sequence", "request_id", "op", "body"],
    )?;
    schema_coordinator(field(root, "coordinator")?)?;
    let op = Op::from_wire(field(root, "op")?.string()?)?;
    let body = field(root, "body")?.object()?;
    match op {
        Op::StartLogin => {
            exact_keys(body, &["ticket"])?;
            schema_login_ticket(field(body, "ticket")?)?;
        }
        Op::ObserveLogin => {
            exact_keys(body, &["binding"])?;
            schema_login_binding(field(body, "binding")?)?;
        }
        Op::SettleLogin => {
            exact_keys(body, &["receipt_id", "witness"])?;
            let witness = field(body, "witness")?.object()?;
            exact_keys(
                witness,
                &["kind", "binding", "storage_handle", "completed_at"],
            )?;
            schema_login_binding(field(witness, "binding")?)?;
        }
        Op::CheckConnection => {
            exact_keys(body, &["ticket"])?;
            schema_check_ticket(field(body, "ticket")?)?;
        }
        Op::CleanupConnection => {
            exact_keys(body, &["ticket"])?;
            schema_cleanup_ticket(field(body, "ticket")?)?;
        }
        Op::ReconcilePage => {
            exact_keys(body, &["snapshot_revision", "after_entry_id", "limit"])?;
        }
        Op::SubmitCode => {
            exact_keys(body, &["binding", "code"])?;
            schema_login_binding(field(body, "binding")?)?;
        }
    }
    Ok(op)
}

fn schema_login_snapshot(value: &JsonNode) -> Result<(), DtoError> {
    let object = value.object()?;
    match field(object, "state")?.string()? {
        "queued" | "starting" | "verifying" => {
            exact_keys(object, &["state", "expires_at", "poll_after_ms"])
        }
        "awaiting_user" => {
            exact_keys(
                object,
                &["state", "expires_at", "poll_after_ms", "challenge"],
            )?;
            // The challenge is provider-shaped: a Codex device flow carries a
            // user code, a Claude authorization-code flow does not.
            let challenge = field(object, "challenge")?.object()?;
            match field(challenge, "kind")?.string()? {
                "device" => exact_keys(
                    challenge,
                    &["kind", "verification_url", "user_code", "expires_at"],
                ),
                "authorization_code" => {
                    exact_keys(challenge, &["kind", "verification_url", "expires_at"])
                }
                _ => Err(DtoError::Invalid),
            }
        }
        "prepared" => exact_keys(
            object,
            &["state", "receipt_id", "storage_handle", "account_label"],
        ),
        "committed" => exact_keys(object, &["state", "receipt_id", "storage_handle"]),
        "failed" => exact_keys(object, &["state", "reason"]),
        "expired" | "canceled" | "interrupted" => exact_keys(object, &["state"]),
        _ => Err(DtoError::Invalid),
    }
}

fn schema_account(value: &JsonNode) -> Result<(), DtoError> {
    let object = value.object()?;
    match field(object, "state")?.string()? {
        "supported" => exact_keys(object, &["state", "account_label"]),
        "disconnected" | "unsupported" => exact_keys(object, &["state"]),
        _ => Err(DtoError::Invalid),
    }
}

fn schema_reconcile_entry(value: &JsonNode) -> Result<(), DtoError> {
    let object = value.object()?;
    exact_keys(
        object,
        &[
            "entry_id",
            "binding",
            "attempt_id",
            "operation_id",
            "phase",
            "storage_handle",
            "receipt_id",
            "cleanup_operation_id",
        ],
    )?;
    schema_connection_binding(field(object, "binding")?)
}

fn schema_success(op: Op, value: &JsonNode) -> Result<(), DtoError> {
    let object = value.object()?;
    match op {
        Op::StartLogin => exact_keys(object, &["disposition", "poll_after_ms"]),
        Op::ObserveLogin | Op::SubmitCode => schema_login_snapshot(value),
        Op::SettleLogin => exact_keys(object, &["state", "receipt_id", "storage_handle"]),
        Op::CheckConnection => match field(object, "state")?.string()? {
            "queued" | "checking" => exact_keys(object, &["state", "check_id", "poll_after_ms"]),
            "finished" => {
                exact_keys(object, &["state", "check_id", "observed_at", "account"])?;
                schema_account(field(object, "account")?)
            }
            "unavailable" => exact_keys(object, &["state", "check_id", "reason"]),
            _ => Err(DtoError::Invalid),
        },
        Op::CleanupConnection => match field(object, "state")?.string()? {
            "pending" => exact_keys(object, &["state", "phase", "poll_after_ms"]),
            "completed" => exact_keys(object, &["state", "provider_logout"]),
            "failed" => exact_keys(object, &["state", "reason", "retry_after_ms"]),
            _ => Err(DtoError::Invalid),
        },
        Op::ReconcilePage => {
            exact_keys(
                object,
                &["snapshot_revision", "entries", "next_after_entry_id"],
            )?;
            let entries = match field(object, "entries")? {
                JsonNode::Array(entries) => entries,
                _ => return Err(DtoError::Invalid),
            };
            for entry in entries {
                schema_reconcile_entry(entry)?;
            }
            Ok(())
        }
    }
}

fn schema_response(tree: &JsonNode) -> Result<(Op, Outcome), DtoError> {
    let root = tree.object()?;
    let outcome = Outcome::from_wire(field(root, "outcome")?.string()?)?;
    let keys: &[&str] = match outcome {
        Outcome::Ok => &[
            "v",
            "coordinator",
            "supervisor_boot_id",
            "sequence",
            "request_id",
            "op",
            "outcome",
            "body",
        ],
        Outcome::Error => &[
            "v",
            "coordinator",
            "supervisor_boot_id",
            "sequence",
            "request_id",
            "op",
            "outcome",
            "error",
        ],
    };
    exact_keys(root, keys)?;
    schema_coordinator(field(root, "coordinator")?)?;
    let op = Op::from_wire(field(root, "op")?.string()?)?;
    match outcome {
        Outcome::Ok => schema_success(op, field(root, "body")?)?,
        Outcome::Error => {
            exact_keys(field(root, "error")?.object()?, &["code", "retry_after_ms"])?;
        }
    }
    Ok((op, outcome))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestEnvelope<B> {
    v: u8,
    coordinator: Coordinator,
    sequence: Counter,
    request_id: Id,
    op: Op,
    body: B,
}

#[derive(Serialize)]
struct RequestEnvelopeRef<'a, B> {
    v: u8,
    coordinator: &'a Coordinator,
    sequence: &'a Counter,
    request_id: &'a Id,
    op: Op,
    body: &'a B,
}

pub(crate) fn decode_request(raw: &[u8]) -> Result<Request, DtoError> {
    let tree = parse_tree(raw, REQUEST_MAX)?;
    let op = schema_request(&tree)?;
    macro_rules! decode_body {
        ($kind:ident, $variant:ident) => {{
            let envelope: RequestEnvelope<$kind> = parse_as(&tree)?;
            if envelope.v != 1 || envelope.op != op {
                return Err(DtoError::Invalid);
            }
            Request {
                coordinator: envelope.coordinator,
                sequence: envelope.sequence,
                request_id: envelope.request_id,
                body: RequestBody::$variant(envelope.body),
            }
        }};
    }
    let request = match op {
        Op::StartLogin => decode_body!(StartLoginBody, StartLogin),
        Op::ObserveLogin => decode_body!(ObserveLoginBody, ObserveLogin),
        Op::SettleLogin => decode_body!(SettleLoginBody, SettleLogin),
        Op::CheckConnection => decode_body!(CheckConnectionBody, CheckConnection),
        Op::CleanupConnection => decode_body!(CleanupConnectionBody, CleanupConnection),
        Op::ReconcilePage => decode_body!(ReconcilePageRequest, ReconcilePage),
        Op::SubmitCode => decode_body!(SubmitCodeBody, SubmitCode),
    };
    match &request.body {
        RequestBody::CleanupConnection(body) => body.ticket.validate()?,
        RequestBody::ReconcilePage(body) => body.validate()?,
        _ => {}
    }
    Ok(request)
}

pub(crate) fn encode_request(request: &Request) -> Result<Vec<u8>, DtoError> {
    request.validate()?;
    macro_rules! encode_body {
        ($body:expr) => {
            encode_json(
                &RequestEnvelopeRef {
                    v: 1,
                    coordinator: &request.coordinator,
                    sequence: &request.sequence,
                    request_id: &request.request_id,
                    op: request.op(),
                    body: $body,
                },
                REQUEST_MAX,
            )?
        };
    }
    let encoded = match &request.body {
        RequestBody::StartLogin(body) => encode_body!(body),
        RequestBody::ObserveLogin(body) => encode_body!(body),
        RequestBody::SettleLogin(body) => encode_body!(body),
        RequestBody::CheckConnection(body) => encode_body!(body),
        RequestBody::CleanupConnection(body) => encode_body!(body),
        RequestBody::ReconcilePage(body) => encode_body!(body),
        RequestBody::SubmitCode(body) => encode_body!(body),
    };
    decode_request(&encoded)?;
    Ok(encoded)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuccessEnvelope<B> {
    v: u8,
    coordinator: Coordinator,
    supervisor_boot_id: Id,
    sequence: Counter,
    request_id: Id,
    op: Op,
    outcome: Outcome,
    body: B,
}

#[derive(Serialize)]
struct SuccessEnvelopeRef<'a, B> {
    v: u8,
    coordinator: &'a Coordinator,
    supervisor_boot_id: &'a Id,
    sequence: &'a Counter,
    request_id: &'a Id,
    op: Op,
    outcome: Outcome,
    body: &'a B,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorEnvelope {
    v: u8,
    coordinator: Coordinator,
    supervisor_boot_id: Id,
    sequence: Counter,
    request_id: Id,
    op: Op,
    outcome: Outcome,
    error: AppError,
}

#[derive(Serialize)]
struct ErrorEnvelopeRef<'a> {
    v: u8,
    coordinator: &'a Coordinator,
    supervisor_boot_id: &'a Id,
    sequence: &'a Counter,
    request_id: &'a Id,
    op: Op,
    outcome: Outcome,
    error: &'a AppError,
}

pub(crate) fn decode_response(raw: &[u8]) -> Result<Response, DtoError> {
    let tree = parse_tree(raw, RESPONSE_MAX)?;
    let (op, outcome) = schema_response(&tree)?;
    if outcome == Outcome::Error {
        let envelope: ErrorEnvelope = parse_as(&tree)?;
        if envelope.v != 1 || envelope.op != op || envelope.outcome != outcome {
            return Err(DtoError::Invalid);
        }
        envelope.error.validate()?;
        return Ok(Response {
            coordinator: envelope.coordinator,
            supervisor_boot_id: envelope.supervisor_boot_id,
            sequence: envelope.sequence,
            request_id: envelope.request_id,
            operation: op,
            outcome,
            body: None,
            error: Some(envelope.error),
        });
    }
    macro_rules! decode_result {
        ($kind:ty, $variant:ident) => {{
            let envelope: SuccessEnvelope<$kind> = parse_as(&tree)?;
            if envelope.v != 1 || envelope.op != op || envelope.outcome != outcome {
                return Err(DtoError::Invalid);
            }
            Response {
                coordinator: envelope.coordinator,
                supervisor_boot_id: envelope.supervisor_boot_id,
                sequence: envelope.sequence,
                request_id: envelope.request_id,
                operation: op,
                outcome,
                body: Some(ResultBody::$variant(envelope.body)),
                error: None,
            }
        }};
    }
    let response = match op {
        Op::StartLogin => decode_result!(StartLoginResult, StartLogin),
        Op::ObserveLogin => decode_result!(LoginSnapshot, ObserveLogin),
        Op::SettleLogin => decode_result!(SettleLoginResult, SettleLogin),
        Op::CheckConnection => decode_result!(CheckResult, CheckConnection),
        Op::CleanupConnection => decode_result!(CleanupResult, CleanupConnection),
        Op::ReconcilePage => decode_result!(ReconcilePage, ReconcilePage),
        Op::SubmitCode => decode_result!(LoginSnapshot, SubmitCode),
    };
    match response.body.as_ref() {
        Some(ResultBody::ObserveLogin(snapshot)) | Some(ResultBody::SubmitCode(snapshot)) => {
            snapshot.validate()?
        }
        Some(ResultBody::ReconcilePage(page)) => page.validate()?,
        _ => {}
    }
    Ok(response)
}

pub(crate) fn encode_response(response: &Response) -> Result<Vec<u8>, DtoError> {
    response.validate()?;
    macro_rules! encode_result {
        ($body:expr) => {
            encode_json(
                &SuccessEnvelopeRef {
                    v: 1,
                    coordinator: &response.coordinator,
                    supervisor_boot_id: &response.supervisor_boot_id,
                    sequence: &response.sequence,
                    request_id: &response.request_id,
                    op: response.operation,
                    outcome: Outcome::Ok,
                    body: $body,
                },
                RESPONSE_MAX,
            )?
        };
    }
    let encoded = match (&response.body, &response.error, response.outcome) {
        (Some(ResultBody::StartLogin(body)), None, Outcome::Ok) => encode_result!(body),
        (Some(ResultBody::ObserveLogin(body)), None, Outcome::Ok) => encode_result!(body),
        (Some(ResultBody::SettleLogin(body)), None, Outcome::Ok) => encode_result!(body),
        (Some(ResultBody::CheckConnection(body)), None, Outcome::Ok) => encode_result!(body),
        (Some(ResultBody::CleanupConnection(body)), None, Outcome::Ok) => encode_result!(body),
        (Some(ResultBody::ReconcilePage(body)), None, Outcome::Ok) => encode_result!(body),
        (None, Some(error), Outcome::Error) => encode_json(
            &ErrorEnvelopeRef {
                v: 1,
                coordinator: &response.coordinator,
                supervisor_boot_id: &response.supervisor_boot_id,
                sequence: &response.sequence,
                request_id: &response.request_id,
                op: response.operation,
                outcome: Outcome::Error,
                error,
            },
            RESPONSE_MAX,
        )?,
        _ => return Err(DtoError::Invalid),
    };
    decode_response(&encoded)?;
    Ok(encoded)
}

pub(crate) fn validate_response(
    request: &Request,
    response: &Response,
    supervisor_boot_id: &Id,
) -> Result<(), DtoError> {
    request.validate()?;
    response.validate()?;
    if request.coordinator != response.coordinator
        || request.sequence != response.sequence
        || request.request_id != response.request_id
        || request.op() != response.op()
        || &response.supervisor_boot_id != supervisor_boot_id
    {
        return Err(DtoError::Correlation);
    }
    if response.outcome == Outcome::Error {
        return Ok(());
    }
    match (&request.body, response.body.as_ref()) {
        (RequestBody::SettleLogin(request), Some(ResultBody::SettleLogin(response))) => {
            if request.receipt_id != response.receipt_id
                || request.witness.storage_handle != response.storage_handle
            {
                return Err(DtoError::Correlation);
            }
        }
        (RequestBody::CheckConnection(request), Some(ResultBody::CheckConnection(response))) => {
            if &request.ticket.check_id != response.check_id() {
                return Err(DtoError::Correlation);
            }
        }
        (RequestBody::ReconcilePage(request), Some(ResultBody::ReconcilePage(response))) => {
            validate_reconcile(request, response)?;
        }
        (RequestBody::StartLogin(_), Some(ResultBody::StartLogin(_)))
        | (RequestBody::ObserveLogin(_), Some(ResultBody::ObserveLogin(_)))
        | (RequestBody::CleanupConnection(_), Some(ResultBody::CleanupConnection(_))) => {}
        _ => return Err(DtoError::Correlation),
    }
    Ok(())
}

fn validate_reconcile(
    request: &ReconcilePageRequest,
    page: &ReconcilePage,
) -> Result<(), DtoError> {
    if page.entries.len() > request.limit as usize {
        return Err(DtoError::Correlation);
    }
    if let Some(revision) = &request.snapshot_revision {
        if revision != &page.snapshot_revision {
            return Err(DtoError::Correlation);
        }
    }
    let mut previous = request.after_entry_id.as_ref().map(Id::as_str);
    for entry in &page.entries {
        let current = entry.entry_id.as_str();
        if previous.is_some_and(|value| current <= value) {
            return Err(DtoError::Correlation);
        }
        previous = Some(current);
    }
    if let Some(cursor) = &page.next_after_entry_id {
        if page.entries.last().map(|entry| &entry.entry_id) != Some(cursor) {
            return Err(DtoError::Correlation);
        }
    }
    Ok(())
}

pub(crate) fn validate_login_snapshot(
    _binding: &LoginBinding,
    snapshot: &LoginSnapshot,
    expected_expires_at: &UnixSec,
    now: &UnixSec,
) -> Result<(), DtoError> {
    let progress_expires = match snapshot {
        LoginSnapshot::Queued { expires_at, .. }
        | LoginSnapshot::Starting { expires_at, .. }
        | LoginSnapshot::AwaitingUser { expires_at, .. }
        | LoginSnapshot::Verifying { expires_at, .. } => Some(expires_at),
        _ => None,
    };
    if let Some(expires_at) = progress_expires {
        if expires_at != expected_expires_at {
            return Err(DtoError::Correlation);
        }
        if now.get() >= expires_at.get() {
            return Err(DtoError::Expired);
        }
    }
    if let LoginSnapshot::AwaitingUser {
        expires_at,
        challenge,
        ..
    } = snapshot
    {
        if challenge.expires_at().get() <= now.get() || challenge.expires_at().get() > expires_at.get()
        {
            return Err(DtoError::Expired);
        }
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
enum ChannelState {
    Idle(i64),
    Waiting(Request),
    Closed,
}

#[derive(Clone, PartialEq, Eq)]
struct Channel {
    coordinator: Coordinator,
    supervisor_boot_id: Id,
    state: ChannelState,
}

impl Channel {
    fn new(coordinator: Coordinator, supervisor_boot_id: Id) -> Self {
        Self {
            coordinator,
            supervisor_boot_id,
            state: ChannelState::Idle(1),
        }
    }

    fn poison(&mut self) {
        self.state = ChannelState::Closed;
    }

    fn fail<T>(&mut self, error: DtoError) -> Result<T, DtoError> {
        self.poison();
        Err(error)
    }

    fn begin(&mut self, request: &Request) -> Result<(), DtoError> {
        let next = match self.state {
            ChannelState::Idle(next) => next,
            _ => return self.fail(DtoError::State),
        };
        if let Err(error) = request.validate() {
            return self.fail(error);
        }
        if request.coordinator != self.coordinator {
            return self.fail(DtoError::Correlation);
        }
        if request.sequence.get() != next {
            return self.fail(DtoError::Sequence);
        }
        self.state = ChannelState::Waiting(request.clone());
        Ok(())
    }

    fn finish(&mut self, response: &Response) -> Result<(), DtoError> {
        let request = match &self.state {
            ChannelState::Waiting(request) => request.clone(),
            _ => return self.fail(DtoError::State),
        };
        if let Err(error) = validate_response(&request, response, &self.supervisor_boot_id) {
            return self.fail(error);
        }
        let completed = request.sequence.get();
        self.state = if completed == I64_MAX {
            ChannelState::Closed
        } else {
            ChannelState::Idle(completed + 1)
        };
        Ok(())
    }
}

pub(crate) struct ClientChannel(Channel);

impl ClientChannel {
    pub(crate) fn new(coordinator: Coordinator, supervisor_boot_id: Id) -> Self {
        Self(Channel::new(coordinator, supervisor_boot_id))
    }

    #[cfg(test)]
    pub(crate) fn new_at_sequence_for_test(
        coordinator: Coordinator,
        supervisor_boot_id: Id,
        sequence: i64,
    ) -> Self {
        let mut channel = Channel::new(coordinator, supervisor_boot_id);
        channel.state = ChannelState::Idle(sequence);
        Self(channel)
    }

    pub(crate) fn begin(&mut self, request: &Request) -> Result<(), DtoError> {
        self.0.begin(request)
    }

    pub(crate) fn finish(&mut self, response: &Response) -> Result<(), DtoError> {
        self.0.finish(response)
    }

    pub(crate) fn poison(&mut self) {
        self.0.poison();
    }
}

pub(crate) struct ServerChannel(Channel);

impl ServerChannel {
    pub(crate) fn new(coordinator: Coordinator, supervisor_boot_id: Id) -> Self {
        Self(Channel::new(coordinator, supervisor_boot_id))
    }

    pub(crate) fn accept(&mut self, request: &Request) -> Result<(), DtoError> {
        self.0.begin(request)
    }

    pub(crate) fn finish(&mut self, response: &Response) -> Result<(), DtoError> {
        self.0.finish(response)
    }

    pub(crate) fn poison(&mut self) {
        self.0.poison();
    }
}
