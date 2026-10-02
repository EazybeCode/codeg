//! Shared knowledge types and fixed caps (plan Shared contracts, as amended by
//! Amendment 1 §A1.1).

use serde::Serialize;

pub(crate) const CAPTURE_TICK_SECS: u64 = 120;
pub(crate) const JOB_TICK_SECS: u64 = 30;
pub(crate) const MAX_EXTRACTIONS_PER_TICK: usize = 25;
pub(crate) const DEBOUNCE_TICKS: i32 = 2;
pub(crate) const BACKOFF_MIN_SECS: i64 = 120;
pub(crate) const BACKOFF_MAX_SECS: i64 = 21_600;
/// Every unit, header included.
pub(crate) const MAX_UNIT_BYTES: usize = 65_536;
pub(crate) const MAX_UNIT_HEADER_BYTES: usize = 1_024;
pub(crate) const MAX_BATCH_UNITS: usize = 12;
pub(crate) const MAX_BATCH_UNIT_BYTES: usize = 262_144;
pub(crate) const MAX_STAGED_PAGE_BYTES: usize = 131_072;
pub(crate) const MAX_INDEX_BYTES: usize = 16_384;
pub(crate) const MAX_PAGE_BYTES: usize = 65_536;
pub(crate) const MAX_PAGES_PER_BATCH: usize = 40;
pub(crate) const MAX_PAGE_LINES_LINT: usize = 150;
pub(crate) const UNCOVERED_AFTER_ATTEMPTS: i32 = 2;
pub(crate) const BATCH_TIMEOUT_SECS: u64 = 1_200;
pub(crate) const ORG_BYTE_BUDGET: i64 = 2 * 1024 * 1024 * 1024;
pub(crate) const ORPHAN_AGE_SECS: i64 = 86_400;
pub(crate) const MAX_EXPORT_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MIN_FREE_LANES_FOR_KNOWLEDGE: usize = 2;
/// Engine (La Suite) markdown is accepted up to this size; above it the bridge
/// answers 413 and the source is `too_large`.
pub(crate) const MAX_ENGINE_MARKDOWN_BYTES: usize = 1_048_576;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceKind {
    Document,
    Chat,
}
impl SourceKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Chat => "chat",
        }
    }
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "document" => Some(Self::Document),
            "chat" => Some(Self::Chat),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CaptureState {
    /// Stored as the SQL literal `'ok'` by `capture::write`.
    #[allow(dead_code)]
    Ok,
    TooLarge,
    EngineUnavailable,
    Failed,
}
impl CaptureState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::TooLarge => "too_large",
            Self::EngineUnavailable => "engine_unavailable",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JobMode {
    Incremental,
    Rebuild,
}
impl JobMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Incremental => "incremental",
            Self::Rebuild => "rebuild",
        }
    }
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "incremental" => Some(Self::Incremental),
            "rebuild" => Some(Self::Rebuild),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JobState {
    Running,
    Paused,
    Completed,
    CompletedWithGaps,
    Stopped,
}
impl JobState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::CompletedWithGaps => "completed_with_gaps",
            Self::Stopped => "stopped",
        }
    }
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "paused" => Some(Self::Paused),
            "completed" => Some(Self::Completed),
            "completed_with_gaps" => Some(Self::CompletedWithGaps),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PauseReason {
    Manual,
    Interrupted,
    CredentialMissing,
    SourceRevoked,
    InvalidOutput,
    RunFailed,
    /// Kept in the contract (the frontend translates it); lanes now wait
    /// (`wait_reason='slot'`) instead of pausing.
    #[allow(dead_code)]
    Busy,
    NotAdmin,
    Timeout,
    PageMoved,
    Stopped,
}
impl PauseReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Interrupted => "interrupted",
            Self::CredentialMissing => "credential_missing",
            Self::SourceRevoked => "source_revoked",
            Self::InvalidOutput => "invalid_output",
            Self::RunFailed => "run_failed",
            Self::Busy => "busy",
            Self::NotAdmin => "not_admin",
            Self::Timeout => "timeout",
            Self::PageMoved => "page_moved",
            Self::Stopped => "stopped",
        }
    }
    #[allow(dead_code)]
    pub(crate) fn parse(value: &str) -> Option<Self> {
        [
            Self::Manual,
            Self::Interrupted,
            Self::CredentialMissing,
            Self::SourceRevoked,
            Self::InvalidOutput,
            Self::RunFailed,
            Self::Busy,
            Self::NotAdmin,
            Self::Timeout,
            Self::PageMoved,
            Self::Stopped,
        ]
        .into_iter()
        .find(|reason| reason.as_str() == value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Coverage {
    Pending,
    Cited,
    SkippedByAgent,
    Uncovered,
}
impl Coverage {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Cited => "cited",
            Self::SkippedByAgent => "skipped_by_agent",
            Self::Uncovered => "uncovered",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Settings {
    pub capture_enabled: bool,
    pub revision: i64,
    #[serde(skip)]
    pub capture_blocked: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub id: String,
    pub source_id: String,
    pub source_kind: SourceKind,
    pub engine: bool,
    pub segment: i32,
    pub from_sequence: Option<i64>,
    pub to_sequence: Option<i64>,
    pub marker: String,
    pub blob_key: String,
    pub sha256: String,
    pub byte_length: i64,
    pub captured_at: i64,
    pub cite_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Readable {
    Readable,
    Withheld { unreadable_sources: i64 },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum KnowledgeError {
    #[error(transparent)]
    Content(#[from] crate::neev::content::types::ContentError),
    #[error(transparent)]
    Auth(#[from] crate::neev::error::AuthError),
    #[error("knowledge storage is not configured")]
    Unavailable,
    #[error("a knowledge job is already active")]
    JobActive,
    #[error("no connected credential for this provider")]
    CredentialMissing,
    #[error("this admin already has a running ask")]
    AskBusy,
    #[error("too many asks are running")]
    AskLimited,
}

impl KnowledgeError {
    pub(crate) fn storage() -> Self {
        Self::Content(crate::neev::content::types::ContentError::Storage)
    }
    pub(crate) fn not_found() -> Self {
        Self::Content(crate::neev::content::types::ContentError::NotFound)
    }
    pub(crate) fn invalid() -> Self {
        Self::Content(crate::neev::content::types::ContentError::Invalid)
    }
    pub(crate) fn conflict() -> Self {
        Self::Content(crate::neev::content::types::ContentError::Conflict)
    }
    pub(crate) fn forbidden() -> Self {
        Self::Auth(crate::neev::error::AuthError::Forbidden)
    }
}

/// Map a database error into the knowledge taxonomy (unique violations become
/// `Conflict`, everything else storage).
pub(crate) fn db_error(error: sea_orm::DbErr) -> KnowledgeError {
    KnowledgeError::Auth(crate::neev::error::database_error(error))
}
