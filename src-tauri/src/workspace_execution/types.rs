use serde::{Deserialize, Serialize};
use crate::models::{FolderDetail, DbConversationSummary};
use crate::workspace_catalog::WorkspaceChatDraft;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceExecutionState {
    Unprovisioned,
    Provisioning,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceChatRuntime {
    pub draft: WorkspaceChatDraft,
    pub state: WorkspaceExecutionState,
    pub folder: Option<FolderDetail>,
    pub conversation: Option<DbConversationSummary>,
    pub branch_name: Option<String>,
    pub starting_commit: Option<String>,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecutionError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("not_found")]
    NotFound,
    #[error("conflict")]
    Conflict,
    #[error("disabled")]
    Disabled,
    #[error("repository_unavailable")]
    RepositoryUnavailable,
    #[error("reauth_required")]
    ReauthRequired,
    #[error("upstream_unavailable")]
    UpstreamUnavailable,
    #[error("allocation_mismatch")]
    AllocationMismatch,
    #[error("busy")]
    Busy,
    #[error("failed")]
    Failed,
    #[error("database_error")]
    Database,
}

impl From<crate::workspace_catalog::CatalogError> for ExecutionError {
    fn from(error: crate::workspace_catalog::CatalogError) -> Self {
        use crate::workspace_catalog::CatalogError;
        match error {
            CatalogError::Unauthorized => Self::Unauthorized,
            CatalogError::NotFound => Self::NotFound,
            CatalogError::Conflict => Self::Conflict,
            CatalogError::RepositoryUnavailable | CatalogError::RepositoryRequired => Self::RepositoryUnavailable,
            CatalogError::ReauthRequired => Self::ReauthRequired,
            CatalogError::UpstreamUnavailable => Self::UpstreamUnavailable,
            CatalogError::Database => Self::Database,
            CatalogError::InvalidInput(_) => Self::Failed,
        }
    }
}

impl From<sea_orm::DbErr> for ExecutionError {
    fn from(error: sea_orm::DbErr) -> Self {
        tracing::error!(error = %error, "workspace execution database operation failed");
        Self::Database
    }
}

impl From<crate::db::error::DbError> for ExecutionError {
    fn from(error: crate::db::error::DbError) -> Self {
        tracing::error!(error = %error, "workspace execution legacy bridge failed");
        Self::Database
    }
}
