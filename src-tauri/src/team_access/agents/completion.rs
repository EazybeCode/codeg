use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};
use uuid::Uuid;

use super::{
    intents::AgentService,
    store,
    types::{
        valid_account_label, valid_storage_handle, AgentError, LoginTicket,
        VerifiedSubscription,
    },
};
use crate::team_access::principal::AuthContext;

const CLEANUP_LIFETIME: Duration = Duration::minutes(10);

impl AgentService {
    pub(crate) async fn complete(
        &self,
        context: &AuthContext,
        ticket: &LoginTicket,
        account: VerifiedSubscription,
    ) -> Result<(), AgentError> {
        if !valid_account_label(&account.account_label)
            || !valid_storage_handle(&account.storage_handle)
        {
            return Err(AgentError::InvalidInput);
        }
        let transaction =
            store::authorized_transaction(&self.db, &self.principals, context).await?;
        let (connection, attempt, operation) =
            store::completion_rows(&transaction, context, ticket).await?;
        let now = self.clock.now();
        validate_pending(context, ticket, &connection, &attempt, &operation, now)?;

        let mut connection = connection.into_active_model();
        connection.state = Set("connected".to_owned());
        connection.account_label = Set(Some(account.account_label));
        connection.checked_at = Set(Some(now));
        connection.error_code = Set(None);
        connection.storage_handle = Set(Some(account.storage_handle));
        connection.updated_at = Set(now);
        connection
            .update(&transaction)
            .await
            .map_err(|_| AgentError::Unavailable)?;

        let mut attempt = attempt.into_active_model();
        attempt.state = Set("completed".to_owned());
        attempt.updated_at = Set(now);
        attempt
            .update(&transaction)
            .await
            .map_err(|_| AgentError::Unavailable)?;

        let mut operation = operation.into_active_model();
        operation.state = Set("completed".to_owned());
        operation.lease_owner = Set(None);
        operation.lease_expires_at = Set(None);
        operation.error_code = Set(None);
        operation.updated_at = Set(now);
        operation
            .update(&transaction)
            .await
            .map_err(|_| AgentError::Unavailable)?;

        store::final_recheck_and_commit(transaction, &self.principals, context).await
    }

    pub(crate) async fn fail(
        &self,
        context: &AuthContext,
        ticket: &LoginTicket,
        error_code: &str,
    ) -> Result<(), AgentError> {
        if !valid_failure_code(error_code) {
            return Err(AgentError::InvalidInput);
        }
        let transaction =
            store::authorized_transaction(&self.db, &self.principals, context).await?;
        let (connection, attempt, operation) =
            store::completion_rows(&transaction, context, ticket).await?;
        let now = self.clock.now();
        validate_pending(context, ticket, &connection, &attempt, &operation, now)?;
        let next_generation = connection
            .generation
            .checked_add(1)
            .ok_or(AgentError::Unavailable)?;

        let mut attempt = attempt.into_active_model();
        attempt.state = Set("failed".to_owned());
        attempt.updated_at = Set(now);
        attempt
            .update(&transaction)
            .await
            .map_err(|_| AgentError::Unavailable)?;

        let mut operation = operation.into_active_model();
        operation.state = Set("abandoned".to_owned());
        operation.lease_owner = Set(None);
        operation.lease_expires_at = Set(None);
        operation.error_code = Set(Some(error_code.to_owned()));
        operation.updated_at = Set(now);
        operation
            .update(&transaction)
            .await
            .map_err(|_| AgentError::Unavailable)?;

        store::set_connection_after_invalidation_with_error(
            &transaction,
            connection,
            next_generation,
            error_code,
            now,
        )
        .await?;
        let expires_at = now
            .checked_add_signed(CLEANUP_LIFETIME)
            .ok_or(AgentError::Unavailable)?;
        store::insert_operation(
            &transaction,
            new_id(),
            ticket.connection_id.clone(),
            "disconnect",
            &server_key("failure"),
            None,
            ticket.generation,
            next_generation,
            "queued",
            None,
            None,
            now,
            expires_at,
        )
        .await?;
        store::final_recheck_and_commit(transaction, &self.principals, context).await
    }
}

fn validate_pending(
    context: &AuthContext,
    ticket: &LoginTicket,
    connection: &crate::db::entities::team_agent_connection::Model,
    attempt: &crate::db::entities::team_agent_attempt::Model,
    operation: &crate::db::entities::team_agent_operation::Model,
    now: chrono::DateTime<Utc>,
) -> Result<(), AgentError> {
    if ticket.org_id != context.org_id
        || ticket.user_id != context.user_id
        || connection.org_id != ticket.org_id
        || connection.user_id != ticket.user_id
        || connection.provider != ticket.provider.as_str()
        || connection.generation != ticket.generation
        || connection.state != "connecting"
        || attempt.connection_id != ticket.connection_id
        || attempt.id != ticket.attempt_id
        || attempt.generation != ticket.generation
        || attempt.session_id != context.session_id
        || attempt.state != "pending"
        || attempt.created_at > now
        || now >= attempt.expires_at
        || ticket.expires_at != attempt.expires_at
        || operation.connection_id != ticket.connection_id
        || operation.id != ticket.operation_id
        || operation.kind != "connect"
        || operation.request_key.starts_with("__neev_")
        || operation.target_attempt_id.as_deref() != Some(ticket.attempt_id.as_str())
        || operation.target_generation != ticket.generation
        || operation.result_generation != ticket.generation
        || operation.state != "leased"
        || operation.lease_owner.as_deref() != Some(ticket.lease_owner.as_str())
        || operation
            .lease_expires_at
            .is_none_or(|expires_at| now >= expires_at)
    {
        return Err(AgentError::Conflict);
    }
    Ok(())
}

fn valid_failure_code(value: &str) -> bool {
    matches!(
        value,
        "provider_unavailable" | "reauth_required" | "unsupported_account" | "interrupted"
    )
}

fn server_key(kind: &str) -> String {
    format!("__neev_{kind}_{}", Uuid::new_v4().simple())
}

fn new_id() -> String {
    Uuid::new_v4().simple().to_string()
}
