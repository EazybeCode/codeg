use super::{
    completion, intents, recovery, store,
    test_support::{account, begin, cleanup_id, lock_connection, snapshot, Harness},
    types::*,
};
use crate::neev::{auth::session, db, test_support as h};

fn completion_lookup(ticket: &LoginTicket, storage_handle: &str) -> completion::CompletionLookup {
    completion::CompletionLookup::new(
        ticket.org_id,
        ticket.user_id,
        ticket.provider,
        ticket.connection_id.clone(),
        ticket.generation,
        ticket.attempt_id.clone(),
        ticket.operation_id.clone(),
        storage_handle.to_owned(),
    )
    .unwrap()
}

async fn operation_lease_snapshot(
    h: &Harness,
    operation_id: &str,
) -> (String, Option<String>, Option<i64>, i64) {
    let row = db::one(
        &h.state.database,
        "SELECT state,lease_owner,lease_expires_at,updated_at FROM neev_agent_operations WHERE id=$1",
        vec![operation_id.into()],
    )
    .await
    .unwrap()
    .unwrap();
    (
        row.try_get("", "state").unwrap(),
        row.try_get("", "lease_owner").unwrap(),
        row.try_get("", "lease_expires_at").unwrap(),
        row.try_get("", "updated_at").unwrap(),
    )
}

async fn import_operation_half_lease(h: &Harness, operation_id: &str, owner_only: bool) {
    h::execute(
        &h.state.database,
        "PRAGMA ignore_check_constraints=ON",
        vec![],
    )
    .await;
    if owner_only {
        h::execute(
            &h.state.database,
            "UPDATE neev_agent_operations SET lease_owner=$1,lease_expires_at=NULL WHERE id=$2",
            vec![store::random_id().into(), operation_id.into()],
        )
        .await;
    } else {
        h::execute(
            &h.state.database,
            "UPDATE neev_agent_operations SET lease_owner=NULL,lease_expires_at=1700000030 WHERE id=$1",
            vec![operation_id.into()],
        )
        .await;
    }
    h::execute(
        &h.state.database,
        "PRAGMA ignore_check_constraints=OFF",
        vec![],
    )
    .await;
}

#[tokio::test]
async fn witness_repeats_after_expiry_and_logout_without_mutating_rows() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin(&h, &h.alice_a, Provider::Codex, "witness-repeat").await;
    let evidence = account();
    let lookup = completion_lookup(&ticket, &evidence.storage_handle);
    completion::complete(&h.state, &h.alice_a, &ticket, evidence.clone())
        .await
        .unwrap();
    let before = snapshot(&h, &ticket).await;

    let first = completion::read_completed_witness(&h.state, &lookup)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.org_id(), ticket.org_id);
    assert_eq!(first.user_id(), ticket.user_id);
    assert_eq!(first.provider(), ticket.provider);
    assert_eq!(first.connection_id(), ticket.connection_id);
    assert_eq!(first.generation(), ticket.generation);
    assert_eq!(first.session_id(), ticket.session_id);
    assert_eq!(first.attempt_id(), ticket.attempt_id);
    assert_eq!(first.operation_id(), ticket.operation_id);
    assert_eq!(first.storage_handle(), evidence.storage_handle);
    assert_eq!(first.completed_at(), 1_700_000_000);

    h.set_now(ticket.expires_at + 1);
    session::revoke(&h.state.database, &h.alice_a.identity, h.clock.as_ref())
        .await
        .unwrap();
    let second = completion::read_completed_witness(&h.state, &lookup)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.session_id(), first.session_id());
    assert_eq!(second.completed_at(), first.completed_at());
    assert_eq!(snapshot(&h, &ticket).await, before);
}

#[tokio::test]
async fn witness_rejects_each_lookup_mismatch_without_mutation() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin(&h, &h.alice_a, Provider::Claude, "witness-mismatch").await;
    let evidence = account();
    completion::complete(&h.state, &h.alice_a, &ticket, evidence.clone())
        .await
        .unwrap();
    let before = snapshot(&h, &ticket).await;

    let mut values = vec![
        (
            h.org_b,
            ticket.user_id,
            ticket.provider,
            ticket.connection_id.clone(),
            ticket.generation,
            ticket.attempt_id.clone(),
            ticket.operation_id.clone(),
            evidence.storage_handle.clone(),
        ),
        (
            ticket.org_id,
            h.bob_a.identity.user_id,
            ticket.provider,
            ticket.connection_id.clone(),
            ticket.generation,
            ticket.attempt_id.clone(),
            ticket.operation_id.clone(),
            evidence.storage_handle.clone(),
        ),
        (
            ticket.org_id,
            ticket.user_id,
            Provider::Codex,
            ticket.connection_id.clone(),
            ticket.generation,
            ticket.attempt_id.clone(),
            ticket.operation_id.clone(),
            evidence.storage_handle.clone(),
        ),
        (
            ticket.org_id,
            ticket.user_id,
            ticket.provider,
            store::random_id(),
            ticket.generation,
            ticket.attempt_id.clone(),
            ticket.operation_id.clone(),
            evidence.storage_handle.clone(),
        ),
        (
            ticket.org_id,
            ticket.user_id,
            ticket.provider,
            ticket.connection_id.clone(),
            ticket.generation + 1,
            ticket.attempt_id.clone(),
            ticket.operation_id.clone(),
            evidence.storage_handle.clone(),
        ),
        (
            ticket.org_id,
            ticket.user_id,
            ticket.provider,
            ticket.connection_id.clone(),
            ticket.generation,
            store::random_id(),
            ticket.operation_id.clone(),
            evidence.storage_handle.clone(),
        ),
        (
            ticket.org_id,
            ticket.user_id,
            ticket.provider,
            ticket.connection_id.clone(),
            ticket.generation,
            ticket.attempt_id.clone(),
            store::random_id(),
            evidence.storage_handle.clone(),
        ),
        (
            ticket.org_id,
            ticket.user_id,
            ticket.provider,
            ticket.connection_id.clone(),
            ticket.generation,
            ticket.attempt_id.clone(),
            ticket.operation_id.clone(),
            store::random_id(),
        ),
    ];
    for (org, user, provider, connection, generation, attempt, operation, handle) in
        values.drain(..)
    {
        let lookup = completion::CompletionLookup::new(
            org, user, provider, connection, generation, attempt, operation, handle,
        )
        .unwrap();
        assert!(completion::read_completed_witness(&h.state, &lookup)
            .await
            .unwrap()
            .is_none());
        assert_eq!(snapshot(&h, &ticket).await, before);
    }

    for invalid in [
        completion::CompletionLookup::new(
            0,
            ticket.user_id,
            ticket.provider,
            ticket.connection_id.clone(),
            ticket.generation,
            ticket.attempt_id.clone(),
            ticket.operation_id.clone(),
            evidence.storage_handle.clone(),
        ),
        completion::CompletionLookup::new(
            ticket.org_id,
            0,
            ticket.provider,
            ticket.connection_id.clone(),
            ticket.generation,
            ticket.attempt_id.clone(),
            ticket.operation_id.clone(),
            evidence.storage_handle.clone(),
        ),
        completion::CompletionLookup::new(
            ticket.org_id,
            ticket.user_id,
            ticket.provider,
            "x".into(),
            ticket.generation,
            ticket.attempt_id.clone(),
            ticket.operation_id.clone(),
            evidence.storage_handle.clone(),
        ),
        completion::CompletionLookup::new(
            ticket.org_id,
            ticket.user_id,
            ticket.provider,
            ticket.connection_id.clone(),
            0,
            ticket.attempt_id.clone(),
            ticket.operation_id.clone(),
            evidence.storage_handle.clone(),
        ),
        completion::CompletionLookup::new(
            ticket.org_id,
            ticket.user_id,
            ticket.provider,
            ticket.connection_id.clone(),
            ticket.generation,
            "x".into(),
            ticket.operation_id.clone(),
            evidence.storage_handle.clone(),
        ),
        completion::CompletionLookup::new(
            ticket.org_id,
            ticket.user_id,
            ticket.provider,
            ticket.connection_id.clone(),
            ticket.generation,
            ticket.attempt_id.clone(),
            "x".into(),
            evidence.storage_handle.clone(),
        ),
        completion::CompletionLookup::new(
            ticket.org_id,
            ticket.user_id,
            ticket.provider,
            ticket.connection_id.clone(),
            ticket.generation,
            ticket.attempt_id.clone(),
            ticket.operation_id.clone(),
            "x".into(),
        ),
    ] {
        assert!(matches!(invalid, Err(AgentError::InvalidInput)));
    }
}

#[tokio::test]
async fn witness_accepts_only_exact_completed_chronology_and_allowed_state() {
    for mutation in [
        "UPDATE neev_agent_attempts SET state='pending' WHERE id=$1",
        "UPDATE neev_agent_attempts SET state='canceled' WHERE id=$1",
        "UPDATE neev_agent_attempts SET state='failed' WHERE id=$1",
        "UPDATE neev_agent_operations SET state='queued' WHERE id=$1",
        "UPDATE neev_agent_operations SET state='abandoned' WHERE id=$1",
        "UPDATE neev_agent_operations SET updated_at=updated_at+1 WHERE id=$1",
        "UPDATE neev_agent_attempts SET updated_at=expires_at WHERE id=$1",
    ] {
        let h = Harness::new(h::database().await).await;
        let ticket = begin(&h, &h.alice_a, Provider::Claude, "witness-shape").await;
        let evidence = account();
        completion::complete(&h.state, &h.alice_a, &ticket, evidence.clone())
            .await
            .unwrap();
        let lookup = completion_lookup(&ticket, &evidence.storage_handle);
        let id = if mutation.contains("attempts") {
            ticket.attempt_id.clone()
        } else {
            ticket.operation_id.clone()
        };
        h::execute(&h.state.database, mutation, vec![id.into()]).await;
        let before = snapshot(&h, &ticket).await;
        assert!(completion::read_completed_witness(&h.state, &lookup)
            .await
            .unwrap()
            .is_none());
        assert_eq!(snapshot(&h, &ticket).await, before);
    }

    let h = Harness::new(h::database().await).await;
    let ticket = begin(&h, &h.alice_a, Provider::Claude, "witness-operation-expiry").await;
    let evidence = account();
    completion::complete(&h.state, &h.alice_a, &ticket, evidence.clone())
        .await
        .unwrap();
    let lookup = completion_lookup(&ticket, &evidence.storage_handle);
    h::execute(
        &h.state.database,
        "UPDATE neev_agent_attempts SET updated_at=created_at+1 WHERE id=$1",
        vec![ticket.attempt_id.clone().into()],
    )
    .await;
    h::execute(&h.state.database, "UPDATE neev_agent_operations SET updated_at=created_at+1,expires_at=created_at+1 WHERE id=$1", vec![ticket.operation_id.clone().into()]).await;
    assert!(completion::read_completed_witness(&h.state, &lookup)
        .await
        .unwrap()
        .is_none());

    for state in ["connecting", "disconnecting", "disconnected", "unavailable"] {
        let h = Harness::new(h::database().await).await;
        let ticket = begin(&h, &h.alice_a, Provider::Claude, "witness-state").await;
        let evidence = account();
        completion::complete(&h.state, &h.alice_a, &ticket, evidence.clone())
            .await
            .unwrap();
        let lookup = completion_lookup(&ticket, &evidence.storage_handle);
        if state == "disconnected" {
            h::execute(&h.state.database, "UPDATE neev_agent_connections SET state=$1,account_label=NULL,checked_at=NULL,storage_handle=NULL,error_code=NULL WHERE id=$2", vec![state.into(), ticket.connection_id.clone().into()]).await;
        } else {
            h::execute(&h.state.database, "UPDATE neev_agent_connections SET state=$1,account_label=NULL,checked_at=NULL,error_code='interrupted' WHERE id=$2", vec![state.into(), ticket.connection_id.clone().into()]).await;
        }
        assert!(completion::read_completed_witness(&h.state, &lookup)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn witness_rejects_each_completed_operation_tuple_mismatch_without_mutation() {
    for mutation in ["kind", "attempt", "target_generation", "result_generation"] {
        let h = Harness::new(h::database().await).await;
        let ticket = begin(&h, &h.alice_a, Provider::Claude, "witness-operation-tuple").await;
        let evidence = account();
        completion::complete(&h.state, &h.alice_a, &ticket, evidence.clone())
            .await
            .unwrap();

        let lookup = if matches!(mutation, "target_generation" | "result_generation") {
            h::execute(
                &h.state.database,
                "UPDATE neev_agent_connections SET generation=2 WHERE id=$1",
                vec![ticket.connection_id.clone().into()],
            )
            .await;
            h::execute(
                &h.state.database,
                "UPDATE neev_agent_attempts SET generation=2 WHERE id=$1",
                vec![ticket.attempt_id.clone().into()],
            )
            .await;
            h::execute(
                &h.state.database,
                "UPDATE neev_agent_operations SET target_generation=2,result_generation=2 WHERE id=$1",
                vec![ticket.operation_id.clone().into()],
            )
            .await;
            completion::CompletionLookup::new(
                ticket.org_id,
                ticket.user_id,
                ticket.provider,
                ticket.connection_id.clone(),
                2,
                ticket.attempt_id.clone(),
                ticket.operation_id.clone(),
                evidence.storage_handle.clone(),
            )
            .unwrap()
        } else {
            completion_lookup(&ticket, &evidence.storage_handle)
        };
        assert!(completion::read_completed_witness(&h.state, &lookup)
            .await
            .unwrap()
            .is_some());

        match mutation {
            "kind" => {
                h::execute(
                    &h.state.database,
                    "UPDATE neev_agent_operations SET kind='cancel' WHERE id=$1",
                    vec![ticket.operation_id.clone().into()],
                )
                .await;
            }
            "attempt" => {
                let other_attempt = store::random_id();
                h::execute(
                    &h.state.database,
                    "INSERT INTO neev_agent_attempts(id,connection_id,generation,session_id,state,created_at,expires_at,updated_at) SELECT $1,connection_id,generation+1,session_id,state,created_at,expires_at,updated_at FROM neev_agent_attempts WHERE id=$2",
                    vec![other_attempt.clone().into(), ticket.attempt_id.clone().into()],
                )
                .await;
                h::execute(
                    &h.state.database,
                    "UPDATE neev_agent_operations SET target_attempt_id=$1 WHERE id=$2",
                    vec![other_attempt.into(), ticket.operation_id.clone().into()],
                )
                .await;
            }
            "target_generation" => {
                h::execute(
                    &h.state.database,
                    "UPDATE neev_agent_operations SET target_generation=1 WHERE id=$1",
                    vec![ticket.operation_id.clone().into()],
                )
                .await;
            }
            "result_generation" => {
                h::execute(
                    &h.state.database,
                    "UPDATE neev_agent_operations SET result_generation=3 WHERE id=$1",
                    vec![ticket.operation_id.clone().into()],
                )
                .await;
            }
            _ => unreachable!(),
        }
        let before = snapshot(&h, &ticket).await;
        assert!(completion::read_completed_witness(&h.state, &lookup)
            .await
            .unwrap()
            .is_none());
        assert_eq!(snapshot(&h, &ticket).await, before);
    }
}

#[tokio::test]
async fn witness_rejects_leases_expired_attempt_and_malformed_projection_without_mutation() {
    for mutation in [
        "leased_operation",
        "completed_lease_residue",
        "expired_attempt",
        "negative_operation_updated_at",
    ] {
        let h = Harness::new(h::database().await).await;
        let ticket = begin(&h, &h.alice_a, Provider::Claude, "witness-stored-shape").await;
        let evidence = account();
        completion::complete(&h.state, &h.alice_a, &ticket, evidence.clone())
            .await
            .unwrap();
        let lookup = completion_lookup(&ticket, &evidence.storage_handle);

        match mutation {
            "leased_operation" => {
                h::execute(
                    &h.state.database,
                    "UPDATE neev_agent_operations SET state='leased',lease_owner=$1,lease_expires_at=expires_at WHERE id=$2",
                    vec![store::random_id().into(), ticket.operation_id.clone().into()],
                )
                .await;
            }
            "completed_lease_residue" => {
                h::execute(
                    &h.state.database,
                    "PRAGMA ignore_check_constraints=ON",
                    vec![],
                )
                .await;
                h::execute(
                    &h.state.database,
                    "UPDATE neev_agent_operations SET lease_owner=$1,lease_expires_at=expires_at WHERE id=$2",
                    vec![store::random_id().into(), ticket.operation_id.clone().into()],
                )
                .await;
                h::execute(
                    &h.state.database,
                    "PRAGMA ignore_check_constraints=OFF",
                    vec![],
                )
                .await;
            }
            "expired_attempt" => {
                h::execute(
                    &h.state.database,
                    "UPDATE neev_agent_attempts SET state='expired' WHERE id=$1",
                    vec![ticket.attempt_id.clone().into()],
                )
                .await;
            }
            "negative_operation_updated_at" => {
                h::execute(
                    &h.state.database,
                    "PRAGMA ignore_check_constraints=ON",
                    vec![],
                )
                .await;
                h::execute(
                    &h.state.database,
                    "UPDATE neev_agent_operations SET updated_at=-1 WHERE id=$1",
                    vec![ticket.operation_id.clone().into()],
                )
                .await;
                h::execute(
                    &h.state.database,
                    "PRAGMA ignore_check_constraints=OFF",
                    vec![],
                )
                .await;
            }
            _ => unreachable!(),
        }
        let before = snapshot(&h, &ticket).await;
        let observed = completion::read_completed_witness(&h.state, &lookup).await;
        if matches!(
            mutation,
            "completed_lease_residue" | "negative_operation_updated_at"
        ) {
            assert!(matches!(observed, Err(AgentError::Unavailable)));
        } else {
            assert!(observed.unwrap().is_none());
        }
        assert_eq!(snapshot(&h, &ticket).await, before);
    }
}

#[tokio::test]
async fn witness_rejects_completed_operation_with_each_half_lease_without_mutation() {
    for owner_only in [true, false] {
        let h = Harness::new(h::database().await).await;
        let ticket = begin(&h, &h.alice_a, Provider::Claude, "witness-half-lease").await;
        let evidence = account();
        completion::complete(&h.state, &h.alice_a, &ticket, evidence.clone())
            .await
            .unwrap();
        let lookup = completion_lookup(&ticket, &evidence.storage_handle);
        import_operation_half_lease(&h, &ticket.operation_id, owner_only).await;
        let before = snapshot(&h, &ticket).await;

        match completion::read_completed_witness(&h.state, &lookup).await {
            Err(AgentError::Unavailable) => {}
            Ok(None) => panic!("expected Unavailable, got Ok(None) for completed half-lease"),
            _ => panic!("expected Unavailable from malformed completed witness operation"),
        }
        assert_eq!(snapshot(&h, &ticket).await, before);
    }
}

#[tokio::test]
async fn witness_rejects_every_newer_generation_state_without_mutation() {
    for state in ["disconnecting", "disconnected", "connected"] {
        let h = Harness::new(h::database().await).await;
        let ticket = begin(&h, &h.alice_a, Provider::Codex, "witness-newer-generation").await;
        let evidence = account();
        completion::complete(&h.state, &h.alice_a, &ticket, evidence.clone())
            .await
            .unwrap();
        let lookup = completion_lookup(&ticket, &evidence.storage_handle);
        match state {
            "disconnecting" => {
                h::execute(&h.state.database, "UPDATE neev_agent_connections SET generation=generation+1,state='disconnecting',account_label=NULL,checked_at=NULL,error_code=NULL WHERE id=$1", vec![ticket.connection_id.clone().into()]).await;
            }
            "disconnected" => {
                h::execute(&h.state.database, "UPDATE neev_agent_connections SET generation=generation+1,state='disconnected',account_label=NULL,checked_at=NULL,storage_handle=NULL,error_code=NULL WHERE id=$1", vec![ticket.connection_id.clone().into()]).await;
            }
            "connected" => {
                // Deliberately retain the exact old handle to isolate the generation fence.
                h::execute(
                    &h.state.database,
                    "UPDATE neev_agent_connections SET generation=generation+1 WHERE id=$1",
                    vec![ticket.connection_id.clone().into()],
                )
                .await;
            }
            _ => unreachable!(),
        }
        let before = snapshot(&h, &ticket).await;
        assert!(completion::read_completed_witness(&h.state, &lookup)
            .await
            .unwrap()
            .is_none());
        assert_eq!(snapshot(&h, &ticket).await, before);
    }
}

#[tokio::test]
async fn witness_accepts_same_generation_reconnect_required_as_history_only() {
    let h = Harness::new(h::database().await).await;
    let ticket = begin(&h, &h.alice_a, Provider::Codex, "witness-reconnect").await;
    let evidence = account();
    completion::complete(&h.state, &h.alice_a, &ticket, evidence.clone())
        .await
        .unwrap();
    h::execute(&h.state.database, "UPDATE neev_agent_connections SET state='reconnect_required',account_label=NULL,checked_at=NULL,error_code='reauth_required',updated_at=1700000001 WHERE id=$1", vec![ticket.connection_id.clone().into()]).await;
    let lookup = completion_lookup(&ticket, &evidence.storage_handle);
    let before = snapshot(&h, &ticket).await;
    let witness = completion::read_completed_witness(&h.state, &lookup)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(witness.generation(), ticket.generation);
    assert_eq!(witness.storage_handle(), evidence.storage_handle);
    assert_eq!(witness.completed_at(), 1_700_000_000);
    assert_eq!(snapshot(&h, &ticket).await, before);

    h::execute(&h.state.database, "UPDATE neev_agent_connections SET generation=generation+1,state='connected',account_label='new account',checked_at=1700000002,storage_handle=$1,error_code=NULL WHERE id=$2", vec![store::random_id().into(), ticket.connection_id.clone().into()]).await;
    assert!(completion::read_completed_witness(&h.state, &lookup)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn cleanup_target_comes_from_exact_cancel_or_disconnect_operation() {
    let h = Harness::new(h::database().await).await;
    let cancel_login = begin(&h, &h.alice_a, Provider::Claude, "typed-cancel").await;
    let cancel = intents::cancel(
        &h.state,
        &h.alice_a,
        Provider::Claude,
        &cancel_login.attempt_id,
        "typed-cancel-op",
    )
    .await
    .unwrap();
    let cancel_ticket = recovery::claim_cleanup(&h.state, &cancel.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        &cancel_ticket.target,
        CleanupTarget::Cancel { attempt_id } if attempt_id == &cancel_login.attempt_id
    ));

    let connected_login = begin(&h, &h.alice_a, Provider::Codex, "typed-disconnect").await;
    completion::complete(&h.state, &h.alice_a, &connected_login, account())
        .await
        .unwrap();
    let disconnect =
        intents::disconnect(&h.state, &h.alice_a, Provider::Codex, "typed-disconnect-op")
            .await
            .unwrap();
    let disconnect_ticket = recovery::claim_cleanup(&h.state, &disconnect.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        disconnect_ticket.target,
        CleanupTarget::Disconnect
    ));
}

#[tokio::test]
async fn cleanup_target_recovery_causes_are_disconnect_not_cancel() {
    let h = Harness::new(h::database().await).await;
    let expired = begin(&h, &h.alice_a, Provider::Claude, "typed-expiry").await;
    h.set_now(expired.expires_at);
    assert_eq!(recovery::expire_pending(&h.state).await.unwrap(), 1);
    let expired_cleanup = cleanup_id(&h, &expired).await;
    let claimed = recovery::claim_cleanup(&h.state, &expired_cleanup)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(claimed.target, CleanupTarget::Disconnect));

    let h = Harness::new(h::database().await).await;
    let startup = begin(&h, &h.alice_a, Provider::Claude, "typed-startup").await;
    assert_eq!(recovery::recover_startup(&h.state).await.unwrap(), 1);
    let startup_cleanup = cleanup_id(&h, &startup).await;
    let claimed = recovery::claim_cleanup(&h.state, &startup_cleanup)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(claimed.target, CleanupTarget::Disconnect));

    let h = Harness::new(h::database().await).await;
    let logout = begin(&h, &h.alice_a, Provider::Claude, "typed-logout").await;
    session::revoke(&h.state.database, &h.alice_a.identity, h.clock.as_ref())
        .await
        .unwrap();
    let logout_cleanup = cleanup_id(&h, &logout).await;
    let claimed = recovery::claim_cleanup(&h.state, &logout_cleanup)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(claimed.target, CleanupTarget::Disconnect));
}

#[tokio::test]
async fn cleanup_target_tampering_and_renewed_leases_preserve_immutable_target() {
    let h = Harness::new(h::database().await).await;
    let login = begin(&h, &h.alice_a, Provider::Claude, "typed-renew").await;
    let receipt = intents::cancel(
        &h.state,
        &h.alice_a,
        Provider::Claude,
        &login.attempt_id,
        "typed-renew-op",
    )
    .await
    .unwrap();
    let first = recovery::claim_cleanup(&h.state, &receipt.operation_id)
        .await
        .unwrap()
        .unwrap();
    let before = operation_lease_snapshot(&h, &receipt.operation_id).await;
    let before_connection = snapshot(&h, &login).await;
    let mut wrong_kind = first.clone();
    wrong_kind.target = CleanupTarget::Disconnect;
    assert_eq!(
        recovery::finish_cleanup(&h.state, &wrong_kind, true).await,
        Err(AgentError::Conflict)
    );
    let mut wrong_attempt = first.clone();
    wrong_attempt.target = CleanupTarget::Cancel {
        attempt_id: store::random_id(),
    };
    assert_eq!(
        operation_lease_snapshot(&h, &receipt.operation_id).await,
        before
    );
    assert_eq!(snapshot(&h, &login).await, before_connection);
    let before = operation_lease_snapshot(&h, &receipt.operation_id).await;
    let before_connection = snapshot(&h, &login).await;
    assert_eq!(
        recovery::finish_cleanup(&h.state, &wrong_attempt, true).await,
        Err(AgentError::Conflict)
    );
    assert_eq!(
        operation_lease_snapshot(&h, &receipt.operation_id).await,
        before
    );
    assert_eq!(snapshot(&h, &login).await, before_connection);

    h.set_now(first.lease_expires_at);
    let renewed = recovery::claim_cleanup(&h.state, &receipt.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        &renewed.target,
        CleanupTarget::Cancel { attempt_id } if attempt_id == &login.attempt_id
    ));
    let before = operation_lease_snapshot(&h, &receipt.operation_id).await;
    let before_connection = snapshot(&h, &login).await;
    assert_eq!(
        recovery::finish_cleanup(&h.state, &first, true).await,
        Err(AgentError::Conflict)
    );
    assert_eq!(
        operation_lease_snapshot(&h, &receipt.operation_id).await,
        before
    );
    assert_eq!(snapshot(&h, &login).await, before_connection);
    recovery::finish_cleanup(&h.state, &renewed, true)
        .await
        .unwrap();
}

#[tokio::test]
async fn cleanup_target_renewed_disconnect_lease_preserves_disconnect() {
    let h = Harness::new(h::database().await).await;
    let login = begin(&h, &h.alice_a, Provider::Codex, "typed-disconnect-renew").await;
    completion::complete(&h.state, &h.alice_a, &login, account())
        .await
        .unwrap();
    let receipt = intents::disconnect(
        &h.state,
        &h.alice_a,
        Provider::Codex,
        "typed-disconnect-renew-op",
    )
    .await
    .unwrap();
    let first = recovery::claim_cleanup(&h.state, &receipt.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(first.target, CleanupTarget::Disconnect));
    h.set_now(first.lease_expires_at);
    let renewed = recovery::claim_cleanup(&h.state, &receipt.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(renewed.target, CleanupTarget::Disconnect));
    assert_ne!(renewed.lease_owner, first.lease_owner);
}

#[tokio::test]
async fn cleanup_target_generation_corruption_never_issues_or_finishes_a_lease() {
    for (target, result) in [(1_i64, 1_i64), (i64::MAX, i64::MAX)] {
        let h = Harness::new(h::database().await).await;
        let login = begin(&h, &h.alice_a, Provider::Claude, "typed-corrupt-claim").await;
        let receipt = intents::cancel(
            &h.state,
            &h.alice_a,
            Provider::Claude,
            &login.attempt_id,
            "typed-corrupt-claim-op",
        )
        .await
        .unwrap();
        h::execute(&h.state.database, "UPDATE neev_agent_operations SET target_generation=$1,result_generation=$2 WHERE id=$3", vec![target.into(), result.into(), receipt.operation_id.clone().into()]).await;
        let before = operation_lease_snapshot(&h, &receipt.operation_id).await;
        let before_connection = snapshot(&h, &login).await;
        assert!(matches!(
            recovery::claim_cleanup(&h.state, &receipt.operation_id).await,
            Err(AgentError::Unavailable)
        ));
        assert_eq!(
            operation_lease_snapshot(&h, &receipt.operation_id).await,
            before
        );
        assert_eq!(snapshot(&h, &login).await, before_connection);
    }

    for (target, result) in [(1_i64, 1_i64), (i64::MAX, i64::MAX)] {
        let h = Harness::new(h::database().await).await;
        let login = begin(&h, &h.alice_a, Provider::Claude, "typed-corrupt-finish").await;
        let receipt = intents::cancel(
            &h.state,
            &h.alice_a,
            Provider::Claude,
            &login.attempt_id,
            "typed-corrupt-finish-op",
        )
        .await
        .unwrap();
        let ticket = recovery::claim_cleanup(&h.state, &receipt.operation_id)
            .await
            .unwrap()
            .unwrap();
        h::execute(
            &h.state.database,
            "UPDATE neev_agent_operations SET target_generation=$1,result_generation=$2 WHERE id=$3",
            vec![target.into(), result.into(), receipt.operation_id.clone().into()],
        )
        .await;
        let before = operation_lease_snapshot(&h, &receipt.operation_id).await;
        let before_connection = snapshot(&h, &login).await;
        assert_eq!(
            recovery::finish_cleanup(&h.state, &ticket, true).await,
            Err(AgentError::Unavailable)
        );
        assert_eq!(
            operation_lease_snapshot(&h, &receipt.operation_id).await,
            before
        );
        assert_eq!(snapshot(&h, &login).await, before_connection);
    }
}

#[tokio::test]
async fn cleanup_target_malformed_completed_row_is_unavailable_without_mutation() {
    let h = Harness::new(h::database().await).await;
    let login = begin(
        &h,
        &h.alice_a,
        Provider::Claude,
        "typed-malformed-completed",
    )
    .await;
    let receipt = intents::cancel(
        &h.state,
        &h.alice_a,
        Provider::Claude,
        &login.attempt_id,
        "typed-malformed-completed-op",
    )
    .await
    .unwrap();
    let ticket = recovery::claim_cleanup(&h.state, &receipt.operation_id)
        .await
        .unwrap()
        .unwrap();
    recovery::finish_cleanup(&h.state, &ticket, true)
        .await
        .unwrap();
    h::execute(
        &h.state.database,
        "UPDATE neev_agent_operations SET result_generation=target_generation WHERE id=$1",
        vec![receipt.operation_id.clone().into()],
    )
    .await;
    let before_operation = operation_lease_snapshot(&h, &receipt.operation_id).await;
    let before_connection = snapshot(&h, &login).await;

    match recovery::claim_cleanup(&h.state, &receipt.operation_id).await {
        Err(AgentError::Unavailable) => {}
        Ok(None) => panic!("expected Unavailable, got Ok(None)"),
        _ => panic!("expected Unavailable from malformed completed cleanup"),
    }
    assert_eq!(
        operation_lease_snapshot(&h, &receipt.operation_id).await,
        before_operation
    );
    assert_eq!(snapshot(&h, &login).await, before_connection);
}

#[tokio::test]
async fn cleanup_target_malformed_live_lease_is_unavailable_without_mutation() {
    let h = Harness::new(h::database().await).await;
    let login = begin(&h, &h.alice_a, Provider::Claude, "typed-malformed-leased").await;
    let receipt = intents::cancel(
        &h.state,
        &h.alice_a,
        Provider::Claude,
        &login.attempt_id,
        "typed-malformed-leased-op",
    )
    .await
    .unwrap();
    recovery::claim_cleanup(&h.state, &receipt.operation_id)
        .await
        .unwrap()
        .unwrap();
    h::execute(
        &h.state.database,
        "UPDATE neev_agent_operations SET result_generation=target_generation WHERE id=$1",
        vec![receipt.operation_id.clone().into()],
    )
    .await;
    let before_operation = operation_lease_snapshot(&h, &receipt.operation_id).await;
    let before_connection = snapshot(&h, &login).await;

    match recovery::claim_cleanup(&h.state, &receipt.operation_id).await {
        Err(AgentError::Unavailable) => {}
        Ok(None) => panic!("expected Unavailable, got Ok(None)"),
        _ => panic!("expected Unavailable from malformed live-leased cleanup"),
    }
    assert_eq!(
        operation_lease_snapshot(&h, &receipt.operation_id).await,
        before_operation
    );
    assert_eq!(snapshot(&h, &login).await, before_connection);
}

#[tokio::test]
async fn cleanup_target_stored_shape_corruption_fails_closed_at_claim_and_finish() {
    for kind in ["cancel", "disconnect"] {
        for finish in [false, true] {
            let h = Harness::new(h::database().await).await;
            let login = begin(&h, &h.alice_a, Provider::Codex, "typed-target-shape").await;
            let receipt = if kind == "cancel" {
                intents::cancel(
                    &h.state,
                    &h.alice_a,
                    Provider::Codex,
                    &login.attempt_id,
                    "typed-target-shape-cancel",
                )
                .await
                .unwrap()
            } else {
                completion::complete(&h.state, &h.alice_a, &login, account())
                    .await
                    .unwrap();
                intents::disconnect(
                    &h.state,
                    &h.alice_a,
                    Provider::Codex,
                    "typed-target-shape-disconnect",
                )
                .await
                .unwrap()
            };
            let ticket = if finish {
                Some(
                    recovery::claim_cleanup(&h.state, &receipt.operation_id)
                        .await
                        .unwrap()
                        .unwrap(),
                )
            } else {
                None
            };

            h::execute(
                &h.state.database,
                "PRAGMA ignore_check_constraints=ON",
                vec![],
            )
            .await;
            if kind == "cancel" {
                h::execute(
                    &h.state.database,
                    "UPDATE neev_agent_operations SET target_attempt_id=NULL WHERE id=$1",
                    vec![receipt.operation_id.clone().into()],
                )
                .await;
            } else {
                h::execute(
                    &h.state.database,
                    "UPDATE neev_agent_operations SET target_attempt_id=$1 WHERE id=$2",
                    vec![
                        login.attempt_id.clone().into(),
                        receipt.operation_id.clone().into(),
                    ],
                )
                .await;
            }
            h::execute(
                &h.state.database,
                "PRAGMA ignore_check_constraints=OFF",
                vec![],
            )
            .await;
            let before_operation = operation_lease_snapshot(&h, &receipt.operation_id).await;
            let before_connection = snapshot(&h, &login).await;
            if let Some(ticket) = ticket {
                assert_eq!(
                    recovery::finish_cleanup(&h.state, &ticket, true).await,
                    Err(AgentError::Unavailable)
                );
            } else {
                assert!(matches!(
                    recovery::claim_cleanup(&h.state, &receipt.operation_id).await,
                    Err(AgentError::Unavailable)
                ));
            }
            assert_eq!(
                operation_lease_snapshot(&h, &receipt.operation_id).await,
                before_operation
            );
            assert_eq!(snapshot(&h, &login).await, before_connection);
        }
    }
}

#[tokio::test]
async fn cleanup_target_each_half_lease_is_unavailable_without_mutation() {
    for completed in [false, true] {
        for owner_only in [true, false] {
            let h = Harness::new(h::database().await).await;
            let login = begin(&h, &h.alice_a, Provider::Claude, "cleanup-half-lease").await;
            let receipt = intents::cancel(
                &h.state,
                &h.alice_a,
                Provider::Claude,
                &login.attempt_id,
                "cleanup-half-lease-op",
            )
            .await
            .unwrap();
            if completed {
                let ticket = recovery::claim_cleanup(&h.state, &receipt.operation_id)
                    .await
                    .unwrap()
                    .unwrap();
                recovery::finish_cleanup(&h.state, &ticket, true)
                    .await
                    .unwrap();
            }
            import_operation_half_lease(&h, &receipt.operation_id, owner_only).await;
            let before_operation = operation_lease_snapshot(&h, &receipt.operation_id).await;
            let before_connection = snapshot(&h, &login).await;

            match recovery::claim_cleanup(&h.state, &receipt.operation_id).await {
                Err(AgentError::Unavailable) => {}
                Ok(None) => panic!("expected Unavailable, got Ok(None) for cleanup half-lease"),
                Ok(Some(_)) => panic!("expected Unavailable, malformed cleanup was claimed"),
                Err(_) => panic!("expected Unavailable from malformed cleanup operation"),
            }
            assert_eq!(
                operation_lease_snapshot(&h, &receipt.operation_id).await,
                before_operation
            );
            assert_eq!(snapshot(&h, &login).await, before_connection);
        }
    }
}

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
        for field in 0..11 {
            let mut wrong = cleanup.clone();
            match field {
                0 => wrong.operation_id = store::random_id(),
                1 => wrong.connection_id = store::random_id(),
                2 => wrong.provider = Provider::Codex,
                3 => wrong.org_id = h.org_b,
                4 => wrong.user_id = h.bob_a.identity.user_id,
                5 => wrong.target = CleanupTarget::Disconnect,
                6 => {
                    wrong.target = CleanupTarget::Cancel {
                        attempt_id: store::random_id(),
                    }
                }
                7 => wrong.target_generation += 1,
                8 => wrong.result_generation += 1,
                9 => wrong.lease_owner = store::random_id(),
                10 => wrong.lease_expires_at += 1,
                _ => unreachable!(),
            }
            let before_operation = operation_lease_snapshot(&h, &receipt.operation_id).await;
            let before_connection = snapshot(&h, &ticket).await;
            assert!(recovery::finish_cleanup(&h.state, &wrong, true)
                .await
                .is_err());
            assert_eq!(
                operation_lease_snapshot(&h, &receipt.operation_id).await,
                before_operation
            );
            assert_eq!(snapshot(&h, &ticket).await, before_connection);
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
async fn postgres_witness_serializes_with_completion_and_disconnect_in_both_orders() {
    for completion_first in [false, true] {
        let (database, admin, schema) = h::postgres_database().await;
        eprintln!("agent witness owned schema: {schema}; completion first={completion_first}");
        let h = Harness::new(database.clone()).await;
        let ticket = begin(&h, &h.alice_a, Provider::Codex, "witness-completion-race").await;
        let evidence = account();
        let blocker = db::begin(&database).await.unwrap();
        db::lock_admission_guard(&blocker).await.unwrap();

        if completion_first {
            let state = h.state.clone();
            let actor = h.alice_a.clone();
            let task_ticket = ticket.clone();
            let task_evidence = evidence.clone();
            let complete = tokio::spawn(async move {
                completion::complete(&state, &actor, &task_ticket, task_evidence).await
            });
            h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
            let state = h.state.clone();
            let lookup = completion_lookup(&ticket, &evidence.storage_handle);
            let witness =
                tokio::spawn(
                    async move { completion::read_completed_witness(&state, &lookup).await },
                );
            h::wait_for_postgres_locks(&admin, "%neev_schema_version%FOR UPDATE%", 2).await;
            blocker.commit().await.unwrap();
            let (completed, observed) =
                tokio::time::timeout(std::time::Duration::from_secs(4), async {
                    (complete.await.unwrap(), witness.await.unwrap())
                })
                .await
                .unwrap();
            assert_eq!(completed, Ok(()));
            assert!(observed.unwrap().is_some());
        } else {
            let state = h.state.clone();
            let lookup = completion_lookup(&ticket, &evidence.storage_handle);
            let witness =
                tokio::spawn(
                    async move { completion::read_completed_witness(&state, &lookup).await },
                );
            h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
            let state = h.state.clone();
            let actor = h.alice_a.clone();
            let task_ticket = ticket.clone();
            let task_evidence = evidence.clone();
            let complete = tokio::spawn(async move {
                completion::complete(&state, &actor, &task_ticket, task_evidence).await
            });
            h::wait_for_postgres_locks(&admin, "%neev_schema_version%FOR UPDATE%", 2).await;
            blocker.commit().await.unwrap();
            let (observed, completed) =
                tokio::time::timeout(std::time::Duration::from_secs(4), async {
                    (witness.await.unwrap(), complete.await.unwrap())
                })
                .await
                .unwrap();
            assert!(observed.unwrap().is_none());
            assert_eq!(completed, Ok(()));
            let lookup = completion_lookup(&ticket, &evidence.storage_handle);
            assert!(completion::read_completed_witness(&h.state, &lookup)
                .await
                .unwrap()
                .is_some());
        }
        drop(h);
        h::cleanup_postgres(database, admin, schema).await;
    }

    for disconnect_first in [false, true] {
        let (database, admin, schema) = h::postgres_database().await;
        eprintln!("agent witness owned schema: {schema}; disconnect first={disconnect_first}");
        let h = Harness::new(database.clone()).await;
        let ticket = begin(&h, &h.alice_a, Provider::Codex, "witness-disconnect-race").await;
        let evidence = account();
        completion::complete(&h.state, &h.alice_a, &ticket, evidence.clone())
            .await
            .unwrap();
        let blocker = db::begin(&database).await.unwrap();
        db::lock_admission_guard(&blocker).await.unwrap();

        if disconnect_first {
            let state = h.state.clone();
            let actor = h.alice_a.clone();
            let disconnect = tokio::spawn(async move {
                intents::disconnect(&state, &actor, Provider::Codex, "witness-disconnect").await
            });
            h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
            let state = h.state.clone();
            let lookup = completion_lookup(&ticket, &evidence.storage_handle);
            let witness =
                tokio::spawn(
                    async move { completion::read_completed_witness(&state, &lookup).await },
                );
            h::wait_for_postgres_locks(&admin, "%neev_schema_version%FOR UPDATE%", 2).await;
            blocker.commit().await.unwrap();
            let (disconnected, observed) =
                tokio::time::timeout(std::time::Duration::from_secs(4), async {
                    (disconnect.await.unwrap(), witness.await.unwrap())
                })
                .await
                .unwrap();
            disconnected.unwrap();
            assert!(observed.unwrap().is_none());
        } else {
            let state = h.state.clone();
            let lookup = completion_lookup(&ticket, &evidence.storage_handle);
            let witness =
                tokio::spawn(
                    async move { completion::read_completed_witness(&state, &lookup).await },
                );
            h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
            let state = h.state.clone();
            let actor = h.alice_a.clone();
            let disconnect = tokio::spawn(async move {
                intents::disconnect(&state, &actor, Provider::Codex, "witness-disconnect").await
            });
            h::wait_for_postgres_locks(&admin, "%neev_schema_version%FOR UPDATE%", 2).await;
            blocker.commit().await.unwrap();
            let (observed, disconnected) =
                tokio::time::timeout(std::time::Duration::from_secs(4), async {
                    (witness.await.unwrap(), disconnect.await.unwrap())
                })
                .await
                .unwrap();
            assert!(observed.unwrap().is_some());
            disconnected.unwrap();
            let lookup = completion_lookup(&ticket, &evidence.storage_handle);
            assert!(completion::read_completed_witness(&h.state, &lookup)
                .await
                .unwrap()
                .is_none());
        }
        drop(h);
        h::cleanup_postgres(database, admin, schema).await;
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
        let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(4), async {
            (
                tasks.remove(0).await.unwrap(),
                tasks.remove(0).await.unwrap(),
            )
        })
        .await
        .expect("start and replay contenders must finish after admission release");
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
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(4), async {
        (
            tasks.remove(0).await.unwrap(),
            tasks.remove(0).await.unwrap(),
        )
    })
    .await
    .expect("quota contenders must finish after admission release");
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
    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(4), async {
        (first.await.unwrap(), second.await.unwrap())
    })
    .await
    .expect("cleanup claim contenders must finish after connection release");
    assert!(first.unwrap().is_some());
    assert!(second.unwrap().is_none());
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
                // Every invalidator — including remove, now admission-first — blocks
                // on the admission guard already held by the running completion.
                h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
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
                // Completion blocks on the admission guard held by the invalidator
                // (remove now takes admission first, like logout/cancel/disconnect).
                h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
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

/// Production-lifecycle regressions for the recovery wiring in `startup::serve`.
///
/// `serve` now calls `recover_startup` once, before the listener accepts
/// anything, and then sweeps with `expire_pending` on a timer. These pin the
/// behaviour that wiring exists to provide, and the safety property that makes
/// the periodic half callable against a *serving* process.
/// Startup recovery is necessary but NOT sufficient on its own, and this test
/// pins exactly where the remaining dependency is.
///
/// `recover_startup` moves a crash-interrupted sign-in to `disconnecting` and
/// **queues a cleanup operation**; the connection only returns to
/// `disconnected` — the one state `intents::begin` will accept — once something
/// claims that cleanup, performs the teardown effect, and calls
/// `finish_cleanup`. There is no such executor bound in this tree (see
/// `scripts/operations/neev_supervisor_transport.py`, "trusted FUTURE
/// launcher"), so wiring recovery into `serve` is one half of the fix and the
/// effect executor is the other. Asserting the halves separately keeps that
/// honest instead of implying sign-in already recovers end to end.
#[tokio::test]
async fn startup_recovery_queues_cleanup_and_only_then_can_a_connect_be_re_begun() {
    let h = Harness::new(h::database().await).await;
    // A sign-in that was still in flight when the previous process died.
    let interrupted = begin(&h, &h.alice_a, Provider::Claude, "interrupted-login").await;

    // Until it is recovered the connection is stuck `connecting`, so the user
    // can never obtain the per-user credential a run requires (§5.4).
    assert!(
        matches!(
            intents::begin(&h.state, &h.alice_a, Provider::Claude, "retry-login").await,
            Err(AgentError::Conflict)
        ),
        "a stuck connection must be what blocks the retry"
    );

    // Exactly what `serve` now does at boot.
    assert_eq!(recovery::recover_startup(&h.state).await.unwrap(), 1);

    // STILL blocked: the connection is `disconnecting` with a queued cleanup.
    // This is the gap — recovery alone does not restore sign-in.
    assert!(
        matches!(
            intents::begin(&h.state, &h.alice_a, Provider::Claude, "retry-login").await,
            Err(AgentError::Conflict)
        ),
        "recovery must not claim to have restored the connection on its own"
    );

    // What the (absent) effect executor would do: claim the queued cleanup,
    // perform the teardown, report success.
    let cleanup = cleanup_id(&h, &interrupted).await;
    let ticket = recovery::claim_cleanup(&h.state, &cleanup)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ticket.target, CleanupTarget::Disconnect));
    recovery::finish_cleanup(&h.state, &ticket, true).await.unwrap();

    // Only now can the user sign in again.
    assert!(
        matches!(
            intents::begin(&h.state, &h.alice_a, Provider::Claude, "retry-login").await,
            Ok(BeginOutcome::Pending(_))
        ),
        "a fully cleaned-up connection must accept a new sign-in"
    );
}

#[tokio::test]
async fn the_periodic_sweep_never_cancels_a_login_that_is_still_in_time() {
    let h = Harness::new(h::database().await).await;
    let live = begin(&h, &h.alice_a, Provider::Claude, "live-login").await;
    // This is what makes `expire_pending` safe to run on a timer against a
    // serving process: an attempt inside its own deadline is untouched, so the
    // sweep can never cancel a login a user is part-way through.
    assert_eq!(recovery::expire_pending(&h.state).await.unwrap(), 0);
    h.set_now(live.expires_at - 1);
    assert_eq!(recovery::expire_pending(&h.state).await.unwrap(), 0);
    // Only once its own deadline passes is it reclaimed.
    h.set_now(live.expires_at);
    assert_eq!(recovery::expire_pending(&h.state).await.unwrap(), 1);
}
