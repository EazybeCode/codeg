use serde::{Deserialize, Serialize};

/// Config stored as JSON in `pk_round.config`. Mirrors the launcher's options
/// so a round is fully reproducible from the DB row alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkRoundConfig {
    /// The agent types selected as contestants, in pick order.
    pub agents: Vec<String>,
    /// Round-level permission policy applied to every contestant.
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    /// Bare mode: contestants are instructed to use no skills at all.
    #[serde(default)]
    pub bare_mode: bool,
    /// Uniform reasoning-effort request applied to every contestant.
    #[serde(default = "default_effort")]
    pub effort: String,
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
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}
