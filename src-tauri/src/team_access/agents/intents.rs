use std::sync::Arc;

use chrono::Duration;
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use super::{
    store,
    types::{
        AgentError, AttemptView, BeginOutcome, ConnectionState, ConnectionView, LoginTicket,
        MutationReceipt, Provider,
    },
};
use crate::team_access::{
    core::TeamClock,
    principal::{AuthContext, PrincipalService},
};

const ATTEMPT_LIFETIME: Duration = Duration::minutes(10);
const QUOTA_WINDOW: Duration = Duration::minutes(15);
const MAX_ATTEMPTS: u64 = 5;

pub(crate) struct AgentService {
    pub(super) db: DatabaseConnection,
    pub(super) principals: Arc<PrincipalService>,
    pub(super) clock: Arc<dyn TeamClock>,
}

impl AgentService {
    pub(crate) fn new(
        db: DatabaseConnection,
        principals: Arc<PrincipalService>,
        clock: Arc<dyn TeamClock>,
    ) -> Self {
        Self {
            db,
            principals,
            clock,
        }
    }

    pub(crate) async fn list(
        &self,
        context: &AuthContext,
    ) -> Result<Vec<ConnectionView>, AgentError> {
        let transaction =
            store::authorized_transaction(&self.db, &self.principals, context).await?;
        let rows = store::connections(&transaction, context).await?;
        let mut claude = None;
        let mut codex = None;
        for row in rows {
            let view = store::project(&row)?;
            let slot = match view.provider {
                Provider::Claude => &mut claude,
                Provider::Codex => &mut codex,
            };
            if slot.replace(view).is_some() {
                return Err(AgentError::Unavailable);
            }
        }
        store::final_recheck_and_commit(transaction, &self.principals, context).await?;
        Ok(vec![
            claude.unwrap_or_else(|| disconnected(Provider::Claude)),
            codex.unwrap_or_else(|| disconnected(Provider::Codex)),
        ])
    }

    pub(crate) async fn attempt(
        &self,
        context: &AuthContext,
        provider: Provider,
        attempt_id: &str,
    ) -> Result<AttemptView, AgentError> {
        let transaction =
            store::authorized_transaction(&self.db, &self.principals, context).await?;
        let Some(connection) = store::connection(&transaction, context, provider).await? else {
            return Err(AgentError::NotFound);
        };
        let Some(attempt) = store::attempt(&transaction, &connection.id, attempt_id).await? else {
            return Err(AgentError::NotFound);
        };
        if attempt.session_id != context.session_id {
            return Err(AgentError::NotFound);
        }
        if !valid_attempt_state(&attempt.state) {
            return Err(AgentError::Unavailable);
        }
        let view = AttemptView {
            state: attempt.state,
            expires_at: attempt.expires_at,
        };
        store::final_recheck_and_commit(transaction, &self.principals, context).await?;
        Ok(view)
    }

    pub(crate) async fn begin(
        &self,
        context: &AuthContext,
        provider: Provider,
        request_key: &str,
    ) -> Result<BeginOutcome, AgentError> {
        validate_request_key(request_key)?;
        let transaction =
            store::authorized_transaction(&self.db, &self.principals, context).await?;
        let existing_connection = store::connection(&transaction, context, provider).await?;
        let now = self.clock.now();

        if let Some(connection) = &existing_connection {
            if let Some(operation) =
                store::operation(&transaction, &connection.id, "connect", request_key).await?
            {
                let outcome = replay_begin(
                    &transaction,
                    context,
                    provider,
                    connection,
                    &operation,
                    now,
                )
                .await?;
                store::final_recheck_and_commit(transaction, &self.principals, context).await?;
                return Ok(outcome);
            }
            if connection.state != "disconnected"
                || connection.storage_handle.is_some()
                || !store::active_operations(&transaction, &connection.id)
                    .await?
                    .is_empty()
            {
                return Err(AgentError::Conflict);
            }
        }

        if store::recent_attempt_count(&transaction, context.user_id, now - QUOTA_WINDOW).await?
            >= MAX_ATTEMPTS
        {
            return Err(AgentError::RateLimited);
        }

        let generation = match &existing_connection {
            Some(row) if row.generation > 0 => row
                .generation
                .checked_add(1)
                .ok_or(AgentError::Unavailable)?,
            Some(_) => return Err(AgentError::Unavailable),
            None => 1,
        };
        let connection = match existing_connection {
            Some(row) => {
                store::set_connection_for_login(&transaction, row, generation, now).await?
            }
            None => {
                store::insert_connection(
                    &transaction,
                    new_id(),
                    context,
                    provider,
                    generation,
                    "connecting",
                    now,
                )
                .await?
            }
        };
        let expires_at = now
            .checked_add_signed(ATTEMPT_LIFETIME)
            .ok_or(AgentError::Unavailable)?;
        let attempt = store::insert_attempt(
            &transaction,
            new_id(),
            connection.id.clone(),
            generation,
            context.session_id.clone(),
            now,
            expires_at,
        )
        .await?;
        let lease_owner = new_id();
        let operation = store::insert_operation(
            &transaction,
            new_id(),
            connection.id.clone(),
            "connect",
            request_key,
            Some(attempt.id.clone()),
            generation,
            generation,
            "leased",
            Some(lease_owner.clone()),
            Some(expires_at),
            now,
            expires_at,
        )
        .await?;
        let ticket = LoginTicket {
            connection_id: connection.id,
            provider,
            generation,
            org_id: context.org_id,
            user_id: context.user_id,
            attempt_id: attempt.id,
            operation_id: operation.id,
            lease_owner,
            expires_at,
        };
        store::final_recheck_and_commit(transaction, &self.principals, context).await?;
        Ok(BeginOutcome::Pending(ticket))
    }

    pub(crate) async fn cancel(
        &self,
        context: &AuthContext,
        provider: Provider,
        attempt_id: &str,
        request_key: &str,
    ) -> Result<MutationReceipt, AgentError> {
        validate_request_key(request_key)?;
        let transaction =
            store::authorized_transaction(&self.db, &self.principals, context).await?;
        let Some(connection) = store::connection(&transaction, context, provider).await? else {
            return Err(AgentError::NotFound);
        };
        let now = self.clock.now();
        if let Some(operation) =
            store::operation(&transaction, &connection.id, "cancel", request_key).await?
        {
            if operation.target_attempt_id.as_deref() != Some(attempt_id) {
                return Err(AgentError::Conflict);
            }
            let Some(attempt) = store::attempt(&transaction, &connection.id, attempt_id).await?
            else {
                return Err(AgentError::NotFound);
            };
            if attempt.session_id != context.session_id {
                return Err(AgentError::NotFound);
            }
            let receipt = receipt(&operation)?;
            store::final_recheck_and_commit(transaction, &self.principals, context).await?;
            return Ok(receipt);
        }

        let Some(attempt) = store::attempt(&transaction, &connection.id, attempt_id).await? else {
            return Err(AgentError::NotFound);
        };
        if attempt.session_id != context.session_id {
            return Err(AgentError::NotFound);
        }
        if attempt.state != "pending"
            || attempt.generation != connection.generation
            || connection.state != "connecting"
        {
            return Err(AgentError::Conflict);
        }
        let matching_connect = store::active_operations(&transaction, &connection.id)
            .await?
            .into_iter()
            .any(|operation| {
                operation.kind == "connect"
                    && operation.target_attempt_id.as_deref() == Some(attempt_id)
                    && operation.target_generation == connection.generation
                    && operation.result_generation == connection.generation
            });
        if !matching_connect {
            return Err(AgentError::Conflict);
        }
        let old_generation = connection.generation;
        let generation = old_generation
            .checked_add(1)
            .ok_or(AgentError::Unavailable)?;
        store::set_attempt_state(&transaction, attempt, "canceled", now).await?;
        store::abandon_connect_operations(&transaction, &connection.id, now).await?;
        store::set_connection_after_invalidation(
            &transaction,
            connection.clone(),
            generation,
            "disconnecting",
            false,
            now,
        )
        .await?;
        let expires_at = now
            .checked_add_signed(ATTEMPT_LIFETIME)
            .ok_or(AgentError::Unavailable)?;
        let operation = store::insert_operation(
            &transaction,
            new_id(),
            connection.id,
            "cancel",
            request_key,
            Some(attempt_id.to_owned()),
            old_generation,
            generation,
            "queued",
            None,
            None,
            now,
            expires_at,
        )
        .await?;
        let receipt = receipt(&operation)?;
        store::final_recheck_and_commit(transaction, &self.principals, context).await?;
        Ok(receipt)
    }

    pub(crate) async fn disconnect(
        &self,
        context: &AuthContext,
        provider: Provider,
        request_key: &str,
    ) -> Result<MutationReceipt, AgentError> {
        validate_request_key(request_key)?;
        let transaction =
            store::authorized_transaction(&self.db, &self.principals, context).await?;
        let Some(connection) = store::connection(&transaction, context, provider).await? else {
            return Err(AgentError::NotFound);
        };
        let now = self.clock.now();
        if let Some(operation) =
            store::operation(&transaction, &connection.id, "disconnect", request_key).await?
        {
            let receipt = receipt(&operation)?;
            store::final_recheck_and_commit(transaction, &self.principals, context).await?;
            return Ok(receipt);
        }
        let active = store::active_operations(&transaction, &connection.id).await?;
        if active
            .iter()
            .any(|operation| matches!(operation.kind.as_str(), "cancel" | "disconnect"))
        {
            return Err(AgentError::Conflict);
        }
        if active.iter().any(|operation| {
            !matches!(
                operation.kind.as_str(),
                "connect" | "cancel" | "disconnect"
            )
        }) {
            return Err(AgentError::Unavailable);
        }
        let old_generation = connection.generation;
        if old_generation <= 0 {
            return Err(AgentError::Unavailable);
        }
        let generation = old_generation
            .checked_add(1)
            .ok_or(AgentError::Unavailable)?;
        let immediate = connection.state == "disconnected"
            && connection.storage_handle.is_none()
            && active.is_empty();
        store::cancel_pending_attempts(&transaction, &connection.id, now).await?;
        store::abandon_connect_operations(&transaction, &connection.id, now).await?;
        store::set_connection_after_invalidation(
            &transaction,
            connection.clone(),
            generation,
            if immediate {
                "disconnected"
            } else {
                "disconnecting"
            },
            immediate,
            now,
        )
        .await?;
        let expires_at = now
            .checked_add_signed(ATTEMPT_LIFETIME)
            .ok_or(AgentError::Unavailable)?;
        let operation = store::insert_operation(
            &transaction,
            new_id(),
            connection.id,
            "disconnect",
            request_key,
            None,
            old_generation,
            generation,
            if immediate { "completed" } else { "queued" },
            None,
            None,
            now,
            expires_at,
        )
        .await?;
        let receipt = receipt(&operation)?;
        store::final_recheck_and_commit(transaction, &self.principals, context).await?;
        Ok(receipt)
    }
}

async fn replay_begin(
    transaction: &sea_orm::DatabaseTransaction,
    context: &AuthContext,
    provider: Provider,
    connection: &crate::db::entities::team_agent_connection::Model,
    operation: &crate::db::entities::team_agent_operation::Model,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<BeginOutcome, AgentError> {
    let Some(attempt_id) = operation.target_attempt_id.as_deref() else {
        return Err(AgentError::Unavailable);
    };
    let Some(attempt) = store::attempt(transaction, &connection.id, attempt_id).await? else {
        return Err(AgentError::Conflict);
    };
    if attempt.session_id != context.session_id {
        return Err(AgentError::Conflict);
    }
    if operation.target_generation <= 0
        || operation.result_generation != operation.target_generation
        || attempt.generation != operation.target_generation
    {
        return Err(AgentError::Unavailable);
    }
    if operation.state == "leased"
        && attempt.state == "pending"
        && connection.state == "connecting"
        && connection.generation == operation.result_generation
        && attempt.created_at <= now
        && now < attempt.expires_at
        && operation.lease_expires_at.is_some_and(|expiry| now < expiry)
        && operation
            .lease_owner
            .as_deref()
            .is_some_and(|owner| !owner.is_empty())
    {
        return Ok(BeginOutcome::Pending(LoginTicket {
            connection_id: connection.id.clone(),
            provider,
            generation: operation.result_generation,
            org_id: context.org_id,
            user_id: context.user_id,
            attempt_id: attempt.id,
            operation_id: operation.id.clone(),
            lease_owner: operation
                .lease_owner
                .clone()
                .ok_or(AgentError::Unavailable)?,
            expires_at: attempt.expires_at,
        }));
    }
    if operation.state == "completed"
        && attempt.state == "completed"
        && connection.generation == operation.result_generation
    {
        return Ok(BeginOutcome::Settled(store::project(connection)?));
    }
    Err(AgentError::Conflict)
}

fn receipt(
    operation: &crate::db::entities::team_agent_operation::Model,
) -> Result<MutationReceipt, AgentError> {
    if operation.result_generation <= 0 || operation.id.is_empty() || operation.id.len() > 64 {
        return Err(AgentError::Unavailable);
    }
    Ok(MutationReceipt {
        operation_id: operation.id.clone(),
        generation: operation.result_generation,
    })
}

fn validate_request_key(request_key: &str) -> Result<(), AgentError> {
    if !(1..=64).contains(&request_key.len())
        || request_key.starts_with("__neev_")
        || !request_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(AgentError::InvalidInput);
    }
    Ok(())
}

fn valid_attempt_state(state: &str) -> bool {
    matches!(
        state,
        "pending" | "completed" | "canceled" | "expired" | "failed"
    )
}

fn disconnected(provider: Provider) -> ConnectionView {
    ConnectionView {
        provider,
        state: ConnectionState::Disconnected,
        account_label: None,
        checked_at: None,
        error_code: None,
    }
}

fn new_id() -> String {
    Uuid::new_v4().simple().to_string()
}
