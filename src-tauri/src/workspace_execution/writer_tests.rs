use super::{*, writer::*};
use async_trait::async_trait;
use std::sync::{Mutex, atomic::{AtomicBool, Ordering}};
use crate::{models::AgentType, workspace_catalog::Scope, db::entities::workspace_chat_execution};
use sea_orm::{ActiveModelTrait, EntityTrait, Set};

struct Access { denied: AtomicBool }
#[async_trait]
impl ExecutionRepositoryAccess for Access {
    async fn verify_access(&self, _: &Scope, _: i32) -> Result<VerifiedExecutionIdentity, ExecutionError> {
        if self.denied.load(Ordering::SeqCst) { return Err(ExecutionError::ReauthRequired); }
        Ok(VerifiedExecutionIdentity { github_repository_id: "123".into(), full_name: "acme/docs".into() })
    }
    async fn resolve(&self, _: &Scope, _: i32, _: &str) -> Result<VerifiedExecutionRepository, ExecutionError> {
        // Existing ready writers must not require the original branch to exist.
        Err(ExecutionError::RepositoryUnavailable)
    }
}
#[derive(Default)]
struct Runner<'a> {
    spawned: Mutex<Vec<(i32, i32, std::path::PathBuf)>>, stopped: AtomicBool,
    manager: Option<&'a crate::acp::manager::ConnectionManager>,
    receivers: Mutex<Vec<tokio::sync::mpsc::Receiver<crate::acp::connection::ConnectionCommand>>>,
    known_stopped: Mutex<std::collections::HashSet<String>>,
    fail_spawn: AtomicBool,
}
#[async_trait]
impl WorkspaceRunner for Runner<'_> {
    async fn spawn(&self, c: &ReadyExecutionContext) -> Result<String, ExecutionError> {
        if self.fail_spawn.load(Ordering::SeqCst) { return Err(ExecutionError::Failed); }
        self.spawned.lock().unwrap().push((c.draft_id, c.conversation_id, c.working_dir.clone()));
        let id = uuid::Uuid::new_v4().to_string();
        if let Some(manager) = self.manager {
            let receiver = manager.insert_test_connection_live(&id, c.agent_type, Some(c.working_dir.clone()), crate::web::event_bridge::EventEmitter::Noop).await;
            manager.get_state(&id).await.unwrap().write().await.selectors_ready = true;
            self.receivers.lock().unwrap().push(receiver);
        }
        Ok(id)
    }
    async fn disconnect(&self, _: &str) -> Result<(), ExecutionError> {
        if self.stopped.load(Ordering::SeqCst) { Ok(()) } else { Err(ExecutionError::Busy) }
    }
    async fn stopped(&self, id: &str) -> bool { self.known_stopped.lock().unwrap().contains(id) }
}
fn git(path: &std::path::Path, args: &[&str]) -> String {
    let result = std::process::Command::new("git").current_dir(path).args(args).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    String::from_utf8(result.stdout).unwrap().trim().into()
}
async fn ready(db: &crate::db::AppDatabase, scope: &Scope, draft: i32, root: &std::path::Path, remote: &std::path::Path, commit: &str) -> WorkspaceChatRuntime {
    let row = reserve(&db.conn, scope, draft, AgentType::ClaudeCode, commit).await.unwrap();
    let repo = VerifiedExecutionRepository { github_repository_id: "123".into(), full_name: "acme/docs".into(), starting_commit: commit.into() };
    let allocated = git_allocator::allocate_local(root, &row, &repo, remote.to_str().unwrap()).unwrap();
    // Provision using a disposable local remote, then record the production
    // identity which read-only ready validation must enforce.
    git(&root.join("orgs/1/repositories/123/repo.git"), &["remote", "set-url", "origin", "https://github.com/acme/docs.git"]);
    let mut active: workspace_chat_execution::ActiveModel = row.clone().into();
    active.provisioning_owner_token = Set(Some("owner".into()));
    active.provisioning_lease_expires_at = Set(Some(chrono::Utc::now() + chrono::Duration::minutes(1)));
    active.update(&db.conn).await.unwrap();
    legacy_bridge::finalize_ready(&db.conn, scope, draft, &row.allocation_id, allocated.worktree_path.to_str().unwrap(), "owner").await.unwrap()
}

#[tokio::test]
async fn workspace_execution_writer_requires_ready_owner_and_exclusive_persisted_lease() {
    let (db, scope, other, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    let runner = Runner::default();
    let access = Access { denied: AtomicBool::new(false) };
    assert_eq!(connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await.unwrap_err(), ExecutionError::Conflict);
    let runtime = ready(&db, &scope, draft.id, root.path(), remote.path(), &commit).await;
    let mut peer: crate::db::entities::user::ActiveModel = crate::db::entities::user::Entity::find_by_id(scope.user_id).one(&db.conn).await.unwrap().unwrap().into();
    peer.id = Set(3); peer.github_user_id = Set(1003); peer.login = Set("peer".into()); peer.insert(&db.conn).await.unwrap();
    assert_eq!(connect_workspace_with_access(&db.conn, &Scope { org_id: scope.org_id, user_id: 3 }, draft.id, &runner, &access, root.path()).await.unwrap_err(), ExecutionError::NotFound);
    assert_eq!(connect_workspace_with_access(&db.conn, &other, draft.id, &runner, &access, root.path()).await.unwrap_err(), ExecutionError::NotFound);
    assert!(runner.spawned.lock().unwrap().is_empty());
    assert!(connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await.is_ok());
    assert_eq!(connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await.unwrap_err(), ExecutionError::Busy);
    let calls = runner.spawned.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, runtime.conversation.unwrap().id);
    assert_ne!(calls[0].0, calls[0].1);
}

#[tokio::test]
async fn workspace_execution_writer_shutdown_and_revocation_preserve_ownership() {
    let (db, scope, other, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    ready(&db, &scope, draft.id, root.path(), remote.path(), &commit).await;
    let runner = Runner::default();
    let access = Access { denied: AtomicBool::new(true) };
    assert_eq!(connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await.unwrap_err(), ExecutionError::ReauthRequired);
    assert!(runner.spawned.lock().unwrap().is_empty());
    access.denied.store(false, Ordering::SeqCst);
    let id = connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await.unwrap();
    assert!(authorize_prompt(&db.conn, &scope, draft.id, &id, &access).await.is_ok());
    access.denied.store(true, Ordering::SeqCst);
    assert_eq!(authorize_prompt(&db.conn, &scope, draft.id, &id, &access).await.unwrap_err(), ExecutionError::ReauthRequired);
    access.denied.store(false, Ordering::SeqCst);
    use sea_orm::ConnectionTrait;
    db.conn.execute_unprepared("UPDATE workspace_writer_lease SET expires_at = '2000-01-01 00:00:00+00:00'").await.unwrap();
    assert_eq!(authorize_prompt(&db.conn, &scope, draft.id, &id, &access).await.unwrap_err(), ExecutionError::Busy);
    assert_eq!(connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await.unwrap_err(), ExecutionError::Busy);
    assert_eq!(disconnect_workspace(&db.conn, &other, &id, &runner).await.unwrap_err(), ExecutionError::NotFound);
    assert_eq!(disconnect_workspace(&db.conn, &scope, &id, &runner).await.unwrap_err(), ExecutionError::Busy);
    assert_eq!(connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await.unwrap_err(), ExecutionError::Busy);
    runner.stopped.store(true, Ordering::SeqCst);
    disconnect_workspace(&db.conn, &scope, &id, &runner).await.unwrap();
    let successor = connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await.unwrap();
    assert_eq!(disconnect_workspace(&db.conn, &scope, &id, &runner).await.unwrap_err(), ExecutionError::NotFound);
    assert!(authorize_prompt(&db.conn, &scope, draft.id, &successor, &access).await.is_ok());
}

#[tokio::test]
async fn workspace_execution_writer_reclaims_only_positively_known_stopped_owner() {
    let (db, scope, _, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    ready(&db, &scope, draft.id, root.path(), remote.path(), &commit).await;
    let runner = Runner::default();
    let access = Access { denied: AtomicBool::new(false) };
    let id = connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await.unwrap();
    assert_eq!(connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await.unwrap_err(), ExecutionError::Busy);
    runner.known_stopped.lock().unwrap().insert(id);
    assert!(connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await.is_ok());
    assert_eq!(runner.spawned.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn workspace_execution_failed_spawn_releases_and_distinct_chats_have_independent_writers() {
    let (db, scope, _, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    ready(&db, &scope, draft.id, root.path(), remote.path(), &commit).await;
    git(&root.path().join("orgs/1/repositories/123/repo.git"), &["remote", "set-url", "origin", remote.path().to_str().unwrap()]);
    ready(&db, &scope, draft.id - 1, root.path(), remote.path(), &commit).await;
    let runner = Runner::default();
    let access = Access { denied: AtomicBool::new(false) };
    runner.fail_spawn.store(true, Ordering::SeqCst);
    assert_eq!(connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await.unwrap_err(), ExecutionError::Failed);
    assert!(crate::db::entities::workspace_writer_lease::Entity::find_by_id(draft.id).one(&db.conn).await.unwrap().is_none());
    runner.fail_spawn.store(false, Ordering::SeqCst);
    let id = connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await.unwrap();
    assert!(connect_workspace_with_access(&db.conn, &scope, draft.id - 1, &runner, &access, root.path()).await.is_ok());
    touch_workspace(&db.conn, &scope, &id).await.unwrap();
    let row = crate::db::entities::workspace_writer_lease::Entity::find_by_id(draft.id).one(&db.conn).await.unwrap().unwrap();
    assert!(row.expires_at > chrono::Utc::now() + chrono::Duration::minutes(4));
    let calls = runner.spawned.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0].2, calls[1].2);
}

#[tokio::test]
async fn workspace_execution_writer_validates_ready_checkout_without_pinning_later_head() {
    for damage in ["detached", "switched", "cache", "symlink", "later_commit"] {
        let (db, scope, _, draft, remote, commit) = tests::fixture().await;
        let root = tempfile::tempdir().unwrap();
        let runtime = ready(&db, &scope, draft.id, root.path(), remote.path(), &commit).await;
        let path = std::path::PathBuf::from(runtime.folder.unwrap().path);
        match damage {
            "detached" => { git(&path, &["checkout", "--detach"]); },
            "switched" => { git(&path, &["checkout", "-b", "unrelated"]); },
            "cache" => { git(&root.path().join("orgs/1/repositories/123/repo.git"), &["remote", "set-url", "origin", "https://github.com/other/repo.git"]); },
            "symlink" => {
                std::fs::rename(&path, path.with_file_name("original")).unwrap();
                #[cfg(unix)] std::os::unix::fs::symlink(remote.path(), &path).unwrap();
            },
            _ => { git(&path, &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "--allow-empty", "-qm", "later"]); },
        }
        let runner = Runner::default();
        let access = Access { denied: AtomicBool::new(false) };
        let result = connect_workspace_with_access(&db.conn, &scope, draft.id, &runner, &access, root.path()).await;
        if damage == "later_commit" {
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(runner.spawned.lock().unwrap().len(), 1);
        } else {
            assert_eq!(result.unwrap_err(), ExecutionError::AllocationMismatch, "{damage}");
            assert!(runner.spawned.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn workspace_execution_generic_cwd_and_conversation_cannot_bypass_managed_entry() {
    let (db, scope, _, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    let runtime = ready(&db, &scope, draft.id, root.path(), remote.path(), &commit).await;
    let path = runtime.folder.unwrap().path;
    for cwd in [&path, &format!("{path}/nested")] {
        assert_eq!(acp_guard::reject_generic(&db.conn, Some(cwd), None, None).await.unwrap_err(), ExecutionError::Conflict);
        let manager = crate::acp::manager::ConnectionManager::new();
        let error = crate::commands::acp::acp_describe_agent_options_core(
            &manager, &db, root.path(), AgentType::ClaudeCode, Some(cwd.to_string()),
        ).await.unwrap_err();
        assert!(error.to_string().contains("conflict"), "{error}");
        assert!(manager.connections.lock().await.is_empty(), "probe must not spawn in a managed checkout");
    }
    assert_eq!(acp_guard::reject_generic(&db.conn, None, Some(runtime.conversation.unwrap().id), None).await.unwrap_err(), ExecutionError::Conflict);
    assert!(acp_guard::reject_generic(&db.conn, Some(&format!("{path}-sibling")), None, None).await.is_ok());
    #[cfg(unix)] {
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert_eq!(acp_guard::reject_generic(&db.conn, alias.join("nested").to_str(), None, None).await.unwrap_err(), ExecutionError::Conflict);
    }
}

#[test]
fn workspace_execution_generic_cannot_enter_reserved_root_before_ready_mapping() {
    let root = tempfile::tempdir().unwrap();
    let pending = root.path().join("orgs/1/workspace-chats/999/worktree");
    assert_eq!(acp_guard::reject_managed_root(root.path(), pending.to_str()).unwrap_err(), ExecutionError::Conflict);
    assert!(acp_guard::reject_managed_root(root.path(), root.path().join("orgs/1/workspace-chats-sibling").to_str()).is_ok());
}

#[tokio::test]
async fn workspace_execution_queued_disconnect_is_not_process_stop_confirmation() {
    let manager = crate::acp::manager::ConnectionManager::new();
    let _receiver = manager.insert_test_connection_live("owned", AgentType::ClaudeCode, None, crate::web::event_bridge::EventEmitter::Noop).await;
    let reaped = manager.connections.lock().await.get("owned").unwrap().child_reaped.clone();
    assert!(manager.disconnect_confirmed("owned", std::time::Duration::from_millis(10)).await.is_err());
    reaped.store(true, Ordering::SeqCst);
    assert!(manager.disconnect_confirmed("owned", std::time::Duration::from_millis(10)).await.is_ok());
}

#[tokio::test]
async fn workspace_execution_generic_cleanup_retains_managed_stop_proof() {
    let manager = crate::acp::manager::ConnectionManager::new();
    let _receiver = manager.insert_test_connection_live("owned", AgentType::ClaudeCode, None, crate::web::event_bridge::EventEmitter::Noop).await;
    let reaped = manager.connections.lock().await.get("owned").unwrap().child_reaped.clone();
    manager.observe_workspace_process("owned", reaped.clone()).await;
    // The actual idle-sweep teardown path removes the manager entry first.
    manager.disconnect("owned").await.unwrap();
    assert!(manager.disconnect_confirmed("owned", std::time::Duration::from_millis(10)).await.is_err());
    reaped.store(true, Ordering::SeqCst);
    assert!(manager.disconnect_confirmed("owned", std::time::Duration::from_millis(10)).await.is_ok());
}

#[tokio::test]
async fn workspace_execution_acp_http_requires_cookie_origin_and_canonical_ids() {
    use crate::web::handlers::{workspace_acp::{WorkspaceAcpHandler, Operation}, workspace_catalog::BrowserMutationPolicy};
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    let (db, scope, other, draft, remote, commit) = tests::fixture().await;
    let root = tempfile::tempdir().unwrap();
    ready(&db, &scope, draft.id, root.path(), remote.path(), &commit).await;
    for (id, user) in [("writer-user", scope.user_id), ("writer-other", other.user_id)] {
        crate::db::entities::session::ActiveModel { id: Set(id.into()), user_id: Set(user), created_at: Set(chrono::Utc::now()), expires_at: Set(chrono::Utc::now() + chrono::Duration::hours(1)) }.insert(&db.conn).await.unwrap();
    }
    let state = crate::app_state::AppState::new_for_test(db, root.path().into());
    let runner = Runner { manager: Some(&state.connection_manager), ..Default::default() };
    let access = Access { denied: AtomicBool::new(false) };
    let policy = BrowserMutationPolicy::from_app_origin(Some("http://localhost:3080"));
    let handler = WorkspaceAcpHandler { state: &state, runner: &runner, access: &access, policy: &policy, enabled: true };
    let body = serde_json::to_vec(&serde_json::json!({"workspaceChatId":draft.id,"agentType":"claude_code"})).unwrap();
    let mut headers = HeaderMap::new(); headers.insert("content-type", HeaderValue::from_static("application/json"));
    assert_eq!(handler.handle(Operation::Connect, &headers, &body).await.status(), StatusCode::UNAUTHORIZED);
    headers.insert("cookie", HeaderValue::from_static("workos_session=writer-other"));
    assert_eq!(handler.handle(Operation::Connect, &headers, &body).await.status(), StatusCode::NOT_FOUND);
    headers.insert("cookie", HeaderValue::from_static("workos_session=writer-user"));
    headers.insert("origin", HeaderValue::from_static("https://evil.invalid"));
    assert_eq!(handler.handle(Operation::Connect, &headers, &body).await.status(), StatusCode::BAD_REQUEST);
    headers.insert("origin", HeaderValue::from_static("http://localhost:3080"));
    let mismatch = serde_json::to_vec(&serde_json::json!({"workspaceChatId":draft.id,"agentType":"claude_code","workingDir":"/unrelated"})).unwrap();
    assert_eq!(handler.handle(Operation::Connect, &headers, &mismatch).await.status(), StatusCode::CONFLICT);
    assert!(runner.spawned.lock().unwrap().is_empty());
    assert_eq!(handler.handle(Operation::Connect, &headers, &body).await.status(), StatusCode::OK);
    assert_eq!(handler.handle(Operation::Connect, &headers, &body).await.status(), StatusCode::CONFLICT);
    assert_eq!(runner.spawned.lock().unwrap().len(), 1);
    let runtime = get_runtime(&state.db.conn, &scope, draft.id).await.unwrap();
    let conversation = runtime.conversation.unwrap().id;
    let folder = runtime.folder.unwrap().id;
    let discover = serde_json::to_vec(&serde_json::json!({"workspaceChatId":draft.id,"conversationId":conversation,"agentType":"claude_code"})).unwrap();
    let response = handler.handle(Operation::Discover, &headers, &discover).await;
    assert_eq!(response.status(), StatusCode::OK);
    let discovered: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let id = discovered["connection_id"].as_str().unwrap();
    let mismatched_owner = serde_json::to_vec(&serde_json::json!({"workspaceChatId":draft.id + 1,"connectionId":id})).unwrap();
    assert_eq!(handler.handle(Operation::Touch, &headers, &mismatched_owner).await.status(), StatusCode::CONFLICT);
    assert_eq!(handler.handle(Operation::Disconnect, &headers, &mismatched_owner).await.status(), StatusCode::CONFLICT);
    use sea_orm::ConnectionTrait;
    state.db.conn.execute_unprepared("UPDATE workspace_writer_lease SET expires_at = '2000-01-01 00:00:00+00:00'").await.unwrap();
    let expired = connection_lease(&state.db.conn, &scope, id, false).await.unwrap();
    assert_eq!(handler.handle(Operation::Discover, &headers, &discover).await.status(), StatusCode::OK);
    let heartbeat = serde_json::to_vec(&serde_json::json!({"connectionId":id})).unwrap();
    assert_eq!(handler.handle(Operation::Touch, &headers, &heartbeat).await.status(), StatusCode::CONFLICT);
    assert_eq!(connection_lease(&state.db.conn, &scope, id, false).await.unwrap().expires_at, expired.expires_at);
    assert_eq!(handler.handle(Operation::Connect, &headers, &body).await.status(), StatusCode::CONFLICT);
    assert_eq!(runner.spawned.lock().unwrap().len(), 1);
    let prompt = serde_json::to_vec(&serde_json::json!({"workspaceChatId":draft.id,"connectionId":id,"folderId":folder,"conversationId":conversation,"blocks":[{"type":"text","text":"real admission"}],"clientMessageId":"owned-message"})).unwrap();
    access.denied.store(true, Ordering::SeqCst);
    assert_eq!(handler.handle(Operation::Prompt, &headers, &prompt).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(connection_lease(&state.db.conn, &scope, id, false).await.unwrap().expires_at, expired.expires_at);
    assert!(runner.receivers.lock().unwrap()[0].try_recv().is_err());
    access.denied.store(false, Ordering::SeqCst);
    let session = state.connection_manager.get_state(id).await.unwrap();
    session.write().await.selectors_ready = false;
    {
    let pending_prompt = handler.handle(Operation::Prompt, &headers, &prompt);
    tokio::pin!(pending_prompt);
    tokio::select! {
        response = &mut pending_prompt => panic!("prompt admitted before initialization: {}", response.status()),
        _ = tokio::time::sleep(std::time::Duration::from_millis(30)) => {},
    }
    assert!(runner.receivers.lock().unwrap()[0].try_recv().is_err());
    access.denied.store(true, Ordering::SeqCst);
    session.write().await.selectors_ready = true;
    assert_eq!(pending_prompt.await.status(), StatusCode::UNAUTHORIZED);
    }
    assert!(runner.receivers.lock().unwrap()[0].try_recv().is_err());
    access.denied.store(false, Ordering::SeqCst);
    assert_eq!(handler.handle(Operation::Prompt, &headers, &prompt).await.status(), StatusCode::OK);
    let renewed = connection_lease(&state.db.conn, &scope, id, true).await.unwrap();
    assert_eq!(renewed.owner_token, expired.owner_token);
    assert_eq!(renewed.connection_id, expired.connection_id);
    assert!(renewed.expires_at > chrono::Utc::now());
    assert_eq!(runner.spawned.lock().unwrap().len(), 1);
    assert!(matches!(runner.receivers.lock().unwrap()[0].try_recv().unwrap(), crate::acp::connection::ConnectionCommand::Prompt { .. }));
    headers.insert("cookie", HeaderValue::from_static("workos_session=writer-other"));
    assert_eq!(handler.handle(Operation::Discover, &headers, &discover).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(runner.spawned.lock().unwrap().len(), 1);
    headers.insert("cookie", HeaderValue::from_static("workos_session=writer-user"));
    state.connection_manager.disconnect(id).await.unwrap();
    runner.known_stopped.lock().unwrap().insert(id.into());
    let response = handler.handle(Operation::Discover, &headers, &discover).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(&axum::body::to_bytes(response.into_body(), 1024).await.unwrap()[..], b"null");
    assert_eq!(runner.spawned.lock().unwrap().len(), 1);
    assert_eq!(handler.handle(Operation::Connect, &headers, &body).await.status(), StatusCode::OK);
    assert_eq!(runner.spawned.lock().unwrap().len(), 2);
}
