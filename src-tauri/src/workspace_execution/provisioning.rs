use crate::{models::AgentType, workspace_catalog::Scope};
use super::{ExecutionError, WorkspaceChatRuntime, git_allocator::GitAllocator, repository_access::ExecutionRepositoryAccess};
use sea_orm::{ActiveModelTrait, EntityTrait, Set, TransactionTrait};
use chrono::{Duration, Utc};
use crate::{db::entities::workspace_chat_execution, workspace_catalog::{begin_scoped_write, verify_repository}};

/// Recheck metadata before a ready writer starts or receives another prompt.
/// This never resolves the original branch or changes the pinned commit.
pub async fn verify_draft_access(db: &sea_orm::DatabaseConnection, scope: &Scope, draft_id: i32, access: &dyn ExecutionRepositoryAccess) -> Result<super::repository_access::VerifiedExecutionIdentity, ExecutionError> {
    let draft = super::state::scoped_draft(db, scope, draft_id).await?;
    let identity = access.verify_access(scope, draft.repository_id).await?;
    if identity.github_repository_id != draft.github_repository_id { return Err(ExecutionError::RepositoryUnavailable); }
    // Access verification did network I/O; repeat local authorization afterward.
    let txn = db.begin().await?;
    let current = super::state::scoped_draft(&txn, scope, draft_id).await?;
    let repository = verify_repository(&txn, scope, current.repository_id).await?;
    if current.repository_id != draft.repository_id || current.github_repository_id != identity.github_repository_id
        || repository.github_repository_id != identity.github_repository_id
    {
        return Err(ExecutionError::RepositoryUnavailable);
    }
    txn.commit().await?;
    Ok(identity)
}

pub async fn provision(db: &sea_orm::DatabaseConnection, scope: &Scope, draft_id: i32, agent: AgentType, access: &dyn ExecutionRepositoryAccess, allocator: &dyn GitAllocator, enabled: bool) -> Result<WorkspaceChatRuntime, ExecutionError> {
    if !enabled { return Err(ExecutionError::Disabled); }
    let draft = super::state::scoped_draft(db, scope, draft_id).await?;
    let existing = workspace_chat_execution::Entity::find_by_id(draft_id).one(db).await?;
    if existing.as_ref().is_some_and(|row| row.agent_type != agent.as_wire()) { return Err(ExecutionError::Conflict); }
    let mut verified = if let Some(row) = &existing {
        let identity = verify_draft_access(db, scope, draft_id, access).await?;
        super::repository_access::VerifiedExecutionRepository { github_repository_id: identity.github_repository_id, full_name: identity.full_name, starting_commit: row.starting_commit.clone() }
    } else {
        access.resolve(scope, draft.repository_id, &draft.starting_branch).await?
    };
    if verified.github_repository_id != draft.github_repository_id { return Err(ExecutionError::RepositoryUnavailable); }
    crate::workspace_catalog::validate_repository_full_name(&verified.full_name)?;
    let row = super::reserve(db, scope, draft_id, agent, &verified.starting_commit).await?;
    // A concurrent reservation may have pinned an earlier authorized commit.
    verified.starting_commit = row.starting_commit.clone();
    if row.state == "ready" { return super::get_runtime(db, scope, draft_id).await; }
    let owner = uuid::Uuid::new_v4().to_string();
    let row = acquire_lease(db, scope, draft_id, &owner, &verified.github_repository_id).await?;
    let result = async {
        let allocation = allocator.allocate(scope, &row, &verified);
        tokio::pin!(allocation);
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(60));
        heartbeat.tick().await;
        let allocation = loop {
            tokio::select! {
                result = &mut allocation => break result?,
                _ = heartbeat.tick() => renew_lease(db, scope, draft_id, &owner).await?,
            }
        };
        if allocation.allocation_id != row.allocation_id || allocation.branch_name != row.branch_name {
            return Err(ExecutionError::AllocationMismatch);
        }
        let path = allocation.worktree_path.to_str().ok_or(ExecutionError::AllocationMismatch)?;
        super::legacy_bridge::finalize_ready(db, scope, draft_id, &row.allocation_id, path, &owner).await
    }.await;
    if let Err(error) = &result {
        // Only this owner can release its attempt. If DB is unavailable, the
        // persisted expiry permits later recovery; never mask the original error.
        let _ = fail_owned(db, scope, draft_id, &owner, error).await;
    }
    result
}

pub(crate) async fn renew_lease(db: &sea_orm::DatabaseConnection, scope: &Scope, draft_id: i32, owner: &str) -> Result<(), ExecutionError> {
    let txn = begin_scoped_write(db, scope).await?;
    super::state::scoped_draft(&txn, scope, draft_id).await?;
    let row = workspace_chat_execution::Entity::find_by_id(draft_id).one(&txn).await?.ok_or(ExecutionError::NotFound)?;
    let now = Utc::now();
    if owner.is_empty() || row.state != "provisioning" || row.provisioning_owner_token.as_deref() != Some(owner)
        || !row.provisioning_lease_expires_at.is_some_and(|expiry| expiry > now)
    { return Err(ExecutionError::Busy); }
    let revision = row.revision;
    let mut active: workspace_chat_execution::ActiveModel = row.into();
    active.provisioning_lease_expires_at = Set(Some(now + Duration::minutes(5)));
    active.updated_at = Set(now);
    active.revision = Set(revision + 1);
    active.update(&txn).await?;
    txn.commit().await?;
    Ok(())
}

async fn acquire_lease(db: &sea_orm::DatabaseConnection, scope: &Scope, draft_id: i32, owner: &str, github_id: &str) -> Result<workspace_chat_execution::Model, ExecutionError> {
    let txn = begin_scoped_write(db, scope).await?;
    let draft = super::state::scoped_draft(&txn, scope, draft_id).await?;
    let repository = verify_repository(&txn, scope, draft.repository_id).await?;
    if repository.github_repository_id != github_id || draft.github_repository_id != github_id { return Err(ExecutionError::RepositoryUnavailable); }
    let row = workspace_chat_execution::Entity::find_by_id(draft_id).one(&txn).await?.ok_or(ExecutionError::NotFound)?;
    let now = Utc::now();
    if row.state == "ready" || row.provisioning_owner_token.is_some() && row.provisioning_lease_expires_at.is_some_and(|expires| expires > now) {
        return Err(ExecutionError::Busy);
    }
    let revision = row.revision;
    let mut active: workspace_chat_execution::ActiveModel = row.into();
    active.state = Set("provisioning".into());
    active.provisioning_owner_token = Set(Some(owner.into()));
    active.provisioning_lease_expires_at = Set(Some(now + Duration::minutes(5)));
    active.last_error_code = Set(None);
    active.updated_at = Set(now);
    active.revision = Set(revision + 1);
    let row = active.update(&txn).await?;
    txn.commit().await?;
    Ok(row)
}

async fn fail_owned(db: &sea_orm::DatabaseConnection, scope: &Scope, draft_id: i32, owner: &str, error: &ExecutionError) -> Result<(), ExecutionError> {
    let txn = begin_scoped_write(db, scope).await?;
    super::state::scoped_draft(&txn, scope, draft_id).await?;
    let row = workspace_chat_execution::Entity::find_by_id(draft_id).one(&txn).await?.ok_or(ExecutionError::NotFound)?;
    if row.state == "provisioning" && row.provisioning_owner_token.as_deref() == Some(owner) {
        let revision = row.revision;
        let mut active: workspace_chat_execution::ActiveModel = row.into();
        active.state = Set("failed".into());
        active.provisioning_owner_token = Set(None);
        active.provisioning_lease_expires_at = Set(None);
        active.last_error_code = Set(Some(error.to_string()));
        active.updated_at = Set(Utc::now());
        active.revision = Set(revision + 1);
        active.update(&txn).await?;
    }
    txn.commit().await?;
    Ok(())
}
