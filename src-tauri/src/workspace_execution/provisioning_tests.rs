use super::*;
use super::git_allocator::allocate_local;
use crate::{models::AgentType, workspace_catalog::Scope, db::entities::workspace_chat_execution};
use sea_orm::{ConnectionTrait, EntityTrait};
use async_trait::async_trait;
use std::{path::PathBuf, sync::atomic::{AtomicBool, Ordering}};
struct Access { commit: String, deleted_branch: AtomicBool, denied: AtomicBool }
#[async_trait]
impl ExecutionRepositoryAccess for Access {
    async fn verify_access(&self, _: &Scope, _: i32) -> Result<VerifiedExecutionIdentity, ExecutionError> {
        if self.denied.load(Ordering::SeqCst) { return Err(ExecutionError::ReauthRequired); }
        Ok(VerifiedExecutionIdentity { github_repository_id: "123".into(), verified_names: vec!["acme/docs".into()], full_name: "acme/docs".into() })
    }
    async fn resolve(&self, scope: &Scope, id: i32, _: &str) -> Result<VerifiedExecutionRepository, ExecutionError> {
        let identity = self.verify_access(scope, id).await?;
        if self.deleted_branch.load(Ordering::SeqCst) { return Err(ExecutionError::RepositoryUnavailable); }
        Ok(VerifiedExecutionRepository { github_repository_id: identity.github_repository_id, verified_names: identity.verified_names, full_name: identity.full_name, starting_commit: self.commit.clone() })
    }
}
struct Local { root: PathBuf, remote: String }
#[async_trait]
impl GitAllocator for Local {
    async fn allocate(&self, _: &Scope, row: &workspace_chat_execution::Model, repo: &VerifiedExecutionRepository) -> Result<AllocatedWorktree, ExecutionError> {
        allocate_local(&self.root, row, repo, &self.remote)
    }
}
fn git(path: &std::path::Path, args: &[&str]) -> String {
    let result = std::process::Command::new("git").args(args).current_dir(path).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    String::from_utf8(result.stdout).unwrap().trim().into()
}

#[tokio::test]
async fn workspace_execution_verified_renames_reuse_ready_and_new_allocations() {
    let (db, scope, _, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    let mut row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    let mut repo = VerifiedExecutionRepository { github_repository_id: "123".into(), full_name: "acme/docs".into(), verified_names: vec!["acme/docs".into()], starting_commit: commit.clone() };
    let first = allocate_local(root.path(), &row, &repo, remote.path().to_str().unwrap()).unwrap();
    let cache = root.path().join("orgs/1/repositories/123/repo.git");
    git(&cache, &["remote", "set-url", "origin", "https://github.com/acme/docs.git"]);
    std::fs::write(first.worktree_path.join("unpublished.md"), "keep this work").unwrap();
    git(&first.worktree_path, &["add", "unpublished.md"]);
    git(&first.worktree_path, &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-m", "unpublished"]);
    std::fs::write(first.worktree_path.join("dirty.md"), "dirty work").unwrap();
    let unpublished_head = git(&first.worktree_path, &["rev-parse", "HEAD"]);
    row.state = "ready".into();
    for name in ["acme/renamed", "acme/renamed-again"] {
        repo.full_name = name.into();
        repo.verified_names.push(name.into());
        let identity = VerifiedExecutionIdentity { github_repository_id: "123".into(), full_name: name.into(), verified_names: repo.verified_names.clone() };
        assert_eq!(super::git_allocator::validate_ready_checkout(root.path(), &row, &identity).unwrap(), first.worktree_path);
        assert_eq!(git(&cache, &["config", "--get", "remote.origin.url"]), format!("https://github.com/{name}.git"));
    }
    let second_row = reserve(&db.conn, &scope, draft.id - 1, AgentType::ClaudeCode, &commit).await.unwrap();
    // Exercise allocation's reconciliation independently of ready validation.
    git(&cache, &["remote", "set-url", "origin", "https://github.com/acme/docs.git"]);
    let second = allocate_local(root.path(), &second_row, &repo, "https://github.com/acme/renamed-again.git").unwrap();
    assert_eq!(git(&cache, &["config", "--get", "remote.origin.url"]), "https://github.com/acme/renamed-again.git");
    assert_ne!(second.allocation_id, first.allocation_id);
    assert_eq!(git(&first.worktree_path, &["rev-parse", "HEAD"]), unpublished_head);
    assert_eq!(std::fs::read_to_string(first.worktree_path.join("dirty.md")).unwrap(), "dirty work");
    git(&cache, &["remote", "set-url", "origin", "https://github.com/unrelated/repo.git"]);
    let identity = VerifiedExecutionIdentity { github_repository_id: "123".into(), full_name: repo.full_name.clone(), verified_names: repo.verified_names.clone() };
    assert_eq!(super::git_allocator::validate_ready_checkout(root.path(), &row, &identity).unwrap_err(), ExecutionError::AllocationMismatch);
    assert_eq!(allocate_local(root.path(), &second_row, &repo, "https://github.com/acme/renamed-again.git").unwrap_err(), ExecutionError::AllocationMismatch);
}

#[tokio::test]
async fn workspace_execution_rename_backfill_uses_scoped_verified_draft_snapshots() {
    use sea_orm::{ActiveModelTrait, Set};
    use sea_orm_migration::{MigrationTrait, SchemaManager, prelude::Table};
    use crate::db::entities::{workspace_repository, workspace_chat_draft};
    let (db, scope, _, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    let mut row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    let repo = VerifiedExecutionRepository { github_repository_id: "123".into(), full_name: "acme/docs".into(), verified_names: vec!["acme/docs".into()], starting_commit: commit };
    let allocation = allocate_local(root.path(), &row, &repo, remote.path().to_str().unwrap()).unwrap();
    let cache = root.path().join("orgs/1/repositories/123/repo.git");
    git(&cache, &["remote", "set-url", "origin", "https://github.com/acme/docs.git"]);
    let repository = workspace_repository::Entity::find_by_id(draft.repository_id).one(&db.conn).await.unwrap().unwrap();
    let mut renamed: workspace_repository::ActiveModel = repository.into();
    renamed.full_name = Set("acme/already-renamed".into());
    renamed.update(&db.conn).await.unwrap();
    // An inconsistent foreign-ID snapshot must not contribute history even
    // when its repository_id happens to refer to this catalog row.
    let mut foreign: workspace_chat_draft::ActiveModel = workspace_chat_draft::Entity::find_by_id(draft.id - 1).one(&db.conn).await.unwrap().unwrap().into();
    foreign.github_repository_id = Set("999".into());
    foreign.full_name = Set("unrelated/repo".into());
    foreign.update(&db.conn).await.unwrap();
    // This in-memory DB alone emulates the pre-fix schema after reconnection.
    let manager = SchemaManager::new(&db.conn);
    manager.alter_table(Table::alter().table(workspace_repository::Entity).drop_column(workspace_repository::Column::VerifiedNames).to_owned()).await.unwrap();
    crate::db::migration::m20260907_000004_workspace_repository_names::Migration.up(&manager).await.unwrap();
    let repository = workspace_repository::Entity::find_by_id(draft.repository_id).one(&db.conn).await.unwrap().unwrap();
    let names = crate::workspace_catalog::verified_repository_names(&repository).unwrap();
    assert_eq!(names, vec!["acme/already-renamed", "acme/docs"]);
    row.state = "ready".into();
    let identity = VerifiedExecutionIdentity { github_repository_id: "123".into(), full_name: repository.full_name, verified_names: names };
    assert_eq!(super::git_allocator::validate_ready_checkout(root.path(), &row, &identity).unwrap(), allocation.worktree_path);
    assert_eq!(git(&cache, &["config", "--get", "remote.origin.url"]), "https://github.com/acme/already-renamed.git");
}

#[tokio::test]
async fn workspace_execution_git_reattaches_reserved_branch_and_rejects_cache_identity_change() {
    let (db, scope, _, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    let row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    let repo = VerifiedExecutionRepository { github_repository_id: "123".into(), verified_names: vec!["acme/docs".into()], full_name: "acme/docs".into(), starting_commit: commit };
    let allocated = allocate_local(root.path(), &row, &repo, remote.path().to_str().unwrap()).unwrap();
    let cache = root.path().join("orgs/1/repositories/123/repo.git");
    git(&cache, &["worktree", "remove", allocated.worktree_path.to_str().unwrap()]);
    let retry = allocate_local(root.path(), &row, &repo, remote.path().to_str().unwrap()).unwrap();
    assert_eq!(retry.allocation_id, row.allocation_id);
    assert_eq!(git(&retry.worktree_path, &["rev-parse", "HEAD"]), row.starting_commit);
    git(&cache, &["remote", "set-url", "origin", "https://github.com/unrelated/repository.git"]);
    std::fs::write(retry.worktree_path.join("keep"), "preserved").unwrap();
    assert_eq!(allocate_local(root.path(), &row, &repo, remote.path().to_str().unwrap()).unwrap_err(), ExecutionError::AllocationMismatch);
    assert_eq!(std::fs::read_to_string(retry.worktree_path.join("keep")).unwrap(), "preserved");
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_execution_git_rejects_symlink_inside_cache_metadata() {
    let (db, scope, _, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    let row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    let repo = VerifiedExecutionRepository { github_repository_id: "123".into(), verified_names: vec!["acme/docs".into()], full_name: "acme/docs".into(), starting_commit: commit };
    allocate_local(root.path(), &row, &repo, remote.path().to_str().unwrap()).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("keep"), "preserved").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("orgs/1/repositories/123/repo.git/objects/foreign")).unwrap();
    assert_eq!(allocate_local(root.path(), &row, &repo, remote.path().to_str().unwrap()).unwrap_err(), ExecutionError::AllocationMismatch);
    assert_eq!(std::fs::read_to_string(outside.path().join("keep")).unwrap(), "preserved");
}

#[test]
fn workspace_execution_os_cache_lock_child() {
    let Some(root) = std::env::var_os("WORKOS_TEST_LOCK_ROOT") else { return; };
    assert!(matches!(super::managed_paths::cache_lock(std::path::Path::new(&root), std::path::Path::new("orgs/1/repositories/123")), Err(ExecutionError::Busy)));
}
#[test]
fn workspace_execution_os_cache_lock_excludes_another_process() {
    let root = tempfile::tempdir().unwrap();
    let parent = std::path::Path::new("orgs/1/repositories/123");
    let lock = super::managed_paths::cache_lock(root.path(), parent).unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "workspace_execution::provisioning_tests::workspace_execution_os_cache_lock_child", "--nocapture"])
        .env("WORKOS_TEST_LOCK_ROOT", root.path()).output().unwrap();
    assert!(child.status.success(), "{}", String::from_utf8_lossy(&child.stdout));
    drop(lock);
    assert!(super::managed_paths::cache_lock(root.path(), parent).is_ok());
}

#[tokio::test]
async fn workspace_execution_live_lease_blocks_disk_and_expired_lease_reauthorizes() {
    use sea_orm::{ActiveModelTrait, Set};
    let (db, scope, _, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    let access = Access { commit: commit.clone(), deleted_branch: AtomicBool::new(true), denied: AtomicBool::new(false) };
    let allocator = Local { root: root.path().into(), remote: remote.path().to_str().unwrap().into() };
    let row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    let mut active: workspace_chat_execution::ActiveModel = row.into();
    active.provisioning_owner_token = Set(Some("previous-worker".into()));
    active.provisioning_lease_expires_at = Set(Some(chrono::Utc::now() + chrono::Duration::hours(1)));
    let row = active.update(&db.conn).await.unwrap();
    let live = serde_json::to_value(get_runtime(&db.conn, &scope, draft.id).await.unwrap()).unwrap();
    assert_eq!(live["reserved_agent_type"], "claude_code");
    assert_eq!(live["provisioning_retryable"], false);
    assert_eq!(provisioning::provision(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &access, &allocator, true).await.unwrap_err(), ExecutionError::Busy);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    let mut active: workspace_chat_execution::ActiveModel = row.into();
    active.provisioning_lease_expires_at = Set(Some(chrono::Utc::now() - chrono::Duration::seconds(1)));
    active.update(&db.conn).await.unwrap();
    let interrupted = serde_json::to_value(get_runtime(&db.conn, &scope, draft.id).await.unwrap()).unwrap();
    assert_eq!(interrupted["provisioning_retryable"], true);
    access.denied.store(true, Ordering::SeqCst);
    assert_eq!(provisioning::provision(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &access, &allocator, true).await.unwrap_err(), ExecutionError::ReauthRequired);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    access.denied.store(false, Ordering::SeqCst);
    let ready = provisioning::provision(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &access, &allocator, true).await.unwrap();
    assert_eq!(ready.starting_commit.as_deref(), Some(commit.as_str()));
    let row = workspace_chat_execution::Entity::find_by_id(draft.id).one(&db.conn).await.unwrap().unwrap();
    assert!(row.provisioning_owner_token.is_none() && row.provisioning_lease_expires_at.is_none());
}

#[tokio::test]
async fn workspace_execution_lease_renewal_requires_current_live_owner() {
    use sea_orm::{ActiveModelTrait, Set};
    let (db, scope, _, draft, _remote, commit) = tests::fixture().await;
    let row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    let mut active: workspace_chat_execution::ActiveModel = row.into();
    active.provisioning_owner_token = Set(Some("owner".into()));
    active.provisioning_lease_expires_at = Set(Some(chrono::Utc::now() + chrono::Duration::seconds(5)));
    active.update(&db.conn).await.unwrap();
    for owner in ["", "wrong"] {
        assert_eq!(provisioning::renew_lease(&db.conn, &scope, draft.id, owner).await.unwrap_err(), ExecutionError::Busy);
    }
    provisioning::renew_lease(&db.conn, &scope, draft.id, "owner").await.unwrap();
    let row = workspace_chat_execution::Entity::find_by_id(draft.id).one(&db.conn).await.unwrap().unwrap();
    assert!(row.provisioning_lease_expires_at.unwrap() > chrono::Utc::now() + chrono::Duration::minutes(4));
    let mut active: workspace_chat_execution::ActiveModel = row.into();
    active.provisioning_lease_expires_at = Set(Some(chrono::Utc::now() - chrono::Duration::seconds(1)));
    active.update(&db.conn).await.unwrap();
    assert_eq!(provisioning::renew_lease(&db.conn, &scope, draft.id, "owner").await.unwrap_err(), ExecutionError::Busy);
}

#[tokio::test]
async fn workspace_execution_disabled_provision_has_no_reservation_or_disk_effects() {
    let (db, scope, _, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    let access = Access { commit, deleted_branch: AtomicBool::new(false), denied: AtomicBool::new(true) };
    let allocator = Local { root: root.path().into(), remote: remote.path().to_str().unwrap().into() };
    assert_eq!(provisioning::provision(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &access, &allocator, false).await.unwrap_err(), ExecutionError::Disabled);
    assert_eq!(workspace_chat_execution::Entity::find_by_id(draft.id).one(&db.conn).await.unwrap(), None);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn workspace_execution_git_rejects_ambient_home_credentials_and_helper_collisions() {
    let (db, scope, _, draft, remote, commit) = tests::fixture().await;
    let row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    let repo = VerifiedExecutionRepository { github_repository_id: "123".into(), verified_names: vec!["acme/docs".into()], full_name: "acme/docs".into(), starting_commit: commit };
    for filename in [".netrc", "git-askpass.sh"] {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("orgs/1/repositories/123/git-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(filename), "unrelated file").unwrap();
        assert_eq!(allocate_local(root.path(), &row, &repo, remote.path().to_str().unwrap()).unwrap_err(), ExecutionError::AllocationMismatch);
        assert_eq!(std::fs::read_to_string(home.join(filename)).unwrap(), "unrelated file");
    }
}
#[tokio::test]
async fn workspace_execution_git_allocates_isolated_chats_and_reconciles_without_overwriting() {
    let (db, scope, _, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    let row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    let repo = VerifiedExecutionRepository { github_repository_id: "123".into(), verified_names: vec!["acme/docs".into()], full_name: "acme/docs".into(), starting_commit: commit.clone() };
    let first = allocate_local(root.path(), &row, &repo, remote.path().to_str().unwrap()).unwrap();
    std::fs::write(first.worktree_path.join("plan.md"), "first chat").unwrap();
    let retry = allocate_local(root.path(), &row, &repo, remote.path().to_str().unwrap()).unwrap();
    assert_eq!(retry.allocation_id, first.allocation_id);
    let second_row = reserve(&db.conn, &scope, draft.id - 1, AgentType::ClaudeCode, &commit).await.unwrap();
    let second = allocate_local(root.path(), &second_row, &repo, remote.path().to_str().unwrap()).unwrap();
    assert_ne!(first.worktree_path, second.worktree_path);
    assert_ne!(first.branch_name, second.branch_name);
    assert!(!second.worktree_path.join("plan.md").exists());
    git(&first.worktree_path, &["checkout", "--detach"]);
    assert_eq!(allocate_local(root.path(), &row, &repo, remote.path().to_str().unwrap()).unwrap_err(), ExecutionError::AllocationMismatch);
    assert_eq!(std::fs::read_to_string(first.worktree_path.join("plan.md")).unwrap(), "first chat");
}
#[tokio::test]
async fn workspace_execution_git_collision_and_symlink_preserve_files() {
    let (db, scope, _, draft, remote, commit) = tests::fixture().await;
    let row = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
    let repo = VerifiedExecutionRepository { github_repository_id: "123".into(), verified_names: vec!["acme/docs".into()], full_name: "acme/docs".into(), starting_commit: commit };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(&row.worktree_relpath);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("keep"), "untouched").unwrap();
    assert_eq!(allocate_local(root.path(), &row, &repo, remote.path().to_str().unwrap()).unwrap_err(), ExecutionError::AllocationMismatch);
    assert_eq!(std::fs::read_to_string(path.join("keep")).unwrap(), "untouched");
    #[cfg(unix)] {
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(root.path(), other.path().join("orgs")).unwrap();
        assert_eq!(allocate_local(other.path(), &row, &repo, remote.path().to_str().unwrap()).unwrap_err(), ExecutionError::AllocationMismatch);
    }
}
#[tokio::test]
async fn workspace_execution_provision_recovers_git_after_db_failure_and_deleted_branch() {
    let (db, scope, _, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    let access = Access { commit: commit.clone(), deleted_branch: AtomicBool::new(false), denied: AtomicBool::new(false) };
    let allocator = Local { root: root.path().into(), remote: remote.path().to_str().unwrap().into() };
    db.conn.execute_unprepared("CREATE TRIGGER fail_ready BEFORE UPDATE OF state ON workspace_chat_execution WHEN NEW.state = 'ready' BEGIN SELECT RAISE(ABORT, 'fixture'); END").await.unwrap();
    assert_eq!(provisioning::provision(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &access, &allocator, true).await.unwrap_err(), ExecutionError::Database);
    let row = workspace_chat_execution::Entity::find_by_id(draft.id).one(&db.conn).await.unwrap().unwrap();
    let path = root.path().join(&row.worktree_relpath);
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), commit);
    std::fs::write(path.join("recovered"), "preserved").unwrap();
    access.deleted_branch.store(true, Ordering::SeqCst);
    db.conn.execute_unprepared("DROP TRIGGER fail_ready").await.unwrap();
    let ready = provisioning::provision(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &access, &allocator, true).await.unwrap();
    assert_eq!(ready.state, WorkspaceExecutionState::Ready);
    assert_eq!(std::fs::read_to_string(path.join("recovered")).unwrap(), "preserved");
    git(&path, &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "--allow-empty", "-qm", "later"]);
    let retry = provisioning::provision(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &access, &allocator, true).await.unwrap();
    assert_eq!(retry.conversation.unwrap().id, ready.conversation.unwrap().id);
    assert_eq!(provisioning::provision(&db.conn, &scope, draft.id, AgentType::Codex, &access, &allocator, true).await.unwrap_err(), ExecutionError::Conflict);
    access.denied.store(true, Ordering::SeqCst);
    assert_eq!(provisioning::provision(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &access, &allocator, true).await.unwrap_err(), ExecutionError::ReauthRequired);
}

#[tokio::test]
async fn workspace_execution_http_scope_strict_json_origin_and_inverse_mapping() {
    use crate::web::handlers::{workspace_execution::{WorkspaceExecutionHandler, ExecutionAgentAccess, Operation}, workspace_catalog::BrowserMutationPolicy};
    use axum::{body::to_bytes, http::{HeaderMap, HeaderValue, StatusCode}};
    use sea_orm::{ActiveModelTrait, Set};
    struct Agents;
    #[async_trait] impl ExecutionAgentAccess for Agents {
        async fn validate(&self, _: AgentType) -> Result<(), ExecutionError> { Ok(()) }
    }
    struct DisabledAgents;
    #[async_trait] impl ExecutionAgentAccess for DisabledAgents {
        async fn validate(&self, _: AgentType) -> Result<(), ExecutionError> { Err(ExecutionError::Disabled) }
    }
    let (db, scope, other, draft, remote, commit) = tests::fixture().await;
    for (id, user_id) in [("execution-user", scope.user_id), ("execution-other", other.user_id)] {
        crate::db::entities::session::ActiveModel { id: Set(id.into()), user_id: Set(user_id), created_at: Set(chrono::Utc::now()), expires_at: Set(chrono::Utc::now() + chrono::Duration::hours(1)) }.insert(&db.conn).await.unwrap();
    }
    let root = tempfile::tempdir().unwrap();
    let access = Access { commit, deleted_branch: AtomicBool::new(false), denied: AtomicBool::new(false) };
    let allocator = Local { root: root.path().into(), remote: remote.path().to_str().unwrap().into() };
    let policy = BrowserMutationPolicy::from_app_origin(Some("http://localhost:3080"));
    let mut handler = WorkspaceExecutionHandler { db: &db.conn, access: &access, allocator: &allocator, agents: &Agents, policy: &policy, enabled: true };
    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    let get = serde_json::to_vec(&serde_json::json!({"draftId":draft.id})).unwrap();
    assert_eq!(handler.handle(Operation::Get, &headers, &get).await.status(), StatusCode::UNAUTHORIZED);
    headers.insert("cookie", HeaderValue::from_static("workos_session=execution-other"));
    assert_eq!(handler.handle(Operation::Get, &headers, &get).await.status(), StatusCode::NOT_FOUND);
    headers.insert("cookie", HeaderValue::from_static("workos_session=execution-user"));
    assert_eq!(handler.handle(Operation::Get, &headers, &get).await.status(), StatusCode::OK);
    let bad = serde_json::to_vec(&serde_json::json!({"draftId":draft.id,"orgId":scope.org_id})).unwrap();
    assert_eq!(handler.handle(Operation::Get, &headers, &bad).await.status(), StatusCode::BAD_REQUEST);
    assert_eq!(handler.handle(Operation::Provision, &headers, b"{\"draftId\":2,\"agentType\":\"made-up\"}").await.status(), StatusCode::BAD_REQUEST);
    let input = serde_json::to_vec(&serde_json::json!({"draftId":draft.id,"agentType":"claude_code"})).unwrap();
    headers.insert("origin", HeaderValue::from_static("https://untrusted.invalid"));
    assert_eq!(handler.handle(Operation::Provision, &headers, &input).await.status(), StatusCode::BAD_REQUEST);
    assert_eq!(workspace_chat_execution::Entity::find_by_id(draft.id).one(&db.conn).await.unwrap(), None);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    headers.insert("origin", HeaderValue::from_static("http://localhost:3080"));
    handler.agents = &DisabledAgents;
    assert_eq!(handler.handle(Operation::Provision, &headers, &input).await.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(workspace_chat_execution::Entity::find_by_id(draft.id).one(&db.conn).await.unwrap(), None);
    handler.agents = &Agents;
    let response = handler.handle(Operation::Provision, &headers, &input).await;
    assert_eq!(response.status(), StatusCode::OK);
    let ready: serde_json::Value = serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let inverse = serde_json::to_vec(&serde_json::json!({"conversationId":ready["conversation"]["id"]})).unwrap();
    let response = handler.handle(Operation::Inverse, &headers, &inverse).await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(value["draft"]["id"], draft.id);
    handler.agents = &DisabledAgents;
    let changed_agent = serde_json::to_vec(&serde_json::json!({"draftId":draft.id,"agentType":"codex"})).unwrap();
    assert_eq!(handler.handle(Operation::Provision, &headers, &changed_agent).await.status(), StatusCode::CONFLICT);
    handler.agents = &Agents;
    headers.insert("cookie", HeaderValue::from_static("workos_session=execution-other"));
    assert_eq!(handler.handle(Operation::Inverse, &headers, &inverse).await.status(), StatusCode::NOT_FOUND);
    headers.insert("cookie", HeaderValue::from_static("workos_session=execution-user"));
    handler.enabled = false;
    assert_eq!(handler.handle(Operation::Provision, &headers, &input).await.status(), StatusCode::SERVICE_UNAVAILABLE);
    let response = handler.handle(Operation::Capabilities, &headers, b"{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let caps: serde_json::Value = serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(caps, serde_json::json!({"execution_enabled":false}));
}

#[tokio::test]
async fn workspace_execution_registered_routes_require_installation_token() {
    use std::sync::Arc;
    let (db, scope, _, draft, _remote, _commit) = tests::fixture().await;
    use sea_orm::{ActiveModelTrait, Set};
    crate::db::entities::session::ActiveModel { id: Set("router-execution".into()), user_id: Set(scope.user_id), created_at: Set(chrono::Utc::now()), expires_at: Set(chrono::Utc::now() + chrono::Duration::hours(1)) }.insert(&db.conn).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(crate::app_state::AppState::new_for_test(db, dir.path().into()));
    let router = crate::web::router::build_router(state, "execution-installation-token".into(), dir.path().into(), Arc::new(crate::web::shutdown::ShutdownSignal::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
    let client = reqwest::Client::new();
    for route in ["get_workspace_chat_runtime", "get_workspace_chat_runtime_for_conversation", "get_workspace_runtime_capabilities", "provision_workspace_chat"] {
        let response = client.post(format!("{origin}/api/{route}")).header("cookie", "workos_session=router-execution").json(&serde_json::json!({})).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED, "{route}");
    }
    let response = client.post(format!("{origin}/api/get_workspace_chat_runtime")).bearer_auth("execution-installation-token").header("cookie", "workos_session=router-execution").json(&serde_json::json!({"draftId": draft.id})).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.json::<serde_json::Value>().await.unwrap()["state"], "unprovisioned");
    task.abort();
}
