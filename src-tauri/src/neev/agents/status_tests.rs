use super::super::{
    completion, intents,
    test_support::{self as a, Harness},
};
use super::*;
use crate::neev::test_support as h;
use sea_orm::DatabaseConnection;

const NOW: i64 = 1_700_000_000;
const BOOT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn claim(epoch: i64) -> CheckedStatusEpoch {
    CheckedStatusEpoch {
        epoch,
        boot_id: BOOT.into(),
    }
}
fn epoch() -> StatusEpochTicket {
    StatusEpochTicket {
        epoch: 1,
        boot_id: BOOT.into(),
    }
}
fn ready(epoch: &StatusEpochTicket) -> ReconciledStatusReady {
    ReconciledStatusReady {
        epoch: epoch.epoch,
        boot_id: epoch.boot_id.clone(),
    }
}
fn supported() -> CheckedAccountOutcome {
    CheckedAccountOutcome::Supported {
        account_label: "checked synthetic".into(),
        observed_at: NOW,
    }
}

async fn fixture(db: DatabaseConnection) -> (Harness, StatusTicket) {
    let h = Harness::new(db).await;
    let login = a::begin(&h, &h.alice_a, Provider::Codex, "status-login").await;
    let account = a::account();
    let handle = account.storage_handle.clone();
    completion::complete(&h.state, &h.alice_a, &login, account)
        .await
        .unwrap();
    // Independent persisted fixture isolates each function's RED from activation.
    db::execute(
        &h.state.database,
        "UPDATE neev_agent_status_owner SET epoch=1,boot_id=$1,phase='ready' WHERE id=1",
        vec![BOOT.into()],
    )
    .await
    .unwrap();
    let ticket = StatusTicket {
        epoch: 1,
        boot_id: BOOT.into(),
        org_id: h.alice_a.org_id,
        user_id: h.alice_a.identity.user_id,
        provider: Provider::Codex,
        connection_id: login.connection_id,
        generation: login.generation,
        session_id: h.alice_a.identity.session_id.clone(),
        storage_handle: handle,
        check_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        issued_at: NOW,
        expires_at: NOW + 60,
    };
    (h, ticket)
}
async fn marker(h: &Harness, t: &StatusTicket) {
    db::execute(&h.state.database, "INSERT INTO neev_agent_status_checks(connection_id,check_id,owner_epoch,session_id,generation,storage_handle,issued_at,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
        vec![t.connection_id.clone().into(),t.check_id.clone().into(),t.epoch.into(),t.session_id.clone().into(),t.generation.into(),t.storage_handle.clone().into(),t.issued_at.into(),t.expires_at.into()]).await.unwrap();
}
async fn metadata(
    h: &Harness,
    t: &StatusTicket,
) -> (
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    String,
    i64,
    i64,
) {
    let r=db::one(&h.state.database,"SELECT state,account_label,checked_at,error_code,storage_handle,generation,updated_at FROM neev_agent_connections WHERE id=$1",vec![t.connection_id.clone().into()]).await.unwrap().unwrap();
    (
        r.try_get("", "state").unwrap(),
        r.try_get("", "account_label").unwrap(),
        r.try_get("", "checked_at").unwrap(),
        r.try_get("", "error_code").unwrap(),
        r.try_get("", "storage_handle").unwrap(),
        r.try_get("", "generation").unwrap(),
        r.try_get("", "updated_at").unwrap(),
    )
}
async fn count(h: &Harness) -> i64 {
    h::count(&h.state.database, "neev_agent_status_checks").await
}
fn admission(r: CheckAdmission) -> StatusTicket {
    match r {
        CheckAdmission::Started(t) | CheckAdmission::Joined(t) => t,
    }
}

#[tokio::test]
async fn epoch_activation_replay_readiness_and_closed_fencing_preserve_metadata() {
    let (h, t) = fixture(h::database().await).await;
    let before = metadata(&h, &t).await;
    marker(&h, &t).await;
    assert!(matches!(
        activate_status_epoch(&h.state, &claim(1)).await,
        Ok(Activation::Existing(_, StatusPhase::Ready))
    ));
    assert_eq!(count(&h).await, 1);
    assert!(matches!(
        activate_status_epoch(
            &h.state,
            &CheckedStatusEpoch {
                epoch: 1,
                boot_id: "c".repeat(32)
            }
        )
        .await,
        Err(StatusError::Stale)
    ));
    let next = match activate_status_epoch(&h.state, &claim(2)).await.unwrap() {
        Activation::Activated(t) => t,
        _ => panic!("higher activation"),
    };
    assert_eq!(count(&h).await, 0);
    assert_eq!(
        enable_status_checks(&h.state, &next, &ready(&epoch())).await,
        Err(StatusError::Invalid)
    );
    assert_eq!(
        enable_status_checks(&h.state, &next, &ready(&next)).await,
        Ok(())
    );
    assert_eq!(
        enable_status_checks(&h.state, &next, &ready(&next)).await,
        Ok(())
    );
    assert_eq!(
        disable_status_checks(&h.state, &epoch()).await,
        Err(StatusError::Stale)
    );
    assert_eq!(
        enable_status_checks(&h.state, &epoch(), &ready(&epoch())).await,
        Err(StatusError::Stale)
    );
    assert_eq!(disable_status_checks(&h.state, &next).await, Ok(()));
    assert_eq!(disable_status_checks(&h.state, &next).await, Ok(()));
    assert_eq!(
        enable_status_checks(&h.state, &next, &ready(&next)).await,
        Err(StatusError::Stale)
    );
    assert!(matches!(
        activate_status_epoch(&h.state, &claim(2)).await,
        Ok(Activation::Existing(_, StatusPhase::Closed))
    ));
    assert!(matches!(
        activate_status_epoch(&h.state, &claim(1)).await,
        Err(StatusError::Stale)
    ));
    assert_eq!(metadata(&h, &t).await, before);
}

#[tokio::test]
async fn sentinel_activation_and_invalid_sealed_input_fail_closed() {
    let h = Harness::new(h::database().await).await;
    for c in [
        claim(0),
        claim(-1),
        CheckedStatusEpoch {
            epoch: 1,
            boot_id: "A".repeat(32),
        },
    ] {
        assert!(matches!(
            activate_status_epoch(&h.state, &c).await,
            Err(StatusError::Invalid)
        ));
    }
    let t = match activate_status_epoch(&h.state, &claim(1)).await.unwrap() {
        Activation::Activated(t) => t,
        _ => panic!("sentinel activation"),
    };
    assert!(matches!(
        prepare_connection_check(&h.state, &h.alice_a, &t, Provider::Codex).await,
        Err(StatusError::Stale)
    ));
    assert_eq!(disable_status_checks(&h.state, &t).await, Ok(()));
}

#[tokio::test]
async fn prepare_starts_joins_without_extending_and_hides_other_session() {
    let (h, t) = fixture(h::database().await).await;
    let before = metadata(&h, &t).await;
    let first = match prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex)
        .await
        .unwrap()
    {
        CheckAdmission::Started(t) => t,
        _ => panic!("start"),
    };
    h.set_now(NOW + 1);
    let joined = match prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex)
        .await
        .unwrap()
    {
        CheckAdmission::Joined(t) => t,
        _ => panic!("join"),
    };
    assert!(first == joined);
    assert_eq!(first.expires_at, NOW + 60);
    assert!(matches!(
        prepare_connection_check(&h.state, &h.alice_second_session, &epoch(), Provider::Codex)
            .await,
        Err(StatusError::Busy)
    ));
    // Claude is checkable too; this account just has no Claude connection yet,
    // which is a plain NotFound rather than "that provider is unsupported".
    assert!(matches!(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Claude).await,
        Err(StatusError::Agent(AgentError::NotFound))
    ));
    for actor in [&h.alice_b, &h.bob_a, &h.admin_a] {
        assert!(matches!(
            prepare_connection_check(&h.state, actor, &epoch(), Provider::Codex).await,
            Err(StatusError::Agent(AgentError::NotFound))
        ));
    }
    assert_eq!(count(&h).await, 1);
    assert_eq!(metadata(&h, &t).await, before);
}

#[tokio::test]
async fn prepare_replaces_expired_or_changed_generation_handle_and_enforces_capacity() {
    let (h, mut t) = fixture(h::database().await).await;
    marker(&h, &t).await;
    h.set_now(NOW + 60);
    let first = admission(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex)
            .await
            .unwrap(),
    );
    assert_ne!(first.check_id, t.check_id);
    db::execute(
        &h.state.database,
        "UPDATE neev_agent_connections SET generation=2,storage_handle=$1 WHERE id=$2",
        vec!["c".repeat(32).into(), t.connection_id.clone().into()],
    )
    .await
    .unwrap();
    let second = admission(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex)
            .await
            .unwrap(),
    );
    assert_ne!(first.check_id, second.check_id);
    assert_eq!(second.generation, 2);
    assert_eq!(second.storage_handle, "c".repeat(32));
    clear_connection_check(&h.state, &epoch(), &second)
        .await
        .unwrap();
    for i in 0..65 {
        if i == 64 {
            assert!(matches!(
                prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex).await,
                Err(StatusError::Busy)
            ));
            assert_eq!(count(&h).await, 64);
        }
        let user = h::native_account(
            &h.state.database,
            &format!("capacity-{i}"),
            &format!("c{i}@example.test"),
        )
        .await;
        h::add_membership(&h.state.database, h.org_a, user, "member").await;
        t.connection_id = uuid::Uuid::new_v4().simple().to_string();
        t.check_id = uuid::Uuid::new_v4().simple().to_string();
        t.issued_at = NOW + 60;
        t.expires_at = NOW + 120;
        db::execute(&h.state.database,"INSERT INTO neev_agent_connections(id,org_id,user_id,provider,generation,state,account_label,checked_at,error_code,storage_handle,created_at,updated_at) VALUES($1,$2,$3,'codex',1,'connected','capacity',1700000000,NULL,$4,1700000000,1700000000)",vec![t.connection_id.clone().into(),h.org_a.into(),user.into(),t.storage_handle.clone().into()]).await.unwrap();
        if i == 63 {
            let last = admission(
                prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex)
                    .await
                    .unwrap(),
            );
            assert_eq!(count(&h).await, 64);
            clear_connection_check(&h.state, &epoch(), &last)
                .await
                .unwrap();
        }
        marker(&h, &t).await;
    }
    assert!(matches!(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex).await,
        Err(StatusError::Unavailable)
    ));
    assert!(matches!(
        activate_status_epoch(&h.state, &claim(2)).await,
        Err(StatusError::Unavailable)
    ));
    assert_eq!(count(&h).await, 65);
}

#[tokio::test]
async fn finalization_projects_only_closed_supported_and_negative_outcomes() {
    for kind in 0..3 {
        let (h, t) = fixture(h::database().await).await;
        marker(&h, &t).await;
        h.set_now(NOW + 5);
        let outcome = match kind {
            0 => supported(),
            1 => CheckedAccountOutcome::Disconnected { observed_at: NOW },
            _ => CheckedAccountOutcome::Unsupported { observed_at: NOW },
        };
        let result = finalize_connection_check(&h.state, &h.alice_a, &t, outcome)
            .await
            .unwrap();
        let CheckFinalized::Applied(view) = result else {
            panic!("expected applied view")
        };
        assert_eq!(view.provider, Provider::Codex);
        assert_eq!(
            view.state,
            if kind == 0 {
                ConnectionState::Connected
            } else {
                ConnectionState::ReconnectRequired
            }
        );
        assert_eq!(
            view.account_label.as_deref(),
            if kind == 0 {
                Some("checked synthetic")
            } else {
                None
            }
        );
        assert_eq!(
            view.checked_at,
            if kind == 0 { Some(NOW + 5) } else { None }
        );
        assert_eq!(
            view.error_code.as_deref(),
            match kind {
                0 => None,
                1 => Some("reauth_required"),
                _ => Some("unsupported_account"),
            }
        );
        let m = metadata(&h, &t).await;
        assert_eq!(
            m.0,
            if kind == 0 {
                "connected"
            } else {
                "reconnect_required"
            }
        );
        assert_eq!(
            m.1.as_deref(),
            if kind == 0 {
                Some("checked synthetic")
            } else {
                None
            }
        );
        assert_eq!(m.2, if kind == 0 { Some(NOW + 5) } else { None });
        assert_eq!(
            m.3.as_deref(),
            match kind {
                0 => None,
                1 => Some("reauth_required"),
                _ => Some("unsupported_account"),
            }
        );
        assert_eq!(m.4, t.storage_handle);
        assert_eq!(m.5, t.generation);
        assert_eq!(m.6, NOW + 5);
        assert_eq!(count(&h).await, 0);
        // Deliberately discard the first reply: replay cannot republish metadata.
        h.set_now(NOW + 6);
        assert!(matches!(
            finalize_connection_check(&h.state, &h.alice_a, &t, supported()).await,
            Ok(CheckFinalized::Stale)
        ));
        assert_eq!(metadata(&h, &t).await, m);
    }
}

#[tokio::test]
async fn unavailable_interrupted_and_expired_clear_only_exact_marker() {
    for reason in [
        CheckUnavailable::ProviderUnavailable,
        CheckUnavailable::Interrupted,
        CheckUnavailable::Expired,
    ] {
        let (h, t) = fixture(h::database().await).await;
        marker(&h, &t).await;
        let before = metadata(&h, &t).await;
        let outcome = if reason == CheckUnavailable::Expired {
            h.set_now(t.expires_at);
            supported()
        } else {
            CheckedAccountOutcome::Unavailable(reason)
        };
        assert!(
            matches!(finalize_connection_check(&h.state,&h.alice_a,&t,outcome).await,Ok(CheckFinalized::Unavailable(r)) if r==reason)
        );
        assert_eq!(metadata(&h, &t).await, before);
        assert_eq!(count(&h).await, 0);
    }
}

#[tokio::test]
async fn every_ticket_field_is_compared_and_foreign_scope_is_not_found() {
    let (h, t) = fixture(h::database().await).await;
    marker(&h, &t).await;
    let before = metadata(&h, &t).await;
    for field in 0..13 {
        let mut wrong = t.clone();
        match field {
            0 => wrong.epoch += 1,
            1 => wrong.boot_id = "c".repeat(32),
            2 => wrong.org_id = h.org_b,
            3 => wrong.user_id += 100,
            4 => wrong.provider = Provider::Claude,
            5 => wrong.connection_id = "c".repeat(32),
            6 => wrong.generation += 1,
            7 => wrong.session_id = uuid::Uuid::new_v4().to_string(),
            8 => wrong.storage_handle = "c".repeat(32),
            9 => wrong.check_id = "c".repeat(32),
            10 => wrong.issued_at -= 1,
            11 => wrong.expires_at += 1,
            _ => {
                wrong.issued_at -= 1;
                wrong.expires_at -= 1;
            }
        }
        let r = finalize_connection_check(&h.state, &h.alice_a, &wrong, supported()).await;
        if matches!(field, 2 | 3 | 7) {
            assert!(matches!(r, Err(StatusError::Agent(AgentError::NotFound))));
        } else if field == 4 {
            // Naming the other provider reaches that provider's connection —
            // which does not exist here — so the ticket is simply stale.
            assert!(matches!(r, Ok(CheckFinalized::Stale)));
        } else if matches!(field, 10 | 11) {
            assert!(matches!(r, Err(StatusError::Invalid)));
        } else {
            assert!(matches!(r, Ok(CheckFinalized::Stale)));
        }
        assert_eq!(metadata(&h, &t).await, before);
        assert_eq!(count(&h).await, 1);
    }
}

#[tokio::test]
async fn invalid_labels_times_and_revoked_native_authority_never_publish() {
    let (h, t) = fixture(h::database().await).await;
    marker(&h, &t).await;
    let before = metadata(&h, &t).await;
    for label in [
        String::new(),
        "x".repeat(161),
        "a\u{202e}b".into(),
        "a\nb".into(),
    ] {
        assert!(matches!(
            finalize_connection_check(
                &h.state,
                &h.alice_a,
                &t,
                CheckedAccountOutcome::Supported {
                    account_label: label,
                    observed_at: NOW
                }
            )
            .await,
            Err(StatusError::Invalid)
        ));
    }
    for time in [-1, NOW - 1, NOW + 1, t.expires_at, i64::MAX] {
        assert!(matches!(
            finalize_connection_check(
                &h.state,
                &h.alice_a,
                &t,
                CheckedAccountOutcome::Disconnected { observed_at: time }
            )
            .await,
            Err(StatusError::Invalid)
        ));
    }
    session::revoke(
        &h.state.database,
        &h.alice_a.identity,
        h.state.clock.as_ref(),
    )
    .await
    .unwrap();
    assert!(matches!(
        finalize_connection_check(&h.state, &h.alice_a, &t, supported()).await,
        Err(StatusError::Agent(AgentError::Auth(_)))
    ));
    assert!(matches!(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex).await,
        Err(StatusError::Agent(AgentError::Auth(_)))
    ));
    assert_eq!(metadata(&h, &t).await, before);
    assert_eq!(count(&h).await, 1);
    clear_connection_check(&h.state, &epoch(), &t)
        .await
        .unwrap();
    assert_eq!(count(&h).await, 0);
}

#[tokio::test]
async fn exact_clear_is_idempotent_and_disable_clears_markers_without_metadata() {
    let (h, t) = fixture(h::database().await).await;
    marker(&h, &t).await;
    let before = metadata(&h, &t).await;
    let mut other = t.clone();
    other.check_id = "c".repeat(32);
    clear_connection_check(&h.state, &epoch(), &other)
        .await
        .unwrap();
    assert_eq!(count(&h).await, 1);
    clear_connection_check(&h.state, &epoch(), &t)
        .await
        .unwrap();
    clear_connection_check(&h.state, &epoch(), &t)
        .await
        .unwrap();
    assert_eq!(count(&h).await, 0);
    marker(&h, &t).await;
    disable_status_checks(&h.state, &epoch()).await.unwrap();
    assert_eq!(count(&h).await, 0);
    assert_eq!(metadata(&h, &t).await, before);
}

#[tokio::test]
async fn failed_exact_marker_delete_rolls_back_successful_metadata_update() {
    let (h, t) = fixture(h::database().await).await;
    marker(&h, &t).await;
    let before = metadata(&h, &t).await;
    db::execute(&h.state.database,"CREATE TRIGGER status_hold_marker BEFORE DELETE ON neev_agent_status_checks BEGIN SELECT RAISE(IGNORE); END",vec![]).await.unwrap();
    assert!(matches!(
        finalize_connection_check(&h.state, &h.alice_a, &t, supported()).await,
        Err(StatusError::Unavailable)
    ));
    assert_eq!(metadata(&h, &t).await, before);
    assert_eq!(count(&h).await, 1);
    db::execute(&h.state.database, "DROP TRIGGER status_hold_marker", vec![])
        .await
        .unwrap();
    assert!(matches!(
        finalize_connection_check(&h.state, &h.alice_a, &t, supported()).await,
        Ok(CheckFinalized::Applied(_))
    ));
}

#[tokio::test]
async fn malformed_persisted_owner_check_and_native_handle_fail_without_repair() {
    let (h, t) = fixture(h::database().await).await;
    marker(&h, &t).await;
    let before = metadata(&h, &t).await;
    db::execute(
        &h.state.database,
        "PRAGMA ignore_check_constraints=ON",
        vec![],
    )
    .await
    .unwrap();
    db::execute(
        &h.state.database,
        "UPDATE neev_agent_status_checks SET issued_at=1700000000.5",
        vec![],
    )
    .await
    .unwrap();
    assert!(matches!(
        activate_status_epoch(&h.state, &claim(2)).await,
        Err(StatusError::Unavailable)
    ));
    assert!(matches!(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex).await,
        Err(StatusError::Unavailable)
    ));
    db::execute(
        &h.state.database,
        "UPDATE neev_agent_status_checks SET issued_at=1700000000",
        vec![],
    )
    .await
    .unwrap();
    db::execute(
        &h.state.database,
        "UPDATE neev_agent_status_owner SET phase='closed'",
        vec![],
    )
    .await
    .unwrap();
    assert!(matches!(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex).await,
        Err(StatusError::Unavailable)
    ));
    assert_eq!(
        disable_status_checks(&h.state, &epoch()).await,
        Err(StatusError::Unavailable)
    );
    assert_eq!(metadata(&h, &t).await, before);
    assert_eq!(count(&h).await, 1);
    db::execute(
        &h.state.database,
        "UPDATE neev_agent_status_owner SET phase='ready'",
        vec![],
    )
    .await
    .unwrap();
    db::execute(
        &h.state.database,
        "UPDATE neev_agent_connections SET storage_handle=$1 WHERE id=$2",
        vec!["A".repeat(32).into(), t.connection_id.clone().into()],
    )
    .await
    .unwrap();
    assert!(matches!(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex).await,
        Err(StatusError::Unavailable)
    ));
}

#[tokio::test]
async fn malformed_numeric_native_connection_metadata_is_not_coerced_or_repaired() {
    let (h, t) = fixture(h::database().await).await;
    marker(&h, &t).await;
    db::execute(
        &h.state.database,
        "UPDATE neev_agent_connections SET checked_at=1700000000.5 WHERE id=$1",
        vec![t.connection_id.clone().into()],
    )
    .await
    .unwrap();
    assert!(matches!(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex).await,
        Err(StatusError::Unavailable)
    ));
    assert!(matches!(
        finalize_connection_check(&h.state, &h.alice_a, &t, supported()).await,
        Err(StatusError::Unavailable)
    ));
    assert_eq!(count(&h).await, 1);
    let row = db::one(
        &h.state.database,
        "SELECT typeof(checked_at) AS kind FROM neev_agent_connections WHERE id=$1",
        vec![t.connection_id.clone().into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.try_get::<String>("", "kind").unwrap(), "real");
}

#[tokio::test]
async fn native_authority_recheck_after_publication_rolls_back_both_writes() {
    let (h, t) = fixture(h::database().await).await;
    marker(&h, &t).await;
    let before = metadata(&h, &t).await;
    db::execute(&h.state.database,"CREATE TRIGGER status_revoke_before_commit AFTER UPDATE ON neev_agent_connections BEGIN UPDATE neev_sessions SET revoked_at=1700000000; END",vec![]).await.unwrap();
    assert!(matches!(
        finalize_connection_check(&h.state, &h.alice_a, &t, supported()).await,
        Err(StatusError::Agent(AgentError::Auth(_)))
    ));
    assert_eq!(metadata(&h, &t).await, before);
    assert_eq!(count(&h).await, 1);
    db::execute(
        &h.state.database,
        "DROP TRIGGER status_revoke_before_commit",
        vec![],
    )
    .await
    .unwrap();
    assert!(matches!(
        finalize_connection_check(&h.state, &h.alice_a, &t, supported()).await,
        Ok(CheckFinalized::Applied(_))
    ));
}

#[tokio::test]
async fn changed_native_generation_or_handle_fences_old_check_and_never_reuses_it() {
    for generation_change in [false, true] {
        let (h, t) = fixture(h::database().await).await;
        marker(&h, &t).await;
        let sql = if generation_change {
            "UPDATE neev_agent_connections SET generation=2 WHERE id=$1"
        } else {
            "UPDATE neev_agent_connections SET storage_handle='cccccccccccccccccccccccccccccccc' WHERE id=$1"
        };
        db::execute(&h.state.database, sql, vec![t.connection_id.clone().into()])
            .await
            .unwrap();
        let before = metadata(&h, &t).await;
        assert!(matches!(
            finalize_connection_check(&h.state, &h.alice_a, &t, supported()).await,
            Ok(CheckFinalized::Stale)
        ));
        assert_eq!(metadata(&h, &t).await, before);
        assert_eq!(count(&h).await, 1);
        let next = admission(
            prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex)
                .await
                .unwrap(),
        );
        assert_ne!(next.check_id, t.check_id);
        assert_eq!(next.generation, if generation_change { 2 } else { 1 });
        assert_eq!(
            next.storage_handle,
            if generation_change {
                t.storage_handle.clone()
            } else {
                "c".repeat(32)
            }
        );
        let mut stale_member = h.alice_a.clone();
        stale_member.member_revision += 1;
        assert!(matches!(
            prepare_connection_check(&h.state, &stale_member, &epoch(), Provider::Codex).await,
            Err(StatusError::Agent(AgentError::Auth(_)))
        ));
        assert!(matches!(
            finalize_connection_check(&h.state, &stale_member, &next, supported()).await,
            Err(StatusError::Agent(AgentError::Auth(_)))
        ));
        assert_eq!(metadata(&h, &t).await, before);
        assert_eq!(count(&h).await, 1);
    }
}

// SeaORM calls this metric callback after the real session fetch completes,
// before its awaiting authorization recheck resumes. Only SQL identity and
// scalar event/time data are inspected; statement values are never accessed.
struct DeadlineProbe {
    queries: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    last_authorization_time: std::sync::Arc<std::sync::atomic::AtomicI64>,
    advanced: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn advance_clock_after_authorization(
    harness: &mut Harness,
    query_number: usize,
    time: i64,
) -> DeadlineProbe {
    use std::sync::{
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
        Arc,
    };
    let queries = Arc::new(AtomicUsize::new(0));
    let last_authorization_time = Arc::new(AtomicI64::new(NOW));
    let advanced = Arc::new(AtomicBool::new(false));
    let probe = DeadlineProbe {
        queries: queries.clone(),
        last_authorization_time: last_authorization_time.clone(),
        advanced: advanced.clone(),
    };
    let clock = harness.clock.clone();
    Arc::get_mut(&mut harness.state).expect("owned test state").database.set_metric_callback(move |info| {
        if info.failed || info.statement.sql != "SELECT id,user_id,account_revision,org_id,member_id,member_revision,org_revision,authenticated_at,expires_at,revoked_at FROM neev_sessions WHERE id=$1" {
            return;
        }
        let current = queries.fetch_add(1, Ordering::SeqCst) + 1;
        if current == query_number {
            clock.0.store(time, Ordering::SeqCst);
            advanced.store(true, Ordering::SeqCst);
        }
        last_authorization_time.store(clock.0.load(Ordering::SeqCst), Ordering::SeqCst);
    });
    probe
}

#[tokio::test]
async fn deadline_prepare_never_commits_after_an_expired_last_authorization_query() {
    use std::sync::atomic::Ordering;
    let (mut harness, t) = fixture(h::database().await).await;
    let before = metadata(&harness, &t).await;
    // The old helper performed three rechecks: initial, local final, duplicate.
    // Removing that last await legitimately removes this injection point. The
    // paired test below still injects expiry at the remaining final recheck.
    let probe = advance_clock_after_authorization(&mut harness, 3, t.expires_at);
    let result =
        prepare_connection_check(&harness.state, &harness.alice_a, &epoch(), Provider::Codex).await;
    match result {
        Ok(result) => {
            let issued = admission(result);
            assert!(
                probe.last_authorization_time.load(Ordering::SeqCst) < issued.expires_at,
                "committed ticket after last actual authorization query observed expiry"
            );
            assert_eq!(issued.issued_at, NOW);
            assert_eq!(issued.expires_at, NOW + 60);
            assert_eq!(count(&harness).await, 1);
        }
        Err(error) => {
            assert_eq!(error, StatusError::Unavailable);
            assert!(probe.advanced.load(Ordering::SeqCst));
            assert_eq!(count(&harness).await, 0);
        }
    }
    assert!(probe.queries.load(Ordering::SeqCst) >= 2);
    assert_eq!(metadata(&harness, &t).await, before);
}

#[tokio::test]
async fn deadline_prepare_expiry_in_final_recheck_rolls_back_with_unexpired_control() {
    use std::sync::atomic::Ordering;
    for expired in [false, true] {
        let (mut harness, t) = fixture(h::database().await).await;
        let before = metadata(&harness, &t).await;
        let probe = advance_clock_after_authorization(
            &mut harness,
            2,
            if expired {
                t.expires_at
            } else {
                t.expires_at - 1
            },
        );
        let result =
            prepare_connection_check(&harness.state, &harness.alice_a, &epoch(), Provider::Codex)
                .await;
        assert!(
            probe.advanced.load(Ordering::SeqCst),
            "final real authorization query must exercise clock advance"
        );
        if expired {
            assert!(matches!(result, Err(StatusError::Unavailable)));
            assert_eq!(count(&harness).await, 0);
        } else {
            let issued = admission(result.unwrap());
            assert_eq!(issued.issued_at, NOW);
            assert_eq!(issued.expires_at, NOW + 60);
            assert!(probe.last_authorization_time.load(Ordering::SeqCst) < issued.expires_at);
            assert_eq!(count(&harness).await, 1);
        }
        assert_eq!(metadata(&harness, &t).await, before);
    }
}

#[tokio::test]
async fn deadline_publication_never_commits_after_an_expired_last_authorization_query() {
    use std::sync::atomic::Ordering;
    let (mut harness, t) = fixture(h::database().await).await;
    marker(&harness, &t).await;
    let before = metadata(&harness, &t).await;
    // Publication has an additional authority recheck after its blocking locks.
    // Query four is the old duplicate; query three is the remaining final check.
    let probe = advance_clock_after_authorization(&mut harness, 4, t.expires_at);
    let result = finalize_connection_check(&harness.state, &harness.alice_a, &t, supported()).await;
    match result {
        Ok(CheckFinalized::Applied(view)) => {
            assert!(probe.last_authorization_time.load(Ordering::SeqCst)<t.expires_at,
                "committed account publication after last actual authorization query observed expiry");
            assert_eq!(view.account_label.as_deref(), Some("checked synthetic"));
            assert_eq!(
                metadata(&harness, &t).await.1.as_deref(),
                Some("checked synthetic")
            );
            assert_eq!(count(&harness).await, 0);
        }
        Err(StatusError::Unavailable) => {
            assert!(probe.advanced.load(Ordering::SeqCst));
            assert_eq!(metadata(&harness, &t).await, before);
            assert_eq!(count(&harness).await, 1);
        }
        _ => panic!("exact unexpired check must apply or reject an advanced deadline"),
    }
    assert!(probe.queries.load(Ordering::SeqCst) >= 3);
}

#[tokio::test]
async fn deadline_publication_expiry_in_final_recheck_rolls_back_with_unexpired_control() {
    use std::sync::atomic::Ordering;
    for expired in [false, true] {
        let (mut harness, t) = fixture(h::database().await).await;
        marker(&harness, &t).await;
        let before = metadata(&harness, &t).await;
        let probe = advance_clock_after_authorization(
            &mut harness,
            3,
            if expired {
                t.expires_at
            } else {
                t.expires_at - 1
            },
        );
        let result =
            finalize_connection_check(&harness.state, &harness.alice_a, &t, supported()).await;
        assert!(
            probe.advanced.load(Ordering::SeqCst),
            "final real authorization query must exercise clock advance"
        );
        if expired {
            assert!(matches!(result, Err(StatusError::Unavailable)));
            assert_eq!(metadata(&harness, &t).await, before);
            assert_eq!(count(&harness).await, 1);
        } else {
            assert!(matches!(result, Ok(CheckFinalized::Applied(_))));
            assert!(probe.last_authorization_time.load(Ordering::SeqCst) < t.expires_at);
            let after = metadata(&harness, &t).await;
            assert_eq!(after.0, "connected");
            assert_eq!(after.1.as_deref(), Some("checked synthetic"));
            assert_eq!(after.2, Some(NOW));
            assert_eq!(after.3, None);
            assert_eq!(after.4, t.storage_handle);
            assert_eq!(after.5, t.generation);
            assert_eq!(after.6, NOW);
            assert_eq!(count(&harness).await, 0);
        }
    }
}

async fn admission_block(db: &DatabaseConnection) -> DatabaseTransaction {
    let tx = db::begin(db).await.unwrap();
    db::lock_admission_guard(&tx).await.unwrap();
    tx
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_old_prepare_and_finalize_serialize_with_higher_epoch_in_both_orders() {
    for finalize in [false, true] {
        for old_first in [false, true] {
            let (database, admin, schema) = h::postgres_database().await;
            let (h, t) = fixture(database.clone()).await;
            if finalize {
                marker(&h, &t).await;
            }
            let before = metadata(&h, &t).await;
            let blocker = admission_block(&database).await;
            let spawn_old = || {
                let state = h.state.clone();
                let actor = h.alice_a.clone();
                let t = t.clone();
                tokio::spawn(async move {
                    if finalize {
                        finalize_connection_check(&state, &actor, &t, supported())
                            .await
                            .map(|r| matches!(r, CheckFinalized::Applied(_)))
                    } else {
                        prepare_connection_check(&state, &actor, &epoch(), Provider::Codex)
                            .await
                            .map(|_| true)
                    }
                })
            };
            let spawn_new = || {
                let state = h.state.clone();
                tokio::spawn(
                    async move { activate_status_epoch(&state, &claim(2)).await.map(|_| ()) },
                )
            };
            let (old, new) = if old_first {
                let old = spawn_old();
                h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
                let new = spawn_new();
                h::wait_for_postgres_locks(&admin, "%neev_schema_version%FOR UPDATE%", 2).await;
                (old, new)
            } else {
                let new = spawn_new();
                h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
                let old = spawn_old();
                h::wait_for_postgres_locks(&admin, "%neev_schema_version%FOR UPDATE%", 2).await;
                (old, new)
            };
            blocker.commit().await.unwrap();
            assert_eq!(new.await.unwrap(), Ok(()));
            let result = old.await.unwrap();
            if old_first {
                assert_eq!(result, Ok(true));
            } else {
                assert!(matches!(result, Ok(false) | Err(StatusError::Stale)));
            }
            assert_eq!(count(&h).await, 0);
            if !(old_first && finalize) {
                assert_eq!(metadata(&h, &t).await, before);
            }
            // Callback's first database read happens only after activation committed.
            assert!(matches!(
                finalize_connection_check(&h.state, &h.alice_a, &t, supported()).await,
                Ok(CheckFinalized::Stale)
            ));
            assert!(matches!(
                prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex).await,
                Err(StatusError::Stale)
            ));
            drop(h);
            h::cleanup_postgres(database, admin, schema).await;
        }
    }
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_same_epoch_enable_disable_serialize_in_both_orders() {
    for enable_first in [false, true] {
        let (database, admin, schema) = h::postgres_database().await;
        let (h, t) = fixture(database.clone()).await;
        db::execute(
            &database,
            "UPDATE neev_agent_status_owner SET phase='initializing'",
            vec![],
        )
        .await
        .unwrap();
        let before = metadata(&h, &t).await;
        let blocker = admission_block(&database).await;
        let spawn = |enable| {
            let state = h.state.clone();
            tokio::spawn(async move {
                if enable {
                    enable_status_checks(&state, &epoch(), &ready(&epoch())).await
                } else {
                    disable_status_checks(&state, &epoch()).await
                }
            })
        };
        let first = spawn(enable_first);
        h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
        let second = spawn(!enable_first);
        h::wait_for_postgres_locks(&admin, "%neev_schema_version%FOR UPDATE%", 2).await;
        blocker.commit().await.unwrap();
        assert_eq!(first.await.unwrap(), Ok(()));
        assert_eq!(
            second.await.unwrap(),
            if enable_first {
                Ok(())
            } else {
                Err(StatusError::Stale)
            }
        );
        assert_eq!(metadata(&h, &t).await, before);
        assert!(matches!(
            activate_status_epoch(&h.state, &claim(1)).await,
            Ok(Activation::Existing(_, StatusPhase::Closed))
        ));
        drop(h);
        h::cleanup_postgres(database, admin, schema).await;
    }
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_finalize_disconnect_logout_and_duplicate_serialize_in_both_orders() {
    for mutation in ["disconnect", "logout", "duplicate"] {
        for finalize_first in [false, true] {
            let (database, admin, schema) = h::postgres_database().await;
            let (h, t) = fixture(database.clone()).await;
            marker(&h, &t).await;
            let blocker = admission_block(&database).await;
            let spawn_final = || {
                let state = h.state.clone();
                let actor = h.alice_a.clone();
                let t = t.clone();
                tokio::spawn(async move {
                    finalize_connection_check(&state, &actor, &t, supported()).await
                })
            };
            let spawn_mutation = || {
                let state = h.state.clone();
                let actor = h.alice_a.clone();
                let t = t.clone();
                tokio::spawn(async move {
                    match mutation {
                        "disconnect" => intents::disconnect(
                            &state,
                            &actor,
                            Provider::Codex,
                            "status-disconnect",
                        )
                        .await
                        .map(|_| false)
                        .map_err(StatusError::Agent),
                        "logout" => {
                            session::revoke(&state.database, &actor.identity, state.clock.as_ref())
                                .await
                                .map(|_| false)
                                .map_err(|e| StatusError::Agent(AgentError::Auth(e)))
                        }
                        _ => finalize_connection_check(&state, &actor, &t, supported())
                            .await
                            .map(|r| matches!(r, CheckFinalized::Applied(_))),
                    }
                })
            };
            let (finalizer, invalidator) = if finalize_first {
                let f = spawn_final();
                h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
                let i = spawn_mutation();
                h::wait_for_postgres_locks(&admin, "%neev_schema_version%FOR UPDATE%", 2).await;
                (f, i)
            } else {
                let i = spawn_mutation();
                h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
                let f = spawn_final();
                h::wait_for_postgres_locks(&admin, "%neev_schema_version%FOR UPDATE%", 2).await;
                (f, i)
            };
            blocker.commit().await.unwrap();
            let f = finalizer.await.unwrap();
            let i = invalidator.await.unwrap().unwrap();
            if finalize_first {
                assert!(matches!(f, Ok(CheckFinalized::Applied(_))));
                assert!(!i);
            } else if mutation == "logout" {
                assert!(matches!(f, Err(StatusError::Agent(AgentError::Auth(_)))));
            } else {
                assert!(matches!(f, Ok(CheckFinalized::Stale)));
                assert_eq!(i, mutation == "duplicate");
            }
            let m = metadata(&h, &t).await;
            if mutation == "logout" {
                assert_eq!(
                    m.1.as_deref(),
                    Some(if finalize_first {
                        "checked synthetic"
                    } else {
                        "synthetic account"
                    })
                );
            }
            if mutation == "duplicate" {
                assert_eq!(m.1.as_deref(), Some("checked synthetic"));
                assert_eq!(count(&h).await, 0);
            }
            drop(h);
            h::cleanup_postgres(database, admin, schema).await;
        }
    }
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_expiry_after_blocking_and_cancellation_leave_observed_marker_state() {
    for cancel in [false, true] {
        for post_admission in [false, true] {
            let (database, admin, schema) = h::postgres_database().await;
            let (h, t) = fixture(database.clone()).await;
            marker(&h, &t).await;
            let before = metadata(&h, &t).await;
            // A standalone owner blocker never waits for an admission-owning task.
            let blocker = if post_admission {
                let tx = db::begin(&database).await.unwrap();
                db::one(
                    &tx,
                    "SELECT id FROM neev_agent_status_owner WHERE id=1 FOR UPDATE",
                    vec![],
                )
                .await
                .unwrap();
                tx
            } else {
                admission_block(&database).await
            };
            let state = h.state.clone();
            let actor = h.alice_a.clone();
            let target = t.clone();
            let task = tokio::spawn(async move {
                finalize_connection_check(&state, &actor, &target, supported()).await
            });
            h::wait_for_postgres_lock(
                &admin,
                if post_admission {
                    "%neev_agent_status_owner%FOR UPDATE%"
                } else {
                    "%neev_schema_version%FOR UPDATE%"
                },
            )
            .await;
            if cancel {
                task.abort();
                assert!(matches!(task.await,Err(e) if e.is_cancelled()));
                blocker.commit().await.unwrap();
                // Acquiring admission establishes that canceled driver's transaction drained.
                admission_block(&database).await.commit().await.unwrap();
                assert_eq!(count(&h).await, 1);
            } else {
                h.set_now(t.expires_at);
                blocker.commit().await.unwrap();
                assert!(matches!(
                    task.await.unwrap(),
                    Ok(CheckFinalized::Unavailable(CheckUnavailable::Expired))
                ));
                assert_eq!(count(&h).await, 0);
            }
            assert_eq!(metadata(&h, &t).await, before);
            drop(h);
            h::cleanup_postgres(database, admin, schema).await;
        }
    }
}

/// Connect Claude alongside the fixture's Codex account, returning a ticket for
/// the Claude connection.
async fn with_claude(h: &Harness) -> StatusTicket {
    let login = a::begin(h, &h.alice_a, Provider::Claude, "status-login-claude").await;
    let account = a::account();
    let handle = account.storage_handle.clone();
    completion::complete(&h.state, &h.alice_a, &login, account)
        .await
        .unwrap();
    StatusTicket {
        epoch: 1,
        boot_id: BOOT.into(),
        org_id: h.alice_a.org_id,
        user_id: h.alice_a.identity.user_id,
        provider: Provider::Claude,
        connection_id: login.connection_id,
        generation: login.generation,
        session_id: h.alice_a.identity.session_id.clone(),
        storage_handle: handle,
        check_id: "cccccccccccccccccccccccccccccccc".into(),
        issued_at: NOW,
        expires_at: NOW + 60,
    }
}

async fn claude_is_checkable_like_codex(db: DatabaseConnection) {
    let (h, codex) = fixture(db).await;
    let claude = with_claude(&h).await;
    // A Claude connection admits a check on its own terms …
    let ticket = admission(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Claude)
            .await
            .unwrap(),
    );
    assert_eq!(ticket.provider, Provider::Claude);
    assert_eq!(ticket.connection_id, claude.connection_id);
    // … and publishes against Claude, naming Claude in the result.
    // `CheckFinalized` is deliberately not Debug (it can carry an account
    // label), so match rather than unwrap it.
    let view = match finalize_connection_check(&h.state, &h.alice_a, &ticket, supported())
        .await
        .unwrap()
    {
        CheckFinalized::Applied(view) => view,
        _ => panic!("expected an applied check"),
    };
    assert_eq!(view.provider, Provider::Claude);
    assert_eq!(view.state, ConnectionState::Connected);
    assert_eq!(view.account_label.as_deref(), Some("checked synthetic"));
    // The Codex connection is untouched: a check publishes its own label, so
    // the login's is proof nothing was written there.
    let (_, codex_label, ..) = metadata(&h, &codex).await;
    assert_eq!(codex_label.as_deref(), Some("synthetic account"));
    assert_eq!(count(&h).await, 0);
}

async fn each_provider_holds_its_own_check(db: DatabaseConnection) {
    let (h, _codex) = fixture(db).await;
    with_claude(&h).await;
    let codex_ticket = admission(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex)
            .await
            .unwrap(),
    );
    // One provider being mid-check must not make the other look busy.
    let claude_ticket = admission(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Claude)
            .await
            .unwrap(),
    );
    assert_ne!(codex_ticket.connection_id, claude_ticket.connection_id);
    assert_eq!(count(&h).await, 2);
    // Each rejoins its own slot rather than stealing the other's.
    assert_eq!(
        admission(
            prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Claude)
                .await
                .unwrap()
        )
        .check_id,
        claude_ticket.check_id
    );
    // Finalising one leaves the other's ticket live.
    finalize_connection_check(&h.state, &h.alice_a, &claude_ticket, supported())
        .await
        .unwrap();
    assert_eq!(count(&h).await, 1);
    assert!(matches!(
        finalize_connection_check(&h.state, &h.alice_a, &codex_ticket, supported()).await,
        Ok(CheckFinalized::Applied(view)) if view.provider == Provider::Codex
    ));
    assert_eq!(count(&h).await, 0);
}

async fn a_ticket_cannot_cross_providers(db: DatabaseConnection) {
    let (h, _codex) = fixture(db).await;
    let claude = with_claude(&h).await;
    let codex_ticket = admission(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Codex)
            .await
            .unwrap(),
    );
    // A real Codex ticket relabelled as Claude must not settle the Claude
    // connection: the provider is part of what the ticket proves.
    let mut forged = codex_ticket.clone();
    forged.provider = Provider::Claude;
    assert!(matches!(
        finalize_connection_check(&h.state, &h.alice_a, &forged, supported()).await,
        Ok(CheckFinalized::Stale)
    ));
    // Nor may it clear the other provider's bookkeeping.
    clear_connection_check(&h.state, &epoch(), &forged)
        .await
        .unwrap_err();
    assert_eq!(count(&h).await, 1);
    // And the Claude connection never moved.
    let (state_name, label, ..) = metadata(&h, &claude).await;
    assert_eq!(state_name, "connected");
    assert_eq!(label.as_deref(), Some("synthetic account"));
    // The genuine Codex ticket still settles.
    assert!(matches!(
        finalize_connection_check(&h.state, &h.alice_a, &codex_ticket, supported()).await,
        Ok(CheckFinalized::Applied(view)) if view.provider == Provider::Codex
    ));
}

async fn a_claude_check_goes_stale_when_its_connection_moves(db: DatabaseConnection) {
    let (h, _codex) = fixture(db).await;
    with_claude(&h).await;
    let ticket = admission(
        prepare_connection_check(&h.state, &h.alice_a, &epoch(), Provider::Claude)
            .await
            .unwrap(),
    );
    // A disconnect between issue and finalize moves the generation on.
    db::execute(
        &h.state.database,
        "UPDATE neev_agent_connections SET generation=generation+1 WHERE id=$1",
        vec![ticket.connection_id.clone().into()],
    )
    .await
    .unwrap();
    assert!(matches!(
        finalize_connection_check(&h.state, &h.alice_a, &ticket, supported()).await,
        Ok(CheckFinalized::Stale)
    ));
    // Stale is not a licence to publish: the row keeps the login's own label.
    let (_, label, ..) = metadata(&h, &ticket).await;
    assert_eq!(label.as_deref(), Some("synthetic account"));
}

#[tokio::test]
async fn sqlite_claude_is_checkable_like_codex() {
    claude_is_checkable_like_codex(h::database().await).await;
}

#[tokio::test]
async fn sqlite_each_provider_holds_its_own_check() {
    each_provider_holds_its_own_check(h::database().await).await;
}

#[tokio::test]
async fn sqlite_a_ticket_cannot_cross_providers() {
    a_ticket_cannot_cross_providers(h::database().await).await;
}

#[tokio::test]
async fn sqlite_a_claude_check_goes_stale_when_its_connection_moves() {
    a_claude_check_goes_stale_when_its_connection_moves(h::database().await).await;
}
