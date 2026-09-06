use std::sync::Arc;
use async_trait::async_trait;
use axum::{body::{Body, Bytes}, extract::Extension, http::{HeaderMap, Response, StatusCode}, response::IntoResponse, Json};
use crate::{app_state::AppState, models::AgentType, workspace_execution::{ExecutionError, git_allocator::{GitAllocator, ManagedGitAllocator}, repository_access::{ExecutionRepositoryAccess, GitHubExecutionRepositoryAccess}}};
use super::workspace_catalog::BrowserMutationPolicy;
use serde::{Deserialize, Serialize};

#[async_trait]
pub(crate) trait ExecutionAgentAccess: Send + Sync {
    async fn validate(&self, agent: AgentType) -> Result<(), ExecutionError>;
}
pub(crate) struct ProductionAgents<'a>(pub &'a crate::db::AppDatabase);
#[async_trait]
impl ExecutionAgentAccess for ProductionAgents<'_> {
    async fn validate(&self, agent: AgentType) -> Result<(), ExecutionError> {
        if !crate::acp::registry::all_acp_agents().contains(&agent) { return Err(ExecutionError::Disabled); }
        let status = crate::commands::acp::acp_get_agent_status_core(agent, self.0).await.map_err(|_| ExecutionError::Failed)?;
        if !status.available || !status.enabled { return Err(ExecutionError::Disabled); }
        Ok(())
    }
}
#[derive(Clone, Copy)]
pub(crate) enum Operation { Get, Inverse, Capabilities, Provision }
pub(crate) struct WorkspaceExecutionHandler<'a> {
    pub db: &'a sea_orm::DatabaseConnection,
    pub access: &'a dyn ExecutionRepositoryAccess,
    pub allocator: &'a dyn GitAllocator,
    pub agents: &'a dyn ExecutionAgentAccess,
    pub policy: &'a BrowserMutationPolicy,
    pub enabled: bool,
}
impl WorkspaceExecutionHandler<'_> {
    pub(crate) async fn handle(&self, operation: Operation, headers: &HeaderMap, body: &[u8]) -> Response<Body> {
        let scope = match super::workspace_catalog::scope_from_headers(headers, self.db).await {
            Ok(scope) => scope, Err(error) => return error_response(error.into()),
        };
        // Request syntax failures retain the reviewed catalog invalid_input code.
        macro_rules! input {
            ($ty:ty) => { match super::workspace_catalog::parse_json::<$ty>(headers, body) {
                Ok(value) => value, Err(error) => return super::workspace_catalog::catalog_error_response(error),
            } };
        }
        match operation {
            Operation::Capabilities => {
                let _: EmptyInput = input!(EmptyInput);
                Json(serde_json::json!({"execution_enabled":self.enabled})).into_response()
            },
            Operation::Get => {
                let input = input!(DraftInput);
                response(crate::workspace_execution::get_runtime(self.db, &scope, input.draft_id).await)
            },
            Operation::Inverse => {
                let input = input!(ConversationInput);
                response(crate::workspace_execution::get_runtime_for_conversation(self.db, &scope, input.conversation_id).await)
            },
            Operation::Provision => {
                if let Err(error) = self.policy.validate(headers) { return super::workspace_catalog::catalog_error_response(error); }
                let input = input!(ProvisionInput);
                if !self.enabled { return error_response(ExecutionError::Disabled); }
                // Resolve ownership before probing a local agent installation.
                if let Err(error) = crate::workspace_execution::get_runtime(self.db, &scope, input.draft_id).await { return error_response(error); }
                use sea_orm::EntityTrait;
                use crate::db::entities::workspace_chat_execution;
                match workspace_chat_execution::Entity::find_by_id(input.draft_id).one(self.db).await {
                    Ok(Some(row)) if row.agent_type != input.agent_type.as_wire() => return error_response(ExecutionError::Conflict),
                    Err(error) => return error_response(error.into()),
                    _ => {},
                }
                if let Err(error) = self.agents.validate(input.agent_type).await { return error_response(error); }
                response(crate::workspace_execution::provisioning::provision(self.db, &scope, input.draft_id, input.agent_type, self.access, self.allocator, true).await)
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DraftInput { draft_id: i32 }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConversationInput { conversation_id: i32 }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProvisionInput { draft_id: i32, agent_type: AgentType }

pub(crate) fn error_response(error: ExecutionError) -> Response<Body> {
    let status = match error {
        ExecutionError::Unauthorized | ExecutionError::ReauthRequired => StatusCode::UNAUTHORIZED,
        ExecutionError::NotFound | ExecutionError::RepositoryUnavailable => StatusCode::NOT_FOUND,
        ExecutionError::Conflict | ExecutionError::AllocationMismatch | ExecutionError::Busy => StatusCode::CONFLICT,
        ExecutionError::Disabled => StatusCode::SERVICE_UNAVAILABLE,
        ExecutionError::UpstreamUnavailable => StatusCode::BAD_GATEWAY,
        ExecutionError::Failed | ExecutionError::Database => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(serde_json::json!({"error":error.to_string()}))).into_response()
}
fn response<T: Serialize>(result: Result<T, ExecutionError>) -> Response<Body> {
    match result { Ok(value) => Json(value).into_response(), Err(error) => error_response(error) }
}

async fn production(state: &AppState, operation: Operation, headers: &HeaderMap, body: &[u8]) -> Response<Body> {
    let access = match GitHubExecutionRepositoryAccess::new(&state.db.conn) { Ok(access) => access, Err(error) => return error_response(error) };
    let allocator = match ManagedGitAllocator::new(state.data_dir.clone(), &state.db.conn) { Ok(allocator) => allocator, Err(error) => return error_response(error) };
    WorkspaceExecutionHandler { db: &state.db.conn, access: &access, allocator: &allocator,
        agents: &ProductionAgents(&state.db), policy: &BrowserMutationPolicy::from_environment(),
        enabled: std::env::var("WORKOS_ENABLE_CATALOG_RUNTIME").ok().as_deref() == Some("true"),
    }.handle(operation, headers, body).await
}

pub async fn get_workspace_chat_runtime(Extension(state): Extension<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response<Body> {
    production(&state, Operation::Get, &headers, &body).await
}
pub async fn get_workspace_chat_runtime_for_conversation(Extension(state): Extension<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response<Body> {
    production(&state, Operation::Inverse, &headers, &body).await
}
pub async fn get_workspace_runtime_capabilities(Extension(state): Extension<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response<Body> {
    production(&state, Operation::Capabilities, &headers, &body).await
}
pub async fn provision_workspace_chat(Extension(state): Extension<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response<Body> {
    production(&state, Operation::Provision, &headers, &body).await
}
