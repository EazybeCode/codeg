use std::{path::{Path, PathBuf}, sync::{Arc, Mutex}, collections::HashSet, io::Write};
use async_trait::async_trait;
use sea_orm::{EntityTrait, ColumnTrait, QueryFilter};
use crate::{app_state::AppState, models::AgentType, workspace_catalog::{Scope, CatalogError, RepositoryAccess, VerifiedRepository}, db::entities::workspace_chat_execution,
    acp::{connection::ConnectionCommand, types::{AcpEvent, PromptInputBlock, ConnectionStatus}}, web::event_bridge::emit_with_state};
use super::super::{ExecutionError, repository_access::*, git_allocator::*, writer::*};

pub fn git(path: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git").current_dir(path).args(args).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

pub struct External {
    pub state: Arc<AppState>, pub root: PathBuf, pub remote: PathBuf, pub commit: String,
    pub stopped: Arc<Mutex<HashSet<String>>>, pub allocation_lock: tokio::sync::Mutex<()>,
}
impl External {
    pub fn new(state: Arc<AppState>, root: &Path) -> Self {
        let remote = root.join("remote");
        if !remote.exists() {
            std::fs::create_dir(&remote).unwrap();
            git(&remote, &["init", "-b", "main"]);
            std::fs::write(remote.join("README.md"), "# Fixture repository\n").unwrap();
            git(&remote, &["add", "README.md"]);
            git(&remote, &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-m", "initial"]);
        }
        let commit = git(&remote, &["rev-parse", "HEAD"]);
        Self { state, root: root.into(), remote, commit, stopped: Arc::default(), allocation_lock: tokio::sync::Mutex::new(()) }
    }
    fn access(&self) -> Result<(), CatalogError> {
        match std::fs::read_to_string(self.root.join("external-access")).unwrap_or_default().trim() {
            "reauth" => Err(CatalogError::ReauthRequired),
            "outage" => Err(CatalogError::UpstreamUnavailable), _ => Ok(()),
        }
    }
}
#[async_trait]
impl RepositoryAccess for External {
    async fn verify(&self, _: &Scope, name: &str, branch: &str) -> Result<VerifiedRepository, CatalogError> {
        self.access()?;
        if name != "fixture-org/docs" || branch != "main" { return Err(CatalogError::RepositoryUnavailable); }
        Ok(VerifiedRepository { github_repository_id: "910003".into(), full_name: name.into(), starting_branch: branch.into() })
    }
}
#[async_trait]
impl ExecutionRepositoryAccess for External {
    async fn verify_access(&self, scope: &Scope, repository_id: i32) -> Result<VerifiedExecutionIdentity, ExecutionError> {
        self.access()?;
        let repo = crate::db::entities::workspace_repository::Entity::find_by_id(repository_id)
            .filter(crate::db::entities::workspace_repository::Column::OrgId.eq(scope.org_id))
            .one(&self.state.db.conn).await?.ok_or(ExecutionError::NotFound)?;
        if repo.github_repository_id != "910003" { return Err(ExecutionError::RepositoryUnavailable); }
        Ok(VerifiedExecutionIdentity { github_repository_id: repo.github_repository_id, full_name: repo.full_name })
    }
    async fn resolve(&self, scope: &Scope, repository_id: i32, branch: &str) -> Result<VerifiedExecutionRepository, ExecutionError> {
        let identity = self.verify_access(scope, repository_id).await?;
        if branch != "main" { return Err(ExecutionError::RepositoryUnavailable); }
        Ok(VerifiedExecutionRepository { github_repository_id: identity.github_repository_id, full_name: identity.full_name, starting_commit: self.commit.clone() })
    }
}
#[async_trait]
impl GitAllocator for External {
    async fn allocate(&self, _: &Scope, row: &workspace_chat_execution::Model, repo: &VerifiedExecutionRepository) -> Result<AllocatedWorktree, ExecutionError> {
        let _lock = self.allocation_lock.lock().await;
        let cache = self.state.data_dir.join(format!("orgs/{}/repositories/{}/repo.git", row.org_id, repo.github_repository_id));
        if cache.exists() { git(&cache, &["remote", "set-url", "origin", self.remote.to_str().unwrap()]); }
        let result = allocate_local(&self.state.data_dir, row, repo, self.remote.to_str().unwrap());
        if cache.exists() { git(&cache, &["remote", "set-url", "origin", "https://github.com/fixture-org/docs.git"]); }
        result
    }
}
#[async_trait]
impl crate::web::handlers::workspace_execution::ExecutionAgentAccess for External {
    async fn validate(&self, agent: AgentType) -> Result<(), ExecutionError> {
        if agent == AgentType::ClaudeCode { Ok(()) } else { Err(ExecutionError::Disabled) }
    }
}
#[async_trait]
impl WorkspaceRunner for External {
    async fn spawn(&self, context: &ReadyExecutionContext) -> Result<String, ExecutionError> {
        if self.root.join("provider-fail-start").exists() { return Err(ExecutionError::Failed); }
        let id = uuid::Uuid::new_v4().to_string();
        let mut rx = self.state.connection_manager.insert_test_connection_live(&id, context.agent_type, Some(context.working_dir.clone()), self.state.emitter.clone()).await;
        let state = self.state.connection_manager.get_state(&id).await.unwrap();
        let emitter = self.state.emitter.clone();
        emit_with_state(&state, &emitter, AcpEvent::SelectorsReady).await;
        let directory = context.working_dir.clone();
        let provider_dir = self.root.join("provider/projects/fixture");
        std::fs::create_dir_all(&provider_dir).unwrap();
        let stopped = self.stopped.clone();
        let connection_id = id.clone();
        let session_id = format!("fixture-chat-{}", context.draft_id);
        tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                match command {
                    ConnectionCommand::Prompt { blocks, user_message } => {
                        let (message_id, user_blocks) = user_message.expect("real linked prompt carries client ID");
                        let prompt = blocks.iter().filter_map(|b| match b { PromptInputBlock::Text { text } => Some(text.as_str()), _ => None }).collect::<Vec<_>>().join("\n");
                        let answer = format!("# Generated Markdown\n\n{prompt}\n");
                        std::fs::write(directory.join("generated.md"), &answer).unwrap();
                        // Native provider persistence, consumed by the unchanged Claude parser.
                        let mut transcript = std::fs::OpenOptions::new().create(true).append(true).open(provider_dir.join(format!("{session_id}.jsonl"))).unwrap();
                        let now = chrono::Utc::now().to_rfc3339();
                        for row in [
                            serde_json::json!({"type":"user","uuid":message_id,"sessionId":session_id,"cwd":directory,"timestamp":now,"message":{"role":"user","content":prompt}}),
                            serde_json::json!({"type":"assistant","uuid":uuid::Uuid::new_v4().to_string(),"sessionId":session_id,"cwd":directory,"timestamp":now,"message":{"id":uuid::Uuid::new_v4().to_string(),"role":"assistant","model":"fixture-provider","content":[{"type":"text","text":answer}],"stop_reason":"end_turn"}}),
                        ] { writeln!(transcript, "{row}").unwrap(); }
                        emit_with_state(&state, &emitter, AcpEvent::StatusChanged { status: ConnectionStatus::Prompting }).await;
                        emit_with_state(&state, &emitter, AcpEvent::SessionStarted { session_id: session_id.clone() }).await;
                        emit_with_state(&state, &emitter, AcpEvent::UserMessage { message_id, blocks: user_blocks }).await;
                        emit_with_state(&state, &emitter, AcpEvent::ContentDelta { text: answer, parent_tool_use_id: None }).await;
                        emit_with_state(&state, &emitter, AcpEvent::TurnComplete { session_id: session_id.clone(), stop_reason: "end_turn".into(), agent_type: "claude_code".into() }).await;
                    },
                    ConnectionCommand::Disconnect => break,
                    _ => {},
                }
            }
            stopped.lock().unwrap().insert(connection_id);
        });
        Ok(id)
    }
    async fn disconnect(&self, id: &str) -> Result<(), ExecutionError> {
        self.state.connection_manager.disconnect(id).await.map_err(|_| ExecutionError::Failed)?;
        for _ in 0..100 {
            if self.stopped(id).await { return Ok(()); }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Err(ExecutionError::Busy)
    }
    async fn stopped(&self, id: &str) -> bool { self.stopped.lock().unwrap().contains(id) }
}
