use std::sync::Arc;

use chrono::Duration;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait,
    IntoActiveModel, PaginatorTrait, QueryFilter, Set, Statement, TransactionTrait,
};
use tokio::sync::Barrier;

use super::{
    AgentError, AgentService, BeginOutcome, CleanupTicket, LoginTicket, Provider,
    VerifiedSubscription,
};
use crate::{
    db::entities::{
        organization_membership, team_agent_attempt, team_agent_connection,
        team_agent_operation, team_session,
    },
    team_access::{
        core::TeamAuthError,
        principal::{PrincipalService, ProofRequirement},
        test_auth::{
            cookie_headers, create_postgres_principal_fixture, drop_postgres_fixture, fixed_now,
            principal_fixture, token_hash, wait_for_postgres_lock, TestClock,
            ALICE_A_MEMBERSHIP_ID, ALICE_A_TOKEN,
        },
    },
};

async fn resolve(principals: &PrincipalService) -> crate::team_access::principal::AuthContext {
    principals
        .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
        .await
        .unwrap()
}

fn pending(outcome: BeginOutcome) -> LoginTicket {
    let BeginOutcome::Pending(ticket) = outcome else {
        panic!("expected pending login");
    };
    ticket
}

fn verified(label: &str) -> VerifiedSubscription {
    VerifiedSubscription {
        account_label: label.to_owned(),
        storage_handle: "11111111111111111111111111111111".to_owned(),
    }
}

async fn service_and_context() -> (
    crate::team_access::test_auth::PrincipalFixture,
    AgentService,
    crate::team_access::principal::AuthContext,
) {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let context = resolve(&principals).await;
    let service = AgentService::new(fixture.db.clone(), principals, fixture.clock.clone());
    (fixture, service, context)
}

async fn wait_for_postgres_lock_count(
    admin: &DatabaseConnection,
    database_name: &str,
    query_fragment: &str,
    expected: i64,
) -> Result<(), String> {
    let pattern = format!("%{query_fragment}%");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting = admin
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT COUNT(*) AS waiting_count FROM pg_stat_activity \
                     WHERE datname = $1 AND wait_event_type = 'Lock' AND query LIKE $2"
                        .to_owned(),
                    [database_name.into(), pattern.clone().into()],
                ))
                .await
                .map_err(|error| format!("observe PostgreSQL lock count: {error}"))?
                .ok_or_else(|| "PostgreSQL lock-count observer returned no row".to_owned())?
                .try_get::<i64>("", "waiting_count")
                .map_err(|error| format!("decode PostgreSQL lock count: {error}"))?;
            if waiting >= expected {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| {
        format!(
            "timed out waiting for {expected} PostgreSQL lock waiters matching {query_fragment}"
        )
    })?
}

async fn finish_postgres_test(
    name: &str,
    admin: DatabaseConnection,
    db: DatabaseConnection,
    result: Result<(), String>,
) -> Result<(), String> {
    match result {
        Ok(()) => drop_postgres_fixture(name, admin, db).await,
        Err(error) => Err(format!("{error}; preserved PostgreSQL database {name}")),
    }
}

async fn lock_postgres_user(
    transaction: &sea_orm::DatabaseTransaction,
    user_id: i32,
) -> Result<(), String> {
    transaction
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM users WHERE id = $1 FOR UPDATE".to_owned(),
            [user_id.into()],
        ))
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "user blocker found no row".to_owned())?;
    Ok(())
}

#[tokio::test]
async fn canceled_login_cannot_complete_with_late_provider_result() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let context = resolve(&principals).await;
    let service = AgentService::new(fixture.db.clone(), principals, fixture.clock.clone());
    let BeginOutcome::Pending(ticket) = service
        .begin(&context, Provider::Codex, "late-completion")
        .await
        .unwrap()
    else {
        panic!("expected pending login");
    };

    service
        .cancel(
            &context,
            Provider::Codex,
            &ticket.attempt_id,
            "cancel-late-completion",
        )
        .await
        .unwrap();
    assert_eq!(
        service
            .complete(
                &context,
                &ticket,
                VerifiedSubscription {
                    account_label: "invented@example.test".to_owned(),
                    storage_handle: "11111111111111111111111111111111".to_owned(),
                },
            )
            .await,
        Err(AgentError::Conflict)
    );

    let connection = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connection.state, "disconnecting");
    assert!(connection.storage_handle.is_none());
}

#[tokio::test]
async fn own_verified_completion_is_atomic_private_and_single_use() {
    let (fixture, service, context) = service_and_context().await;
    let ticket = pending(
        service
            .begin(&context, Provider::Claude, "complete-own")
            .await
            .unwrap(),
    );

    service
        .complete(&context, &ticket, verified("alice@example.test"))
        .await
        .unwrap();
    assert_eq!(
        service
            .complete(&context, &ticket, verified("overwrite@example.test"))
            .await,
        Err(AgentError::Conflict)
    );

    let connection = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connection.state, "connected");
    assert_eq!(connection.generation, ticket.generation);
    assert_eq!(connection.account_label.as_deref(), Some("alice@example.test"));
    assert_eq!(connection.checked_at, Some(fixed_now()));
    assert_eq!(
        connection.storage_handle.as_deref(),
        Some("11111111111111111111111111111111")
    );
    assert!(connection.error_code.is_none());
    let attempt = team_agent_attempt::Entity::find_by_id(&ticket.attempt_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(attempt.state, "completed");
    let operation = team_agent_operation::Entity::find_by_id(&ticket.operation_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(operation.state, "completed");
    assert!(operation.lease_owner.is_none());
    assert!(operation.lease_expires_at.is_none());

    let view = service.list(&context).await.unwrap();
    let claude = view
        .iter()
        .find(|view| view.provider == Provider::Claude)
        .unwrap();
    assert_eq!(claude.account_label.as_deref(), Some("alice@example.test"));
    assert_eq!(claude.checked_at, Some(fixed_now()));
}

#[tokio::test]
async fn completion_rejects_malformed_safe_metadata_without_mutation() {
    for (label, handle) in [
        ("", "11111111111111111111111111111111"),
        ("bad\nlabel", "11111111111111111111111111111111"),
        ("bad\u{202e}label", "11111111111111111111111111111111"),
        ("ok", "11111111-1111-1111-1111-111111111111"),
        ("ok", "1111111111111111111111111111111z"),
    ] {
        let (fixture, service, context) = service_and_context().await;
        let ticket = pending(
            service
                .begin(&context, Provider::Codex, "invalid-metadata")
                .await
                .unwrap(),
        );
        assert_eq!(
            service
                .complete(
                    &context,
                    &ticket,
                    VerifiedSubscription {
                        account_label: label.to_owned(),
                        storage_handle: handle.to_owned(),
                    },
                )
                .await,
            Err(AgentError::InvalidInput)
        );
        let connection = team_agent_connection::Entity::find_by_id(ticket.connection_id)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(connection.state, "connecting");
        assert!(connection.account_label.is_none());
        assert!(connection.storage_handle.is_none());
    }

    let (fixture, service, context) = service_and_context().await;
    let ticket = pending(
        service
            .begin(&context, Provider::Codex, "long-label")
            .await
            .unwrap(),
    );
    assert_eq!(
        service
            .complete(&context, &ticket, verified(&"é".repeat(161)))
            .await,
        Err(AgentError::InvalidInput)
    );
    assert_eq!(
        team_agent_attempt::Entity::find_by_id(ticket.attempt_id)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .state,
        "pending"
    );
}

#[tokio::test]
async fn completion_rejects_ticket_and_lease_tampering_and_expired_proof() {
    let (fixture, service, context) = service_and_context().await;
    let ticket = pending(
        service
            .begin(&context, Provider::Codex, "ticket-fields")
            .await
            .unwrap(),
    );
    let mut variants = Vec::new();
    let mut changed = ticket.clone();
    changed.generation += 1;
    variants.push(changed);
    let mut changed = ticket.clone();
    changed.org_id += 1;
    variants.push(changed);
    let mut changed = ticket.clone();
    changed.user_id += 1;
    variants.push(changed);
    let mut changed = ticket.clone();
    changed.provider = Provider::Claude;
    variants.push(changed);
    let mut changed = ticket.clone();
    changed.lease_owner = "22222222222222222222222222222222".to_owned();
    variants.push(changed);
    let mut changed = ticket.clone();
    changed.expires_at += Duration::seconds(1);
    variants.push(changed);
    for changed in variants {
        assert_eq!(
            service
                .complete(&context, &changed, verified("tampered@example.test"))
                .await,
            Err(AgentError::Conflict)
        );
    }
    let expired_service = AgentService::new(
        fixture.db.clone(),
        fixture.service(),
        Arc::new(TestClock::new(fixed_now() + Duration::minutes(10))),
    );
    assert_eq!(
        expired_service
            .complete(&context, &ticket, verified("expired@example.test"))
            .await,
        Err(AgentError::Conflict)
    );
    assert_eq!(
        team_agent_attempt::Entity::find_by_id(ticket.attempt_id)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .state,
        "pending"
    );
}

#[tokio::test]
async fn completion_requires_the_still_live_initiating_session() {
    let (fixture, service, context) = service_and_context().await;
    let ticket = pending(
        service
            .begin(&context, Provider::Codex, "deleted-session")
            .await
            .unwrap(),
    );
    team_session::Entity::delete_by_id(&context.session_id)
        .exec(&fixture.db)
        .await
        .unwrap();
    assert_eq!(
        service
            .complete(&context, &ticket, verified("late@example.test"))
            .await,
        Err(AgentError::Auth(TeamAuthError::Unauthorized))
    );
    let connection = team_agent_connection::Entity::find_by_id(ticket.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(connection.state, "connected");
    assert!(connection.storage_handle.is_none());
}

#[tokio::test]
async fn completion_and_failure_hide_other_session_attempts_like_missing_attempts() {
    let (fixture, service, context) = service_and_context().await;
    let other_token = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    team_session::ActiveModel {
        token_hash: Set(token_hash(other_token)),
        user_id: Set(context.user_id),
        org_id: Set(context.org_id),
        membership_id: Set(context.membership_id),
        authorization_revision: Set(context.authorization_revision),
        credential_generation: Set(context.credential_generation),
        csrf_hash: Set(
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
                .to_owned(),
        ),
        created_at: Set(fixed_now() - Duration::minutes(1)),
        expires_at: Set(fixed_now() + Duration::hours(1)),
    }
    .insert(&fixture.db)
    .await
    .unwrap();
    let other = service
        .principals
        .resolve(&cookie_headers(other_token), ProofRequirement::Cached)
        .await
        .unwrap();
    let ticket = pending(
        service
            .begin(&other, Provider::Codex, "other-session-completion")
            .await
            .unwrap(),
    );
    let mut missing = ticket.clone();
    missing.attempt_id = "ffffffffffffffffffffffffffffffff".to_owned();

    assert_eq!(
        service
            .complete(&context, &ticket, verified("hidden@example.test"))
            .await,
        Err(AgentError::NotFound)
    );
    assert_eq!(
        service
            .complete(&context, &missing, verified("missing@example.test"))
            .await,
        Err(AgentError::NotFound)
    );
    assert_eq!(
        service.fail(&context, &ticket, "provider_unavailable").await,
        Err(AgentError::NotFound)
    );
    assert_eq!(
        service
            .fail(&context, &missing, "provider_unavailable")
            .await,
        Err(AgentError::NotFound)
    );
    let row = team_agent_connection::Entity::find_by_id(ticket.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "connecting");
    assert!(row.storage_handle.is_none());
}

#[tokio::test]
async fn provider_failure_invalidates_generation_and_queues_owned_cleanup() {
    let (fixture, service, context) = service_and_context().await;
    let ticket = pending(
        service
            .begin(&context, Provider::Codex, "unsupported-billing")
            .await
            .unwrap(),
    );
    assert_eq!(
        service.fail(&context, &ticket, "invented_code").await,
        Err(AgentError::InvalidInput)
    );
    service
        .fail(&context, &ticket, "unsupported_account")
        .await
        .unwrap();
    assert_eq!(
        service.fail(&context, &ticket, "unsupported_account").await,
        Err(AgentError::Conflict)
    );

    let connection = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connection.state, "disconnecting");
    assert_eq!(connection.generation, ticket.generation + 1);
    assert_eq!(connection.error_code.as_deref(), Some("unsupported_account"));
    assert!(connection.storage_handle.is_none());
    let attempt = team_agent_attempt::Entity::find_by_id(&ticket.attempt_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(attempt.state, "failed");
    let connect = team_agent_operation::Entity::find_by_id(&ticket.operation_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connect.state, "abandoned");
    assert!(connect.lease_owner.is_none());
    let cleanup = team_agent_operation::Entity::find()
        .filter(team_agent_operation::Column::ConnectionId.eq(&ticket.connection_id))
        .filter(team_agent_operation::Column::Kind.eq("disconnect"))
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert!(cleanup.request_key.starts_with("__neev_failure_"));
    assert_eq!(cleanup.target_generation, ticket.generation);
    assert_eq!(cleanup.result_generation, ticket.generation + 1);
    assert!(cleanup.target_attempt_id.is_none());
}

#[tokio::test]
async fn cleanup_claim_failure_retry_and_success_are_lease_bound() {
    let (fixture, service, context) = service_and_context().await;
    let login = pending(
        service
            .begin(&context, Provider::Codex, "cleanup-source")
            .await
            .unwrap(),
    );
    let receipt = service
        .cancel(
            &context,
            Provider::Codex,
            &login.attempt_id,
            "cleanup-operation",
        )
        .await
        .unwrap();
    let first = service
        .claim_cleanup(&receipt.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.connection_id, login.connection_id);
    assert_eq!(first.provider, Provider::Codex);
    assert_eq!(first.org_id, context.org_id);
    assert_eq!(first.user_id, context.user_id);
    assert_eq!(first.target_generation, login.generation);
    assert_eq!(first.result_generation, receipt.generation);
    assert_eq!(first.lease_expires_at, fixed_now() + Duration::seconds(30));
    assert!(service
        .claim_cleanup(&receipt.operation_id)
        .await
        .unwrap()
        .is_none());

    service.finish_cleanup(&first, false).await.unwrap();
    let operation = team_agent_operation::Entity::find_by_id(&receipt.operation_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(operation.state, "queued");
    assert!(operation.lease_owner.is_none());
    assert_eq!(operation.error_code.as_deref(), Some("cleanup_failed"));
    let connection = team_agent_connection::Entity::find_by_id(&login.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connection.state, "disconnecting");
    assert_eq!(connection.error_code.as_deref(), Some("cleanup_failed"));

    let retry = service
        .claim_cleanup(&receipt.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(retry.lease_owner, first.lease_owner);
    assert_eq!(
        service.finish_cleanup(&first, true).await,
        Err(AgentError::Conflict)
    );
    service.finish_cleanup(&retry, true).await.unwrap();
    assert_eq!(
        service.finish_cleanup(&retry, true).await,
        Err(AgentError::Conflict)
    );
    let connection = team_agent_connection::Entity::find_by_id(&login.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connection.state, "disconnected");
    assert!(connection.account_label.is_none());
    assert!(connection.checked_at.is_none());
    assert!(connection.error_code.is_none());
    assert!(connection.storage_handle.is_none());
}

#[tokio::test]
async fn cleanup_expired_lease_is_taken_over_and_operation_expiry_is_not_abandonment() {
    let (fixture, service, context) = service_and_context().await;
    let login = pending(
        service
            .begin(&context, Provider::Claude, "cleanup-expiry")
            .await
            .unwrap(),
    );
    let receipt = service
        .cancel(
            &context,
            Provider::Claude,
            &login.attempt_id,
            "cleanup-expiry-operation",
        )
        .await
        .unwrap();
    fixture.clock.advance(Duration::minutes(11));
    let expired_operation_claim = service
        .claim_cleanup(&receipt.operation_id)
        .await
        .unwrap()
        .unwrap();
    fixture.clock.advance(Duration::seconds(31));
    let takeover = service
        .claim_cleanup(&receipt.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(takeover.lease_owner, expired_operation_claim.lease_owner);
    assert_eq!(
        service
            .finish_cleanup(&expired_operation_claim, true)
            .await,
        Err(AgentError::Conflict)
    );
    service.finish_cleanup(&takeover, true).await.unwrap();
}

#[tokio::test]
async fn cleanup_ticket_fields_are_immutable() {
    let (_fixture, service, context) = service_and_context().await;
    let login = pending(
        service
            .begin(&context, Provider::Codex, "cleanup-ticket")
            .await
            .unwrap(),
    );
    let receipt = service
        .cancel(
            &context,
            Provider::Codex,
            &login.attempt_id,
            "cleanup-ticket-operation",
        )
        .await
        .unwrap();
    let ticket = service
        .claim_cleanup(&receipt.operation_id)
        .await
        .unwrap()
        .unwrap();
    let mut variants = Vec::new();
    let mut changed = ticket.clone();
    changed.provider = Provider::Claude;
    variants.push(changed);
    let mut changed = ticket.clone();
    changed.org_id += 1;
    variants.push(changed);
    let mut changed = ticket.clone();
    changed.user_id += 1;
    variants.push(changed);
    let mut changed = ticket.clone();
    changed.target_generation += 1;
    changed.result_generation += 1;
    variants.push(changed);
    let mut changed = ticket.clone();
    changed.result_generation += 1;
    variants.push(changed);
    let mut changed = ticket.clone();
    changed.lease_owner = "22222222222222222222222222222222".to_owned();
    variants.push(changed);
    let mut changed = ticket.clone();
    changed.lease_expires_at += Duration::seconds(1);
    variants.push(changed);
    for changed in variants {
        assert_eq!(
            service.finish_cleanup(&changed, true).await,
            Err(AgentError::Conflict)
        );
    }
    service.finish_cleanup(&ticket, true).await.unwrap();
}

async fn move_connection_to_newer_connected_state(
    fixture: &crate::team_access::test_auth::PrincipalFixture,
    ticket: &CleanupTicket,
) {
    let row = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let mut active = row.into_active_model();
    active.generation = Set(ticket.result_generation + 1);
    active.state = Set("reconnect_required".to_owned());
    active.account_label = Set(Some("newer@example.test".to_owned()));
    active.checked_at = Set(Some(fixed_now() + Duration::minutes(1)));
    active.error_code = Set(Some("reauth_required".to_owned()));
    active.storage_handle = Set(Some("22222222222222222222222222222222".to_owned()));
    active.update(&fixture.db).await.unwrap();
}

async fn claimed_cleanup() -> (
    crate::team_access::test_auth::PrincipalFixture,
    AgentService,
    CleanupTicket,
) {
    let (fixture, service, context) = service_and_context().await;
    let login = pending(
        service
            .begin(&context, Provider::Codex, "stale-cleanup")
            .await
            .unwrap(),
    );
    let receipt = service
        .cancel(
            &context,
            Provider::Codex,
            &login.attempt_id,
            "stale-cleanup-operation",
        )
        .await
        .unwrap();
    let ticket = service
        .claim_cleanup(&receipt.operation_id)
        .await
        .unwrap()
        .unwrap();
    (fixture, service, ticket)
}

#[tokio::test]
async fn stale_cleanup_success_and_failure_never_overwrite_newer_connection_state() {
    for succeeded in [true, false] {
        let (fixture, service, ticket) = claimed_cleanup().await;
        move_connection_to_newer_connected_state(&fixture, &ticket).await;
        service.finish_cleanup(&ticket, succeeded).await.unwrap();
        let row = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.generation, ticket.result_generation + 1);
        assert_eq!(row.state, "reconnect_required");
        assert_eq!(row.account_label.as_deref(), Some("newer@example.test"));
        assert_eq!(row.error_code.as_deref(), Some("reauth_required"));
        assert_eq!(
            row.storage_handle.as_deref(),
            Some("22222222222222222222222222222222")
        );
        let operation = team_agent_operation::Entity::find_by_id(&ticket.operation_id)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(operation.state, if succeeded { "completed" } else { "queued" });
    }
}

#[tokio::test]
async fn expiry_and_startup_recovery_are_idempotent_and_preserve_connected_accounts() {
    let (fixture, service, context) = service_and_context().await;
    let connected = pending(
        service
            .begin(&context, Provider::Claude, "connected-before-recovery")
            .await
            .unwrap(),
    );
    service
        .complete(&context, &connected, verified("connected@example.test"))
        .await
        .unwrap();
    team_agent_attempt::ActiveModel {
        id: Set("stale-connected-attempt".to_owned()),
        connection_id: Set(connected.connection_id.clone()),
        generation: Set(connected.generation + 1),
        session_id: Set(context.session_id.clone()),
        state: Set("pending".to_owned()),
        created_at: Set(fixed_now()),
        expires_at: Set(fixed_now() + Duration::hours(1)),
        updated_at: Set(fixed_now()),
    }
    .insert(&fixture.db)
    .await
    .unwrap();
    let expiring = pending(
        service
            .begin(&context, Provider::Codex, "expires")
            .await
            .unwrap(),
    );
    assert_eq!(service.expire_pending().await.unwrap(), 0);
    fixture.clock.advance(Duration::minutes(10));
    assert_eq!(service.expire_pending().await.unwrap(), 1);
    assert_eq!(service.expire_pending().await.unwrap(), 0);
    let expired = team_agent_attempt::Entity::find_by_id(&expiring.attempt_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(expired.state, "expired");
    let row = team_agent_connection::Entity::find_by_id(&expiring.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.error_code.as_deref(), Some("expired"));
    assert_eq!(row.generation, expiring.generation + 1);
    let connected_row = team_agent_connection::Entity::find_by_id(&connected.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connected_row.state, "connected");
    assert_eq!(connected_row.account_label.as_deref(), Some("connected@example.test"));
    assert_eq!(service.recover_startup().await.unwrap(), 1);
    assert_eq!(service.recover_startup().await.unwrap(), 0);
    assert_eq!(
        team_agent_attempt::Entity::find_by_id("stale-connected-attempt")
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .state,
        "expired"
    );
    let connected_row = team_agent_connection::Entity::find_by_id(&connected.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connected_row.state, "connected");
    assert_eq!(connected_row.account_label.as_deref(), Some("connected@example.test"));

    let (fixture, service, context) = service_and_context().await;
    let pending_login = pending(
        service
            .begin(&context, Provider::Codex, "startup-interruption")
            .await
            .unwrap(),
    );
    assert_eq!(service.recover_startup().await.unwrap(), 1);
    assert_eq!(service.recover_startup().await.unwrap(), 0);
    let attempt = team_agent_attempt::Entity::find_by_id(&pending_login.attempt_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(attempt.state, "expired");
    let row = team_agent_connection::Entity::find_by_id(&pending_login.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.error_code.as_deref(), Some("interrupted"));
}

#[tokio::test]
async fn recovery_generation_overflow_rolls_back_every_mutation() {
    let (fixture, service, context) = service_and_context().await;
    let ticket = pending(
        service
            .begin(&context, Provider::Codex, "recovery-overflow")
            .await
            .unwrap(),
    );
    let connection = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let mut active = connection.into_active_model();
    active.generation = Set(i64::MAX);
    active.update(&fixture.db).await.unwrap();
    let attempt = team_agent_attempt::Entity::find_by_id(&ticket.attempt_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let mut active = attempt.into_active_model();
    active.generation = Set(i64::MAX);
    active.update(&fixture.db).await.unwrap();
    fixture.clock.advance(Duration::minutes(10));
    assert_eq!(service.expire_pending().await, Err(AgentError::Unavailable));
    assert_eq!(
        team_agent_attempt::Entity::find_by_id(&ticket.attempt_id)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .state,
        "pending"
    );
    let row = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.generation, i64::MAX);
    assert_eq!(row.state, "connecting");
    assert_eq!(
        team_agent_operation::Entity::find()
            .filter(team_agent_operation::Column::ConnectionId.eq(ticket.connection_id))
            .filter(team_agent_operation::Column::Kind.eq("cancel"))
            .count(&fixture.db)
            .await
            .unwrap(),
        0
    );
}

/// Requires the local disposable PostgreSQL guard used by team_access tests.
#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn postgres_starts_serialize_different_keys_and_replay_the_same_key() {
    let (name, admin, fixture) = create_postgres_principal_fixture(Vec::new())
        .await
        .expect("PostgreSQL fixture");
    let db = fixture.db.clone();
    let principals = fixture.service();
    let context = resolve(&principals).await;
    let service = Arc::new(AgentService::new(
        db.clone(),
        principals.clone(),
        fixture.clock.clone(),
    ));
    let result: Result<(), String> = async {
        let blocker = db.begin().await.map_err(|error| error.to_string())?;
        lock_postgres_user(&blocker, context.user_id).await?;
        let gate = Arc::new(Barrier::new(3));
        let left = {
            let service = service.clone();
            let context = context.clone();
            let gate = gate.clone();
            tokio::spawn(async move {
                gate.wait().await;
                service.begin(&context, Provider::Codex, "different-left").await
            })
        };
        let right = {
            let service = service.clone();
            let context = context.clone();
            let gate = gate.clone();
            tokio::spawn(async move {
                gate.wait().await;
                service
                    .begin(&context, Provider::Codex, "different-right")
                    .await
            })
        };
        gate.wait().await;
        wait_for_postgres_lock_count(&admin, &name, "FROM \"users\"", 2).await?;
        blocker.commit().await.map_err(|error| error.to_string())?;
        let (left, right) = tokio::join!(left, right);
        let outcomes = [
            left.map_err(|error| error.to_string())?,
            right.map_err(|error| error.to_string())?,
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Ok(BeginOutcome::Pending(_))))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Err(AgentError::Conflict)))
                .count(),
            1
        );

        let blocker = db.begin().await.map_err(|error| error.to_string())?;
        lock_postgres_user(&blocker, context.user_id).await?;
        let gate = Arc::new(Barrier::new(3));
        let left = {
            let service = service.clone();
            let context = context.clone();
            let gate = gate.clone();
            tokio::spawn(async move {
                gate.wait().await;
                service.begin(&context, Provider::Claude, "same-key").await
            })
        };
        let right = {
            let service = service.clone();
            let context = context.clone();
            let gate = gate.clone();
            tokio::spawn(async move {
                gate.wait().await;
                service.begin(&context, Provider::Claude, "same-key").await
            })
        };
        gate.wait().await;
        wait_for_postgres_lock_count(&admin, &name, "FROM \"users\"", 2).await?;
        blocker.commit().await.map_err(|error| error.to_string())?;
        let left = pending(
            left.await
                .map_err(|error| error.to_string())?
                .map_err(|error| format!("same-key left: {error:?}"))?,
        );
        let right = pending(
            right
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| format!("same-key right: {error:?}"))?,
        );
        assert_eq!(left.attempt_id, right.attempt_id);
        assert_eq!(left.operation_id, right.operation_id);
        Ok(())
    }
    .await;
    drop(service);
    drop(principals);
    drop(fixture);
    finish_postgres_test(&name, admin, db, result)
        .await
        .unwrap();
}

/// Requires the local disposable PostgreSQL guard used by team_access tests.
#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn postgres_member_quota_serializes_across_providers() {
    let (name, admin, fixture) = create_postgres_principal_fixture(Vec::new())
        .await
        .expect("PostgreSQL fixture");
    let db = fixture.db.clone();
    let principals = fixture.service();
    let context = resolve(&principals).await;
    let service = Arc::new(AgentService::new(
        db.clone(),
        principals.clone(),
        fixture.clock.clone(),
    ));
    let result: Result<(), String> = async {
        team_agent_connection::ActiveModel {
            id: Set("11111111111111111111111111111111".to_owned()),
            org_id: Set(context.org_id),
            user_id: Set(context.user_id),
            provider: Set("claude".to_owned()),
            generation: Set(4),
            state: Set("disconnected".to_owned()),
            account_label: Set(None),
            checked_at: Set(None),
            error_code: Set(None),
            storage_handle: Set(None),
            created_at: Set(fixed_now() - Duration::minutes(2)),
            updated_at: Set(fixed_now()),
        }
        .insert(&db)
        .await
        .map_err(|error| error.to_string())?;
        for generation in 1..=4 {
            team_agent_attempt::ActiveModel {
                id: Set(format!("{generation:032x}")),
                connection_id: Set("11111111111111111111111111111111".to_owned()),
                generation: Set(generation),
                session_id: Set(context.session_id.clone()),
                state: Set("completed".to_owned()),
                created_at: Set(fixed_now() - Duration::minutes(1)),
                expires_at: Set(fixed_now() + Duration::minutes(9)),
                updated_at: Set(fixed_now()),
            }
            .insert(&db)
            .await
            .map_err(|error| error.to_string())?;
        }
        let blocker = db.begin().await.map_err(|error| error.to_string())?;
        lock_postgres_user(&blocker, context.user_id).await?;
        let gate = Arc::new(Barrier::new(3));
        let claude = {
            let service = service.clone();
            let context = context.clone();
            let gate = gate.clone();
            tokio::spawn(async move {
                gate.wait().await;
                service.begin(&context, Provider::Claude, "quota-claude").await
            })
        };
        let codex = {
            let service = service.clone();
            let context = context.clone();
            let gate = gate.clone();
            tokio::spawn(async move {
                gate.wait().await;
                service.begin(&context, Provider::Codex, "quota-codex").await
            })
        };
        gate.wait().await;
        wait_for_postgres_lock_count(&admin, &name, "FROM \"users\"", 2).await?;
        blocker.commit().await.map_err(|error| error.to_string())?;
        let outcomes = [
            claude.await.map_err(|error| error.to_string())?,
            codex.await.map_err(|error| error.to_string())?,
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Ok(BeginOutcome::Pending(_))))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Err(AgentError::RateLimited)))
                .count(),
            1
        );
        Ok(())
    }
    .await;
    drop(service);
    drop(principals);
    drop(fixture);
    finish_postgres_test(&name, admin, db, result)
        .await
        .unwrap();
}

/// Requires the local disposable PostgreSQL guard used by team_access tests.
#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn postgres_completion_waiting_behind_session_deletion_rejects() {
    let (name, admin, fixture) = create_postgres_principal_fixture(Vec::new())
        .await
        .expect("PostgreSQL fixture");
    let db = fixture.db.clone();
    let principals = fixture.service();
    let context = resolve(&principals).await;
    let service = Arc::new(AgentService::new(
        db.clone(),
        principals.clone(),
        fixture.clock.clone(),
    ));
    let ticket = pending(
        service
            .begin(&context, Provider::Codex, "delete-race")
            .await
            .unwrap(),
    );
    let result: Result<(), String> = async {
        let blocker = db.begin().await.map_err(|error| error.to_string())?;
        blocker
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT token_hash FROM team_session WHERE token_hash = $1 FOR UPDATE".to_owned(),
                [context.session_id.clone().into()],
            ))
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "session blocker found no row".to_owned())?;
        let completion = {
            let service = service.clone();
            let context = context.clone();
            let ticket = ticket.clone();
            tokio::spawn(async move {
                service
                    .complete(&context, &ticket, verified("blocked@example.test"))
                    .await
            })
        };
        wait_for_postgres_lock(&admin, &name, "team_session").await?;
        team_session::Entity::delete_by_id(&context.session_id)
            .exec(&blocker)
            .await
            .map_err(|error| error.to_string())?;
        blocker.commit().await.map_err(|error| error.to_string())?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), completion)
            .await
            .map_err(|_| "completion remained blocked".to_owned())?
            .map_err(|error| error.to_string())?;
        assert_eq!(result, Err(AgentError::Auth(TeamAuthError::Unauthorized)));
        let connection = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
            .one(&db)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing connection".to_owned())?;
        assert_eq!(connection.state, "connecting");
        assert!(connection.storage_handle.is_none());
        Ok(())
    }
    .await;
    drop(service);
    drop(principals);
    drop(fixture);
    finish_postgres_test(&name, admin, db, result)
        .await
        .unwrap();
}

/// Requires the local disposable PostgreSQL guard used by team_access tests.
#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn postgres_completion_holding_session_guard_finishes_before_deletion() {
    let (name, admin, fixture) = create_postgres_principal_fixture(Vec::new())
        .await
        .expect("PostgreSQL fixture");
    let db = fixture.db.clone();
    let principals = fixture.service();
    let context = resolve(&principals).await;
    let service = Arc::new(AgentService::new(
        db.clone(),
        principals.clone(),
        fixture.clock.clone(),
    ));
    let ticket = pending(
        service
            .begin(&context, Provider::Codex, "completion-before-delete")
            .await
            .unwrap(),
    );
    let result: Result<(), String> = async {
        let blocker = db.begin().await.map_err(|error| error.to_string())?;
        blocker
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM team_agent_connection WHERE id = $1 FOR UPDATE".to_owned(),
                [ticket.connection_id.clone().into()],
            ))
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "connection blocker found no row".to_owned())?;
        let completion = {
            let service = service.clone();
            let context = context.clone();
            let ticket = ticket.clone();
            tokio::spawn(async move {
                service
                    .complete(&context, &ticket, verified("first@example.test"))
                    .await
            })
        };
        wait_for_postgres_lock(&admin, &name, "team_agent_connection").await?;
        let deletion = {
            let db = db.clone();
            let session_id = context.session_id.clone();
            tokio::spawn(async move {
                let transaction = db.begin().await.map_err(|error| error.to_string())?;
                transaction
                    .query_one(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "SELECT token_hash FROM team_session WHERE token_hash = $1 FOR UPDATE"
                            .to_owned(),
                        [session_id.clone().into()],
                    ))
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "session deletion found no row".to_owned())?;
                team_session::Entity::delete_by_id(&session_id)
                    .exec(&transaction)
                    .await
                    .map_err(|error| error.to_string())?;
                transaction.commit().await.map_err(|error| error.to_string())
            })
        };
        wait_for_postgres_lock(&admin, &name, "team_session").await?;
        blocker.commit().await.map_err(|error| error.to_string())?;
        let completion = tokio::time::timeout(std::time::Duration::from_secs(5), completion)
            .await
            .map_err(|_| "completion remained blocked".to_owned())?
            .map_err(|error| error.to_string())?;
        if completion.is_err() {
            return Err(format!("completion-first positive control failed: {completion:?}"));
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), deletion)
            .await
            .map_err(|_| "session deletion remained blocked".to_owned())?
            .map_err(|error| error.to_string())??;
        let row = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
            .one(&db)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing completed connection".to_owned())?;
        if row.state != "connected" {
            return Err(format!("completion-first state was {}", row.state));
        }
        Ok(())
    }
    .await;
    drop(service);
    drop(principals);
    drop(fixture);
    finish_postgres_test(&name, admin, db, result)
        .await
        .unwrap();
}

/// Requires the local disposable PostgreSQL guard used by team_access tests.
#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn postgres_completion_waiting_behind_credential_generation_change_rejects() {
    let (name, admin, fixture) = create_postgres_principal_fixture(Vec::new())
        .await
        .expect("PostgreSQL fixture");
    let db = fixture.db.clone();
    let principals = fixture.service();
    let context = resolve(&principals).await;
    let service = Arc::new(AgentService::new(
        db.clone(),
        principals.clone(),
        fixture.clock.clone(),
    ));
    let ticket = pending(
        service
            .begin(&context, Provider::Codex, "generation-race")
            .await
            .unwrap(),
    );
    let result: Result<(), String> = async {
        let blocker = db.begin().await.map_err(|error| error.to_string())?;
        blocker
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM organization_membership WHERE id = $1 FOR UPDATE".to_owned(),
                [ALICE_A_MEMBERSHIP_ID.into()],
            ))
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "membership blocker found no row".to_owned())?;
        let completion = {
            let service = service.clone();
            let context = context.clone();
            let ticket = ticket.clone();
            tokio::spawn(async move {
                service
                    .complete(&context, &ticket, verified("blocked@example.test"))
                    .await
            })
        };
        wait_for_postgres_lock(&admin, &name, "organization_membership").await?;
        organization_membership::Entity::update_many()
            .col_expr(
                organization_membership::Column::CredentialGeneration,
                sea_orm::sea_query::Expr::value(context.credential_generation + 1),
            )
            .filter(organization_membership::Column::Id.eq(ALICE_A_MEMBERSHIP_ID))
            .exec(&blocker)
            .await
            .map_err(|error| error.to_string())?;
        blocker.commit().await.map_err(|error| error.to_string())?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), completion)
            .await
            .map_err(|_| "completion remained blocked".to_owned())?
            .map_err(|error| error.to_string())?;
        assert_eq!(result, Err(AgentError::Auth(TeamAuthError::Unauthorized)));
        let connection = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
            .one(&db)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing connection".to_owned())?;
        assert_eq!(connection.state, "connecting");
        assert!(connection.storage_handle.is_none());
        Ok(())
    }
    .await;
    drop(service);
    drop(principals);
    drop(fixture);
    finish_postgres_test(&name, admin, db, result)
        .await
        .unwrap();
}

/// Requires the local disposable PostgreSQL guard used by team_access tests.
#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn postgres_completion_holding_membership_guard_finishes_before_generation_change() {
    let (name, admin, fixture) = create_postgres_principal_fixture(Vec::new())
        .await
        .expect("PostgreSQL fixture");
    let db = fixture.db.clone();
    let principals = fixture.service();
    let context = resolve(&principals).await;
    let service = Arc::new(AgentService::new(
        db.clone(),
        principals.clone(),
        fixture.clock.clone(),
    ));
    let ticket = pending(
        service
            .begin(&context, Provider::Codex, "completion-before-generation")
            .await
            .unwrap(),
    );
    let result: Result<(), String> = async {
        let blocker = db.begin().await.map_err(|error| error.to_string())?;
        blocker
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM team_agent_connection WHERE id = $1 FOR UPDATE".to_owned(),
                [ticket.connection_id.clone().into()],
            ))
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "connection blocker found no row".to_owned())?;
        let completion = {
            let service = service.clone();
            let context = context.clone();
            let ticket = ticket.clone();
            tokio::spawn(async move {
                service
                    .complete(&context, &ticket, verified("first@example.test"))
                    .await
            })
        };
        wait_for_postgres_lock(&admin, &name, "team_agent_connection").await?;
        let change = {
            let db = db.clone();
            let generation = context.credential_generation + 1;
            tokio::spawn(async move {
                let transaction = db.begin().await.map_err(|error| error.to_string())?;
                transaction
                    .query_one(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "SELECT id FROM organization_membership WHERE id = $1 FOR UPDATE"
                            .to_owned(),
                        [ALICE_A_MEMBERSHIP_ID.into()],
                    ))
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "membership change found no row".to_owned())?;
                organization_membership::Entity::update_many()
                    .col_expr(
                        organization_membership::Column::CredentialGeneration,
                        sea_orm::sea_query::Expr::value(generation),
                    )
                    .filter(organization_membership::Column::Id.eq(ALICE_A_MEMBERSHIP_ID))
                    .exec(&transaction)
                    .await
                    .map_err(|error| error.to_string())?;
                transaction.commit().await.map_err(|error| error.to_string())
            })
        };
        wait_for_postgres_lock(&admin, &name, "organization_membership").await?;
        blocker.commit().await.map_err(|error| error.to_string())?;
        let completion = tokio::time::timeout(std::time::Duration::from_secs(5), completion)
            .await
            .map_err(|_| "completion remained blocked".to_owned())?
            .map_err(|error| error.to_string())?;
        if completion.is_err() {
            return Err(format!("completion-first positive control failed: {completion:?}"));
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), change)
            .await
            .map_err(|_| "credential generation change remained blocked".to_owned())?
            .map_err(|error| error.to_string())??;
        let row = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
            .one(&db)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing completed connection".to_owned())?;
        if row.state != "connected" {
            return Err(format!("completion-first state was {}", row.state));
        }
        Ok(())
    }
    .await;
    drop(service);
    drop(principals);
    drop(fixture);
    finish_postgres_test(&name, admin, db, result)
        .await
        .unwrap();
}

async fn postgres_cancel_completion_order(cancel_first: bool) -> Result<(), String> {
    let (name, admin, fixture) = create_postgres_principal_fixture(Vec::new()).await?;
    let db = fixture.db.clone();
    let principals = fixture.service();
    let context = resolve(&principals).await;
    let service = Arc::new(AgentService::new(
        db.clone(),
        principals.clone(),
        fixture.clock.clone(),
    ));
    let ticket = pending(
        service
            .begin(&context, Provider::Codex, "cancel-complete-race")
            .await
            .map_err(|error| format!("begin: {error:?}"))?,
    );
    let result: Result<(), String> = async {
        let blocker = db.begin().await.map_err(|error| error.to_string())?;
        blocker
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM team_agent_connection WHERE id = $1 FOR UPDATE".to_owned(),
                [ticket.connection_id.clone().into()],
            ))
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "connection blocker found no row".to_owned())?;
        let cancel_task = || {
            let service = service.clone();
            let context = context.clone();
            let ticket = ticket.clone();
            tokio::spawn(async move {
                service
                    .cancel(
                        &context,
                        Provider::Codex,
                        &ticket.attempt_id,
                        "ordered-cancel",
                    )
                    .await
            })
        };
        let complete_task = || {
            let service = service.clone();
            let context = context.clone();
            let ticket = ticket.clone();
            tokio::spawn(async move {
                service
                    .complete(&context, &ticket, verified("winner@example.test"))
                    .await
            })
        };
        if cancel_first {
            let cancel = cancel_task();
            wait_for_postgres_lock(&admin, &name, "team_agent_connection").await?;
            let complete = complete_task();
            blocker.commit().await.map_err(|error| error.to_string())?;
            assert!(cancel.await.map_err(|error| error.to_string())?.is_ok());
            assert_eq!(
                complete.await.map_err(|error| error.to_string())?,
                Err(AgentError::Conflict)
            );
        } else {
            let complete = complete_task();
            wait_for_postgres_lock(&admin, &name, "team_agent_connection").await?;
            let cancel = cancel_task();
            blocker.commit().await.map_err(|error| error.to_string())?;
            assert!(complete.await.map_err(|error| error.to_string())?.is_ok());
            assert!(matches!(
                cancel.await.map_err(|error| error.to_string())?,
                Err(AgentError::Conflict)
            ));
        }
        let row = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
            .one(&db)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing connection".to_owned())?;
        assert_eq!(
            row.state,
            if cancel_first { "disconnecting" } else { "connected" }
        );
        Ok(())
    }
    .await;
    drop(service);
    drop(principals);
    drop(fixture);
    finish_postgres_test(&name, admin, db, result).await
}

/// Requires the local disposable PostgreSQL guard used by team_access tests.
#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn postgres_cancel_and_completion_orderings_never_reactivate_canceled_state() {
    postgres_cancel_completion_order(true).await.unwrap();
    postgres_cancel_completion_order(false).await.unwrap();
}

/// Requires the local disposable PostgreSQL guard used by team_access tests.
#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn postgres_parallel_cleanup_claims_issue_one_live_lease() {
    let (name, admin, fixture) = create_postgres_principal_fixture(Vec::new())
        .await
        .expect("PostgreSQL fixture");
    let db = fixture.db.clone();
    let principals = fixture.service();
    let context = resolve(&principals).await;
    let service = Arc::new(AgentService::new(
        db.clone(),
        principals.clone(),
        fixture.clock.clone(),
    ));
    let login = pending(
        service
            .begin(&context, Provider::Codex, "claim-race")
            .await
            .unwrap(),
    );
    let receipt = service
        .cancel(
            &context,
            Provider::Codex,
            &login.attempt_id,
            "claim-race-cancel",
        )
        .await
        .unwrap();
    let result: Result<(), String> = async {
        let blocker = db.begin().await.map_err(|error| error.to_string())?;
        lock_postgres_user(&blocker, context.user_id).await?;
        let gate = Arc::new(Barrier::new(3));
        let left = {
            let service = service.clone();
            let operation_id = receipt.operation_id.clone();
            let gate = gate.clone();
            tokio::spawn(async move {
                gate.wait().await;
                service.claim_cleanup(&operation_id).await
            })
        };
        let right = {
            let service = service.clone();
            let operation_id = receipt.operation_id.clone();
            let gate = gate.clone();
            tokio::spawn(async move {
                gate.wait().await;
                service.claim_cleanup(&operation_id).await
            })
        };
        gate.wait().await;
        wait_for_postgres_lock_count(&admin, &name, "FROM \"users\"", 2).await?;
        blocker.commit().await.map_err(|error| error.to_string())?;
        let outcomes = [
            left.await.map_err(|error| error.to_string())?,
            right.await.map_err(|error| error.to_string())?,
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Ok(Some(_))))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Ok(None)))
                .count(),
            1
        );
        Ok(())
    }
    .await;
    drop(service);
    drop(principals);
    drop(fixture);
    finish_postgres_test(&name, admin, db, result)
        .await
        .unwrap();
}
