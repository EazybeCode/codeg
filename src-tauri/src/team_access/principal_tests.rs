use std::{sync::Arc, time::Duration as StdDuration};

use chrono::Duration;
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DbBackend, EntityTrait, IntoActiveModel, Set, Statement,
    TransactionTrait,
};
use tokio::sync::{Barrier, Semaphore};

use super::{
    core::TeamAuthError,
    credentials::CredentialService,
    github::GithubEvidenceError,
    principal::{ProofRequirement, PrincipalService},
    session::TeamRole,
    test_auth::{
        cookie_headers, create_postgres_principal_fixture, drop_postgres_fixture, fixed_now,
        membership_evidence, principal_fixture, token_hash, tokens, wait_for_postgres_lock,
        ALICE_A_MEMBERSHIP_ID, ALICE_A_TOKEN, ALICE_B_MEMBERSHIP_ID, ALICE_B_TOKEN,
        ALICE_GITHUB_ID, ALICE_USER_ID, BOB_A_TOKEN, CAROL_B_TOKEN,
        PRINCIPAL_ORG_A_GITHUB_ID, PRINCIPAL_ORG_A_ID, PRINCIPAL_ORG_B_GITHUB_ID,
        PRINCIPAL_ORG_B_ID,
    },
};
use crate::db::entities::{organization_membership, team_github_credential, team_session};

fn alice_a_evidence(role: TeamRole) -> super::github::GithubMembershipEvidence {
    membership_evidence(
        ALICE_GITHUB_ID,
        PRINCIPAL_ORG_A_GITHUB_ID,
        "alice-a",
        role,
    )
}

fn alice_b_evidence(role: TeamRole) -> super::github::GithubMembershipEvidence {
    membership_evidence(
        ALICE_GITHUB_ID,
        PRINCIPAL_ORG_B_GITHUB_ID,
        "alice-b",
        role,
    )
}

async fn set_verified_at(
    db: &sea_orm::DatabaseConnection,
    membership_id: i32,
    verified_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    let model = organization_membership::Entity::find_by_id(membership_id)
        .one(db)
        .await
        .unwrap()
        .unwrap();
    let mut active = model.into_active_model();
    active.verified_at = Set(verified_at);
    active.update(db).await.unwrap();
}

async fn permits(semaphore: &Semaphore, count: u32, message: &'static str) {
    let permits = tokio::time::timeout(StdDuration::from_secs(5), semaphore.acquire_many(count))
        .await
        .expect(message)
        .expect("test semaphore remains open");
    permits.forget();
}

#[tokio::test]
async fn cached_principal_accepts_only_current_proof_and_returns_bound_internal_context() {
    for age in [0, 59] {
        let fixture = principal_fixture(Vec::new()).await;
        set_verified_at(
            &fixture.db,
            ALICE_A_MEMBERSHIP_ID,
            Some(fixed_now() - Duration::seconds(age)),
        )
        .await;
        let context = fixture
            .service()
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
            .await
            .unwrap();
        assert_eq!(context.session_id, token_hash(ALICE_A_TOKEN));
        assert_eq!(context.csrf_hash, format!("{:064x}", ALICE_A_MEMBERSHIP_ID));
        assert_eq!(context.user_id, ALICE_USER_ID);
        assert_eq!(context.org_id, PRINCIPAL_ORG_A_ID);
        assert_eq!(context.membership_id, ALICE_A_MEMBERSHIP_ID);
        assert_eq!(context.role, TeamRole::Admin);
        assert_eq!(context.authorization_revision, 7);
        assert_eq!(context.credential_generation, 1);
        assert_eq!(
            context.verified_at,
            fixed_now() - Duration::seconds(age)
        );
        assert_eq!(fixture.github.verification_calls(), 0);
    }
}

#[tokio::test]
async fn bob_member_and_carol_admin_positive_controls_resolve_their_own_orgs() {
    let fixture = principal_fixture(Vec::new()).await;
    let service = fixture.service();
    let bob = service
        .resolve(&cookie_headers(BOB_A_TOKEN), ProofRequirement::Cached)
        .await
        .unwrap();
    let carol = service
        .resolve(&cookie_headers(CAROL_B_TOKEN), ProofRequirement::Cached)
        .await
        .unwrap();
    assert_eq!(
        (bob.user_id, bob.org_id, bob.role),
        (302, PRINCIPAL_ORG_A_ID, TeamRole::Member)
    );
    assert_eq!(
        (carol.user_id, carol.org_id, carol.role),
        (303, PRINCIPAL_ORG_B_ID, TeamRole::Admin)
    );
}

#[tokio::test]
async fn expired_future_or_missing_cached_proof_never_becomes_fresh_on_provider_failure() {
    for verified_at in [
        Some(fixed_now() - Duration::seconds(60)),
        Some(fixed_now() + Duration::seconds(1)),
        None,
    ] {
        let fixture = principal_fixture(vec![Err(GithubEvidenceError::Unavailable)]).await;
        set_verified_at(&fixture.db, ALICE_A_MEMBERSHIP_ID, verified_at).await;
        assert_eq!(
            fixture
                .service()
                .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
                .await,
            Err(TeamAuthError::Unavailable)
        );
        let stored = organization_membership::Entity::find_by_id(ALICE_A_MEMBERSHIP_ID)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.verified_at, verified_at);
    }
}

#[tokio::test]
async fn fresh_always_verifies_and_completed_proofs_are_not_reused() {
    let fixture = principal_fixture(vec![
        Ok(alice_a_evidence(TeamRole::Admin)),
        Ok(alice_a_evidence(TeamRole::Admin)),
    ])
    .await;
    let service = fixture.service();

    let first = service
        .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
        .await
        .unwrap();
    fixture.clock.advance(Duration::seconds(1));
    let second = service
        .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
        .await
        .unwrap();

    assert_eq!(fixture.github.verification_calls(), 2);
    assert_eq!(first.verified_at, fixed_now());
    assert_eq!(second.verified_at, fixed_now() + Duration::seconds(1));
}

#[tokio::test]
async fn concurrent_fresh_consumers_share_only_the_in_flight_proof() {
    let fixture = principal_fixture(vec![
        Ok(alice_a_evidence(TeamRole::Admin)),
        Ok(alice_a_evidence(TeamRole::Admin)),
    ])
    .await;
    let gate = fixture.github.gate_membership();
    let flight_observer = Arc::new(Semaphore::new(0));
    let mut principal = PrincipalService::new(
        fixture.db.clone(),
        fixture.credentials.clone(),
        fixture.github.clone(),
        fixture.clock.clone(),
    );
    principal.observe_proof_flight(flight_observer.clone());
    let service = Arc::new(principal);
    let start = Arc::new(Barrier::new(3));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let service = service.clone();
        let start = start.clone();
        tasks.push(tokio::spawn(async move {
            start.wait().await;
            service
                .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
                .await
        }));
    }
    start.wait().await;
    permits(
        &flight_observer,
        2,
        "both consumers reached the proof flight",
    )
    .await;
    assert_eq!(fixture.github.verification_calls(), 1);
    gate.release.notify_waiters();
    for task in tasks {
        assert_eq!(
            tokio::time::timeout(StdDuration::from_secs(5), task)
                .await
                .expect("shared consumer completed")
                .unwrap()
                .unwrap()
                .org_id,
            PRINCIPAL_ORG_A_ID
        );
    }

    fixture.github.clear_membership_gate();
    fixture.clock.advance(Duration::seconds(1));
    service
        .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
        .await
        .unwrap();
    assert_eq!(fixture.github.verification_calls(), 2);
}

#[tokio::test]
async fn stable_identity_generation_and_database_shapes_are_independent_authority_requirements() {
    let wrong_github = principal_fixture(Vec::new()).await;
    let mut credential = team_github_credential::Entity::find_by_id(ALICE_USER_ID)
        .one(&wrong_github.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    credential.github_user_id = Set(99_999);
    credential.update(&wrong_github.db).await.unwrap();
    assert_eq!(
        wrong_github
            .service()
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
            .await,
        Err(TeamAuthError::Unauthorized)
    );

    let wrong_generation = principal_fixture(Vec::new()).await;
    let mut credential = team_github_credential::Entity::find_by_id(ALICE_USER_ID)
        .one(&wrong_generation.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    credential.generation = Set(2);
    credential.update(&wrong_generation.db).await.unwrap();
    assert_eq!(
        wrong_generation
            .service()
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
            .await,
        Err(TeamAuthError::Unauthorized)
    );

    let malformed_csrf = principal_fixture(Vec::new()).await;
    let mut session = team_session::Entity::find_by_id(token_hash(ALICE_A_TOKEN))
        .one(&malformed_csrf.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    session.csrf_hash = Set("raw-client-secret".into());
    session.update(&malformed_csrf.db).await.unwrap();
    assert_eq!(
        malformed_csrf
            .service()
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
            .await,
        Err(TeamAuthError::Unauthorized)
    );
}

#[tokio::test]
async fn legacy_user_scope_role_and_token_columns_never_supply_authority() {
    let fixture = principal_fixture(Vec::new()).await;
    let context = fixture
        .service()
        .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
        .await
        .unwrap();
    assert_eq!(context.org_id, PRINCIPAL_ORG_A_ID);
    assert_eq!(context.role, TeamRole::Admin);
    assert_ne!(context.org_id, 202);
    assert!(fixture.github.membership_calls().is_empty());
}

#[tokio::test]
async fn provider_failures_fail_closed_and_apply_only_the_specified_conditional_transition() {
    let unavailable = principal_fixture(vec![Err(GithubEvidenceError::Unavailable)]).await;
    assert_eq!(
        unavailable
            .service()
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
            .await,
        Err(TeamAuthError::Unavailable)
    );
    assert_eq!(
        organization_membership::Entity::find_by_id(ALICE_A_MEMBERSHIP_ID)
            .one(&unavailable.db)
            .await
            .unwrap()
            .unwrap()
            .verified_at,
        Some(fixed_now())
    );

    let not_member = principal_fixture(vec![Err(GithubEvidenceError::NotMember)]).await;
    assert_eq!(
        not_member
            .service()
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
            .await,
        Err(TeamAuthError::Unauthorized)
    );
    let revoked = organization_membership::Entity::find_by_id(ALICE_A_MEMBERSHIP_ID)
        .one(&not_member.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(revoked.state, "revoked");
    assert_eq!(revoked.authorization_revision, 8);
    assert_eq!(revoked.verified_at, None);
    let other_org = organization_membership::Entity::find_by_id(ALICE_B_MEMBERSHIP_ID)
        .one(&not_member.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other_org.state, "active");
    assert_eq!(other_org.authorization_revision, 7);

    let reauth = principal_fixture(vec![Err(GithubEvidenceError::ReauthenticationRequired)]).await;
    assert_eq!(
        reauth
            .service()
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
            .await,
        Err(TeamAuthError::Unauthorized)
    );
    let credential = team_github_credential::Entity::find_by_id(ALICE_USER_ID)
        .one(&reauth.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(credential.state, "reauth_required");
    assert!(credential.encrypted_payload.is_none());
}

#[tokio::test]
async fn mismatched_provider_stable_ids_never_persist_authority() {
    for evidence in [
        membership_evidence(
            99_999,
            PRINCIPAL_ORG_A_GITHUB_ID,
            "alice-a",
            TeamRole::Admin,
        ),
        membership_evidence(
            ALICE_GITHUB_ID,
            88_888,
            "alice-a",
            TeamRole::Admin,
        ),
    ] {
        let fixture = principal_fixture(vec![Ok(evidence)]).await;
        let old_proof = fixed_now() - Duration::seconds(30);
        set_verified_at(&fixture.db, ALICE_A_MEMBERSHIP_ID, Some(old_proof)).await;
        assert_eq!(
            fixture
                .service()
                .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
                .await,
            Err(TeamAuthError::Unauthorized)
        );
        assert_eq!(
            organization_membership::Entity::find_by_id(ALICE_A_MEMBERSHIP_ID)
                .one(&fixture.db)
                .await
                .unwrap()
                .unwrap()
                .verified_at,
            Some(old_proof)
        );
    }
}

#[tokio::test]
async fn credential_generation_change_invalidates_cached_membership_proof() {
    let fixture = principal_fixture(vec![Err(GithubEvidenceError::Unavailable)]).await;
    let transaction = fixture.db.begin().await.unwrap();
    fixture
        .credentials
        .install_in_transaction(
            &transaction,
            ALICE_USER_ID,
            ALICE_GITHUB_ID,
            &tokens("new-login-token", None, None, None),
            fixed_now(),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    assert_eq!(
        fixture
            .service()
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
            .await,
        Err(TeamAuthError::Unavailable)
    );
    let membership = organization_membership::Entity::find_by_id(ALICE_A_MEMBERSHIP_ID)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(membership.credential_generation, 2);
    assert_eq!(membership.verified_at, None);
}

#[tokio::test]
async fn local_block_revocation_and_logout_during_verification_cannot_be_overwritten() {
    for change in ["blocked", "revoked", "logout"] {
        let fixture = principal_fixture(vec![Ok(alice_a_evidence(TeamRole::Admin))]).await;
        let gate = fixture.github.gate_membership();
        let service = fixture.service();
        let resolve = tokio::spawn(async move {
            service
                .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
                .await
        });
        permits(&gate.started, 1, "provider verification started").await;
        if change == "logout" {
            team_session::Entity::delete_by_id(token_hash(ALICE_A_TOKEN))
                .exec(&fixture.db)
                .await
                .unwrap();
        } else {
            let membership = organization_membership::Entity::find_by_id(ALICE_A_MEMBERSHIP_ID)
                .one(&fixture.db)
                .await
                .unwrap()
                .unwrap();
            let mut active = membership.into_active_model();
            active.state = Set(change.into());
            active.authorization_revision = Set(8);
            active.verified_at = Set(None);
            active.update(&fixture.db).await.unwrap();
        }
        gate.release.notify_waiters();
        assert_eq!(
            tokio::time::timeout(StdDuration::from_secs(5), resolve)
                .await
                .expect("racing resolver completed")
                .unwrap(),
            Err(TeamAuthError::Unauthorized)
        );
        let membership = organization_membership::Entity::find_by_id(ALICE_A_MEMBERSHIP_ID)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap();
        if change != "logout" {
            assert_eq!(membership.state, change);
            assert_eq!(membership.authorization_revision, 8);
            assert_eq!(membership.verified_at, None);
        }
    }
}

#[tokio::test]
async fn stale_reauthentication_failure_cannot_invalidate_a_newer_login() {
    let fixture = principal_fixture(vec![Err(GithubEvidenceError::ReauthenticationRequired)]).await;
    let gate = fixture.github.gate_membership();
    let service = fixture.service();
    let resolve = tokio::spawn(async move {
        service
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
            .await
    });
    permits(&gate.started, 1, "stale reauthentication proof started").await;

    let transaction = fixture.db.begin().await.unwrap();
    fixture
        .credentials
        .install_in_transaction(
            &transaction,
            ALICE_USER_ID,
            ALICE_GITHUB_ID,
            &tokens("newer-login", None, None, None),
            fixed_now(),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    gate.release.notify_waiters();

    assert_eq!(
        tokio::time::timeout(StdDuration::from_secs(5), resolve)
            .await
            .expect("stale reauthentication resolver completed")
            .unwrap(),
        Err(TeamAuthError::Unauthorized)
    );
    let credential = team_github_credential::Entity::find_by_id(ALICE_USER_ID)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(credential.state, "active");
    assert_eq!(credential.generation, 2);
    assert!(team_session::Entity::find_by_id(token_hash(ALICE_A_TOKEN))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn github_role_change_increments_revision_without_repairing_existing_sessions() {
    let fixture = principal_fixture(vec![Ok(alice_a_evidence(TeamRole::Member))]).await;
    assert_eq!(
        fixture
            .service()
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
            .await,
        Err(TeamAuthError::Unauthorized)
    );
    let membership = organization_membership::Entity::find_by_id(ALICE_A_MEMBERSHIP_ID)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(membership.role, "member");
    assert_eq!(membership.authorization_revision, 8);
    assert_eq!(membership.verified_at, Some(fixed_now()));
    let session = team_session::Entity::find_by_id(token_hash(ALICE_A_TOKEN))
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.authorization_revision, 7);
}

#[tokio::test]
async fn same_user_different_org_cookies_never_share_or_switch_tenant_authority() {
    let fixture = principal_fixture(vec![
        Ok(alice_a_evidence(TeamRole::Admin)),
        Ok(alice_b_evidence(TeamRole::Member)),
    ])
    .await;
    let service = fixture.service();
    let alice_a = service
        .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
        .await
        .unwrap();
    let alice_b = service
        .resolve(&cookie_headers(ALICE_B_TOKEN), ProofRequirement::Fresh)
        .await
        .unwrap();
    assert_eq!(alice_a.org_id, 201);
    assert_eq!(alice_a.membership_id, 401);
    assert_eq!(alice_b.org_id, 202);
    assert_eq!(alice_b.membership_id, 404);
    assert_eq!(fixture.github.verification_calls(), 2);
    assert_eq!(
        fixture.github.membership_calls(),
        vec![
            ("alice-access".into(), ALICE_GITHUB_ID, 10_201, "alice-a".into()),
            ("alice-access".into(), ALICE_GITHUB_ID, 10_202, "alice-b".into()),
        ]
    );
}

#[tokio::test]
async fn every_shared_proof_consumer_rechecks_its_own_session() {
    let fixture = principal_fixture(vec![Ok(alice_a_evidence(TeamRole::Admin))]).await;
    let gate = fixture.github.gate_membership();
    let flight_observer = Arc::new(Semaphore::new(0));
    let mut principal = PrincipalService::new(
        fixture.db.clone(),
        fixture.credentials.clone(),
        fixture.github.clone(),
        fixture.clock.clone(),
    );
    principal.observe_proof_flight(flight_observer.clone());
    let service = Arc::new(principal);
    let first = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
                .await
        })
    };
    permits(&gate.started, 1, "first shared consumer reached provider").await;
    permits(&flight_observer, 1, "first consumer retained proof flight").await;
    let second = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
                .await
        })
    };
    permits(&flight_observer, 1, "second consumer retained proof flight").await;
    team_session::Entity::delete_by_id(token_hash(ALICE_A_TOKEN))
        .exec(&fixture.db)
        .await
        .unwrap();
    gate.release.notify_waiters();
    for consumer in [first, second] {
        assert_eq!(
            tokio::time::timeout(StdDuration::from_secs(5), consumer)
                .await
                .expect("revoked shared consumer completed")
                .unwrap(),
            Err(TeamAuthError::Unauthorized)
        );
    }
}

#[tokio::test]
async fn recheck_in_transaction_rejects_changed_revision_state_generation_and_session() {
    for change in ["revision", "state", "generation", "session"] {
        let fixture = principal_fixture(Vec::new()).await;
        let service = fixture.service();
        let context = service
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
            .await
            .unwrap();
        match change {
            "session" => {
                team_session::Entity::delete_by_id(token_hash(ALICE_A_TOKEN))
                    .exec(&fixture.db)
                    .await
                    .unwrap();
            }
            "generation" => {
                let membership = organization_membership::Entity::find_by_id(ALICE_A_MEMBERSHIP_ID)
                    .one(&fixture.db)
                    .await
                    .unwrap()
                    .unwrap();
                let mut active = membership.into_active_model();
                active.credential_generation = Set(2);
                active.update(&fixture.db).await.unwrap();
            }
            "revision" | "state" => {
                let membership = organization_membership::Entity::find_by_id(ALICE_A_MEMBERSHIP_ID)
                    .one(&fixture.db)
                    .await
                    .unwrap()
                    .unwrap();
                let mut active = membership.into_active_model();
                if change == "revision" {
                    active.authorization_revision = Set(8);
                } else {
                    active.state = Set("revoked".into());
                }
                active.update(&fixture.db).await.unwrap();
            }
            _ => unreachable!(),
        }
        let transaction = fixture.db.begin().await.unwrap();
        assert_eq!(
            service.recheck_in_transaction(&transaction, &context).await,
            Err(TeamAuthError::Unauthorized)
        );
        transaction.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn recheck_in_transaction_is_a_current_no_network_guard() {
    let fixture = principal_fixture(Vec::new()).await;
    let service = fixture.service();
    let context = service
        .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
        .await
        .unwrap();
    let transaction = fixture.db.begin().await.unwrap();
    service
        .recheck_in_transaction(&transaction, &context)
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
    assert_eq!(fixture.github.verification_calls(), 0);

    fixture.clock.advance(Duration::seconds(60));
    let transaction = fixture.db.begin().await.unwrap();
    assert_eq!(
        service.recheck_in_transaction(&transaction, &context).await,
        Err(TeamAuthError::Unauthorized)
    );
    transaction.rollback().await.unwrap();
}

#[tokio::test]
async fn final_guard_accepts_a_newer_proof_for_unchanged_authority() {
    let fixture = principal_fixture(vec![Ok(alice_a_evidence(TeamRole::Admin))]).await;
    let service = fixture.service();
    let original = service
        .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
        .await
        .unwrap();
    fixture.clock.advance(Duration::seconds(1));
    let renewed = service
        .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
        .await
        .unwrap();
    assert_eq!(
        renewed.verified_at,
        original.verified_at + Duration::seconds(1)
    );

    let transaction = fixture.db.begin().await.unwrap();
    service
        .recheck_in_transaction(&transaction, &original)
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
}

#[tokio::test]
async fn a_live_refresh_lease_never_authorizes_with_the_old_token() {
    let fixture = principal_fixture(vec![Ok(alice_a_evidence(TeamRole::Admin))]).await;
    let refresh_gate = fixture.oauth.gate_refresh();
    fixture.oauth.reply_refresh(Ok(tokens(
        "replacement-access",
        Some("replacement-refresh"),
        Some(fixed_now() + Duration::hours(2)),
        Some(fixed_now() + Duration::hours(3)),
    )));
    let transaction = fixture.db.begin().await.unwrap();
    fixture
        .credentials
        .install_in_transaction(
            &transaction,
            ALICE_USER_ID,
            ALICE_GITHUB_ID,
            &tokens(
                "expiring-access",
                Some("refresh-me"),
                Some(fixed_now() + Duration::seconds(30)),
                Some(fixed_now() + Duration::hours(1)),
            ),
            fixed_now(),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let first_credentials = fixture.credentials.clone();
    let first_refresh = tokio::spawn(async move {
        first_credentials
            .access(ALICE_USER_ID, ALICE_GITHUB_ID)
            .await
    });
    permits(&refresh_gate.started, 1, "credential refresh started").await;

    let live_lease_observer = Arc::new(Semaphore::new(0));
    let mut second_credentials = CredentialService::new(
        fixture.db.clone(),
        fixture.vault.clone(),
        fixture.oauth.clone(),
        fixture.clock.clone(),
    );
    second_credentials.observe_live_lease(live_lease_observer.clone());
    let service = Arc::new(PrincipalService::new(
        fixture.db.clone(),
        Arc::new(second_credentials),
        fixture.github.clone(),
        fixture.clock.clone(),
    ));
    let resolve = tokio::spawn(async move {
        service
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
            .await
    });
    permits(
        &live_lease_observer,
        1,
        "second credential service observed live lease",
    )
    .await;
    assert_eq!(fixture.github.verification_calls(), 0);
    refresh_gate.release.notify_waiters();

    assert_eq!(
        tokio::time::timeout(StdDuration::from_secs(5), first_refresh)
            .await
            .expect("first refresh completed")
            .unwrap()
            .unwrap()
            .generation,
        3
    );
    let context = tokio::time::timeout(StdDuration::from_secs(5), resolve)
        .await
        .expect("lease-waiting principal completed")
        .unwrap()
        .unwrap();
    assert_eq!(context.credential_generation, 3);
    assert_eq!(fixture.github.verification_calls(), 1);
    assert_eq!(fixture.github.membership_calls()[0].0, "replacement-access");
}

#[tokio::test]
async fn proof_age_is_checked_after_provider_wait() {
    let fixture = principal_fixture(vec![Ok(alice_a_evidence(TeamRole::Admin))]).await;
    let gate = fixture.github.gate_membership();
    let service = fixture.service();
    let resolve = tokio::spawn(async move {
        service
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
            .await
    });
    permits(&gate.started, 1, "aging proof started").await;
    fixture.clock.advance(Duration::seconds(60));
    gate.release.notify_waiters();
    assert_eq!(
        tokio::time::timeout(StdDuration::from_secs(5), resolve)
            .await
            .expect("aged proof resolver completed")
            .unwrap(),
        Err(TeamAuthError::Unauthorized)
    );
    let membership = organization_membership::Entity::find_by_id(ALICE_A_MEMBERSHIP_ID)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(membership.verified_at, Some(fixed_now()));
}

async fn postgres_membership_lock(
    db: &sea_orm::DatabaseConnection,
) -> sea_orm::DatabaseTransaction {
    let transaction = db.begin().await.expect("begin PostgreSQL membership lock");
    transaction
        .execute(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT id FROM organization_membership \
                 WHERE id = {ALICE_A_MEMBERSHIP_ID} FOR UPDATE"
            ),
        ))
        .await
        .expect("lock PostgreSQL membership row");
    transaction
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn principal_postgres_proof_persistence_rechecks_age_after_membership_lock_wait() {
    let (name, admin, fixture) = create_postgres_principal_fixture(vec![Ok(alice_a_evidence(
        TeamRole::Admin,
    ))])
    .await
    .expect("create PostgreSQL principal fixture");
    let old_proof = fixed_now() - Duration::seconds(30);
    set_verified_at(&fixture.db, ALICE_A_MEMBERSHIP_ID, Some(old_proof)).await;
    let locker = postgres_membership_lock(&fixture.db).await;
    let service = fixture.service();
    let resolve = tokio::spawn(async move {
        service
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
            .await
    });
    wait_for_postgres_lock(&admin, &name, "organization_membership")
        .await
        .expect("observe proof persistence membership lock wait");
    fixture.clock.advance(Duration::seconds(60));
    locker.commit().await.expect("release membership lock");
    assert_eq!(
        tokio::time::timeout(StdDuration::from_secs(5), resolve)
            .await
            .expect("proof resolver completed")
            .unwrap(),
        Err(TeamAuthError::Unauthorized)
    );
    assert_eq!(
        organization_membership::Entity::find_by_id(ALICE_A_MEMBERSHIP_ID)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .verified_at,
        Some(old_proof)
    );
    drop_postgres_fixture(&name, admin, fixture.db.clone())
        .await
        .expect("drop PostgreSQL principal fixture");
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn principal_postgres_final_guard_rechecks_session_expiry_after_membership_lock_wait() {
    let (name, admin, fixture) = create_postgres_principal_fixture(Vec::new())
        .await
        .expect("create PostgreSQL principal fixture");
    set_verified_at(
        &fixture.db,
        ALICE_A_MEMBERSHIP_ID,
        Some(fixed_now() - Duration::seconds(10)),
    )
    .await;
    let session = team_session::Entity::find_by_id(token_hash(ALICE_A_TOKEN))
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let mut active = session.into_active_model();
    active.expires_at = Set(fixed_now() + Duration::seconds(30));
    active.update(&fixture.db).await.unwrap();
    let service = fixture.service();
    let context = service
        .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
        .await
        .unwrap();
    let locker = postgres_membership_lock(&fixture.db).await;
    let guard_db = fixture.db.clone();
    let guard_service = service.clone();
    let guard = tokio::spawn(async move {
        let transaction = guard_db.begin().await.unwrap();
        let result = guard_service
            .recheck_in_transaction(&transaction, &context)
            .await;
        transaction.rollback().await.unwrap();
        result
    });
    wait_for_postgres_lock(&admin, &name, "organization_membership")
        .await
        .expect("observe final guard membership lock wait");
    fixture.clock.advance(Duration::seconds(30));
    locker.commit().await.expect("release membership lock");
    assert_eq!(
        tokio::time::timeout(StdDuration::from_secs(5), guard)
            .await
            .expect("final guard completed")
            .unwrap(),
        Err(TeamAuthError::Unauthorized)
    );
    drop_postgres_fixture(&name, admin, fixture.db.clone())
        .await
        .expect("drop PostgreSQL principal fixture");
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn principal_postgres_not_member_cannot_overwrite_a_waiting_local_block() {
    let (name, admin, fixture) = create_postgres_principal_fixture(vec![Err(
        GithubEvidenceError::NotMember,
    )])
    .await
    .expect("create PostgreSQL principal fixture");
    let locker = postgres_membership_lock(&fixture.db).await;
    locker
        .execute(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "UPDATE organization_membership \
                 SET state = 'blocked', authorization_revision = 8, verified_at = NULL \
                 WHERE id = {ALICE_A_MEMBERSHIP_ID}"
            ),
        ))
        .await
        .expect("stage local block");
    let service = fixture.service();
    let resolve = tokio::spawn(async move {
        service
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Fresh)
            .await
    });
    wait_for_postgres_lock(&admin, &name, "organization_membership")
        .await
        .expect("observe NotMember membership lock wait");
    locker.commit().await.expect("commit local block");
    assert_eq!(
        tokio::time::timeout(StdDuration::from_secs(5), resolve)
            .await
            .expect("NotMember resolver completed")
            .unwrap(),
        Err(TeamAuthError::Unauthorized)
    );
    let membership = organization_membership::Entity::find_by_id(ALICE_A_MEMBERSHIP_ID)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(membership.state, "blocked");
    assert_eq!(membership.authorization_revision, 8);
    assert_eq!(membership.verified_at, None);
    drop_postgres_fixture(&name, admin, fixture.db.clone())
        .await
        .expect("drop PostgreSQL principal fixture");
}
