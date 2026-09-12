use super::{
    intents,
    test_support::Harness,
    types::{AgentError, BeginOutcome, ConnectionState, LoginTicket, Provider},
};
use crate::neev::{db, error::AuthError, test_support as h};
use sea_orm::{ConnectionTrait, DatabaseConnection};

async fn begin_ticket(
    h: &Harness,
    actor: &crate::neev::types::OrgContext,
    provider: Provider,
    key: &str,
) -> LoginTicket {
    let BeginOutcome::Pending(ticket) = intents::begin(&h.state, actor, provider, key)
        .await
        .unwrap()
    else {
        panic!("new login must be pending");
    };
    ticket
}

async fn count_for_connection(db: &DatabaseConnection, table: &str, connection_id: &str) -> i64 {
    db::one(
        db,
        &format!("SELECT COUNT(*) AS n FROM {table} WHERE connection_id=$1"),
        vec![connection_id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "n")
    .unwrap()
}

async fn insert_disconnected(
    h: &Harness,
    actor: &crate::neev::types::OrgContext,
    provider: Provider,
    generation: i64,
) -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    db::execute(
        &h.state.database,
        "INSERT INTO neev_agent_connections(id,org_id,user_id,provider,generation,state,account_label,checked_at,error_code,storage_handle,created_at,updated_at) VALUES($1,$2,$3,$4,$5,'disconnected',NULL,NULL,NULL,NULL,1700000000,1700000000)",
        vec![id.clone().into(), actor.org_id.into(), actor.identity.user_id.into(), super::store::provider_name(provider).into(), generation.into()],
    )
    .await
    .unwrap();
    id
}

fn assert_auth<T>(result: Result<T, AgentError>, expected: AuthError) {
    match result {
        Err(AgentError::Auth(actual)) => assert_eq!(actual, expected),
        _ => panic!("intent must reject stale native authority"),
    }
}

async fn assert_all_intents_denied(h: &Harness, ticket: &LoginTicket, expected: AuthError) {
    assert_auth(intents::list(&h.state, &h.alice_a).await, expected);
    assert_auth(
        intents::attempt(&h.state, &h.alice_a, ticket.provider, &ticket.attempt_id).await,
        expected,
    );
    assert_auth(
        intents::begin(&h.state, &h.alice_a, Provider::Claude, "authority-begin").await,
        expected,
    );
    assert_auth(
        intents::cancel(
            &h.state,
            &h.alice_a,
            ticket.provider,
            &ticket.attempt_id,
            "authority-cancel",
        )
        .await,
        expected,
    );
    assert_auth(
        intents::disconnect(
            &h.state,
            &h.alice_a,
            ticket.provider,
            "authority-disconnect",
        )
        .await,
        expected,
    );
}

#[tokio::test]
async fn pending_attempt_is_visible_only_to_its_exact_native_session() {
    let h = Harness::new(h::database().await).await;
    let BeginOutcome::Pending(ticket) =
        intents::begin(&h.state, &h.alice_a, Provider::Codex, "first-login")
            .await
            .unwrap()
    else {
        panic!("new login must be pending");
    };
    for actor in [&h.alice_b, &h.bob_a, &h.admin_a, &h.alice_second_session] {
        assert!(matches!(
            intents::attempt(&h.state, actor, Provider::Codex, &ticket.attempt_id).await,
            Err(AgentError::NotFound)
        ));
    }
    assert_eq!(
        intents::attempt(&h.state, &h.alice_a, Provider::Codex, &ticket.attempt_id)
            .await
            .unwrap()
            .state,
        "pending"
    );
}

#[tokio::test]
async fn list_is_stable_and_synthesizes_only_own_disconnected_providers() {
    let h = Harness::new(h::database().await).await;
    let initial = intents::list(&h.state, &h.alice_a).await.unwrap();
    assert_eq!(initial.len(), 2);
    assert_eq!(initial[0].provider, Provider::Claude);
    assert_eq!(initial[1].provider, Provider::Codex);
    assert!(initial.iter().all(|view| {
        view.state == ConnectionState::Disconnected
            && view.account_label.is_none()
            && view.checked_at.is_none()
            && view.error_code.is_none()
    }));
    assert_eq!(
        h::count(&h.state.database, "neev_agent_connections").await,
        0
    );

    let ticket = begin_ticket(&h, &h.alice_a, Provider::Codex, "own-view").await;
    db::execute(
        &h.state.database,
        "UPDATE neev_agent_connections SET state='connected',account_label='Alice synthetic',checked_at=1700000001,storage_handle=$1,updated_at=1700000001 WHERE id=$2",
        vec![uuid::Uuid::new_v4().simple().to_string().into(), ticket.connection_id.into()],
    )
    .await
    .unwrap();
    let own = intents::list(&h.state, &h.alice_a).await.unwrap();
    assert_eq!(own[1].state, ConnectionState::Connected);
    assert_eq!(own[1].account_label.as_deref(), Some("Alice synthetic"));
    assert_eq!(own[1].checked_at, Some(1_700_000_001));
    let bob = intents::list(&h.state, &h.bob_a).await.unwrap();
    assert!(bob.iter().all(|view| view.account_label.is_none()));
}

#[tokio::test]
async fn begin_same_key_replays_without_new_rows_or_quota_charge() {
    let h = Harness::new(h::database().await).await;
    let first = begin_ticket(&h, &h.alice_a, Provider::Claude, "same-key").await;
    let second = begin_ticket(&h, &h.alice_a, Provider::Claude, "same-key").await;
    assert_eq!(second.connection_id, first.connection_id);
    assert_eq!(second.attempt_id, first.attempt_id);
    assert_eq!(second.operation_id, first.operation_id);
    assert_eq!(second.generation, first.generation);
    assert_eq!(
        count_for_connection(
            &h.state.database,
            "neev_agent_attempts",
            &first.connection_id
        )
        .await,
        1
    );
    assert_eq!(
        count_for_connection(
            &h.state.database,
            "neev_agent_operations",
            &first.connection_id
        )
        .await,
        1
    );
    assert!(matches!(
        intents::begin(
            &h.state,
            &h.alice_second_session,
            Provider::Claude,
            "same-key"
        )
        .await,
        Err(AgentError::Conflict)
    ));
    assert!(matches!(
        intents::begin(&h.state, &h.alice_a, Provider::Claude, "different-key").await,
        Err(AgentError::Conflict)
    ));
}

#[tokio::test]
async fn settled_begin_replay_ignores_old_deadlines_and_does_not_charge_again() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin_ticket(&h, &h.alice_a, Provider::Claude, "settled-key").await;
    db::execute(
        &h.state.database,
        "UPDATE neev_agent_connections SET state='connected',account_label='Settled account',checked_at=1700000001,storage_handle=$1,updated_at=1700000001 WHERE id=$2",
        vec![uuid::Uuid::new_v4().simple().to_string().into(), ticket.connection_id.clone().into()],
    )
    .await
    .unwrap();
    db::execute(
        &h.state.database,
        "UPDATE neev_agent_attempts SET state='completed',updated_at=1700000001 WHERE id=$1",
        vec![ticket.attempt_id.into()],
    )
    .await
    .unwrap();
    db::execute(
        &h.state.database,
        "UPDATE neev_agent_operations SET state='completed',lease_owner=NULL,lease_expires_at=NULL,updated_at=1700000001 WHERE id=$1",
        vec![ticket.operation_id.into()],
    )
    .await
    .unwrap();
    h.set_now(1_700_001_000);
    let BeginOutcome::Settled(view) =
        intents::begin(&h.state, &h.alice_a, Provider::Claude, "settled-key")
            .await
            .unwrap()
    else {
        panic!("completed receipt must settle");
    };
    assert_eq!(view.state, ConnectionState::Connected);
    assert_eq!(view.account_label.as_deref(), Some("Settled account"));
    assert_eq!(h::count(&h.state.database, "neev_agent_attempts").await, 1);
}

#[tokio::test]
async fn request_keys_enforce_ascii_length_and_reserved_namespace() {
    let h = Harness::new(h::database().await).await;
    for key in ["", "contains space", "ümlaut", "__neev_internal"] {
        assert!(matches!(
            intents::begin(&h.state, &h.alice_a, Provider::Claude, key).await,
            Err(AgentError::InvalidInput)
        ));
    }
    let too_long = "a".repeat(65);
    assert!(matches!(
        intents::begin(&h.state, &h.alice_a, Provider::Claude, &too_long).await,
        Err(AgentError::InvalidInput)
    ));
    begin_ticket(&h, &h.alice_a, Provider::Claude, "a").await;
    begin_ticket(&h, &h.alice_a, Provider::Codex, &"z".repeat(64)).await;
}

#[tokio::test]
async fn cancel_advances_generation_invalidates_attempt_and_replays_exact_target() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin_ticket(&h, &h.alice_a, Provider::Codex, "cancel-start").await;
    let receipt = intents::cancel(
        &h.state,
        &h.alice_a,
        Provider::Codex,
        &ticket.attempt_id,
        "cancel-key",
    )
    .await
    .unwrap();
    assert_eq!(receipt.generation, ticket.generation + 1);
    assert_eq!(
        intents::attempt(&h.state, &h.alice_a, Provider::Codex, &ticket.attempt_id)
            .await
            .unwrap()
            .state,
        "canceled"
    );
    assert_eq!(
        intents::list(&h.state, &h.alice_a).await.unwrap()[1].state,
        ConnectionState::Disconnecting
    );
    let replay = intents::cancel(
        &h.state,
        &h.alice_a,
        Provider::Codex,
        &ticket.attempt_id,
        "cancel-key",
    )
    .await
    .unwrap();
    assert_eq!(replay.operation_id, receipt.operation_id);
    assert_eq!(replay.generation, receipt.generation);
    assert!(matches!(
        intents::cancel(
            &h.state,
            &h.alice_a,
            Provider::Codex,
            &uuid::Uuid::new_v4().simple().to_string(),
            "cancel-key"
        )
        .await,
        Err(AgentError::Conflict)
    ));
    assert!(matches!(
        intents::cancel(
            &h.state,
            &h.alice_second_session,
            Provider::Codex,
            &ticket.attempt_id,
            "other-session"
        )
        .await,
        Err(AgentError::NotFound)
    ));
}

#[tokio::test]
async fn disconnect_invalidates_login_and_replays_without_replacing_cleanup() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin_ticket(&h, &h.alice_a, Provider::Claude, "disconnect-start").await;
    let receipt = intents::disconnect(&h.state, &h.alice_a, Provider::Claude, "disconnect-key")
        .await
        .unwrap();
    assert_eq!(receipt.generation, ticket.generation + 1);
    assert_eq!(
        intents::attempt(&h.state, &h.alice_a, Provider::Claude, &ticket.attempt_id)
            .await
            .unwrap()
            .state,
        "canceled"
    );
    h.set_now(1_700_000_601);
    let replay = intents::disconnect(&h.state, &h.alice_a, Provider::Claude, "disconnect-key")
        .await
        .unwrap();
    assert_eq!(replay.operation_id, receipt.operation_id);
    assert!(matches!(
        intents::disconnect(&h.state, &h.alice_a, Provider::Claude, "new-cleanup").await,
        Err(AgentError::Conflict)
    ));
    let row = db::one(
        &h.state.database,
        "SELECT state,lease_owner,lease_expires_at FROM neev_agent_operations WHERE id=$1",
        vec![ticket.operation_id.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.try_get::<String>("", "state").unwrap(), "abandoned");
    assert_eq!(
        row.try_get::<Option<String>>("", "lease_owner").unwrap(),
        None
    );
    assert_eq!(
        row.try_get::<Option<i64>>("", "lease_expires_at").unwrap(),
        None
    );
}

#[tokio::test]
async fn disconnect_of_idle_disconnected_connection_completes_immediately() {
    let h = Harness::new(h::database().await).await;
    let connection_id = insert_disconnected(&h, &h.alice_a, Provider::Claude, 7).await;
    let receipt = intents::disconnect(&h.state, &h.alice_a, Provider::Claude, "already-idle")
        .await
        .unwrap();
    assert_eq!(receipt.generation, 8);
    let row = db::one(
        &h.state.database,
        "SELECT state,generation,storage_handle FROM neev_agent_connections WHERE id=$1",
        vec![connection_id.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.try_get::<String>("", "state").unwrap(), "disconnected");
    assert_eq!(row.try_get::<i64>("", "generation").unwrap(), 8);
    assert_eq!(
        row.try_get::<Option<String>>("", "storage_handle").unwrap(),
        None
    );
    let operation = db::one(
        &h.state.database,
        "SELECT state FROM neev_agent_operations WHERE id=$1",
        vec![receipt.operation_id.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        operation.try_get::<String>("", "state").unwrap(),
        "completed"
    );
}

#[tokio::test]
async fn lifecycle_intents_recheck_session_membership_org_and_account() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin_ticket(&h, &h.alice_a, Provider::Codex, "authority-start").await;
    h::execute(
        &h.state.database,
        "UPDATE neev_sessions SET revoked_at=1700000000 WHERE id=$1",
        vec![h.alice_a.identity.session_id.clone().into()],
    )
    .await;
    assert_all_intents_denied(&h, &ticket, AuthError::Unauthorized).await;
    h::execute(
        &h.state.database,
        "UPDATE neev_sessions SET revoked_at=NULL WHERE id=$1",
        vec![h.alice_a.identity.session_id.clone().into()],
    )
    .await;
    h::execute(
        &h.state.database,
        "UPDATE neev_memberships SET active=false WHERE org_id=$1 AND user_id=$2",
        vec![h.org_a.into(), h.alice_id.into()],
    )
    .await;
    assert_all_intents_denied(&h, &ticket, AuthError::Forbidden).await;
    h::execute(
        &h.state.database,
        "UPDATE neev_memberships SET active=true WHERE org_id=$1 AND user_id=$2",
        vec![h.org_a.into(), h.alice_id.into()],
    )
    .await;
    h::execute(
        &h.state.database,
        "UPDATE neev_organizations SET disabled=true WHERE id=$1",
        vec![h.org_a.into()],
    )
    .await;
    assert_all_intents_denied(&h, &ticket, AuthError::Forbidden).await;
    h::execute(
        &h.state.database,
        "UPDATE neev_organizations SET disabled=false WHERE id=$1",
        vec![h.org_a.into()],
    )
    .await;
    h::execute(
        &h.state.database,
        "UPDATE neev_accounts SET disabled=true WHERE id=$1",
        vec![h.alice_id.into()],
    )
    .await;
    assert_all_intents_denied(&h, &ticket, AuthError::Unauthorized).await;
    assert_eq!(
        h::count(&h.state.database, "neev_agent_operations").await,
        1
    );
}

#[tokio::test]
async fn generation_and_deadline_overflow_leave_no_partial_rows() {
    let h = Harness::new(h::database().await).await;
    let id = insert_disconnected(&h, &h.alice_a, Provider::Claude, i64::MAX).await;
    assert!(matches!(
        intents::begin(
            &h.state,
            &h.alice_a,
            Provider::Claude,
            "overflow-generation"
        )
        .await,
        Err(AgentError::Conflict)
    ));
    let generation = db::one(
        &h.state.database,
        "SELECT generation FROM neev_agent_connections WHERE id=$1",
        vec![id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<i64>("", "generation")
    .unwrap();
    assert_eq!(generation, i64::MAX);
    h::execute(
        &h.state.database,
        "UPDATE neev_sessions SET expires_at=$1 WHERE id=$2",
        vec![
            i64::MAX.into(),
            h.alice_b.identity.session_id.clone().into(),
        ],
    )
    .await;
    h.set_now(i64::MAX - 500);
    assert!(matches!(
        intents::begin(&h.state, &h.alice_b, Provider::Codex, "overflow-deadline").await,
        Err(AgentError::InvalidInput)
    ));
    assert_eq!(
        db::one(
            &h.state.database,
            "SELECT COUNT(*) AS n FROM neev_agent_connections WHERE org_id=$1 AND user_id=$2",
            vec![h.org_b.into(), h.alice_id.into()],
        )
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn attempt_quota_is_user_wide_and_excludes_the_exact_nine_hundred_second_boundary() {
    let h = Harness::new(h::database().await).await;
    let (org_c, _) = h::organization(&h.state.database, h.alice_id, "admin").await;
    let (org_d, _) = h::organization(&h.state.database, h.alice_id, "admin").await;
    let alice_c = h::scoped_context(&h.state.database, h.alice_id, org_c, 1_700_000_000).await;
    let alice_d = h::scoped_context(&h.state.database, h.alice_id, org_d, 1_700_000_000).await;
    for (actor, provider, key) in [
        (&h.alice_a, Provider::Claude, "quota-a-claude"),
        (&h.alice_a, Provider::Codex, "quota-a-codex"),
        (&h.alice_b, Provider::Claude, "quota-b-claude"),
        (&h.alice_b, Provider::Codex, "quota-b-codex"),
        (&alice_c, Provider::Claude, "quota-c-claude"),
    ] {
        begin_ticket(&h, actor, provider, key).await;
    }
    assert!(matches!(
        intents::begin(&h.state, &alice_d, Provider::Claude, "quota-sixth").await,
        Err(AgentError::RateLimited)
    ));
    assert_eq!(h::count(&h.state.database, "neev_agent_attempts").await, 5);
    h.set_now(1_700_000_900);
    begin_ticket(&h, &alice_d, Provider::Claude, "quota-boundary").await;
}

async fn simultaneous_same_key_contract(database: DatabaseConnection) {
    let h = Harness::new(database).await;
    let first = intents::begin(&h.state, &h.alice_a, Provider::Codex, "simultaneous");
    let second = intents::begin(&h.state, &h.alice_a, Provider::Codex, "simultaneous");
    let (first, second) = tokio::join!(first, second);
    let BeginOutcome::Pending(first) = first.unwrap() else {
        panic!("first start must be pending");
    };
    let BeginOutcome::Pending(second) = second.unwrap() else {
        panic!("second start must replay pending");
    };
    assert_eq!(first.attempt_id, second.attempt_id);
    assert_eq!(first.operation_id, second.operation_id);
    assert_eq!(h::count(&h.state.database, "neev_agent_attempts").await, 1);
    assert!(matches!(
        intents::attempt(
            &h.state,
            &h.alice_second_session,
            Provider::Codex,
            &first.attempt_id
        )
        .await,
        Err(AgentError::NotFound)
    ));

    let left = intents::begin(&h.state, &h.alice_a, Provider::Claude, "simultaneous-left");
    let right = intents::begin(&h.state, &h.alice_a, Provider::Claude, "simultaneous-right");
    let (left, right) = tokio::join!(left, right);
    assert!(
        matches!(&left, Ok(BeginOutcome::Pending(_)))
            && matches!(&right, Err(AgentError::Conflict))
            || matches!(&right, Ok(BeginOutcome::Pending(_)))
                && matches!(&left, Err(AgentError::Conflict))
    );
    assert_eq!(h::count(&h.state.database, "neev_agent_attempts").await, 2);
}

#[tokio::test]
async fn sqlite_serializes_simultaneous_same_key_starts() {
    simultaneous_same_key_contract(h::database().await).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_authority_and_admission_contracts() {
    let (database, admin, schema) = h::postgres_database().await;
    eprintln!("agent lifecycle owned schema: {schema}");
    simultaneous_same_key_contract(database.clone()).await;
    h::cleanup_postgres(database, admin, schema).await;
}

#[tokio::test]
async fn provider_scope_is_part_of_attempt_authority_and_schema_stores_no_challenge() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin_ticket(&h, &h.alice_a, Provider::Claude, "provider-scope").await;
    assert!(matches!(
        intents::attempt(&h.state, &h.alice_a, Provider::Codex, &ticket.attempt_id).await,
        Err(AgentError::NotFound)
    ));
    let columns = h
        .state
        .database
        .query_all(db::statement(
            &h.state.database,
            "SELECT name FROM pragma_table_info('neev_agent_attempts') ORDER BY cid",
            vec![],
        ))
        .await
        .unwrap();
    let names: Vec<String> = columns
        .iter()
        .map(|row| row.try_get("", "name").unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "id",
            "connection_id",
            "generation",
            "session_id",
            "state",
            "created_at",
            "expires_at",
            "updated_at"
        ]
    );
}
