use std::{path::Path, sync::Arc};
use axum::{Router, Json, body::{Body, Bytes}, extract::{State, OriginalUri}, http::{HeaderMap, Response}, response::IntoResponse, routing::post, middleware};
use crate::{app_state::AppState, web::{handlers::{workspace_catalog::{WorkspaceCatalogHandler, BrowserMutationPolicy}, workspace_execution::{WorkspaceExecutionHandler, Operation}}, router::build_router, shutdown::ShutdownSignal}};
use super::{external::External, TOKEN};

pub async fn router(state: Arc<AppState>, root: &Path) -> Router {
    let production = build_router(state.clone(), TOKEN.into(), std::env::var("CODEG_STATIC_DIR").unwrap().into(), Arc::new(ShutdownSignal::new()));
    let fixture = Arc::new(External::new(state, root));
    let mut routes = Router::new();
    for name in ["list_workspace_nodes", "create_workspace_node", "update_workspace_node", "connect_workspace_repository", "list_workspace_repositories", "list_workspace_chat_drafts", "create_workspace_chat_draft", "get_workspace_chat_runtime", "get_workspace_chat_runtime_for_conversation", "get_workspace_runtime_capabilities", "provision_workspace_chat", "acp_connect", "acp_prompt", "acp_disconnect", "acp_touch_connection", "acp_find_connection_for_conversation", "acp_list_agents", "acp_get_agent_status", "list_github_repos"] {
        routes = routes.route(&format!("/api/{name}"), post(dispatch));
    }
    routes.with_state(fixture)
        .route_layer(middleware::from_fn(|request, next| crate::web::auth::require_token(request, next, TOKEN.into())))
        .fallback_service(production)
}

async fn dispatch(State(external): State<Arc<External>>, OriginalUri(uri): OriginalUri, headers: HeaderMap, body: Bytes) -> Response<Body> {
    let name = uri.path().rsplit('/').next().unwrap();
    let policy = BrowserMutationPolicy::from_app_origin(Some("http://localhost:3097"));
    let catalog = WorkspaceCatalogHandler::new(&external.state.db.conn, external.as_ref(), &policy);
    match name {
        "list_workspace_nodes" => catalog.list_workspace_nodes(&headers, &body).await,
        "create_workspace_node" => catalog.create_workspace_node(&headers, &body).await,
        "update_workspace_node" => catalog.update_workspace_node(&headers, &body).await,
        "connect_workspace_repository" => catalog.connect_workspace_repository(&headers, &body).await,
        "list_workspace_repositories" => catalog.list_workspace_repositories(&headers, &body).await,
        "list_workspace_chat_drafts" => catalog.list_workspace_chat_drafts(&headers, &body).await,
        "create_workspace_chat_draft" => catalog.create_workspace_chat_draft(&headers, &body).await,
        "get_workspace_chat_runtime" | "get_workspace_chat_runtime_for_conversation" | "get_workspace_runtime_capabilities" | "provision_workspace_chat" => {
            let operation = match name { "get_workspace_chat_runtime" => Operation::Get, "get_workspace_chat_runtime_for_conversation" => Operation::Inverse, "get_workspace_runtime_capabilities" => Operation::Capabilities, _ => Operation::Provision };
            WorkspaceExecutionHandler { db: &external.state.db.conn, access: external.as_ref(), allocator: external.as_ref(), agents: external.as_ref(), policy: &policy, enabled: true }.handle(operation, &headers, &body).await
        },
        "acp_connect" | "acp_prompt" | "acp_disconnect" | "acp_touch_connection" | "acp_find_connection_for_conversation" => crate::web::handlers::workspace_acp::handle_workspace_acp_fixture(&external.state, external.as_ref(), external.as_ref(), name, "http://localhost:3097", &headers, &body).await,
        "acp_list_agents" => Json(vec![agent()]).into_response(),
        "acp_get_agent_status" => Json(crate::acp::types::AcpAgentStatus { agent_type: crate::models::AgentType::ClaudeCode, available: true, enabled: true, installed_version: Some("fixture".into()), is_acp_adapter: true }).into_response(),
        "list_github_repos" => Json(serde_json::json!([{"full_name":"fixture-org/docs","name":"docs","owner":"fixture-org","private":true,"default_branch":"main"}])).into_response(),
        _ => unreachable!(),
    }
}

fn agent() -> crate::acp::types::AcpAgentInfo {
    crate::acp::types::AcpAgentInfo {
        agent_type: crate::models::AgentType::ClaudeCode, skills_capable: false,
        registry_id: "claude-code".into(), registry_version: Some("fixture".into()), supports_custom_version: false,
        name: "Claude Code".into(), description: "External provider fixture".into(), available: true,
        distribution_type: "binary".into(), is_acp_adapter: true, custom_source: None, enabled: true, sort_order: 0,
        installed_version: Some("fixture".into()), env: Default::default(), host_tools_agent_mode: false,
        config_json: None, config_file_path: None, opencode_auth_json: None, codex_auth_json: None, codex_config_toml: None,
        codex_model_catalog: None, codex_sandbox_settings: None, cline_secrets_json: None, hermes_config_yaml: None,
        grok_config_toml: None, grok_settings: None, cursor_cli_config_json: None, cursor_settings: None,
        model_provider_id: None, icon_url: None,
    }
}
