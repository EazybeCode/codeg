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
