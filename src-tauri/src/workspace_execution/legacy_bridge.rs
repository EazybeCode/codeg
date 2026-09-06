use std::path::{Component, Path};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use crate::{
    db::{entities::{folder, workspace_chat_execution}, service::{conversation_service, folder_service}},
    models::AgentType,
    workspace_catalog::{begin_scoped_write, Scope},
};
use super::{state::{runtime_in, scoped_draft}, ExecutionError, WorkspaceChatRuntime};

/// Finalize only after the allocator has verified the exact isolated checkout.
/// All three writes are atomic. A ready retry checks the persisted identity and
/// returns it without requiring an old provisioning lease to remain live.
pub async fn finalize_ready(
    db: &DatabaseConnection,
    scope: &Scope,
    draft_id: i32,
    allocation_id: &str,
    worktree_path: &str,
    provisioning_owner_token: &str,
) -> Result<WorkspaceChatRuntime, ExecutionError> {
    let txn = begin_scoped_write(db, scope).await?;
    let draft = scoped_draft(&txn, scope, draft_id).await?;
    let execution = workspace_chat_execution::Entity::find_by_id(draft_id)
        .filter(workspace_chat_execution::Column::OrgId.eq(scope.org_id))
        .one(&txn).await?.ok_or(ExecutionError::NotFound)?;
    let path = Path::new(worktree_path);
    if execution.allocation_id != allocation_id || !path.is_absolute()
        || path.components().any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        || !path.ends_with(&execution.worktree_relpath)
    {
        return Err(ExecutionError::AllocationMismatch);
    }
    if execution.state == "ready" {
        let runtime = runtime_in(&txn, scope, draft_id).await?;
        if runtime.folder.as_ref().map(|folder| folder.path.as_str()) != Some(worktree_path) {
            return Err(ExecutionError::AllocationMismatch);
        }
        txn.commit().await?;
        return Ok(runtime);
    }
    if provisioning_owner_token.is_empty()
        || execution.provisioning_owner_token.as_deref() != Some(provisioning_owner_token)
        || !execution.provisioning_lease_expires_at.is_some_and(|expires| expires > Utc::now())
    {
        return Err(ExecutionError::Busy);
    }
    if execution.state != "provisioning"
        || execution.legacy_folder_id.is_some() || execution.legacy_conversation_id.is_some()
    {
        return Err(ExecutionError::Conflict);
    }
    if folder::Entity::find().filter(folder::Column::Path.eq(worktree_path)).one(&txn).await?.is_some() {
        return Err(ExecutionError::AllocationMismatch);
    }
    let agent = AgentType::from_wire(&execution.agent_type).ok_or(ExecutionError::Database)?;
    let folder = folder_service::create_workspace_chat_folder(
        &txn, worktree_path, draft.title.as_deref().unwrap_or("Workspace chat"),
        &execution.branch_name, &draft.full_name,
    ).await?;
    let conversation = conversation_service::create_workspace_chat(
        &txn, folder.id, agent, draft.title, execution.branch_name.clone(),
    ).await?;
    let revision = execution.revision;
    let mut ready: workspace_chat_execution::ActiveModel = execution.into();
    ready.state = Set("ready".into());
    ready.legacy_folder_id = Set(Some(folder.id));
    ready.legacy_conversation_id = Set(Some(conversation.id));
    ready.last_error_code = Set(None);
    ready.revision = Set(revision + 1);
    ready.updated_at = Set(Utc::now());
    ready.provisioning_owner_token = Set(None);
    ready.provisioning_lease_expires_at = Set(None);
    ready.update(&txn).await?;
    let runtime = runtime_in(&txn, scope, draft_id).await?;
    txn.commit().await?;
    Ok(runtime)
}
