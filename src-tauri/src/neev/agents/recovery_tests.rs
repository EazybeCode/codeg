use super::{
    completion, intents, recovery, store,
    test_support::{account, begin, cleanup_id, lock_connection, snapshot, Harness},
    types::*,
};
use crate::neev::{auth::session, db, test_support as h};

// Missing final-result fencing must never revive a canceled login.
#[tokio::test]
async fn canceled_completion_conflicts_and_own_completion_connects() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin(&h, &h.alice_a, Provider::Claude, "cancel-final").await;
    intents::cancel(
        &h.state,
        &h.alice_a,
        Provider::Claude,
        &ticket.attempt_id,
        "cancel",
    )
    .await
    .unwrap();
    let outcome = completion::complete(&h.state, &h.alice_a, &ticket, account()).await;
    assert!(matches!(outcome, Err(AgentError::Conflict)));
    let own = intents::list(&h.state, &h.alice_a).await.unwrap();
    assert!(own.iter().all(|v| v.state != ConnectionState::Connected));
    let own = begin(&h, &h.alice_a, Provider::Codex, "own-final").await;
    completion::complete(&h.state, &h.alice_a, &own, account())
        .await
        .unwrap();
    let view = intents::list(&h.state, &h.alice_a).await.unwrap();
    let connected = view.iter().find(|v| v.provider == Provider::Codex).unwrap();
    assert_eq!(connected.state, ConnectionState::Connected);
    assert_eq!(
        connected.account_label.as_deref(),
        Some("synthetic account")
    );
    assert_eq!(connected.checked_at, Some(1_700_000_000));
    assert_eq!(
        completion::complete(&h.state, &h.alice_a, &own, account()).await,
        Err(AgentError::Conflict)
    );
}

#[tokio::test]
async fn failed_login_queues_cleanup_and_retries_after_original_expiry() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin(&h, &h.alice_a, Provider::Claude, "fail").await;
    completion::fail(&h.state, &h.alice_a, &ticket, "unsupported_account")
        .await
        .unwrap();
    let view = intents::list(&h.state, &h.alice_a).await.unwrap();
    assert_eq!(view[0].state, ConnectionState::Disconnecting);
    assert_eq!(view[0].error_code.as_deref(), Some("unsupported_account"));
    let op = cleanup_id(&h, &ticket).await;
    assert!(recovery::claim_cleanup(&h.state, &ticket.operation_id)
        .await
        .unwrap()
        .is_none());
    h.set_now(1_700_000_601);
    let first = recovery::claim_cleanup(&h.state, &op)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.lease_expires_at, 1_700_000_631);
    assert!(recovery::claim_cleanup(&h.state, &op)
        .await
        .unwrap()
        .is_none());
    recovery::finish_cleanup(&h.state, &first, false)
        .await
        .unwrap();
    assert_eq!(
        intents::list(&h.state, &h.alice_a).await.unwrap()[0]
            .error_code
            .as_deref(),
        Some("cleanup_failed")
    );
    let retry = recovery::claim_cleanup(&h.state, &op)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(retry.lease_owner, first.lease_owner);
    assert_eq!(
        recovery::finish_cleanup(&h.state, &first, true).await,
        Err(AgentError::Conflict)
    );
    h.set_now(1_700_000_631);
    assert_eq!(
        recovery::finish_cleanup(&h.state, &retry, true).await,
        Err(AgentError::Conflict)
    );
    let takeover = recovery::claim_cleanup(&h.state, &op)
        .await
        .unwrap()
        .unwrap();
    recovery::finish_cleanup(&h.state, &takeover, true)
        .await
        .unwrap();
    assert_eq!(
        intents::list(&h.state, &h.alice_a).await.unwrap()[0].state,
        ConnectionState::Disconnected
    );
    assert!(recovery::claim_cleanup(&h.state, &op)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn final_results_reject_every_tampered_field_and_unsafe_metadata_without_writes() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin(&h, &h.alice_a, Provider::Claude, "tampering").await;
    let before = snapshot(&h, &ticket).await;
    for field in 0..10 {
        let mut altered = ticket.clone();
        match field {
            0 => altered.connection_id = store::random_id(),
            1 => altered.provider = Provider::Codex,
            2 => altered.generation += 1,
            3 => altered.org_id = h.org_b,
            4 => altered.user_id = h.bob_a.identity.user_id,
            5 => altered.session_id = h.alice_second_session.identity.session_id.clone(),
            6 => altered.attempt_id = store::random_id(),
            7 => altered.operation_id = store::random_id(),
            8 => altered.lease_owner = store::random_id(),
            9 => altered.expires_at += 1,
            _ => unreachable!(),
        }
        assert!(
            completion::complete(&h.state, &h.alice_a, &altered, account())
                .await
                .is_err()
        );
        assert!(completion::fail(&h.state, &h.alice_a, &altered, "expired")
            .await
            .is_err());
        assert_eq!(snapshot(&h, &ticket).await, before);
    }
    for bad in [
        "".to_string(),
        "x".repeat(161),
        "a\n".into(),
        "a\u{061c}".into(),
        "a\u{200e}".into(),
        "a\u{202e}".into(),
        "a\u{2069}".into(),
    ] {
        assert!(!store::safe_label(&bad));
        let mut evidence = account();
        evidence.account_label = bad;
        assert_eq!(
            completion::complete(&h.state, &h.alice_a, &ticket, evidence).await,
            Err(AgentError::InvalidInput)
        );
    }
    for bad in [
        "".to_string(),
        "f".repeat(31),
        "f".repeat(33),
        "z".repeat(32),
        uuid::Uuid::new_v4().to_string(),
    ] {
        assert!(!store::simple_id(&bad));
        let mut evidence = account();
        evidence.storage_handle = bad;
        assert_eq!(
            completion::complete(&h.state, &h.alice_a, &ticket, evidence).await,
            Err(AgentError::InvalidInput)
        );
    }
    assert_eq!(
        completion::fail(&h.state, &h.alice_a, &ticket, "invented-provider-detail").await,
        Err(AgentError::InvalidInput)
    );
    assert_eq!(snapshot(&h, &ticket).await, before);
    assert!(store::safe_label(&"界".repeat(160)));
    assert!(store::safe_label("界"));
    let mut evidence = account();
    evidence.account_label = "界".repeat(160);
    completion::complete(&h.state, &h.alice_a, &ticket, evidence)
        .await
        .unwrap();
}

#[tokio::test]
async fn native_foreign_scope_is_not_found_before_and_after_invalidation() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin(&h, &h.alice_a, Provider::Claude, "foreign").await;
    for canceled in [false, true] {
        if canceled {
            intents::cancel(
                &h.state,
                &h.alice_a,
                Provider::Claude,
                &ticket.attempt_id,
                "cancel",
            )
            .await
            .unwrap();
        }
        let before = snapshot(&h, &ticket).await;
        for actor in [&h.bob_a, &h.alice_b, &h.alice_second_session] {
            assert_eq!(
                completion::complete(&h.state, actor, &ticket, account()).await,
                Err(AgentError::NotFound)
            );
            assert_eq!(
                completion::fail(&h.state, actor, &ticket, "expired").await,
                Err(AgentError::NotFound)
            );
        }
        let mut wrong_lease = ticket.clone();
        wrong_lease.lease_owner = store::random_id();
        assert_eq!(
            completion::complete(&h.state, &h.alice_a, &wrong_lease, account()).await,
            Err(AgentError::Conflict)
        );
        assert_eq!(
            completion::fail(&h.state, &h.alice_a, &wrong_lease, "expired").await,
            Err(AgentError::Conflict)
        );
        assert_eq!(snapshot(&h, &ticket).await, before);
    }
}

#[tokio::test]
async fn logout_cancels_expired_pending_exact_session_and_rolls_back_on_overflow() {
    let h = Harness::new(h::database().await).await;
    let own = begin(&h, &h.alice_a, Provider::Claude, "logout").await;
    let other = begin(
        &h,
        &h.alice_second_session,
        Provider::Codex,
        "other-session",
    )
    .await;
    h.set_now(1_700_000_600);
    session::revoke(&h.state.database, &h.alice_a.identity, h.clock.as_ref())
        .await
        .unwrap();
    assert_eq!(snapshot(&h, &own).await.6, "canceled");
    assert_eq!(snapshot(&h, &other).await.6, "pending");
    assert_eq!(snapshot(&h, &other).await.1, "connecting");
    let op = cleanup_id(&h, &own).await;
    let cleanup = recovery::claim_cleanup(&h.state, &op)
        .await
        .unwrap()
        .unwrap();
    recovery::finish_cleanup(&h.state, &cleanup, true)
        .await
        .unwrap();

    let h = Harness::new(h::database().await).await;
    let own = begin(&h, &h.alice_a, Provider::Claude, "overflow").await;
    h::execute(
        &h.state.database,
        "UPDATE neev_agent_connections SET generation=$1 WHERE id=$2",
        vec![i64::MAX.into(), own.connection_id.clone().into()],
    )
    .await;
    h::execute(
        &h.state.database,
        "UPDATE neev_agent_attempts SET generation=$1 WHERE id=$2",
        vec![i64::MAX.into(), own.attempt_id.clone().into()],
    )
    .await;
    let before = snapshot(&h, &own).await;
    assert!(
        session::revoke(&h.state.database, &h.alice_a.identity, h.clock.as_ref())
            .await
            .is_err()
    );
    assert_eq!(snapshot(&h, &own).await, before);
    assert!(intents::list(&h.state, &h.alice_a).await.is_ok());
}

#[tokio::test]
async fn cleanup_tampering_and_old_results_cannot_change_newer_connected_row() {
    for succeeded in [false, true] {
        let h = Harness::new(h::database().await).await;
        let ticket = begin(&h, &h.alice_a, Provider::Claude, "stale-cleanup").await;
        let receipt = intents::cancel(
            &h.state,
            &h.alice_a,
            Provider::Claude,
            &ticket.attempt_id,
            "cancel",
        )
        .await
        .unwrap();
        let cleanup = recovery::claim_cleanup(&h.state, &receipt.operation_id)
            .await
            .unwrap()
            .unwrap();
        for field in 0..9 {
            let mut wrong = cleanup.clone();
            match field {
                0 => wrong.operation_id = store::random_id(),
                1 => wrong.connection_id = store::random_id(),
                2 => wrong.provider = Provider::Codex,
                3 => wrong.org_id = h.org_b,
                4 => wrong.user_id = h.bob_a.identity.user_id,
                5 => wrong.target_generation += 1,
                6 => wrong.result_generation += 1,
                7 => wrong.lease_owner = store::random_id(),
                8 => wrong.lease_expires_at += 1,
                _ => unreachable!(),
            }
            assert!(recovery::finish_cleanup(&h.state, &wrong, true)
                .await
                .is_err());
        }
        let metadata = account();
        h::execute(&h.state.database,"UPDATE neev_agent_connections SET generation=3,state='connected',account_label=$1,checked_at=1700000001,storage_handle=$2,error_code=NULL WHERE id=$3",vec![metadata.account_label.into(),metadata.storage_handle.into(),ticket.connection_id.clone().into()]).await;
        let before = snapshot(&h, &ticket).await;
        recovery::finish_cleanup(&h.state, &cleanup, succeeded)
            .await
            .unwrap();
        assert_eq!(snapshot(&h, &ticket).await, before);
    }
}

#[tokio::test]
async fn cleanup_claim_overflow_and_expired_final_results_leave_rows_unchanged() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin(&h, &h.alice_a, Provider::Claude, "expired-final").await;
    h.set_now(1_700_000_600);
    let before = snapshot(&h, &ticket).await;
    assert_eq!(
        completion::complete(&h.state, &h.alice_a, &ticket, account()).await,
        Err(AgentError::Conflict)
    );
    assert_eq!(
        completion::fail(&h.state, &h.alice_a, &ticket, "expired").await,
        Err(AgentError::Conflict)
    );
    assert_eq!(snapshot(&h, &ticket).await, before);
    let receipt = intents::cancel(
        &h.state,
        &h.alice_a,
        Provider::Claude,
        &ticket.attempt_id,
        "expired-cancel",
    )
    .await
    .unwrap();
    h.set_now(i64::MAX - 29);
    assert!(matches!(
        recovery::claim_cleanup(&h.state, &receipt.operation_id).await,
        Err(AgentError::InvalidInput)
    ));
    h.set_now(1_700_000_600);
    assert!(recovery::claim_cleanup(&h.state, &receipt.operation_id)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn expiry_restart_and_logout_invalidate_only_pending_exact_session() {
    let h = Harness::new(h::database().await).await;
    let expired = begin(&h, &h.alice_a, Provider::Claude, "expiry").await;
    let connected = begin(&h, &h.alice_second_session, Provider::Codex, "connected").await;
    completion::complete(&h.state, &h.alice_second_session, &connected, account())
        .await
        .unwrap();
    h.set_now(1_700_000_599);
    assert_eq!(recovery::expire_pending(&h.state).await.unwrap(), 0);
    h.set_now(1_700_000_600);
    assert_eq!(recovery::expire_pending(&h.state).await.unwrap(), 1);
    assert_eq!(recovery::expire_pending(&h.state).await.unwrap(), 0);
    assert_eq!(
        completion::complete(&h.state, &h.alice_a, &expired, account()).await,
        Err(AgentError::Conflict)
    );
    let pending = begin(&h, &h.bob_a, Provider::Claude, "restart").await;
    assert_eq!(recovery::recover_startup(&h.state).await.unwrap(), 1);
    assert_eq!(recovery::recover_startup(&h.state).await.unwrap(), 0);
    assert_eq!(
        completion::complete(&h.state, &h.bob_a, &pending, account()).await,
        Err(AgentError::Conflict)
    );
    assert_eq!(
        intents::list(&h.state, &h.alice_second_session)
            .await
            .unwrap()[1]
            .state,
        ConnectionState::Connected
    );
    let before = snapshot(&h, &connected).await;
    session::revoke(
        &h.state.database,
        &h.alice_second_session.identity,
        h.clock.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(snapshot(&h, &connected).await, before);
}

#[tokio::test]
async fn recovery_pages_stale_pending_without_overwriting_a_newer_connected_account() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin(&h, &h.alice_a, Provider::Claude, "pages").await;
    completion::complete(&h.state, &h.alice_a, &ticket, account())
        .await
        .unwrap();
    // Invented interrupted historical generations exercise the second 100-ID page.
    for generation in 2..=102 {
        h::execute(&h.state.database,"INSERT INTO neev_agent_attempts(id,connection_id,generation,session_id,state,created_at,expires_at,updated_at) VALUES($1,$2,$3,$4,'pending',1700000000,1700000600,1700000000)",vec![store::random_id().into(),ticket.connection_id.clone().into(),generation.into(),ticket.session_id.clone().into()]).await;
    }
    h::execute(
        &h.state.database,
        "UPDATE neev_agent_connections SET generation=103 WHERE id=$1",
        vec![ticket.connection_id.clone().into()],
    )
    .await;
    let before = snapshot(&h, &ticket).await;
    assert_eq!(recovery::recover_startup(&h.state).await.unwrap(), 101);
    assert_eq!(recovery::recover_startup(&h.state).await.unwrap(), 0);
    assert_eq!(snapshot(&h, &ticket).await, before);
    assert_eq!(
        h::count(&h.state.database, "neev_agent_operations").await,
        1
    );
}

#[tokio::test]
async fn final_result_authority_failures_leave_agent_rows_unchanged() {
    for mutation in [
        "UPDATE neev_accounts SET disabled=true WHERE id=$1",
        "UPDATE neev_accounts SET auth_revision=auth_revision+1 WHERE id=$1",
        "UPDATE neev_organizations SET disabled=true WHERE id=$1",
        "UPDATE neev_organizations SET revision=revision+1 WHERE id=$1",
        "UPDATE neev_memberships SET active=false WHERE id=$1",
        "UPDATE neev_memberships SET revision=revision+1 WHERE id=$1",
        "UPDATE neev_sessions SET expires_at=1700000001 WHERE user_id=$1",
    ] {
        let h = Harness::new(h::database().await).await;
        let ticket = begin(&h, &h.alice_a, Provider::Claude, "authority").await;
        let id = if mutation.contains("neev_organizations") {
            h.org_a
        } else if mutation.contains("neev_memberships") {
            h.alice_a.member_id
        } else {
            h.alice_id
        };
        h::execute(&h.state.database, mutation, vec![id.into()]).await;
        h.set_now(1_700_000_001);
        let before = snapshot(&h, &ticket).await;
        assert!(matches!(
            completion::complete(&h.state, &h.alice_a, &ticket, account()).await,
            Err(AgentError::Auth(_))
        ));
        assert!(matches!(
            completion::fail(&h.state, &h.alice_a, &ticket, "expired").await,
            Err(AgentError::Auth(_))
        ));
        assert_eq!(snapshot(&h, &ticket).await, before);
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_complete_and_fail_sample_expiry_after_blocking_locks() {
    for failure in [false, true] {
        for elapsed in [599, 600] {
            let (database, admin, schema) = h::postgres_database().await;
            eprintln!("agent recovery owned schema: {schema}");
            let h = Harness::new(database.clone()).await;
            let ticket = begin(&h, &h.alice_a, Provider::Claude, "blocked-expiry").await;
            let before = snapshot(&h, &ticket).await;
            let blocker = lock_connection(&h, &ticket).await;
            let state = h.state.clone();
            let actor = h.alice_a.clone();
            let task_ticket = ticket.clone();
            let task = tokio::spawn(async move {
                if failure {
                    completion::fail(&state, &actor, &task_ticket, "expired").await
                } else {
                    completion::complete(&state, &actor, &task_ticket, account()).await
                }
            });
            h::wait_for_postgres_lock(&admin, "%neev_agent_connections%FOR UPDATE%").await;
            h.set_now(1_700_000_000 + elapsed);
            blocker.commit().await.unwrap();
            let result = tokio::time::timeout(std::time::Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap();
            if elapsed == 600 {
                assert_eq!(result, Err(AgentError::Conflict));
                assert_eq!(snapshot(&h, &ticket).await, before);
            } else {
                assert_eq!(result, Ok(()));
            }
            drop(h);
            h::cleanup_postgres(database, admin, schema).await;
        }
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_foreign_lookups_never_wait_for_other_user_or_other_org_connection() {
    let (database, admin, schema) = h::postgres_database().await;
    eprintln!("agent recovery owned schema: {schema}");
    let h = Harness::new(database.clone()).await;
    for actor in [&h.bob_a, &h.alice_b] {
        let ticket = begin(&h, actor, Provider::Claude, "foreign-lock").await;
        let blocker = lock_connection(&h, &ticket).await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            assert_eq!(
                completion::complete(&h.state, &h.alice_a, &ticket, account()).await,
                Err(AgentError::NotFound)
            );
            assert_eq!(
                completion::fail(&h.state, &h.alice_a, &ticket, "expired").await,
                Err(AgentError::NotFound)
            );
            assert_eq!(
                intents::attempt(&h.state, &h.alice_a, Provider::Claude, &ticket.attempt_id).await,
                Err(AgentError::NotFound)
            );
        })
        .await;
        blocker.rollback().await.unwrap();
        result.expect("foreign lookup must not lock foreign connection");
    }
    drop(h);
    h::cleanup_postgres(database, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_admission_serializes_starts_replay_quota_and_cleanup_claims() {
    let (database, admin, schema) = h::postgres_database().await;
    eprintln!("agent recovery owned schema: {schema}");
    let h = Harness::new(database.clone()).await;
    for same_key in [false, true] {
        let blocker = db::begin(&database).await.unwrap();
        db::lock_admission_guard(&blocker).await.unwrap();
        let mut tasks = Vec::new();
        for index in 0..2 {
            let state = h.state.clone();
            let actor = h.alice_a.clone();
            tasks.push(tokio::spawn(async move {
                intents::begin(
                    &state,
                    &actor,
                    if same_key {
                        Provider::Codex
                    } else {
                        Provider::Claude
                    },
                    if same_key {
                        "replay"
                    } else if index == 0 {
                        "left"
                    } else {
                        "right"
                    },
                )
                .await
            }));
        }
        h::wait_for_postgres_locks(&admin, "%neev_schema_version%FOR UPDATE%", 2).await;
        blocker.commit().await.unwrap();
        let a = tasks.remove(0).await.unwrap();
        let b = tasks.remove(0).await.unwrap();
        if same_key {
            match (a, b) {
                (Ok(BeginOutcome::Pending(a)), Ok(BeginOutcome::Pending(b))) => {
                    assert_eq!(a.attempt_id, b.attempt_id);
                    assert_eq!(a.operation_id, b.operation_id);
                }
                _ => panic!("both calls must replay one login"),
            }
        } else {
            assert!(matches!(
                (&a, &b),
                (Ok(BeginOutcome::Pending(_)), Err(AgentError::Conflict))
                    | (Err(AgentError::Conflict), Ok(BeginOutcome::Pending(_)))
            ));
        }
    }
    // Four starts member-wide before two contenders in different organizations.
    for (actor, provider) in [
        (&h.alice_a, Provider::Claude),
        (&h.alice_a, Provider::Codex),
    ] {
        let receipt = intents::disconnect(&h.state, actor, provider, "quota-reset")
            .await
            .unwrap();
        let claim = recovery::claim_cleanup(&h.state, &receipt.operation_id)
            .await
            .unwrap()
            .unwrap();
        recovery::finish_cleanup(&h.state, &claim, true)
            .await
            .unwrap();
    }
    for key in ["quota-third", "quota-fourth"] {
        let ticket = begin(&h, &h.alice_a, Provider::Claude, key).await;
        let receipt = intents::cancel(
            &h.state,
            &h.alice_a,
            Provider::Claude,
            &ticket.attempt_id,
            key,
        )
        .await
        .unwrap();
        let claim = recovery::claim_cleanup(&h.state, &receipt.operation_id)
            .await
            .unwrap()
            .unwrap();
        recovery::finish_cleanup(&h.state, &claim, true)
            .await
            .unwrap();
    }
    let blocker = db::begin(&database).await.unwrap();
    db::lock_admission_guard(&blocker).await.unwrap();
    let mut tasks = Vec::new();
    for actor in [h.alice_a.clone(), h.alice_b.clone()] {
        let state = h.state.clone();
        tasks.push(tokio::spawn(async move {
            intents::begin(&state, &actor, Provider::Codex, "quota-final").await
        }));
    }
    h::wait_for_postgres_locks(&admin, "%neev_schema_version%FOR UPDATE%", 2).await;
    blocker.commit().await.unwrap();
    let a = tasks.remove(0).await.unwrap();
    let b = tasks.remove(0).await.unwrap();
    assert!(matches!(
        (&a, &b),
        (Ok(BeginOutcome::Pending(_)), Err(AgentError::RateLimited))
            | (Err(AgentError::RateLimited), Ok(BeginOutcome::Pending(_)))
    ));
    let ticket = begin(&h, &h.bob_a, Provider::Claude, "claim-race").await;
    let receipt = intents::cancel(
        &h.state,
        &h.bob_a,
        Provider::Claude,
        &ticket.attempt_id,
        "cancel",
    )
    .await
    .unwrap();
    let blocker = lock_connection(&h, &ticket).await;
    let state = h.state.clone();
    let op = receipt.operation_id.clone();
    let first = tokio::spawn(async move { recovery::claim_cleanup(&state, &op).await });
    h::wait_for_postgres_lock(&admin, "%neev_agent_connections%FOR UPDATE%").await;
    let state = h.state.clone();
    let op = receipt.operation_id;
    let second = tokio::spawn(async move { recovery::claim_cleanup(&state, &op).await });
    h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
    blocker.commit().await.unwrap();
    assert!(first.await.unwrap().unwrap().is_some());
    assert!(second.await.unwrap().unwrap().is_none());
    drop(h);
    h::cleanup_postgres(database, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_completion_and_invalidators_serialize_in_both_orders() {
    for mutation in ["cancel", "disconnect", "logout", "remove"] {
        for completion_first in [false, true] {
            let (database, admin, schema) = h::postgres_database().await;
            eprintln!("agent recovery owned schema: {schema}; {mutation}; completion first={completion_first}");
            let h = Harness::new(database.clone()).await;
            let ticket = begin(&h, &h.alice_a, Provider::Claude, "overlap").await;
            // A connection lock stalls finalization/intent, while an account lock stalls removal.
            let blocker = if !completion_first && mutation == "remove" {
                let tx = db::begin(&database).await.unwrap();
                db::one(
                    &tx,
                    "SELECT id FROM neev_accounts WHERE id=$1 FOR UPDATE",
                    vec![h.alice_id.into()],
                )
                .await
                .unwrap();
                tx
            } else {
                lock_connection(&h, &ticket).await
            };
            let spawn_complete = || {
                let state = h.state.clone();
                let actor = h.alice_a.clone();
                let ticket = ticket.clone();
                tokio::spawn(async move {
                    completion::complete(&state, &actor, &ticket, account()).await
                })
            };
            let spawn_mutation = || {
                let state = h.state.clone();
                let actor = h.alice_a.clone();
                let administrator = h.admin_a.clone();
                let ticket = ticket.clone();
                tokio::spawn(async move {
                    match mutation {
                        "cancel" => intents::cancel(
                            &state,
                            &actor,
                            Provider::Claude,
                            &ticket.attempt_id,
                            "cancel",
                        )
                        .await
                        .map(|_| ()),
                        "disconnect" => {
                            intents::disconnect(&state, &actor, Provider::Claude, "disconnect")
                                .await
                                .map(|_| ())
                        }
                        "logout" => {
                            session::revoke(&state.database, &actor.identity, state.clock.as_ref())
                                .await
                                .map_err(AgentError::Auth)
                        }
                        "remove" => {
                            crate::neev::org::remove(&state, &administrator, actor.member_id)
                                .await
                                .map_err(AgentError::Auth)
                        }
                        _ => unreachable!(),
                    }
                })
            };
            let (complete, invalidator) = if completion_first {
                let complete = spawn_complete();
                h::wait_for_postgres_lock(&admin, "%neev_agent_connections%FOR UPDATE%").await;
                let invalidator = spawn_mutation();
                h::wait_for_postgres_lock(
                    &admin,
                    if mutation == "remove" {
                        "%neev_accounts%FOR UPDATE%"
                    } else {
                        "%neev_schema_version%FOR UPDATE%"
                    },
                )
                .await;
                (complete, invalidator)
            } else {
                let invalidator = spawn_mutation();
                h::wait_for_postgres_lock(
                    &admin,
                    if mutation == "remove" {
                        "%neev_accounts%FOR UPDATE%"
                    } else {
                        "%neev_agent_connections%FOR UPDATE%"
                    },
                )
                .await;
                let complete = spawn_complete();
                if mutation == "remove" {
                    h::wait_for_postgres_locks(&admin, "%neev_accounts%FOR UPDATE%", 2).await;
                } else {
                    h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
                }
                (complete, invalidator)
            };
            blocker.commit().await.unwrap();
            let (result, invalidated) =
                tokio::time::timeout(std::time::Duration::from_secs(4), async {
                    (complete.await.unwrap(), invalidator.await.unwrap())
                })
                .await
                .unwrap();
            if completion_first {
                assert_eq!(result, Ok(()));
                if mutation == "cancel" {
                    assert_eq!(invalidated, Err(AgentError::Conflict));
                } else {
                    assert_eq!(invalidated, Ok(()));
                }
                let row = snapshot(&h, &ticket).await;
                assert_eq!(
                    row.1,
                    if mutation == "disconnect" {
                        "disconnecting"
                    } else {
                        "connected"
                    }
                );
            } else {
                assert_eq!(invalidated, Ok(()));
                assert!(result.is_err());
                assert_ne!(snapshot(&h, &ticket).await.1, "connected");
            }
            // Removal must not strand already authorized cleanup.
            if mutation == "remove" && !completion_first {
                assert_eq!(recovery::recover_startup(&h.state).await.unwrap(), 1);
                let op = cleanup_id(&h, &ticket).await;
                let cleanup = recovery::claim_cleanup(&h.state, &op)
                    .await
                    .unwrap()
                    .unwrap();
                recovery::finish_cleanup(&h.state, &cleanup, true)
                    .await
                    .unwrap();
            }
            drop(h);
            h::cleanup_postgres(database, admin, schema).await;
        }
    }
}
