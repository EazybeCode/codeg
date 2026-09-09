use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseTransaction, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder, QuerySelect, Set, TransactionTrait,
};
use uuid::Uuid;

use super::{
    intents::AgentService,
    store,
    types::{AgentError, CleanupTicket, Provider},
};
use crate::db::entities::{
    team_agent_attempt, team_agent_connection, team_agent_operation, team_session,
};

const CLEANUP_LEASE: Duration = Duration::seconds(30);
const CLEANUP_LIFETIME: Duration = Duration::minutes(10);
const RECOVERY_PAGE_SIZE: u64 = 100;

impl AgentService {
    pub(crate) async fn claim_cleanup(
        &self,
        operation_id: &str,
    ) -> Result<Option<CleanupTicket>, AgentError> {
        if !valid_id(operation_id) {
            return Err(AgentError::InvalidInput);
        }
        let Some((transaction, connection, operation)) =
            cleanup_transaction(&self.db, operation_id).await?
        else {
            return Ok(None);
        };
        let now = self.clock.now();
        if !matches!(operation.kind.as_str(), "cancel" | "disconnect")
            || operation.target_generation <= 0
            || operation.result_generation <= operation.target_generation
        {
            return Err(AgentError::Conflict);
        }
        let claimable = operation.state == "queued"
            || (operation.state == "leased"
                && operation
                    .lease_expires_at
                    .is_some_and(|expires_at| expires_at <= now));
        if !claimable {
            transaction
                .commit()
                .await
                .map_err(|_| AgentError::Unavailable)?;
            return Ok(None);
        }
        let lease_owner = new_id();
        let lease_expires_at = now
            .checked_add_signed(CLEANUP_LEASE)
            .ok_or(AgentError::Unavailable)?;
        let mut active = operation.clone().into_active_model();
        active.state = Set("leased".to_owned());
        active.lease_owner = Set(Some(lease_owner.clone()));
        active.lease_expires_at = Set(Some(lease_expires_at));
        active.error_code = Set(None);
        active.updated_at = Set(now);
        active
            .update(&transaction)
            .await
            .map_err(|_| AgentError::Unavailable)?;
        let provider = Provider::from_str(&connection.provider).ok_or(AgentError::Unavailable)?;
        let ticket = CleanupTicket {
            operation_id: operation.id,
            connection_id: connection.id,
            provider,
            org_id: connection.org_id,
            user_id: connection.user_id,
            target_generation: operation.target_generation,
            result_generation: operation.result_generation,
            lease_owner,
            lease_expires_at,
        };
        transaction
            .commit()
            .await
            .map_err(|_| AgentError::Unavailable)?;
        Ok(Some(ticket))
    }

    pub(crate) async fn finish_cleanup(
        &self,
        ticket: &CleanupTicket,
        succeeded: bool,
    ) -> Result<(), AgentError> {
        if !valid_id(&ticket.operation_id)
            || !valid_id(&ticket.connection_id)
            || !valid_id(&ticket.lease_owner)
            || ticket.org_id <= 0
            || ticket.user_id <= 0
            || ticket.target_generation <= 0
            || ticket.result_generation <= ticket.target_generation
        {
            return Err(AgentError::InvalidInput);
        }
        let Some((transaction, connection, operation)) =
            cleanup_transaction(&self.db, &ticket.operation_id).await?
        else {
            return Err(AgentError::NotFound);
        };
        let now = self.clock.now();
        if connection.id != ticket.connection_id
            || connection.provider != ticket.provider.as_str()
            || connection.org_id != ticket.org_id
            || connection.user_id != ticket.user_id
            || operation.target_generation != ticket.target_generation
            || operation.result_generation != ticket.result_generation
            || operation.state != "leased"
            || operation.lease_owner.as_deref() != Some(ticket.lease_owner.as_str())
            || operation.lease_expires_at != Some(ticket.lease_expires_at)
            || now >= ticket.lease_expires_at
        {
            return Err(AgentError::Conflict);
        }

        let mut active = operation.into_active_model();
        active.state = Set(if succeeded { "completed" } else { "queued" }.to_owned());
        active.lease_owner = Set(None);
        active.lease_expires_at = Set(None);
        active.error_code = Set((!succeeded).then(|| "cleanup_failed".to_owned()));
        active.updated_at = Set(now);
        active
            .update(&transaction)
            .await
            .map_err(|_| AgentError::Unavailable)?;

        if connection.generation == ticket.result_generation && connection.state == "disconnecting"
        {
            let mut active = connection.into_active_model();
            if succeeded {
                active.state = Set("disconnected".to_owned());
                active.account_label = Set(None);
                active.checked_at = Set(None);
                active.error_code = Set(None);
                active.storage_handle = Set(None);
            } else {
                active.error_code = Set(Some("cleanup_failed".to_owned()));
            }
            active.updated_at = Set(now);
            active
                .update(&transaction)
                .await
                .map_err(|_| AgentError::Unavailable)?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| AgentError::Unavailable)
    }

    pub(crate) async fn expire_pending(&self) -> Result<u64, AgentError> {
        self.recover_attempts(false).await
    }

    pub(crate) async fn recover_startup(&self) -> Result<u64, AgentError> {
        self.recover_attempts(true).await
    }

    async fn recover_attempts(&self, startup: bool) -> Result<u64, AgentError> {
        let now = self.clock.now();
        let mut count = 0;
        loop {
            let mut query = team_agent_attempt::Entity::find()
                .filter(team_agent_attempt::Column::State.eq("pending"))
                .order_by_asc(team_agent_attempt::Column::Id)
                .limit(RECOVERY_PAGE_SIZE);
            if !startup {
                query = query.filter(team_agent_attempt::Column::ExpiresAt.lte(now));
            }
            let ids = query
                .select_only()
                .column(team_agent_attempt::Column::Id)
                .into_tuple::<String>()
                .all(&self.db)
                .await
                .map_err(|_| AgentError::Unavailable)?;
            if ids.is_empty() {
                return Ok(count);
            }
            let mut progressed = false;
            for id in ids {
                if invalidate_attempt(&self.db, &id, now, startup).await? {
                    count += 1;
                    progressed = true;
                }
            }
            if !progressed {
                return Ok(count);
            }
        }
    }
}

pub(crate) async fn invalidate_session_in_transaction(
    transaction: &DatabaseTransaction,
    session_id: &str,
    now: DateTime<Utc>,
) -> Result<(), AgentError> {
    let session = team_session::Entity::find_by_id(session_id)
        .one(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)?
        .ok_or(AgentError::Auth(
            crate::team_access::core::TeamAuthError::Unauthorized,
        ))?;
    let mut connection_ids = team_agent_attempt::Entity::find()
        .filter(team_agent_attempt::Column::SessionId.eq(session_id))
        .filter(team_agent_attempt::Column::State.eq("pending"))
        .select_only()
        .column(team_agent_attempt::Column::ConnectionId)
        .into_tuple::<String>()
        .all(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)?;
    connection_ids.sort();
    connection_ids.dedup();
    for connection_id in connection_ids {
        let Some(mut connection) = store::connection_by_id(transaction, &connection_id).await? else {
            continue;
        };
        if connection.org_id != session.org_id || connection.user_id != session.user_id {
            return Err(AgentError::Unavailable);
        }
        let attempts = team_agent_attempt::Entity::find()
            .filter(team_agent_attempt::Column::ConnectionId.eq(&connection_id))
            .filter(team_agent_attempt::Column::SessionId.eq(session_id))
            .filter(team_agent_attempt::Column::State.eq("pending"))
            .order_by_asc(team_agent_attempt::Column::Id)
            .all(transaction)
            .await
            .map_err(|_| AgentError::Unavailable)?;
        for attempt in attempts {
            invalidate_locked_attempt(transaction, connection.clone(), attempt, now, "canceled")
                .await?;
            connection = store::connection_by_id(transaction, &connection_id)
                .await?
                .ok_or(AgentError::Unavailable)?;
        }
    }
    Ok(())
}

async fn cleanup_transaction(
    db: &sea_orm::DatabaseConnection,
    operation_id: &str,
) -> Result<
    Option<(
        DatabaseTransaction,
        team_agent_connection::Model,
        team_agent_operation::Model,
    )>,
    AgentError,
> {
    let transaction = db.begin().await.map_err(|_| AgentError::Unavailable)?;
    let Some(initial_operation) = team_agent_operation::Entity::find_by_id(operation_id)
        .one(&transaction)
        .await
        .map_err(|_| AgentError::Unavailable)?
    else {
        transaction
            .commit()
            .await
            .map_err(|_| AgentError::Unavailable)?;
        return Ok(None);
    };
    let initial_connection =
        team_agent_connection::Entity::find_by_id(&initial_operation.connection_id)
            .one(&transaction)
            .await
            .map_err(|_| AgentError::Unavailable)?
            .ok_or(AgentError::Unavailable)?;
    store::lock_cleanup_owner(
        &transaction,
        initial_connection.user_id,
        initial_connection.org_id,
    )
    .await?;
    let connection = store::connection_by_id(&transaction, &initial_connection.id)
        .await?
        .ok_or(AgentError::Unavailable)?;
    let operation = store::operation_by_id(&transaction, &connection.id, operation_id)
        .await?
        .ok_or(AgentError::Unavailable)?;
    if connection.org_id != initial_connection.org_id
        || connection.user_id != initial_connection.user_id
        || operation.connection_id != connection.id
    {
        return Err(AgentError::Unavailable);
    }
    Ok(Some((transaction, connection, operation)))
}

async fn invalidate_attempt(
    db: &sea_orm::DatabaseConnection,
    attempt_id: &str,
    now: DateTime<Utc>,
    startup: bool,
) -> Result<bool, AgentError> {
    let transaction = db.begin().await.map_err(|_| AgentError::Unavailable)?;
    let Some(initial_attempt) = team_agent_attempt::Entity::find_by_id(attempt_id)
        .one(&transaction)
        .await
        .map_err(|_| AgentError::Unavailable)?
    else {
        transaction
            .commit()
            .await
            .map_err(|_| AgentError::Unavailable)?;
        return Ok(false);
    };
    let initial_connection =
        team_agent_connection::Entity::find_by_id(&initial_attempt.connection_id)
            .one(&transaction)
            .await
            .map_err(|_| AgentError::Unavailable)?
            .ok_or(AgentError::Unavailable)?;
    store::lock_cleanup_owner(
        &transaction,
        initial_connection.user_id,
        initial_connection.org_id,
    )
    .await?;
    let connection = store::connection_by_id(&transaction, &initial_connection.id)
        .await?
        .ok_or(AgentError::Unavailable)?;
    let attempt = store::attempt(&transaction, &connection.id, attempt_id)
        .await?
        .ok_or(AgentError::Unavailable)?;
    if attempt.state != "pending" || (!startup && attempt.expires_at > now) {
        transaction
            .commit()
            .await
            .map_err(|_| AgentError::Unavailable)?;
        return Ok(false);
    }
    let error_code = if startup { "interrupted" } else { "expired" };
    invalidate_locked_attempt(&transaction, connection, attempt, now, error_code).await?;
    transaction
        .commit()
        .await
        .map_err(|_| AgentError::Unavailable)?;
    Ok(true)
}

async fn invalidate_locked_attempt(
    transaction: &DatabaseTransaction,
    connection: team_agent_connection::Model,
    attempt: team_agent_attempt::Model,
    now: DateTime<Utc>,
    error_code: &str,
) -> Result<(), AgentError> {
    if attempt.generation != connection.generation || connection.state != "connecting" {
        let mut active = attempt.into_active_model();
        active.state = Set(if error_code == "canceled" {
            "canceled"
        } else {
            "expired"
        }
        .to_owned());
        active.updated_at = Set(now);
        active
            .update(transaction)
            .await
            .map_err(|_| AgentError::Unavailable)?;
        return Ok(());
    }
    let next_generation = connection
        .generation
        .checked_add(1)
        .ok_or(AgentError::Unavailable)?;
    let attempt_id = attempt.id.clone();
    let old_generation = connection.generation;
    let connection_id = connection.id.clone();
    let mut active = attempt.into_active_model();
    active.state = Set(if error_code == "canceled" {
        "canceled"
    } else {
        "expired"
    }
    .to_owned());
    active.updated_at = Set(now);
    active
        .update(transaction)
        .await
        .map_err(|_| AgentError::Unavailable)?;
    store::abandon_connect_operations(transaction, &connection_id, now).await?;
    store::set_connection_after_invalidation_with_error(
        transaction,
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
        transaction,
        new_id(),
        connection_id,
        "cancel",
        &server_key(error_code),
        Some(attempt_id),
        old_generation,
        next_generation,
        "queued",
        None,
        None,
        now,
        expires_at,
    )
    .await?;
    Ok(())
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn server_key(kind: &str) -> String {
    format!("__neev_{kind}_{}", Uuid::new_v4().simple())
}

fn new_id() -> String {
    Uuid::new_v4().simple().to_string()
}
