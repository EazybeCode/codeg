use axum::{body::{Body, Bytes}, extract::Extension, http::{HeaderMap, Response}, response::IntoResponse, Json};
use serde::Deserialize;
use sea_orm::{EntityTrait, ColumnTrait, QueryFilter};
use std::sync::Arc;
use crate::{app_state::AppState, workspace_execution::{ExecutionError, ExecutionRepositoryAccess, writer::{self, WorkspaceRunner}, acp_guard}, db::entities::workspace_writer_lease};
use super::workspace_catalog::BrowserMutationPolicy;

#[derive(Clone, Copy)]
pub(crate) enum Operation { Connect, Prompt, Disconnect, Touch, Discover }
pub(crate) struct WorkspaceAcpHandler<'a> {
    pub state: &'a AppState,
    pub access: &'a dyn ExecutionRepositoryAccess,
    pub runner: &'a dyn WorkspaceRunner,
    pub policy: &'a BrowserMutationPolicy,
    pub enabled: bool,
}
impl WorkspaceAcpHandler<'_> {
    pub(crate) async fn handle(&self, operation: Operation, headers: &HeaderMap, body: &[u8]) -> Response<Body> {
        if let Err(error) = super::workspace_catalog::scope_from_headers(headers, &self.state.db.conn).await {
            return super::workspace_execution::error_response(error.into());
        }
        if !matches!(operation, Operation::Discover) {
            if let Err(error) = self.policy.validate(headers) { return super::workspace_catalog::catalog_error_response(error); }
        }
        if let Err(error) = super::workspace_catalog::parse_json::<Input>(headers, body) {
            return super::workspace_catalog::catalog_error_response(error);
        }
        match self.run(operation, headers, body).await {
            Ok(value) => Json(value).into_response(),
            Err(error) => super::workspace_execution::error_response(error),
        }
    }
    async fn run(&self, operation: Operation, headers: &HeaderMap, body: &[u8]) -> Result<serde_json::Value, ExecutionError> {
        let db = &self.state.db.conn;
        let scope = super::workspace_catalog::scope_from_headers(headers, db).await?;
        // This is the same cookie/session + browser-origin policy as catalog
        // provisioning. No organization or user identity comes from JSON.
        if !self.enabled { return Err(ExecutionError::Disabled); }
        let input: Input = super::workspace_catalog::parse_json(headers, body)?;
        let draft = input.workspace_chat_id;
        match operation {
            Operation::Connect => {
                let draft = draft.ok_or(ExecutionError::Conflict)?;
                let runtime = crate::workspace_execution::get_runtime(db, &scope, draft).await?;
                let folder = runtime.folder.ok_or(ExecutionError::Conflict)?;
                let conversation = runtime.conversation.ok_or(ExecutionError::Conflict)?;
                if input.agent_type != Some(conversation.agent_type)
                    || input.working_dir.as_deref().is_some_and(|dir| dir != folder.path)
                    || input.conversation_id.is_some_and(|id| id != conversation.id)
                    || input.folder_id.is_some_and(|id| id != folder.id)
                    || input.session_id.as_ref().is_some_and(|id| Some(id) != conversation.external_id.as_ref())
                { return Err(ExecutionError::Conflict); }
                let id = writer::connect_workspace_with_access(db, &scope, draft, self.runner, self.access, &self.state.data_dir).await?;
                Ok(serde_json::json!(id))
            },
            Operation::Prompt => {
                let id = input.connection_id.as_deref().ok_or(ExecutionError::Conflict)?;
                let lease = writer::connection_lease(db, &scope, id, false).await?;
                if Some(lease.draft_id) != draft { return Err(ExecutionError::Conflict); }
                self.wait_ready(id).await?;
                // Authorization is rechecked after initialization, immediately
                // before admission; startup can take long enough for revocation.
                let (context, owner) = writer::prepare_owned_prompt(db, &scope, draft.ok_or(ExecutionError::Conflict)?, id, self.access).await?;
                if input.folder_id != Some(context.folder_id) || input.conversation_id != Some(context.conversation_id) { return Err(ExecutionError::Conflict); }
                // Validate the live manager entry as well as the persisted lease.
                self.validate_connection(id, &context).await?;
                if self.runner.stopped(id).await { return Err(ExecutionError::Busy); }
                writer::renew_owned_prompt(db, &scope, &owner).await?;
                writer::connection_lease(db, &scope, id, true).await?;
                self.state.connection_manager.send_prompt_linked_with_message_id(&self.state.db, id,
                    input.blocks.ok_or(ExecutionError::Conflict)?, Some(context.folder_id), Some(context.conversation_id), None, input.client_message_id)
                    .await.map_err(|e| match e { crate::acp::error::AcpError::TurnInProgress => ExecutionError::Busy, _ => ExecutionError::Failed })?;
                Ok(serde_json::Value::Null)
            },
            Operation::Disconnect => {
                let id = input.connection_id.as_deref().ok_or(ExecutionError::Conflict)?;
                let lease = writer::connection_lease(db, &scope, id, false).await?;
                if draft.is_some_and(|draft| draft != lease.draft_id) { return Err(ExecutionError::Conflict); }
                writer::disconnect_workspace(db, &scope, id, self.runner).await?;
                Ok(serde_json::Value::Null)
            },
            Operation::Touch => {
                let id = input.connection_id.as_deref().ok_or(ExecutionError::Conflict)?;
                let lease = writer::connection_lease(db, &scope, id, true).await?;
                if draft.is_some_and(|draft| draft != lease.draft_id) { return Err(ExecutionError::Conflict); }
                writer::touch_workspace(db, &scope, id).await?;
                Ok(serde_json::json!(self.state.connection_manager.touch(id).await))
            },
            Operation::Discover => {
                let draft = draft.ok_or(ExecutionError::Conflict)?;
                let runtime = crate::workspace_execution::get_runtime(db, &scope, draft).await?;
                let folder = runtime.folder.ok_or(ExecutionError::Conflict)?;
                let conversation = runtime.conversation.ok_or(ExecutionError::Conflict)?;
                if input.conversation_id != Some(conversation.id) || input.agent_type != Some(conversation.agent_type) { return Err(ExecutionError::Conflict); }
                let Some(lease) = workspace_writer_lease::Entity::find_by_id(draft).one(db).await? else { return Ok(serde_json::Value::Null); };
                let id = lease.connection_id.as_deref().ok_or(ExecutionError::Busy)?;
                writer::connection_lease(db, &scope, id, false).await?;
                if self.runner.stopped(id).await { return Ok(serde_json::Value::Null); }
                let context = writer::ReadyExecutionContext { draft_id: draft, folder_id: folder.id, conversation_id: conversation.id, agent_type: conversation.agent_type, working_dir: folder.path.into() };
                let event_seq = self.validate_connection(id, &context).await?;
                Ok(serde_json::json!({"connection_id":id,"event_seq":event_seq}))
            },
        }
    }
    async fn validate_connection(&self, id: &str, context: &writer::ReadyExecutionContext) -> Result<u64, ExecutionError> {
        let state = self.state.connection_manager.get_state(id).await.ok_or(ExecutionError::Busy)?;
        let state = state.read().await;
        if state.agent_type != context.agent_type || state.working_dir.as_ref() != Some(&context.working_dir)
            || state.conversation_id.is_some_and(|id| id != context.conversation_id)
            || state.folder_id.is_some_and(|id| id != context.folder_id)
            || matches!(state.status, crate::acp::types::ConnectionStatus::Disconnected | crate::acp::types::ConnectionStatus::Error)
        { return Err(ExecutionError::Busy); }
        Ok(state.event_seq)
    }
    async fn wait_ready(&self, id: &str) -> Result<(), ExecutionError> {
        let state = self.state.connection_manager.get_state(id).await.ok_or(ExecutionError::Busy)?;
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                {
                    let state = state.read().await;
                    if matches!(state.status, crate::acp::types::ConnectionStatus::Disconnected | crate::acp::types::ConnectionStatus::Error) {
                        return Err(ExecutionError::Failed);
                    }
                    if state.selectors_ready { return Ok(()); }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.map_err(|_| ExecutionError::Busy)?
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    workspace_chat_id: Option<i32>, connection_id: Option<String>,
    agent_type: Option<crate::models::AgentType>, working_dir: Option<String>, session_id: Option<String>,
    folder_id: Option<i32>, conversation_id: Option<i32>,
    blocks: Option<Vec<crate::acp::types::PromptInputBlock>>, client_message_id: Option<String>,
    preferred_mode_id: Option<String>,
    preferred_config_values: Option<std::collections::BTreeMap<String, String>>,
}

async fn dispatch(state: Arc<AppState>, operation: Operation, headers: HeaderMap, body: Bytes) -> Response<Body> {
    let input: Input = match super::workspace_catalog::parse_json(&headers, &body) {
        Ok(input) => input, Err(error) => return super::workspace_catalog::catalog_error_response(error),
    };
    let managed_connection = if let Some(id) = input.connection_id.as_deref() {
        match workspace_writer_lease::Entity::find().filter(workspace_writer_lease::Column::ConnectionId.eq(id)).one(&state.db.conn).await {
            Ok(row) => row.is_some(), Err(_) => return super::workspace_execution::error_response(ExecutionError::Database),
        }
    } else { false };
    if input.workspace_chat_id.is_some() || managed_connection {
        let access = match crate::workspace_execution::GitHubExecutionRepositoryAccess::new(&state.db.conn) { Ok(access) => access, Err(error) => return super::workspace_execution::error_response(error) };
        let policy = BrowserMutationPolicy::from_environment();
        if !matches!(operation, Operation::Discover) {
            if let Err(error) = policy.validate(&headers) { return super::workspace_catalog::catalog_error_response(error); }
        }
        let runner = crate::workspace_execution::acp_runner::AcpWorkspaceRunner { state: &state,
            preferred_mode_id: input.preferred_mode_id, preferred_config_values: input.preferred_config_values.unwrap_or_default() };
        return WorkspaceAcpHandler { state: &state, access: &access, runner: &runner,
            policy: &policy, enabled: std::env::var("WORKOS_ENABLE_CATALOG_RUNTIME").ok().as_deref() == Some("true")
        }.handle(operation, &headers, &body).await;
    }
    let connection_cwd = if let Some(id) = input.connection_id.as_deref() {
        if let Some(session) = state.connection_manager.get_state(id).await {
            session.read().await.working_dir.as_ref().map(|p| p.to_string_lossy().into_owned())
        } else { None }
    } else { None };
    if let Err(error) = acp_guard::reject_managed_root(&state.data_dir, input.working_dir.as_deref().or(connection_cwd.as_deref())) {
        return super::workspace_execution::error_response(error);
    }
    if let Err(error) = acp_guard::reject_generic(&state.db.conn, input.working_dir.as_deref().or(connection_cwd.as_deref()), input.conversation_id, input.connection_id.as_deref()).await {
        return super::workspace_execution::error_response(error);
    }
    macro_rules! legacy {
        ($function:ident, $ty:ty) => {{
            let input: $ty = match serde_json::from_slice(&body) { Ok(value) => value, Err(_) => return super::workspace_catalog::catalog_error_response(crate::workspace_catalog::CatalogError::InvalidInput("invalid_input".into())) };
            super::acp::$function(Extension(state), Json(input)).await.into_response()
        }};
    }
    match operation {
        Operation::Connect => legacy!(acp_connect, super::acp::AcpConnectParams),
        Operation::Prompt => legacy!(acp_prompt, super::acp::AcpPromptParams),
        Operation::Disconnect => legacy!(acp_disconnect, super::acp::AcpDisconnectParams),
        Operation::Touch => legacy!(acp_touch_connection, super::acp::AcpTouchConnectionParams),
        Operation::Discover => legacy!(acp_find_connection_for_conversation, super::acp::AcpFindConnectionForConversationParams),
    }
}
macro_rules! route {
    ($name:ident, $operation:ident) => {
        pub async fn $name(Extension(state): Extension<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response<Body> {
            dispatch(state, Operation::$operation, headers, body).await
        }
    };
}
route!(acp_connect, Connect);
route!(acp_prompt, Prompt);
route!(acp_disconnect, Disconnect);
route!(acp_touch_connection, Touch);
route!(acp_find_connection_for_conversation, Discover);

/// External-process/repository fixture seam for localhost HTTP/WS acceptance.
/// The fixture's router supplies the normal installation middleware; cookie,
/// origin, canonical identity, real leases and manager admission stay here.
#[cfg(any(test, feature = "test-utils"))]
#[allow(clippy::too_many_arguments)]
pub async fn handle_workspace_acp_fixture(
    state: &AppState, access: &dyn ExecutionRepositoryAccess, runner: &dyn WorkspaceRunner,
    operation: &str, origin: &str, headers: &HeaderMap, body: &[u8],
) -> Response<Body> {
    let operation = match operation {
        "acp_connect" => Operation::Connect, "acp_prompt" => Operation::Prompt,
        "acp_disconnect" => Operation::Disconnect, "acp_touch_connection" => Operation::Touch,
        "acp_find_connection_for_conversation" => Operation::Discover,
        _ => return super::workspace_execution::error_response(ExecutionError::NotFound),
    };
    WorkspaceAcpHandler { state, access, runner, policy: &BrowserMutationPolicy::from_app_origin(Some(origin)), enabled: true }
        .handle(operation, headers, body).await
}
