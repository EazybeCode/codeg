use serde::{Deserialize, Serialize};

/// One contestant entry in the round config. Supports both a plain string
/// (backward compat with old rounds: `"claude_code"`) and a labeled object
/// (new format: `{"agent":"claude_code","label":"Sonnet"}`). The label
/// disambiguates same-agent slots in control-variable PK
/// (e.g. "Claude · Sonnet" vs "Claude · Opus").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PkContestantEntry {
    Simple(String),
    Labeled {
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

impl PkContestantEntry {
    pub fn agent(&self) -> &str {
        match self {
            PkContestantEntry::Simple(a) => a,
            PkContestantEntry::Labeled { agent, .. } => agent,
        }
    }
    pub fn label(&self) -> Option<&str> {
        match self {
            PkContestantEntry::Simple(_) => None,
            PkContestantEntry::Labeled { label, .. } => label.as_deref(),
        }
    }
}

/// Config stored as JSON in `pk_round.config`. Mirrors the launcher's options
/// so a round is fully reproducible from the DB row alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkRoundConfig {
    /// The agent types selected as contestants, in pick order. Each entry
    /// is either a plain string (old format) or a labeled object (new format).
    pub agents: Vec<PkContestantEntry>,
    /// Round-level permission policy applied to every contestant.
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    /// Bare mode: contestants are instructed to use no skills at all.
    #[serde(default)]
    pub bare_mode: bool,
    /// Uniform reasoning-effort request applied to every contestant.
    #[serde(default = "default_effort")]
    pub effort: String,
    /// Optional judge agent — after all contestants finish, this agent reads
    /// every diff and produces a structured verdict. Stored in config (not a
    /// separate column) because it is round-level input, set at creation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_agent: Option<String>,
}

fn default_permission_mode() -> String {
    "default".into()
}

fn default_effort() -> String {
    "default".into()
}

/// A PK round summary as returned to the frontend. Carries the round's own
/// fields plus the live contestant status (computed from the linked
/// conversations, not stored on the round row itself).
#[derive(Debug, Clone, Serialize)]
pub struct PkRoundInfo {
    pub id: i32,
    pub folder_id: i32,
    pub task: String,
    pub config: PkRoundConfig,
    pub status: String,
    pub failure_reason: Option<String>,
    /// JSON-serialized judge verdict, or null if no judge / not yet run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judge_result: Option<serde_json::Value>,
    /// idle | running | done | error | skipped
    pub judge_status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}
