use async_trait::async_trait;
use crate::workspace_catalog::Scope;
use super::ExecutionError;
#[derive(Clone, Debug)]
pub struct VerifiedExecutionIdentity { pub github_repository_id: String, pub full_name: String }
#[derive(Clone, Debug)]
pub struct VerifiedExecutionRepository { pub github_repository_id: String, pub full_name: String, pub starting_commit: String }
#[async_trait]
pub trait ExecutionRepositoryAccess: Send + Sync {
    async fn verify_access(&self, scope: &Scope, repository_id: i32) -> Result<VerifiedExecutionIdentity, ExecutionError>;
    async fn resolve(&self, scope: &Scope, repository_id: i32, starting_branch: &str) -> Result<VerifiedExecutionRepository, ExecutionError>;
}

pub struct GitHubExecutionRepositoryAccess {
    db: sea_orm::DatabaseConnection,
    github: crate::workspace_catalog::GitHubRepositoryAccess,
}
impl GitHubExecutionRepositoryAccess {
    pub fn new(db: &sea_orm::DatabaseConnection) -> Result<Self, ExecutionError> {
        Ok(Self { db: db.clone(), github: crate::workspace_catalog::GitHubRepositoryAccess::new(db)? })
    }
    async fn repository(&self, scope: &Scope, id: i32) -> Result<crate::db::entities::workspace_repository::Model, ExecutionError> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use crate::db::entities::workspace_repository;
        crate::workspace_catalog::verify_scope(&self.db, scope).await?;
        workspace_repository::Entity::find_by_id(id)
            .filter(workspace_repository::Column::OrgId.eq(scope.org_id))
            .one(&self.db).await?.ok_or(ExecutionError::NotFound)
    }
}
#[async_trait]
impl ExecutionRepositoryAccess for GitHubExecutionRepositoryAccess {
    async fn verify_access(&self, scope: &Scope, repository_id: i32) -> Result<VerifiedExecutionIdentity, ExecutionError> {
        let repository = self.repository(scope, repository_id).await?;
        let (id, full_name) = self.github.verified_identity(scope, &repository.full_name).await?;
        if id != repository.github_repository_id { return Err(ExecutionError::RepositoryUnavailable); }
        Ok(VerifiedExecutionIdentity { github_repository_id: id, full_name })
    }
    async fn resolve(&self, scope: &Scope, repository_id: i32, starting_branch: &str) -> Result<VerifiedExecutionRepository, ExecutionError> {
        let repository = self.repository(scope, repository_id).await?;
        let (id, full_name, starting_commit) = self.github.resolve_commit(scope, &repository.full_name, starting_branch).await?;
        if id != repository.github_repository_id { return Err(ExecutionError::RepositoryUnavailable); }
        Ok(VerifiedExecutionRepository { github_repository_id: id, full_name, starting_commit })
    }
}
