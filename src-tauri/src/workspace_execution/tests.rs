use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait, PaginatorTrait, Set};
use crate::{
    db::{entities::{conversation, folder, workspace_chat_draft, workspace_chat_execution}, AppDatabase},
    models::AgentType,
    workspace_catalog::{create_draft, create_node, CreateDraftInput, CreateNodeInput, RepositoryDefault, Scope, WorkspaceChatDraft, WorkspaceNodeKind, draft_tests::{setup, seed_repository, FakeRepositoryAccess}},
};
use super::{get_runtime, get_runtime_for_conversation, reserve, legacy_bridge::finalize_ready, ExecutionError, WorkspaceExecutionState};

async fn fixture() -> (AppDatabase, Scope, Scope, WorkspaceChatDraft, tempfile::TempDir, String) {
    let (db, scope, other) = setup().await;
    let repository = seed_repository(&db, scope.org_id, "123", "acme/docs").await;
    let node = create_node(&db.conn, &scope, CreateNodeInput {
        parent_id: None, kind: WorkspaceNodeKind::Project, name: "Docs".into(),
        repository_default: Some(RepositoryDefault { repository_id: repository.id, starting_branch: "main".into() }),
    }).await.unwrap();
    // Keep canonical draft IDs distinct from the legacy conversation sequence.
    create_draft(&db.conn, &scope,
        &FakeRepositoryAccess::new(&[("acme/docs", "main", "123", "acme/docs")]),
        CreateDraftInput { node_id: node.id, repository_override: None, title: None, request_id: uuid::Uuid::new_v4().to_string() }
    ).await.unwrap();
    let draft = create_draft(&db.conn, &scope,
        &FakeRepositoryAccess::new(&[("acme/docs", "main", "123", "acme/docs")]),
        CreateDraftInput { node_id: node.id, repository_override: None, title: Some("First chat".into()), request_id: uuid::Uuid::new_v4().to_string() }
    ).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    for args in [vec!["init", "-q"], vec!["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "--allow-empty", "-qm", "initial"]] {
        assert!(std::process::Command::new("git").args(args).current_dir(dir.path()).status().unwrap().success());
    }
    let output = std::process::Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir.path()).output().unwrap();
    let commit = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    (db, scope, other, draft, dir, commit)
}

async fn lease(db: &AppDatabase, row: workspace_chat_execution::Model, owner: Option<&str>, seconds: i64) {
    let mut row: workspace_chat_execution::ActiveModel = row.into();
    row.provisioning_owner_token = Set(owner.map(Into::into));
    row.provisioning_lease_expires_at = Set(Some(Utc::now() + Duration::seconds(seconds)));
    row.update(&db.conn).await.unwrap();
}

#[test]
fn workspace_execution_preserves_reconnect_and_retry_error_codes() {
    use crate::workspace_catalog::CatalogError;
    assert_eq!(ExecutionError::from(CatalogError::ReauthRequired), ExecutionError::ReauthRequired);
    assert_eq!(ExecutionError::from(CatalogError::UpstreamUnavailable), ExecutionError::UpstreamUnavailable);
    assert_eq!(ExecutionError::from(CatalogError::InvalidInput("secret".into())).to_string(), "failed");
}

#[tokio::test]
async fn workspace_execution_concurrent_reservations_keep_one_row_and_failed_metadata_is_scrubbed() {
    let (db, scope, _, draft, _dir, commit) = fixture().await;
    let (first, second) = tokio::join!(
        reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit),
        reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit),
    );
    let first = first.unwrap();
    assert_eq!(first, second.unwrap());
    let mut failed: workspace_chat_execution::ActiveModel = first.into();
    failed.state = Set("failed".into());
    failed.last_error_code = Set(Some("provider token secret".into()));
    failed.update(&db.conn).await.unwrap();
    let runtime = get_runtime(&db.conn, &scope, draft.id).await.unwrap();
    assert_eq!(runtime.state, WorkspaceExecutionState::Failed);
    assert_eq!(runtime.error_code.as_deref(), Some("failed"));
    assert!(runtime.folder.is_none() && runtime.conversation.is_none());
}

#[tokio::test]
async fn workspace_execution_reservation_reuses_allocation_and_keeps_draft_immutable() {
    let (db, scope, _, draft, _dir, commit) = fixture().await;
    let before = workspace_chat_draft::Entity::find_by_id(draft.id).one(&db.conn).await.unwrap().unwrap();
    let runtime = get_runtime(&db.conn, &scope, draft.id).await.unwrap();
    assert_eq!(runtime.state, WorkspaceExecutionState::Unprovisioned);
    assert!(runtime.folder.is_none() && runtime.conversation.is_none());
    let first = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    let again = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &"b".repeat(40)).await.unwrap();
    assert_eq!(first, again);
    assert_eq!(first.starting_commit, commit);
    assert_eq!(first.branch_name, format!("workos/chat/{}", draft.id));
    assert_eq!(first.worktree_relpath, format!("orgs/1/workspace-chats/{}/worktree", draft.id));
    assert_eq!(workspace_chat_execution::Entity::find().count(&db.conn).await.unwrap(), 1);
    assert_eq!(workspace_chat_draft::Entity::find_by_id(draft.id).one(&db.conn).await.unwrap().unwrap(), before);
    assert_eq!(get_runtime(&db.conn, &scope, draft.id).await.unwrap().state, WorkspaceExecutionState::Provisioning);
    assert_eq!(reserve(&db.conn, &scope, draft.id, AgentType::Codex, &commit).await.unwrap_err(), ExecutionError::Conflict);
}

#[tokio::test]
async fn workspace_execution_checks_creator_org_and_actual_membership() {
    let (db, scope, other, draft, _dir, commit) = fixture().await;
    let mut second_user: crate::db::entities::user::ActiveModel = crate::db::entities::user::Entity::find_by_id(scope.user_id).one(&db.conn).await.unwrap().unwrap().into();
    second_user.id = Set(3); second_user.github_user_id = Set(1003); second_user.login = Set("second".into());
    second_user.insert(&db.conn).await.unwrap();
    let peer = Scope { org_id: scope.org_id, user_id: 3 };
    for foreign in [&other, &peer] {
        assert_eq!(reserve(&db.conn, foreign, draft.id, AgentType::ClaudeCode, &commit).await.unwrap_err(), ExecutionError::NotFound);
        assert_eq!(get_runtime(&db.conn, foreign, draft.id).await.unwrap_err(), ExecutionError::NotFound);
    }
    let invalid = Scope { org_id: scope.org_id, user_id: other.user_id };
    assert_eq!(get_runtime(&db.conn, &invalid, draft.id).await.unwrap_err(), ExecutionError::Unauthorized);
    let row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    lease(&db, row.clone(), Some("owner"), 60).await;
    let path = _dir.path().join(&row.worktree_relpath).to_str().unwrap().to_owned();
    let ready = finalize_ready(&db.conn, &scope, draft.id, &row.allocation_id, &path, "owner").await.unwrap();
    let conversation_id = ready.conversation.unwrap().id;
    assert_eq!(get_runtime_for_conversation(&db.conn, &peer, conversation_id).await.unwrap_err(), ExecutionError::NotFound);
    assert_eq!(get_runtime_for_conversation(&db.conn, &invalid, conversation_id).await.unwrap_err(), ExecutionError::Unauthorized);
}

#[tokio::test]
async fn workspace_execution_finalization_is_atomic_hidden_regular_and_idempotent() {
    let (db, scope, other, draft, dir, commit) = fixture().await;
    let row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    let path = dir.path().join(&row.worktree_relpath).to_str().unwrap().to_owned();
    lease(&db, row.clone(), Some("owner"), 60).await;
    // A database failure after both inserts must roll the entire bridge back.
    db.conn.execute_unprepared("CREATE TRIGGER fail_execution_ready BEFORE UPDATE OF state ON workspace_chat_execution WHEN NEW.state = 'ready' BEGIN SELECT RAISE(ABORT, 'fixture finalization failure'); END").await.unwrap();
    assert_eq!(finalize_ready(&db.conn, &scope, draft.id, &row.allocation_id, &path, "owner").await.unwrap_err(), ExecutionError::Database);
    assert_eq!(folder::Entity::find().count(&db.conn).await.unwrap(), 0);
    assert_eq!(conversation::Entity::find().count(&db.conn).await.unwrap(), 0);
    assert_eq!(get_runtime(&db.conn, &scope, draft.id).await.unwrap().state, WorkspaceExecutionState::Provisioning);
    db.conn.execute_unprepared("DROP TRIGGER fail_execution_ready").await.unwrap();
    let ready = finalize_ready(&db.conn, &scope, draft.id, &row.allocation_id, &path, "owner").await.unwrap();
    let retried = finalize_ready(&db.conn, &scope, draft.id, &row.allocation_id, &path, "owner").await.unwrap();
    assert_eq!(ready.state, WorkspaceExecutionState::Ready);
    let folder = ready.folder.as_ref().unwrap();
    let conversation = ready.conversation.as_ref().unwrap();
    assert_eq!(serde_json::to_value(&folder.kind).unwrap(), "workspace_chat");
    assert_eq!(serde_json::to_value(&conversation.kind).unwrap(), "regular");
    assert_eq!(conversation.folder_id, folder.id);
    assert_eq!(conversation.git_branch.as_deref(), Some(row.branch_name.as_str()));
    assert_eq!(retried.conversation.unwrap().id, conversation.id);
    assert_eq!(crate::db::service::folder_service::list_all_folder_details(&db.conn).await.unwrap().len(), 1);
    assert!(crate::db::service::folder_service::list_open_folder_details(&db.conn).await.unwrap().is_empty());
    assert_eq!(folder::Entity::find().count(&db.conn).await.unwrap(), 1);
    assert_eq!(conversation::Entity::find().count(&db.conn).await.unwrap(), 1);
    let inverse = get_runtime_for_conversation(&db.conn, &scope, conversation.id).await.unwrap();
    assert_ne!(conversation.id, draft.id);
    assert_eq!(inverse.draft.id, draft.id);
    assert_eq!(get_runtime_for_conversation(&db.conn, &other, conversation.id).await.unwrap_err(), ExecutionError::NotFound);
    assert_eq!(finalize_ready(&db.conn, &scope, draft.id, &row.allocation_id, "/different", "owner").await.unwrap_err(), ExecutionError::AllocationMismatch);
    assert_eq!(workspace_chat_draft::Entity::find_by_id(draft.id).one(&db.conn).await.unwrap().unwrap().execution_state, "unprovisioned");
    // Nullable unique mappings protect all three identities independently.
    let ready_row = workspace_chat_execution::Entity::find_by_id(draft.id).one(&db.conn).await.unwrap().unwrap();
    for duplicate in ["allocation", "folder", "conversation"] {
        let mut conflicting: workspace_chat_execution::ActiveModel = ready_row.clone().into();
        conflicting.draft_id = Set(draft.id - 1);
        if duplicate != "allocation" {
            conflicting.allocation_id = Set(uuid::Uuid::new_v4().to_string());
        }
        if duplicate != "folder" { conflicting.legacy_folder_id = Set(None); }
        if duplicate != "conversation" { conflicting.legacy_conversation_id = Set(None); }
        assert!(conflicting.insert(&db.conn).await.is_err(), "duplicate {duplicate} must be rejected");
    }
    use sea_orm_migration::MigratorTrait;
    assert!(crate::db::migration::Migrator::down(&db.conn, Some(1)).await.is_err());
    assert_eq!(conversation::Entity::find().count(&db.conn).await.unwrap(), 1);
}

#[tokio::test]
async fn workspace_execution_finalization_rejects_unowned_expired_or_wrong_lease() {
    let (db, scope, _, draft, dir, commit) = fixture().await;
    let row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    let path = dir.path().join(&row.worktree_relpath).to_str().unwrap().to_owned();
    for (owner, seconds, supplied) in [(None, 60, "owner"), (Some("owner"), -1, "owner"), (Some("owner"), 60, "other"), (Some(""), 60, "")] {
        lease(&db, row.clone(), owner, seconds).await;
        assert_eq!(finalize_ready(&db.conn, &scope, draft.id, &row.allocation_id, &path, supplied).await.unwrap_err(), ExecutionError::Busy);
        assert_eq!(folder::Entity::find().count(&db.conn).await.unwrap(), 0);
        assert_eq!(conversation::Entity::find().count(&db.conn).await.unwrap(), 0);
    }
}

#[tokio::test]
async fn workspace_execution_legacy_creation_cannot_reuse_managed_folder() {
    let (db, scope, _, draft, dir, commit) = fixture().await;
    let row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    lease(&db, row.clone(), Some("owner"), 60).await;
    let path = dir.path().join(&row.worktree_relpath).to_str().unwrap().to_owned();
    let runtime = finalize_ready(&db.conn, &scope, draft.id, &row.allocation_id, &path, "owner").await.unwrap();
    assert!(crate::commands::conversations::create_conversation_core(&db.conn, runtime.folder.unwrap().id, AgentType::ClaudeCode, None).await.is_err());
    assert!(crate::commands::conversations::create_chat_conversation_core(&db.conn, dir.path(), AgentType::ClaudeCode, None, Some(&path)).await.is_err());
    assert!(!std::path::Path::new(&path).exists(), "legacy creation must reject before mkdir");
    assert_eq!(conversation::Entity::find().count(&db.conn).await.unwrap(), 1);
}

async fn ready_fixture() -> (AppDatabase, tempfile::TempDir, super::WorkspaceChatRuntime) {
    let (db, scope, _, draft, dir, commit) = fixture().await;
    let row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit)
        .await.unwrap();
    lease(&db, row.clone(), Some("owner"), 60).await;
    let path = dir.path().join(&row.worktree_relpath);
    std::fs::create_dir_all(&path).unwrap();
    let runtime = finalize_ready(
        &db.conn, &scope, draft.id, &row.allocation_id, path.to_str().unwrap(), "owner",
    ).await.unwrap();
    (db, dir, runtime)
}

#[tokio::test]
async fn workspace_execution_folderless_rejects_uncreated_managed_descendant() {
    let (db, dir, runtime) = ready_fixture().await;
    let descendant = std::path::Path::new(&runtime.folder.unwrap().path).join("new/subdir");
    let result = crate::commands::conversations::create_chat_conversation_core(
        &db.conn, dir.path(), AgentType::ClaudeCode, None, descendant.to_str(),
    ).await;
    assert!(result.is_err(), "managed descendants must not become scratch chats");
    assert!(!descendant.parent().unwrap().exists(), "reject before mkdir");
    assert_eq!(folder::Entity::find().count(&db.conn).await.unwrap(), 1);
    assert_eq!(conversation::Entity::find().count(&db.conn).await.unwrap(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_execution_folderless_rejects_missing_descendant_through_symlink_ancestor() {
    let (db, dir, runtime) = ready_fixture().await;
    let managed = runtime.folder.unwrap().path;
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&managed, &alias).unwrap();
    let descendant = alias.join("new/subdir");
    let result = crate::commands::conversations::create_chat_conversation_core(
        &db.conn, dir.path(), AgentType::ClaudeCode, None, descendant.to_str(),
    ).await;
    assert!(result.is_err(), "an existing symlink ancestor must resolve before checking scope");
    assert!(!std::path::Path::new(&managed).join("new").exists());
    assert_eq!(folder::Entity::find().count(&db.conn).await.unwrap(), 1);
    assert_eq!(conversation::Entity::find().count(&db.conn).await.unwrap(), 1);
}

#[tokio::test]
async fn workspace_execution_folderless_allows_unrelated_sibling_prefix() {
    let (db, dir, runtime) = ready_fixture().await;
    let managed = runtime.folder.unwrap().path;
    let sibling = std::path::PathBuf::from(format!("{managed}-scratch")).join("new");
    let created = crate::commands::conversations::create_chat_conversation_core(
        &db.conn, dir.path(), AgentType::ClaudeCode, None, sibling.to_str(),
    ).await.unwrap();
    assert_eq!(std::path::Path::new(&created.folder.path), sibling.as_path());
    assert!(sibling.is_dir());
    assert_eq!(folder::Entity::find().count(&db.conn).await.unwrap(), 2);
    assert_eq!(conversation::Entity::find().count(&db.conn).await.unwrap(), 2);
}

#[tokio::test]
async fn workspace_execution_delegation_constructor_cannot_reuse_managed_folder() {
    let (db, _dir, runtime) = ready_fixture().await;
    let folder_id = runtime.folder.unwrap().id;
    let parent_id = runtime.conversation.unwrap().id;
    let mut results = Vec::new();
    for delegation in [None, Some(crate::acp::delegation::spawner::DelegationLink {
        parent_conversation_id: parent_id,
        parent_tool_use_id: "tool-fixture".into(),
        delegation_call_id: "call-fixture".into(),
    })] {
        results.push(crate::db::service::conversation_service::create_with_delegation(
            &db.conn, folder_id, AgentType::ClaudeCode, None, None, delegation,
        ).await);
    }
    assert!(results.iter().all(Result::is_err), "both regular and delegated legacy construction must reject");
    assert_eq!(folder::Entity::find().count(&db.conn).await.unwrap(), 1);
    assert_eq!(conversation::Entity::find().count(&db.conn).await.unwrap(), 1);
}
