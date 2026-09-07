use sea_orm::{DatabaseConnection, EntityTrait, ColumnTrait, QueryFilter};
use crate::db::entities::{workspace_chat_execution, workspace_writer_lease, folder};
use super::ExecutionError;
pub fn reject_managed_root(root: &std::path::Path, working_dir: Option<&str>) -> Result<(), ExecutionError> {
    let Some(path) = working_dir else { return Ok(()); };
    let resolve = crate::commands::conversations::resolve_chat_creation_path;
    let root = resolve(&root.join("orgs")).map_err(|_| ExecutionError::AllocationMismatch)?;
    let candidate = resolve(std::path::Path::new(path)).map_err(|_| ExecutionError::AllocationMismatch)?;
    if let Ok(relative) = candidate.strip_prefix(root) {
        let mut components = relative.components();
        if components.next().is_some() && components.next().is_some_and(|part| part.as_os_str() == "workspace-chats") {
            return Err(ExecutionError::Conflict);
        }
    }
    Ok(())
}
pub async fn reject_generic(db: &DatabaseConnection, working_dir: Option<&str>, conversation_id: Option<i32>, connection_id: Option<&str>) -> Result<(), ExecutionError> {
    if let Some(id) = conversation_id {
        if workspace_chat_execution::Entity::find().filter(workspace_chat_execution::Column::LegacyConversationId.eq(id)).one(db).await?.is_some() {
            return Err(ExecutionError::Conflict);
        }
    }
    if let Some(id) = connection_id {
        if workspace_writer_lease::Entity::find().filter(workspace_writer_lease::Column::ConnectionId.eq(id)).one(db).await?.is_some() {
            return Err(ExecutionError::Conflict);
        }
    }
    if let Some(path) = working_dir {
        let candidate = crate::commands::conversations::resolve_chat_creation_path(std::path::Path::new(path)).map_err(|_| ExecutionError::AllocationMismatch)?;
        for folder in folder::Entity::find().filter(folder::Column::Kind.eq(folder::FolderKind::WorkspaceChat)).all(db).await? {
            let root = crate::commands::conversations::resolve_chat_creation_path(std::path::Path::new(&folder.path)).map_err(|_| ExecutionError::AllocationMismatch)?;
            if candidate.starts_with(root) { return Err(ExecutionError::Conflict); }
        }
    }
    Ok(())
}
