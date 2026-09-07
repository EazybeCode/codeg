use async_trait::async_trait;
use crate::{app_state::AppState, commands::acp, db::entities::conversation};
use sea_orm::EntityTrait;
use super::{ExecutionError, writer::{ReadyExecutionContext, WorkspaceRunner}};

pub(crate) struct AcpWorkspaceRunner<'a> {
    pub state: &'a AppState,
    pub preferred_mode_id: Option<String>,
    pub preferred_config_values: std::collections::BTreeMap<String, String>,
}
#[async_trait]
impl WorkspaceRunner for AcpWorkspaceRunner<'_> {
    async fn spawn(&self, context: &ReadyExecutionContext) -> Result<String, ExecutionError> {
        let state = self.state;
        let conversation = conversation::Entity::find_by_id(context.conversation_id).one(&state.db.conn).await?.ok_or(ExecutionError::NotFound)?;
        // Resume only the server's mapped session. No caller supplied session
        // or directory may retarget the managed writer.
        let session = if context.agent_type == crate::models::AgentType::Cline { None } else { conversation.external_id };
        let status = acp::acp_get_agent_status_core(context.agent_type, &state.db).await.map_err(|_| ExecutionError::Failed)?;
        if !status.enabled || !status.available { return Err(ExecutionError::Disabled); }
        acp::verify_agent_installed(context.agent_type).await.map_err(|_| ExecutionError::Disabled)?;
        let mut env = acp::build_session_runtime_env(&state.db, context.agent_type, session.as_deref(), &state.data_dir).await.map_err(|_| ExecutionError::Failed)?;
        env.insert("WORKOS_MANAGED_CHAT".into(), "true".into());
        state.connection_manager.spawn_agent(context.agent_type, Some(context.working_dir.to_string_lossy().into()), session, env,
            "workspace".into(), state.emitter.clone(), self.preferred_mode_id.clone(), self.preferred_config_values.clone()).await.map_err(|_| ExecutionError::Failed)
    }
    async fn disconnect(&self, connection_id: &str) -> Result<(), ExecutionError> {
        self.state.connection_manager.disconnect_confirmed(connection_id, std::time::Duration::from_secs(5)).await.map_err(|_| ExecutionError::Busy)
    }
    async fn stopped(&self, connection_id: &str) -> bool {
        self.state.connection_manager.workspace_process_stopped(connection_id).await
    }
}
