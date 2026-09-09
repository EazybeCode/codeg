use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait,
    PaginatorTrait, QueryFilter, Set, Statement,
};

use super::{
    AgentError, AgentService, BeginOutcome, ConnectionState, LoginTicket, Provider,
};
use crate::{
    db::entities::{
        organization_membership, team_agent_attempt, team_agent_connection,
        team_agent_operation, team_github_credential, team_session,
    },
    team_access::{
        core::TeamAuthError,
        principal::{AuthContext, PrincipalService, ProofRequirement},
        test_auth::{
            cookie_headers, fixed_now, principal_fixture, token_hash, PrincipalFixture,
            ALICE_A_TOKEN, ALICE_B_TOKEN, ALICE_USER_ID, BOB_A_TOKEN, CAROL_B_TOKEN,
        },
    },
};

async fn resolve(
    principals: &PrincipalService,
    token: &str,
) -> AuthContext {
    principals
        .resolve(&cookie_headers(token), ProofRequirement::Cached)
        .await
        .unwrap()
}

fn agent_service(fixture: &PrincipalFixture, principals: Arc<PrincipalService>) -> AgentService {
    AgentService::new(fixture.db.clone(), principals, fixture.clock.clone())
}

fn pending(outcome: BeginOutcome) -> LoginTicket {
    let BeginOutcome::Pending(ticket) = outcome else {
        panic!("expected pending login");
    };
    ticket
}

async fn seed_connection(
    db: &DatabaseConnection,
    id: &str,
    context: &AuthContext,
    provider: Provider,
    generation: i64,
    state: &str,
    account_label: Option<&str>,
    checked_at: Option<DateTime<Utc>>,
    storage_handle: Option<&str>,
) {
    team_agent_connection::ActiveModel {
        id: Set(id.to_owned()),
        org_id: Set(context.org_id),
        user_id: Set(context.user_id),
        provider: Set(match provider {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        }
        .to_owned()),
        generation: Set(generation),
        state: Set(state.to_owned()),
        account_label: Set(account_label.map(str::to_owned)),
        checked_at: Set(checked_at),
        error_code: Set(None),
        storage_handle: Set(storage_handle.map(str::to_owned)),
        created_at: Set(fixed_now() - Duration::hours(1)),
        updated_at: Set(fixed_now()),
    }
    .insert(db)
    .await
    .unwrap();
}

async fn seed_attempt(
    db: &DatabaseConnection,
    id: &str,
    connection_id: &str,
    generation: i64,
    session_id: &str,
    state: &str,
    created_at: DateTime<Utc>,
) {
    team_agent_attempt::ActiveModel {
        id: Set(id.to_owned()),
        connection_id: Set(connection_id.to_owned()),
        generation: Set(generation),
        session_id: Set(session_id.to_owned()),
        state: Set(state.to_owned()),
        created_at: Set(created_at),
        expires_at: Set(created_at + Duration::minutes(10)),
        updated_at: Set(created_at),
    }
    .insert(db)
    .await
    .unwrap();
}

async fn complete_login(db: &DatabaseConnection, ticket: &LoginTicket) {
    let attempt = team_agent_attempt::Entity::find_by_id(&ticket.attempt_id)
        .one(db)
        .await
        .unwrap()
        .unwrap();
    let mut active: team_agent_attempt::ActiveModel = attempt.into();
    active.state = Set("completed".to_owned());
    active.created_at = Set(fixed_now() - Duration::minutes(20));
    active.expires_at = Set(fixed_now() - Duration::minutes(10));
    active.updated_at = Set(fixed_now());
    active.update(db).await.unwrap();

    let operation = team_agent_operation::Entity::find_by_id(&ticket.operation_id)
        .one(db)
        .await
        .unwrap()
        .unwrap();
    let mut active: team_agent_operation::ActiveModel = operation.into();
    active.state = Set("completed".to_owned());
    active.lease_owner = Set(None);
    active.lease_expires_at = Set(None);
    active.created_at = Set(fixed_now() - Duration::minutes(20));
    active.expires_at = Set(fixed_now() - Duration::minutes(10));
    active.updated_at = Set(fixed_now());
    active.update(db).await.unwrap();

    let connection = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
        .one(db)
        .await
        .unwrap()
        .unwrap();
    let mut active: team_agent_connection::ActiveModel = connection.into();
    active.state = Set("connected".to_owned());
    active.account_label = Set(Some("alice@example.test".to_owned()));
    active.checked_at = Set(Some(fixed_now()));
    active.storage_handle = Set(Some("native-profile-handle".to_owned()));
    active.updated_at = Set(fixed_now());
    active.update(db).await.unwrap();
}

#[tokio::test]
async fn login_attempt_is_visible_only_to_the_initiating_member_org_and_session() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let alice_a = resolve(&principals, ALICE_A_TOKEN).await;
    let bob_a = resolve(&principals, BOB_A_TOKEN).await;
    let alice_b = resolve(&principals, ALICE_B_TOKEN).await;
    let service = agent_service(&fixture, principals.clone());

    let first = pending(
        service
            .begin(&alice_a, Provider::Codex, "first-login")
            .await
            .unwrap(),
    );
    assert_eq!(
        service
            .attempt(&bob_a, Provider::Codex, &first.attempt_id)
            .await,
        Err(AgentError::NotFound)
    );
    assert_eq!(
        service
            .attempt(&alice_b, Provider::Codex, &first.attempt_id)
            .await,
        Err(AgentError::NotFound)
    );
    assert_eq!(
        service
            .attempt(&bob_a, Provider::Codex, "missing-attempt")
            .await,
        Err(AgentError::NotFound)
    );
    assert_eq!(
        service
            .attempt(&alice_a, Provider::Claude, &first.attempt_id)
            .await,
        Err(AgentError::NotFound)
    );
    assert_eq!(
        service
            .attempt(&alice_a, Provider::Codex, &first.attempt_id)
            .await
            .unwrap()
            .state,
        "pending"
    );

    let bob_ticket = pending(
        service
            .begin(&bob_a, Provider::Claude, "bob-login")
            .await
            .unwrap(),
    );
    assert_eq!(
        service
            .attempt(&alice_a, Provider::Claude, &bob_ticket.attempt_id)
            .await,
        Err(AgentError::NotFound)
    );
}

#[tokio::test]
async fn same_person_other_session_cannot_read_or_replay_a_login() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let alice = resolve(&principals, ALICE_A_TOKEN).await;
    let other_token = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    team_session::ActiveModel {
        token_hash: Set(token_hash(other_token)),
        user_id: Set(alice.user_id),
        org_id: Set(alice.org_id),
        membership_id: Set(alice.membership_id),
        authorization_revision: Set(alice.authorization_revision),
        credential_generation: Set(alice.credential_generation),
        csrf_hash: Set("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into()),
        created_at: Set(fixed_now() - Duration::minutes(1)),
        expires_at: Set(fixed_now() + Duration::hours(1)),
    }
    .insert(&fixture.db)
    .await
    .unwrap();
    let other_session = resolve(&principals, other_token).await;
    let service = agent_service(&fixture, principals);
    let ticket = pending(
        service
            .begin(&alice, Provider::Codex, "session-bound")
            .await
            .unwrap(),
    );

    assert_eq!(
        service
            .attempt(&other_session, Provider::Codex, &ticket.attempt_id)
            .await,
        Err(AgentError::NotFound)
    );
    assert!(matches!(
        service
            .begin(&other_session, Provider::Codex, "session-bound")
            .await,
        Err(AgentError::Conflict)
    ));
}

#[tokio::test]
async fn list_is_ordered_virtual_scoped_and_bounded() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let alice_a = resolve(&principals, ALICE_A_TOKEN).await;
    let alice_b = resolve(&principals, ALICE_B_TOKEN).await;
    let bob_a = resolve(&principals, BOB_A_TOKEN).await;
    let carol_b = resolve(&principals, CAROL_B_TOKEN).await;
    let service = agent_service(&fixture, principals);

    for context in [&alice_a, &alice_b, &bob_a, &carol_b] {
        let views = service.list(context).await.unwrap();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].provider, Provider::Claude);
        assert_eq!(views[0].state, ConnectionState::Disconnected);
        assert_eq!(views[1].provider, Provider::Codex);
        assert_eq!(views[1].state, ConnectionState::Disconnected);
    }
    assert_eq!(
        team_agent_connection::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        0
    );

    seed_connection(
        &fixture.db,
        "alice-a-codex",
        &alice_a,
        Provider::Codex,
        4,
        "connected",
        Some("alice@example.test"),
        Some(fixed_now()),
        Some("opaque-handle"),
    )
    .await;
    let views = service.list(&alice_a).await.unwrap();
    assert_eq!(views[1].state, ConnectionState::Connected);
    assert!(views[1].account_label.as_deref() == Some("alice@example.test"));
    assert!(views[1].checked_at == Some(fixed_now()));
    assert_eq!(service.list(&bob_a).await.unwrap()[1].state, ConnectionState::Disconnected);
    assert_eq!(service.list(&alice_b).await.unwrap()[1].state, ConnectionState::Disconnected);
}

#[tokio::test]
async fn begin_replays_pending_and_settled_without_new_rows_or_quota_debit() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let alice = resolve(&principals, ALICE_A_TOKEN).await;
    let service = agent_service(&fixture, principals);

    let first = pending(
        service
            .begin(&alice, Provider::Codex, "same-login")
            .await
            .unwrap(),
    );
    let replay = pending(
        service
            .begin(&alice, Provider::Codex, "same-login")
            .await
            .unwrap(),
    );
    assert!(first.attempt_id == replay.attempt_id);
    assert!(first.operation_id == replay.operation_id);
    assert!(first.lease_owner == replay.lease_owner);
    assert_eq!(first.generation, replay.generation);
    assert_eq!(
        team_agent_attempt::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        team_agent_operation::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        team_agent_connection::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        1
    );

    complete_login(&fixture.db, &first).await;
    let settled = service
        .begin(&alice, Provider::Codex, "same-login")
        .await
        .unwrap();
    let BeginOutcome::Settled(view) = settled else {
        panic!("completed login must replay a settled projection");
    };
    assert_eq!(view.state, ConnectionState::Connected);
    assert!(view.account_label.as_deref() == Some("alice@example.test"));
    assert_eq!(
        team_agent_attempt::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        team_agent_operation::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        1
    );

    let connection = team_agent_connection::Entity::find_by_id(&first.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let mut active: team_agent_connection::ActiveModel = connection.into();
    active.generation = Set(first.generation + 1);
    active.updated_at = Set(fixed_now());
    active.update(&fixture.db).await.unwrap();
    assert!(matches!(
        service
            .begin(&alice, Provider::Codex, "same-login")
            .await,
        Err(AgentError::Conflict)
    ));
}

#[tokio::test]
async fn expired_pending_replay_conflicts_without_replacing_its_identity() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let alice = resolve(&principals, ALICE_A_TOKEN).await;
    let service = agent_service(&fixture, principals);
    let ticket = pending(
        service
            .begin(&alice, Provider::Codex, "expiring-login")
            .await
            .unwrap(),
    );
    let attempt = team_agent_attempt::Entity::find_by_id(&ticket.attempt_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let mut active: team_agent_attempt::ActiveModel = attempt.into();
    active.created_at = Set(fixed_now() - Duration::minutes(20));
    active.expires_at = Set(fixed_now() - Duration::minutes(10));
    active.updated_at = Set(fixed_now());
    active.update(&fixture.db).await.unwrap();
    let operation = team_agent_operation::Entity::find_by_id(&ticket.operation_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let mut active: team_agent_operation::ActiveModel = operation.into();
    active.created_at = Set(fixed_now() - Duration::minutes(20));
    active.expires_at = Set(fixed_now() - Duration::minutes(10));
    active.lease_expires_at = Set(Some(fixed_now() - Duration::minutes(10)));
    active.updated_at = Set(fixed_now());
    active.update(&fixture.db).await.unwrap();

    assert!(matches!(
        service
            .begin(&alice, Provider::Codex, "expiring-login")
            .await,
        Err(AgentError::Conflict)
    ));
    assert_eq!(
        team_agent_attempt::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        1
    );
    let connection = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connection.generation, ticket.generation);
    assert_eq!(connection.state, "connecting");
}

#[tokio::test]
async fn request_keys_and_generation_overflow_fail_without_mutation() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let alice = resolve(&principals, ALICE_A_TOKEN).await;
    let service = agent_service(&fixture, principals);

    for key in ["", "bad.key", "déjà", "__neev_recovery"] {
        assert!(matches!(
            service.begin(&alice, Provider::Codex, key).await,
            Err(AgentError::InvalidInput)
        ));
    }
    let too_long = "a".repeat(65);
    assert!(matches!(
        service.begin(&alice, Provider::Codex, &too_long).await,
        Err(AgentError::InvalidInput)
    ));
    assert_eq!(
        team_agent_connection::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        0
    );

    seed_connection(
        &fixture.db,
        "overflow-connection",
        &alice,
        Provider::Codex,
        i64::MAX,
        "disconnected",
        None,
        None,
        None,
    )
    .await;
    assert!(matches!(
        service.begin(&alice, Provider::Codex, "valid-key").await,
        Err(AgentError::Unavailable)
    ));
    let row = team_agent_connection::Entity::find_by_id("overflow-connection")
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.generation, i64::MAX);
    assert_eq!(row.state, "disconnected");
}

#[tokio::test]
async fn quota_is_user_wide_cross_org_provider_and_excludes_exact_boundary() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let alice_a = resolve(&principals, ALICE_A_TOKEN).await;
    let alice_b = resolve(&principals, ALICE_B_TOKEN).await;
    let service = agent_service(&fixture, principals);
    seed_connection(
        &fixture.db,
        "quota-a",
        &alice_a,
        Provider::Claude,
        3,
        "disconnected",
        None,
        None,
        None,
    )
    .await;
    seed_connection(
        &fixture.db,
        "quota-b",
        &alice_b,
        Provider::Codex,
        2,
        "disconnected",
        None,
        None,
        None,
    )
    .await;
    for generation in 1..=3 {
        seed_attempt(
            &fixture.db,
            &format!("quota-a-{generation}"),
            "quota-a",
            generation,
            &alice_a.session_id,
            "completed",
            fixed_now() - Duration::minutes(14),
        )
        .await;
    }
    for generation in 1..=2 {
        seed_attempt(
            &fixture.db,
            &format!("quota-b-{generation}"),
            "quota-b",
            generation,
            &alice_b.session_id,
            "completed",
            fixed_now() - Duration::minutes(1),
        )
        .await;
    }
    assert!(matches!(
        service.begin(&alice_a, Provider::Codex, "quota-hit").await,
        Err(AgentError::RateLimited)
    ));

    let boundary_fixture = principal_fixture(Vec::new()).await;
    let principals = boundary_fixture.service();
    let alice_a = resolve(&principals, ALICE_A_TOKEN).await;
    let alice_b = resolve(&principals, ALICE_B_TOKEN).await;
    let service = agent_service(&boundary_fixture, principals);
    seed_connection(
        &boundary_fixture.db,
        "boundary-a",
        &alice_a,
        Provider::Claude,
        3,
        "disconnected",
        None,
        None,
        None,
    )
    .await;
    seed_connection(
        &boundary_fixture.db,
        "boundary-b",
        &alice_b,
        Provider::Codex,
        2,
        "disconnected",
        None,
        None,
        None,
    )
    .await;
    seed_attempt(
        &boundary_fixture.db,
        "at-boundary",
        "boundary-a",
        1,
        &alice_a.session_id,
        "completed",
        fixed_now() - Duration::minutes(15),
    )
    .await;
    for generation in 2..=3 {
        seed_attempt(
            &boundary_fixture.db,
            &format!("boundary-a-{generation}"),
            "boundary-a",
            generation,
            &alice_a.session_id,
            "completed",
            fixed_now() - Duration::minutes(14),
        )
        .await;
    }
    for generation in 1..=2 {
        seed_attempt(
            &boundary_fixture.db,
            &format!("boundary-b-{generation}"),
            "boundary-b",
            generation,
            &alice_b.session_id,
            "completed",
            fixed_now() - Duration::minutes(1),
        )
        .await;
    }
    let ticket = pending(
        service
            .begin(&alice_a, Provider::Codex, "boundary-allows")
            .await
            .unwrap(),
    );
    assert_eq!(ticket.generation, 1);
}

#[tokio::test]
async fn simultaneous_sqlite_starts_replay_one_ticket() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let alice = resolve(&principals, ALICE_A_TOKEN).await;
    let service = Arc::new(agent_service(&fixture, principals));
    let left_service = service.clone();
    let right_service = service.clone();
    let left_context = alice.clone();
    let right_context = alice.clone();
    let (left, right) = tokio::join!(
        async move {
            left_service
                .begin(&left_context, Provider::Codex, "simultaneous")
                .await
        },
        async move {
            right_service
                .begin(&right_context, Provider::Codex, "simultaneous")
                .await
        }
    );
    let left = pending(left.unwrap());
    let right = pending(right.unwrap());
    assert!(left.attempt_id == right.attempt_id);
    assert!(left.operation_id == right.operation_id);
    assert_eq!(
        team_agent_attempt::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn cancel_is_scoped_idempotent_and_records_invalidation_metadata() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let alice = resolve(&principals, ALICE_A_TOKEN).await;
    let bob = resolve(&principals, BOB_A_TOKEN).await;
    let service = agent_service(&fixture, principals);
    let ticket = pending(
        service
            .begin(&alice, Provider::Codex, "cancel-source")
            .await
            .unwrap(),
    );
    assert!(matches!(
        service
            .cancel(&bob, Provider::Codex, &ticket.attempt_id, "cancel-key")
            .await,
        Err(AgentError::NotFound)
    ));
    let receipt = service
        .cancel(&alice, Provider::Codex, &ticket.attempt_id, "cancel-key")
        .await
        .unwrap();
    assert_eq!(receipt.generation, ticket.generation + 1);
    let replay = service
        .cancel(&alice, Provider::Codex, &ticket.attempt_id, "cancel-key")
        .await
        .unwrap();
    assert!(receipt.operation_id == replay.operation_id);
    assert_eq!(receipt.generation, replay.generation);
    assert!(matches!(
        service
            .cancel(&alice, Provider::Codex, "another-target", "cancel-key")
            .await,
        Err(AgentError::Conflict)
    ));
    assert!(matches!(
        service
            .begin(&alice, Provider::Codex, "cannot-overwrite-cleanup")
            .await,
        Err(AgentError::Conflict)
    ));
    assert!(matches!(
        service
            .begin(&alice, Provider::Codex, "cancel-source")
            .await,
        Err(AgentError::Conflict)
    ));

    let connection = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connection.generation, ticket.generation + 1);
    assert_eq!(connection.state, "disconnecting");
    assert!(connection.account_label.is_none());
    assert!(connection.checked_at.is_none());
    let attempt = team_agent_attempt::Entity::find_by_id(&ticket.attempt_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(attempt.state, "canceled");
    let connect = team_agent_operation::Entity::find_by_id(&ticket.operation_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connect.state, "abandoned");
    assert!(connect.lease_owner.is_none());
    assert!(connect.lease_expires_at.is_none());
    let cancel = team_agent_operation::Entity::find_by_id(&receipt.operation_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancel.kind, "cancel");
    assert_eq!(cancel.state, "queued");
    assert_eq!(cancel.target_attempt_id.as_deref(), Some(ticket.attempt_id.as_str()));
    assert_eq!(cancel.target_generation, ticket.generation);
    assert_eq!(cancel.result_generation, ticket.generation + 1);
    assert_eq!(cancel.expires_at, fixed_now() + Duration::minutes(10));
}

#[tokio::test]
async fn disconnect_invalidates_login_preserves_handle_and_replays_receipt() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let alice = resolve(&principals, ALICE_A_TOKEN).await;
    let service = agent_service(&fixture, principals);
    let ticket = pending(
        service
            .begin(&alice, Provider::Codex, "disconnect-source")
            .await
            .unwrap(),
    );
    let receipt = service
        .disconnect(&alice, Provider::Codex, "disconnect-key")
        .await
        .unwrap();
    let replay = service
        .disconnect(&alice, Provider::Codex, "disconnect-key")
        .await
        .unwrap();
    assert!(receipt.operation_id == replay.operation_id);
    assert_eq!(receipt.generation, ticket.generation + 1);
    assert!(matches!(
        service
            .disconnect(&alice, Provider::Codex, "different-cleanup")
            .await,
        Err(AgentError::Conflict)
    ));
    let connection = team_agent_connection::Entity::find_by_id(&ticket.connection_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connection.state, "disconnecting");
    assert_eq!(connection.generation, ticket.generation + 1);
    let attempt = team_agent_attempt::Entity::find_by_id(&ticket.attempt_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(attempt.state, "canceled");
    let connect = team_agent_operation::Entity::find_by_id(&ticket.operation_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(connect.state, "abandoned");

    let connected_fixture = principal_fixture(Vec::new()).await;
    let principals = connected_fixture.service();
    let alice = resolve(&principals, ALICE_A_TOKEN).await;
    let service = agent_service(&connected_fixture, principals);
    seed_connection(
        &connected_fixture.db,
        "connected-cleanup",
        &alice,
        Provider::Claude,
        7,
        "connected",
        Some("private-account-label"),
        Some(fixed_now()),
        Some("opaque-handle"),
    )
    .await;
    let receipt = service
        .disconnect(&alice, Provider::Claude, "connected-disconnect")
        .await
        .unwrap();
    assert_eq!(receipt.generation, 8);
    let row = team_agent_connection::Entity::find_by_id("connected-cleanup")
        .one(&connected_fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "disconnecting");
    assert!(row.account_label.is_none());
    assert!(row.checked_at.is_none());
    assert!(row.storage_handle.as_deref() == Some("opaque-handle"));
}

#[tokio::test]
async fn disconnected_without_work_records_an_immediate_disconnect() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let alice = resolve(&principals, ALICE_A_TOKEN).await;
    let service = agent_service(&fixture, principals);
    assert!(matches!(
        service
            .disconnect(&alice, Provider::Claude, "absent")
            .await,
        Err(AgentError::NotFound)
    ));
    seed_connection(
        &fixture.db,
        "idle-disconnected",
        &alice,
        Provider::Claude,
        2,
        "disconnected",
        None,
        None,
        None,
    )
    .await;
    let receipt = service
        .disconnect(&alice, Provider::Claude, "immediate")
        .await
        .unwrap();
    assert_eq!(receipt.generation, 3);
    let operation = team_agent_operation::Entity::find_by_id(receipt.operation_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(operation.state, "completed");
    assert_eq!(operation.target_generation, 2);
    assert_eq!(operation.result_generation, 3);
    assert_eq!(operation.expires_at, fixed_now() + Duration::minutes(10));
    let row = team_agent_connection::Entity::find_by_id("idle-disconnected")
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "disconnected");
    assert_eq!(row.generation, 3);
}

#[tokio::test]
async fn all_methods_recheck_changed_authority_and_session_or_credential_staleness() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let alice = resolve(&principals, ALICE_A_TOKEN).await;
    let service = agent_service(&fixture, principals);
    let ticket = pending(
        service
            .begin(&alice, Provider::Codex, "before-revoke")
            .await
            .unwrap(),
    );
    organization_membership::Entity::update_many()
        .col_expr(
            organization_membership::Column::State,
            sea_orm::sea_query::Expr::value("revoked"),
        )
        .filter(organization_membership::Column::Id.eq(alice.membership_id))
        .exec(&fixture.db)
        .await
        .unwrap();
    assert!(matches!(
        service.list(&alice).await,
        Err(AgentError::Auth(TeamAuthError::Unauthorized))
    ));
    assert!(matches!(
        service
            .attempt(&alice, Provider::Codex, &ticket.attempt_id)
            .await,
        Err(AgentError::Auth(TeamAuthError::Unauthorized))
    ));
    assert!(matches!(
        service.begin(&alice, Provider::Claude, "after-revoke").await,
        Err(AgentError::Auth(TeamAuthError::Unauthorized))
    ));
    assert!(matches!(
        service
            .cancel(&alice, Provider::Codex, &ticket.attempt_id, "after-revoke")
            .await,
        Err(AgentError::Auth(TeamAuthError::Unauthorized))
    ));
    assert!(matches!(
        service
            .disconnect(&alice, Provider::Codex, "after-revoke")
            .await,
        Err(AgentError::Auth(TeamAuthError::Unauthorized))
    ));
    assert_eq!(
        team_agent_connection::Entity::find_by_id(&ticket.connection_id)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .generation,
        ticket.generation
    );

    let session_fixture = principal_fixture(Vec::new()).await;
    let principals = session_fixture.service();
    let alice = resolve(&principals, ALICE_A_TOKEN).await;
    let service = agent_service(&session_fixture, principals);
    team_session::Entity::delete_by_id(&alice.session_id)
        .exec(&session_fixture.db)
        .await
        .unwrap();
    assert!(matches!(
        service.list(&alice).await,
        Err(AgentError::Auth(TeamAuthError::Unauthorized))
    ));

    let credential_fixture = principal_fixture(Vec::new()).await;
    let principals = credential_fixture.service();
    let alice = resolve(&principals, ALICE_A_TOKEN).await;
    let service = agent_service(&credential_fixture, principals);
    team_github_credential::Entity::update_many()
        .col_expr(
            team_github_credential::Column::State,
            sea_orm::sea_query::Expr::value("reauth_required"),
        )
        .col_expr(
            team_github_credential::Column::EncryptedPayload,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .filter(team_github_credential::Column::UserId.eq(ALICE_USER_ID))
        .exec(&credential_fixture.db)
        .await
        .unwrap();
    assert!(matches!(
        service.list(&alice).await,
        Err(AgentError::Auth(TeamAuthError::Unauthorized))
    ));
}

#[tokio::test]
async fn malformed_stored_projection_fails_closed() {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let alice = resolve(&principals, ALICE_A_TOKEN).await;
    let service = agent_service(&fixture, principals);
    seed_connection(
        &fixture.db,
        "malformed-row",
        &alice,
        Provider::Claude,
        1,
        "disconnected",
        None,
        None,
        None,
    )
    .await;
    fixture
        .db
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA ignore_check_constraints = ON".to_owned(),
        ))
        .await
        .unwrap();
    fixture
        .db
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "UPDATE team_agent_connection SET state = 'mystery' WHERE id = 'malformed-row'"
                .to_owned(),
        ))
        .await
        .unwrap();
    fixture
        .db
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA ignore_check_constraints = OFF".to_owned(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        service.list(&alice).await,
        Err(AgentError::Unavailable)
    ));
}
