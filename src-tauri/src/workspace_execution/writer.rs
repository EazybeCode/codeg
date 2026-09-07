use async_trait::async_trait;
use std::path::{Path, PathBuf};
use sea_orm::{DatabaseConnection, EntityTrait, ActiveModelTrait, ColumnTrait, QueryFilter, Set};
use chrono::{Utc, Duration};
use crate::{models::AgentType, workspace_catalog::{Scope, begin_scoped_write}, db::entities::{workspace_writer_lease as lease, workspace_chat_execution}};
use super::{ExecutionError, ExecutionRepositoryAccess};

#[derive(Debug)]
pub struct ReadyExecutionContext {
    pub draft_id: i32,
    pub folder_id: i32,
    pub conversation_id: i32,
    pub agent_type: AgentType,
    pub working_dir: PathBuf,
}
#[async_trait]
pub trait WorkspaceRunner: Send + Sync {
    async fn spawn(&self, context: &ReadyExecutionContext) -> Result<String, ExecutionError>;
    /// Ok means the owned process has been positively confirmed stopped.
    async fn disconnect(&self, connection_id: &str) -> Result<(), ExecutionError>;
    /// Read-only proof from the owning runtime. Unknown is always false.
    async fn stopped(&self, _connection_id: &str) -> bool { false }
}
/// Compatibility entry for trusted callers with a server-persisted mapping.
/// HTTP supplies AppState.data_dir explicitly through the internal helper.
pub async fn connect_workspace(db: &DatabaseConnection, scope: &Scope, draft_id: i32, runner: &dyn WorkspaceRunner) -> Result<String, ExecutionError> {
    let context = ready_context(db, scope, draft_id).await?;
    let root = context.working_dir.ancestors().nth(5).ok_or(ExecutionError::AllocationMismatch)?;
    let access = super::GitHubExecutionRepositoryAccess::new(db)?;
    connect_workspace_with_access(db, scope, draft_id, runner, &access, root).await
}
pub async fn connect_workspace_with_access(
    db: &DatabaseConnection, scope: &Scope, draft_id: i32,
    runner: &dyn WorkspaceRunner, access: &dyn ExecutionRepositoryAccess, root: &Path,
) -> Result<String, ExecutionError> {
    let context = ready_context(db, scope, draft_id).await?;
    let identity = super::verify_draft_access(db, scope, draft_id, access).await?;
    let row = workspace_chat_execution::Entity::find_by_id(draft_id).one(db).await?.ok_or(ExecutionError::NotFound)?;
    let path = super::git_allocator::validate_ready_checkout(root, &row, &identity)?;
    if context.working_dir != path { return Err(ExecutionError::AllocationMismatch); }
    if let Some(previous) = lease::Entity::find_by_id(draft_id).one(db).await? {
        if previous.org_id != scope.org_id || previous.creator_user_id != scope.user_id { return Err(ExecutionError::NotFound); }
        let connection_id = previous.connection_id.as_deref().ok_or(ExecutionError::Busy)?;
        if !runner.stopped(connection_id).await { return Err(ExecutionError::Busy); }
        // Explicit Send may reclaim a positively reaped owned process. A
        // foreign/restarted runtime has no such proof and remains Busy.
        release(db, scope, draft_id, &previous.owner_token, Some(connection_id)).await?;
    }
    let owner = uuid::Uuid::new_v4().to_string();
    let txn = begin_scoped_write(db, scope).await?;
    let current = super::state::runtime_in(&txn, scope, draft_id).await?;
    if current.folder.as_ref().map(|f| f.path.as_str()) != context.working_dir.to_str()
        || current.conversation.as_ref().map(|c| c.id) != Some(context.conversation_id)
    { return Err(ExecutionError::AllocationMismatch); }
    // Expiry revokes permission. It never proves a previous process stopped.
    if lease::Entity::find_by_id(draft_id).one(&txn).await?.is_some() { return Err(ExecutionError::Busy); }
    let now = Utc::now();
    lease::ActiveModel { draft_id: Set(draft_id), org_id: Set(scope.org_id), creator_user_id: Set(scope.user_id), owner_token: Set(owner.clone()), connection_id: Set(None), expires_at: Set(now + Duration::minutes(5)), updated_at: Set(now) }.insert(&txn).await?;
    txn.commit().await?;
    let id = match runner.spawn(&context).await {
        Ok(id) => id,
        Err(error) => {
            release(db, scope, draft_id, &owner, None).await?;
            return Err(error);
        }
    };
    let result = lease::Entity::update_many()
        .col_expr(lease::Column::ConnectionId, sea_orm::sea_query::Expr::value(Some(id.clone())))
        .filter(lease::Column::DraftId.eq(draft_id)).filter(lease::Column::OwnerToken.eq(&owner))
        .filter(lease::Column::ConnectionId.is_null()).exec(db).await;
    if !matches!(result, Ok(ref result) if result.rows_affected == 1) {
        // A DB failure after spawn must retain ownership unless shutdown is proven.
        if runner.disconnect(&id).await.is_ok() { release(db, scope, draft_id, &owner, None).await?; }
        return Err(ExecutionError::Database);
    }
    Ok(id)
}

async fn ready_context(db: &DatabaseConnection, scope: &Scope, draft: i32) -> Result<ReadyExecutionContext, ExecutionError> {
    let runtime = super::get_runtime(db, scope, draft).await?;
    let folder = runtime.folder.ok_or(ExecutionError::Conflict)?;
    let conversation = runtime.conversation.ok_or(ExecutionError::Conflict)?;
    Ok(ReadyExecutionContext { draft_id: draft, folder_id: folder.id, conversation_id: conversation.id,
        agent_type: conversation.agent_type, working_dir: PathBuf::from(folder.path) })
}

async fn release(db: &DatabaseConnection, scope: &Scope, draft: i32, owner: &str, connection: Option<&str>) -> Result<(), ExecutionError> {
    let txn = begin_scoped_write(db, scope).await?;
    let query = lease::Entity::delete_many().filter(lease::Column::DraftId.eq(draft))
        .filter(lease::Column::OrgId.eq(scope.org_id)).filter(lease::Column::CreatorUserId.eq(scope.user_id))
        .filter(lease::Column::OwnerToken.eq(owner));
    let query = match connection { Some(id) => query.filter(lease::Column::ConnectionId.eq(id)), None => query.filter(lease::Column::ConnectionId.is_null()) };
    if query.exec(&txn).await?.rows_affected != 1 { return Err(ExecutionError::Busy); }
    txn.commit().await?;
    Ok(())
}
pub async fn connection_lease(db: &DatabaseConnection, scope: &Scope, connection_id: &str, live: bool) -> Result<lease::Model, ExecutionError> {
    crate::workspace_catalog::verify_scope(db, scope).await?;
    let row = lease::Entity::find().filter(lease::Column::ConnectionId.eq(connection_id))
        .filter(lease::Column::OrgId.eq(scope.org_id)).filter(lease::Column::CreatorUserId.eq(scope.user_id))
        .one(db).await?.ok_or(ExecutionError::NotFound)?;
    super::state::scoped_draft(db, scope, row.draft_id).await?;
    if live && row.expires_at <= Utc::now() { return Err(ExecutionError::Busy); }
    Ok(row)
}
pub async fn disconnect_workspace(db: &DatabaseConnection, scope: &Scope, connection_id: &str, runner: &dyn WorkspaceRunner) -> Result<(), ExecutionError> {
    let row = connection_lease(db, scope, connection_id, false).await?;
    runner.disconnect(connection_id).await?;
    release(db, scope, row.draft_id, &row.owner_token, Some(connection_id)).await
}
pub async fn authorize_prompt(db: &DatabaseConnection, scope: &Scope, draft_id: i32, connection_id: &str, access: &dyn ExecutionRepositoryAccess) -> Result<ReadyExecutionContext, ExecutionError> {
    let context = ready_context(db, scope, draft_id).await?;
    super::verify_draft_access(db, scope, draft_id, access).await?;
    let row = connection_lease(db, scope, connection_id, true).await?;
    if row.draft_id != draft_id { return Err(ExecutionError::Conflict); }
    Ok(context)
}
/// Fresh authorization is not yet write permission. The HTTP handler must
/// validate this exact current-runtime connection and readiness before renewal.
pub(crate) async fn prepare_owned_prompt(db: &DatabaseConnection, scope: &Scope, draft_id: i32, connection_id: &str, access: &dyn ExecutionRepositoryAccess) -> Result<(ReadyExecutionContext, lease::Model), ExecutionError> {
    let context = ready_context(db, scope, draft_id).await?;
    super::verify_draft_access(db, scope, draft_id, access).await?;
    let row = connection_lease(db, scope, connection_id, false).await?;
    if row.draft_id != draft_id { return Err(ExecutionError::Conflict); }
    Ok((context, row))
}
/// Explicit Send only, after prepare_owned_prompt and live manager validation.
/// Unlike passive touch this may renew an expired lease, never replace its owner.
pub(crate) async fn renew_owned_prompt(db: &DatabaseConnection, scope: &Scope, row: &lease::Model) -> Result<(), ExecutionError> {
    let id = row.connection_id.as_deref().ok_or(ExecutionError::Busy)?;
    let txn = begin_scoped_write(db, scope).await?;
    super::state::scoped_draft(&txn, scope, row.draft_id).await?;
    let now = Utc::now();
    let result = lease::Entity::update_many()
        .col_expr(lease::Column::ExpiresAt, sea_orm::sea_query::Expr::value(now + Duration::minutes(5)))
        .col_expr(lease::Column::UpdatedAt, sea_orm::sea_query::Expr::value(now))
        .filter(lease::Column::DraftId.eq(row.draft_id)).filter(lease::Column::OrgId.eq(scope.org_id))
        .filter(lease::Column::CreatorUserId.eq(scope.user_id)).filter(lease::Column::OwnerToken.eq(&row.owner_token))
        .filter(lease::Column::ConnectionId.eq(id)).exec(&txn).await?;
    if result.rows_affected != 1 { return Err(ExecutionError::Busy); }
    txn.commit().await?;
    Ok(())
}
pub async fn touch_workspace(db: &DatabaseConnection, scope: &Scope, connection_id: &str) -> Result<(), ExecutionError> {
    let row = connection_lease(db, scope, connection_id, true).await?;
    let now = Utc::now();
    let result = lease::Entity::update_many()
        .col_expr(lease::Column::ExpiresAt, sea_orm::sea_query::Expr::value(now + Duration::minutes(5)))
        .col_expr(lease::Column::UpdatedAt, sea_orm::sea_query::Expr::value(now))
        .filter(lease::Column::DraftId.eq(row.draft_id)).filter(lease::Column::OwnerToken.eq(row.owner_token))
        .filter(lease::Column::ConnectionId.eq(connection_id)).filter(lease::Column::ExpiresAt.gt(now)).exec(db).await?;
    if result.rows_affected != 1 { return Err(ExecutionError::Busy); }
    Ok(())
}
