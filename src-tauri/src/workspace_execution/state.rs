use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    QueryFilter, Set, TransactionTrait,
};
use crate::{
    db::{entities::{conversation, folder, workspace_chat_draft, workspace_chat_execution},
        service::{conversation_service, folder_service}},
    models::AgentType,
    workspace_catalog::{begin_scoped_write, to_workspace_chat_draft, verify_repository, verify_scope, Scope},
};
use super::{ExecutionError, WorkspaceChatRuntime, WorkspaceExecutionState};

pub(crate) async fn scoped_draft<C: ConnectionTrait>(
    db: &C, scope: &Scope, draft_id: i32,
) -> Result<workspace_chat_draft::Model, ExecutionError> {
    verify_scope(db, scope).await?;
    workspace_chat_draft::Entity::find_by_id(draft_id)
        .filter(workspace_chat_draft::Column::OrgId.eq(scope.org_id))
        .filter(workspace_chat_draft::Column::CreatorUserId.eq(scope.user_id))
        .one(db).await?.ok_or(ExecutionError::NotFound)
}

pub(crate) async fn runtime_in<C: ConnectionTrait>(
    db: &C, scope: &Scope, draft_id: i32,
) -> Result<WorkspaceChatRuntime, ExecutionError> {
    let draft = scoped_draft(db, scope, draft_id).await?;
    let execution = workspace_chat_execution::Entity::find_by_id(draft_id)
        .filter(workspace_chat_execution::Column::OrgId.eq(scope.org_id))
        .one(db).await?;
    let mut runtime = WorkspaceChatRuntime {
        draft: to_workspace_chat_draft(draft),
        state: WorkspaceExecutionState::Unprovisioned,
        folder: None, conversation: None, branch_name: None, starting_commit: None, error_code: None,
    };
    let Some(execution) = execution else { return Ok(runtime); };
    runtime.state = match execution.state.as_str() {
        "provisioning" => WorkspaceExecutionState::Provisioning,
        "ready" => WorkspaceExecutionState::Ready,
        "failed" => WorkspaceExecutionState::Failed,
        _ => return Err(ExecutionError::Database),
    };
    runtime.branch_name = Some(execution.branch_name.clone());
    runtime.starting_commit = Some(execution.starting_commit);
    runtime.error_code = execution.last_error_code.as_deref().map(scrub_error_code);
    if runtime.state == WorkspaceExecutionState::Ready {
        let folder = folder::Entity::find_by_id(execution.legacy_folder_id.ok_or(ExecutionError::AllocationMismatch)?)
            .filter(folder::Column::DeletedAt.is_null()).one(db).await?
            .ok_or(ExecutionError::AllocationMismatch)?;
        let conversation = conversation::Entity::find_by_id(execution.legacy_conversation_id.ok_or(ExecutionError::AllocationMismatch)?)
            .filter(conversation::Column::DeletedAt.is_null()).one(db).await?
            .ok_or(ExecutionError::AllocationMismatch)?;
        if folder.kind != folder::FolderKind::WorkspaceChat
            || conversation.kind != conversation::ConversationKind::Regular
            || conversation.folder_id != folder.id
            || conversation.agent_type != execution.agent_type
            || conversation.git_branch.as_deref() != Some(execution.branch_name.as_str())
        {
            return Err(ExecutionError::AllocationMismatch);
        }
        runtime.folder = Some(folder_service::to_detail(folder));
        runtime.conversation = Some(conversation_service::conv_to_summary(conversation));
    }
    Ok(runtime)
}

fn scrub_error_code(code: &str) -> String {
    match code {
        "unauthorized" | "not_found" | "conflict" | "disabled" | "repository_unavailable"
        | "reauth_required" | "upstream_unavailable" | "allocation_mismatch" | "busy"
        | "failed" | "database_error" => code.into(),
        _ => "failed".into(),
    }
}

/// Read a consistent snapshot; the immutable draft never owns runtime state.
pub async fn get_runtime(
    db: &DatabaseConnection, scope: &Scope, draft_id: i32,
) -> Result<WorkspaceChatRuntime, ExecutionError> {
    let txn = db.begin().await?;
    let runtime = runtime_in(&txn, scope, draft_id).await?;
    txn.commit().await?;
    Ok(runtime)
}

/// Recover canonical identity for a persisted legacy tab, without conflating IDs.
pub async fn get_runtime_for_conversation(
    db: &DatabaseConnection, scope: &Scope, conversation_id: i32,
) -> Result<WorkspaceChatRuntime, ExecutionError> {
    let txn = db.begin().await?;
    verify_scope(&txn, scope).await?;
    let execution = workspace_chat_execution::Entity::find()
        .filter(workspace_chat_execution::Column::OrgId.eq(scope.org_id))
        .filter(workspace_chat_execution::Column::LegacyConversationId.eq(conversation_id))
        .one(&txn).await?.ok_or(ExecutionError::NotFound)?;
    let runtime = runtime_in(&txn, scope, execution.draft_id).await?;
    txn.commit().await?;
    Ok(runtime)
}

/// Reserve intent before any filesystem mutation. Retries retain the original
/// commit and state; provisioning lease acquisition/retry transitions are separate.
pub async fn reserve(
    db: &DatabaseConnection, scope: &Scope, draft_id: i32,
    agent_type: AgentType, starting_commit: &str,
) -> Result<workspace_chat_execution::Model, ExecutionError> {
    let txn = begin_scoped_write(db, scope).await?;
    let draft = scoped_draft(&txn, scope, draft_id).await?;
    let repository = verify_repository(&txn, scope, draft.repository_id).await?;
    if repository.github_repository_id != draft.github_repository_id {
        return Err(ExecutionError::RepositoryUnavailable);
    }
    if let Some(existing) = workspace_chat_execution::Entity::find_by_id(draft_id).one(&txn).await? {
        if existing.org_id != scope.org_id {
            return Err(ExecutionError::NotFound);
        }
        if existing.agent_type != agent_type.as_wire() {
            return Err(ExecutionError::Conflict);
        }
        txn.commit().await?;
        return Ok(existing);
    }
    if !matches!(starting_commit.len(), 40 | 64)
        || !starting_commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ExecutionError::AllocationMismatch);
    }
    let now = Utc::now();
    let execution = workspace_chat_execution::ActiveModel {
        draft_id: Set(draft_id), org_id: Set(scope.org_id),
        agent_type: Set(agent_type.as_wire().into_owned()), state: Set("provisioning".into()),
        allocation_id: Set(uuid::Uuid::new_v4().to_string()),
        starting_commit: Set(starting_commit.to_ascii_lowercase()),
        branch_name: Set(format!("workos/chat/{draft_id}")),
        worktree_relpath: Set(format!("orgs/{}/workspace-chats/{draft_id}/worktree", scope.org_id)),
        legacy_folder_id: Set(None), legacy_conversation_id: Set(None), revision: Set(0),
        started_at: Set(now), updated_at: Set(now), last_error_code: Set(None),
        provisioning_owner_token: Set(None), provisioning_lease_expires_at: Set(None),
    }.insert(&txn).await?;
    txn.commit().await?;
    Ok(execution)
}
