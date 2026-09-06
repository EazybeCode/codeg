use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub org_id: i32,
    pub user_id: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceNodeKind {
    Project,
    Folder,
    WorkingDirectory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryDefault {
    pub repository_id: i32,
    pub starting_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceNode {
    pub id: i32,
    pub org_id: i32,
    pub parent_id: Option<i32>,
    pub kind: WorkspaceNodeKind,
    pub name: String,
    pub repository_default: Option<RepositoryDefault>,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRepository {
    pub id: i32,
    pub org_id: i32,
    pub github_repository_id: String,
    pub full_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateDraftInput {
    pub node_id: i32,
    pub repository_override: Option<RepositoryDefault>,
    pub title: Option<String>,
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceChatDraft {
    pub id: i32,
    pub org_id: i32,
    pub node_id: i32,
    pub creator_user_id: i32,
    pub repository_id: i32,
    pub github_repository_id: String,
    pub full_name: String,
    pub starting_branch: String,
    pub title: Option<String>,
    pub execution_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateNodeInput {
    pub parent_id: Option<i32>,
    pub kind: WorkspaceNodeKind,
    pub name: String,
    pub repository_default: Option<RepositoryDefault>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateNodeInput {
    pub node_id: i32,
    pub expected_revision: i64,
    pub parent_id: Option<i32>,
    pub name: String,
    pub repository_default: Option<RepositoryDefault>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("not found")]
    NotFound,
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("conflict")]
    Conflict,
    #[error("repository required")]
    RepositoryRequired,
    #[error("repository unavailable")]
    RepositoryUnavailable,
    #[error("reauthentication required")]
    ReauthRequired,
    #[error("upstream unavailable")]
    UpstreamUnavailable,
    #[error("database error")]
    Database,
}

impl From<sea_orm::DbErr> for CatalogError {
    fn from(error: sea_orm::DbErr) -> Self {
        tracing::error!(error = %error, "workspace catalog database operation failed");
        Self::Database
    }
}
