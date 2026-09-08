use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::Duration;
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DbBackend, EntityTrait, IntoActiveModel, Set, Statement,
    TransactionTrait,
};

use super::{
    core::{TeamAuthError, TeamClock},
    credentials::{CredentialService, StoredTokens},
    oauth::{GithubTokenSet, OAuthError},
    secrets::{SecretBinding, SecretVault},
    test_auth::{
        fixed_now, sqlite_fixture, tokens, ALICE_GITHUB_ID, ALICE_USER_ID,
    },
};
use crate::db::entities::{organization_membership, team_github_credential, team_session};

fn service(fixture: &super::test_auth::Fixture) -> CredentialService {
    CredentialService::new(
        fixture.db.clone(),
        fixture.vault.clone(),
        fixture.oauth.clone(),
        fixture.clock.clone(),
    )
}

async fn install(fixture: &super::test_auth::Fixture, tokens: &GithubTokenSet) -> i64 {
    let transaction = fixture.db.begin().await.expect("begin credential install");
    let generation = service(fixture)
        .install_in_transaction(
            &transaction,
            ALICE_USER_ID,
            ALICE_GITHUB_ID,
            tokens,
            fixture.clock.now(),
        )
        .await
        .expect("install credential");
    transaction.commit().await.expect("commit credential install");
    generation
}

async fn rebind_fixture_credential_generation(
    fixture: &super::test_auth::Fixture,
    from: i64,
    to: i64,
) {
    let row = team_github_credential::Entity::find_by_id(ALICE_USER_ID)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.generation, from);
    let plaintext = fixture
        .vault
        .open(
            SecretBinding::GithubCredential {
                user_id: ALICE_USER_ID,
                github_user_id: ALICE_GITHUB_ID,
                generation: from,
            },
            row.encrypted_payload.as_deref().unwrap(),
        )
        .unwrap();
    let encrypted_payload = Some(
        fixture
            .vault
            .seal(
                SecretBinding::GithubCredential {
                    user_id: ALICE_USER_ID,
                    github_user_id: ALICE_GITHUB_ID,
                    generation: to,
                },
                &plaintext,
            )
            .unwrap(),
    );
    let mut active = row.into_active_model();
    active.encrypted_payload = Set(encrypted_payload);
    active.generation = Set(to);
    active.update(&fixture.db).await.unwrap();
}

#[test]
fn stored_tokens_round_trip_maximum_opaque_values_and_reject_malformed_shapes() {
    let now = fixed_now();
    let opaque = "\\\"{}[],:!~".repeat(410).chars().take(4096).collect::<String>();
    assert_eq!(opaque.len(), 4096);
    let original = tokens(
        &opaque,
        Some(&opaque),
        Some(now + Duration::minutes(5)),
        Some(now + Duration::hours(1)),
    );
    let stored = StoredTokens::from_tokens(&original).expect("encode maximum tokens");
    let json = serde_json::to_vec(&stored).expect("serialize stored tokens");
    assert!(json.len() < 16_384);
    let decoded = stored.to_tokens().expect("decode maximum tokens");
    assert_eq!(decoded.access_token, opaque);
    assert_eq!(decoded.refresh_token.as_deref(), Some(opaque.as_str()));

    let malformed = [
        r#"{"version":2,"access_token_b64":"YQ==","refresh_token_b64":null,"access_expires_at":null,"refresh_expires_at":null}"#,
        r#"{"version":1,"access_token_b64":"***","refresh_token_b64":null,"access_expires_at":null,"refresh_expires_at":null}"#,
        r#"{"version":1,"access_token_b64":"YQ==","refresh_token_b64":"Yg==","access_expires_at":null,"refresh_expires_at":null}"#,
    ];
    for value in malformed {
        let stored: StoredTokens = serde_json::from_str(value).expect("shape is JSON");
        assert!(matches!(stored.to_tokens(), Err(TeamAuthError::Unavailable)));
    }
    let oversized = STANDARD.encode(vec![b'a'; 4097]);
    let value = format!(r#"{{"version":1,"access_token_b64":"{oversized}","refresh_token_b64":null,"access_expires_at":null,"refresh_expires_at":null}}"#);
    let stored: StoredTokens = serde_json::from_str(&value).expect("oversized shape is JSON");
    assert!(matches!(stored.to_tokens(), Err(TeamAuthError::Unavailable)));
    let overlong_encoded = "Y".repeat(5_465);
    let value = format!(r#"{{"version":1,"access_token_b64":"{overlong_encoded}","refresh_token_b64":null,"access_expires_at":null,"refresh_expires_at":null}}"#);
    let stored: StoredTokens =
        serde_json::from_str(&value).expect("overlong encoded shape is JSON");
    assert!(matches!(stored.to_tokens(), Err(TeamAuthError::Unavailable)));
}

#[tokio::test]
async fn active_nonexpiring_credential_returns_vault_bound_grant_without_legacy_columns() {
    let fixture = sqlite_fixture(vec![]).await;
    let generation = install(&fixture, &tokens("invented-active", None, None, None)).await;
    assert_eq!(generation, 1);

    let grant = service(&fixture)
        .access(ALICE_USER_ID, ALICE_GITHUB_ID)
        .await
        .expect("active grant");
    assert_eq!(grant.user_id, ALICE_USER_ID);
    assert_eq!(grant.github_user_id, ALICE_GITHUB_ID);
    assert_eq!(grant.generation, 1);
    assert_eq!(grant.access_token, "invented-active");
    assert_eq!(fixture.oauth.refresh_calls(), 0);

    let row = team_github_credential::Entity::find_by_id(ALICE_USER_ID)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let encrypted = row.encrypted_payload.unwrap();
    assert!(!encrypted.contains("invented-active"));
    let wrong_binding = fixture.vault.open(
        SecretBinding::GithubCredential {
            user_id: ALICE_USER_ID,
            github_user_id: ALICE_GITHUB_ID,
            generation: 2,
        },
        &encrypted,
    );
    assert!(wrong_binding.is_err());
}

#[tokio::test]
async fn first_install_does_not_promote_generation_zero_or_stale_bindings() {
    let fixture = sqlite_fixture(vec![]).await;
    assert_eq!(install(&fixture, &tokens("invented-first", None, None, None)).await, 1);

    let zero_membership = organization_membership::Entity::find_by_id(405)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(zero_membership.credential_generation, 0);
    assert_eq!(zero_membership.verified_at, Some(fixed_now()));
    assert_eq!(
        team_session::Entity::find_by_id("session-zero")
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .credential_generation,
        0
    );
    assert_eq!(
        team_session::Entity::find_by_id("session-mismatch")
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .credential_generation,
        5
    );
}

#[tokio::test]
async fn access_more_than_sixty_seconds_from_expiry_does_not_refresh() {
    let now = fixed_now();
    let fixture = sqlite_fixture(vec![]).await;
    install(
        &fixture,
        &tokens(
            "invented-not-yet-due",
            Some("invented-refresh"),
            Some(now + Duration::seconds(61)),
            Some(now + Duration::hours(1)),
        ),
    )
    .await;

    let grant = service(&fixture)
        .access(ALICE_USER_ID, ALICE_GITHUB_ID)
        .await
        .unwrap();
    assert_eq!(grant.access_token, "invented-not-yet-due");
    assert_eq!(fixture.oauth.refresh_calls(), 0);
}

#[tokio::test]
async fn sixty_second_boundary_rotates_complete_pair_and_only_matching_bindings() {
    let now = fixed_now();
    let fixture = sqlite_fixture(vec![Ok(tokens(
        "invented-rotated-access",
        Some("invented-rotated-refresh"),
        Some(now + Duration::hours(1)),
        Some(now + Duration::hours(8)),
    ))])
    .await;
    let first = tokens(
        "invented-expiring-access",
        Some("invented-original-refresh"),
        Some(now + Duration::seconds(60)),
        Some(now + Duration::hours(4)),
    );
    let transaction = fixture.db.begin().await.unwrap();
    service(&fixture)
        .install_in_transaction(
            &transaction,
            ALICE_USER_ID,
            ALICE_GITHUB_ID,
            &first,
            now,
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    rebind_fixture_credential_generation(&fixture, 1, 11).await;

    let grant = service(&fixture)
        .access(ALICE_USER_ID, ALICE_GITHUB_ID)
        .await
        .expect("rotated grant");
    assert_eq!(fixture.oauth.refresh_inputs(), ["invented-original-refresh"]);
    assert_eq!(grant.generation, 12);
    assert_eq!(grant.access_token, "invented-rotated-access");

    let session = team_session::Entity::find_by_id("session-a")
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.credential_generation, 12);
    assert_eq!(session.authorization_revision, 7);
    let mismatched = team_session::Entity::find_by_id("session-mismatch")
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mismatched.credential_generation, 5);
    let blocked = organization_membership::Entity::find_by_id(402)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(blocked.state, "blocked");
    assert_eq!(blocked.role, "member");
    assert_eq!(blocked.credential_generation, 12);
    assert_eq!(blocked.verified_at, None);
    assert_eq!(blocked.authorization_revision, 7);

    let credential = team_github_credential::Entity::find_by_id(ALICE_USER_ID)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let plaintext = fixture
        .vault
        .open(
            SecretBinding::GithubCredential {
                user_id: ALICE_USER_ID,
                github_user_id: ALICE_GITHUB_ID,
                generation: 12,
            },
            credential.encrypted_payload.as_deref().unwrap(),
        )
        .unwrap();
    let stored: StoredTokens = serde_json::from_slice(&plaintext).unwrap();
    let persisted = stored.to_tokens().unwrap();
    assert_eq!(persisted.access_token, "invented-rotated-access");
    assert_eq!(
        persisted.refresh_token.as_deref(),
        Some("invented-rotated-refresh")
    );
    assert_eq!(persisted.access_expires_at, Some(now + Duration::hours(1)));
    assert_eq!(persisted.refresh_expires_at, Some(now + Duration::hours(8)));
}

#[tokio::test]
async fn missing_or_expired_refresh_invalidates_proof_and_matching_sessions() {
    let now = fixed_now();
    for (reply, refresh_lifetime, advance, expected_calls) in [
        (
            Some(Ok(tokens(
                "malformed-provider-access",
                None,
                Some(now + Duration::hours(1)),
                None,
            ))),
            Duration::hours(1),
            Duration::zero(),
            1,
        ),
        (None, Duration::seconds(2), Duration::seconds(3), 0),
    ] {
        let fixture = sqlite_fixture(reply.into_iter().collect()).await;
        install(
            &fixture,
            &tokens(
                "access",
                Some("refresh"),
                Some(now + Duration::seconds(30)),
                Some(now + refresh_lifetime),
            ),
        )
        .await;
        rebind_fixture_credential_generation(&fixture, 1, 11).await;
        fixture.clock.advance(advance);
        let result = service(&fixture).access(ALICE_USER_ID, ALICE_GITHUB_ID).await;
        assert!(matches!(result, Err(TeamAuthError::Unauthorized)));
        let row = team_github_credential::Entity::find_by_id(ALICE_USER_ID)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "reauth_required");
        assert!(row.encrypted_payload.is_none());
        assert_eq!(fixture.oauth.refresh_calls(), expected_calls);
        assert!(team_session::Entity::find_by_id("session-a")
            .one(&fixture.db)
            .await
            .unwrap()
            .is_none());
        assert!(team_session::Entity::find_by_id("session-mismatch")
            .one(&fixture.db)
            .await
            .unwrap()
            .is_some());
        let blocked = organization_membership::Entity::find_by_id(402)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(blocked.state, "blocked");
        assert_eq!(blocked.verified_at, None);
    }
}

#[tokio::test]
async fn expired_refresh_lease_is_invalidated_without_using_old_access() {
    let now = fixed_now();
    let fixture = sqlite_fixture(vec![]).await;
    install(
        &fixture,
        &tokens(
            "still-unexpired-access",
            Some("refresh"),
            Some(now + Duration::hours(1)),
            Some(now + Duration::hours(2)),
        ),
    )
    .await;
    rebind_fixture_credential_generation(&fixture, 1, 11).await;
    let mut row = team_github_credential::Entity::find_by_id(ALICE_USER_ID)
        .one(&fixture.db).await.unwrap().unwrap().into_active_model();
    row.state = Set("refreshing".into());
    row.refresh_owner = Set(Some(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
    ));
    row.refresh_expires_at = Set(Some(now));
    row.update(&fixture.db).await.unwrap();

    assert!(matches!(
        service(&fixture).access(ALICE_USER_ID, ALICE_GITHUB_ID).await,
        Err(TeamAuthError::Unauthorized)
    ));
    assert_eq!(fixture.oauth.refresh_calls(), 0);
    assert_eq!(
        team_github_credential::Entity::find_by_id(ALICE_USER_ID)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .state,
        "reauth_required"
    );
    assert!(team_session::Entity::find_by_id("session-a")
        .one(&fixture.db)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn caller_rollback_persists_no_credential_or_generation_changes() {
    let fixture = sqlite_fixture(vec![]).await;
    let transaction = fixture.db.begin().await.unwrap();
    assert_eq!(
        service(&fixture)
            .install_in_transaction(
                &transaction,
                ALICE_USER_ID,
                ALICE_GITHUB_ID,
                &tokens("rolled-back", None, None, None),
                fixed_now(),
            )
            .await
            .unwrap(),
        1
    );
    transaction.rollback().await.unwrap();
    assert!(team_github_credential::Entity::find_by_id(ALICE_USER_ID)
        .one(&fixture.db)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        team_session::Entity::find_by_id("session-a")
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .credential_generation,
        11
    );
}

#[tokio::test]
async fn failed_outer_database_commit_persists_no_credential() {
    let fixture = sqlite_fixture(vec![]).await;
    fixture
        .db
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE deferred_commit_guard (user_id INTEGER, \
             FOREIGN KEY(user_id) REFERENCES users(id) DEFERRABLE INITIALLY DEFERRED)"
                .to_owned(),
        ))
        .await
        .unwrap();
    let transaction = fixture.db.begin().await.unwrap();
    service(&fixture)
        .install_in_transaction(
            &transaction,
            ALICE_USER_ID,
            ALICE_GITHUB_ID,
            &tokens("commit-must-fail", None, None, None),
            fixed_now(),
        )
        .await
        .unwrap();
    transaction
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO deferred_commit_guard (user_id) VALUES (999999)".to_owned(),
        ))
        .await
        .expect("deferred violation is accepted before commit");
    assert!(transaction.commit().await.is_err());
    assert!(team_github_credential::Entity::find_by_id(ALICE_USER_ID)
        .one(&fixture.db)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        team_session::Entity::find_by_id("session-zero")
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .credential_generation,
        0
    );
}

#[tokio::test]
async fn identity_mismatch_and_expired_install_fail_closed() {
    let now = fixed_now();
    let fixture = sqlite_fixture(vec![]).await;
    install(&fixture, &tokens("active", None, None, None)).await;
    assert!(matches!(
        service(&fixture)
            .access(ALICE_USER_ID, ALICE_GITHUB_ID + 1)
            .await,
        Err(TeamAuthError::Unavailable)
    ));
    for expired in [
        tokens("access", Some("refresh"), Some(now), Some(now + Duration::hours(1))),
        tokens("access", Some("refresh"), Some(now + Duration::minutes(1)), Some(now)),
    ] {
        let transaction = fixture.db.begin().await.unwrap();
        assert!(matches!(
            service(&fixture)
                .install_in_transaction(
                    &transaction,
                    ALICE_USER_ID,
                    ALICE_GITHUB_ID,
                    &expired,
                    now,
                )
                .await,
            Err(TeamAuthError::Unauthorized)
        ));
        transaction.rollback().await.unwrap();
    }
    let transaction = fixture.db.begin().await.unwrap();
    assert!(service(&fixture)
        .install_in_transaction(
            &transaction,
            ALICE_USER_ID,
            ALICE_GITHUB_ID,
            &tokens(
                "longer-access",
                Some("shorter-refresh"),
                Some(now + Duration::hours(2)),
                Some(now + Duration::hours(1)),
            ),
            now,
        )
        .await
        .is_ok());
    transaction.rollback().await.unwrap();
}

#[tokio::test]
async fn unavailable_database_during_refresh_returns_no_losing_token() {
    let now = fixed_now();
    let (oauth, gate) = super::test_auth::FakeOAuth::gated(vec![Ok(tokens(
        "must-not-return",
        Some("replacement"),
        Some(now + Duration::hours(1)),
        Some(now + Duration::hours(2)),
    ))]);
    let mut fixture = sqlite_fixture(vec![]).await;
    fixture.oauth = Arc::new(oauth);
    install(
        &fixture,
        &tokens(
            "old",
            Some("refresh"),
            Some(now + Duration::seconds(1)),
            Some(now + Duration::hours(1)),
        ),
    )
    .await;
    fixture.clock.advance(Duration::seconds(1));
    let refreshing = service(&fixture);
    let task = tokio::spawn(async move { refreshing.access(ALICE_USER_ID, ALICE_GITHUB_ID).await });
    gate.started.acquire().await.unwrap().forget();
    fixture.db.clone().close().await.unwrap();
    gate.release.notify_waiters();
    assert!(matches!(task.await.unwrap(), Err(TeamAuthError::Unavailable)));
}

#[tokio::test]
async fn oauth_failure_fails_closed_and_stale_invalidation_cannot_erase_new_login() {
    let now = fixed_now();
    let fixture = sqlite_fixture(vec![Err(OAuthError::Unavailable)]).await;
    install(
        &fixture,
        &tokens(
            "access",
            Some("refresh"),
            Some(now + Duration::seconds(1)),
            Some(now + Duration::hours(1)),
        ),
    )
    .await;
    fixture.clock.advance(Duration::seconds(1));
    assert!(matches!(
        service(&fixture).access(ALICE_USER_ID, ALICE_GITHUB_ID).await,
        Err(TeamAuthError::Unauthorized)
    ));
    assert_eq!(fixture.oauth.refresh_calls(), 1);

    let replacement = tokens("new-login", None, None, None);
    assert_eq!(install(&fixture, &replacement).await, 2);
    assert!(!service(&fixture)
        .invalidate_if_generation(ALICE_USER_ID, ALICE_GITHUB_ID, 1)
        .await
        .unwrap());
    assert_eq!(
        service(&fixture)
            .access(ALICE_USER_ID, ALICE_GITHUB_ID)
            .await
            .unwrap()
            .access_token,
        "new-login"
    );
}

#[tokio::test]
async fn delayed_refresh_loses_to_new_login_and_discards_its_tokens() {
    let now = fixed_now();
    let (oauth, gate) = super::test_auth::FakeOAuth::gated(vec![Ok(tokens(
        "losing-access",
        Some("losing-refresh"),
        Some(now + Duration::hours(1)),
        Some(now + Duration::hours(2)),
    ))]);
    let mut fixture = sqlite_fixture(vec![]).await;
    fixture.oauth = Arc::new(oauth);
    install(
        &fixture,
        &tokens(
            "old",
            Some("old-refresh"),
            Some(now + Duration::seconds(1)),
            Some(now + Duration::hours(1)),
        ),
    )
    .await;
    fixture.clock.advance(Duration::seconds(1));
    let refresher = service(&fixture);
    let task = tokio::spawn(async move { refresher.access(ALICE_USER_ID, ALICE_GITHUB_ID).await });
    gate.started.acquire().await.unwrap().forget();
    assert_eq!(install(&fixture, &tokens("new-login", None, None, None)).await, 2);
    gate.release.notify_waiters();
    let grant = task.await.unwrap().unwrap();
    assert_eq!(grant.generation, 2);
    assert_eq!(grant.access_token, "new-login");
}

#[tokio::test]
async fn delayed_refresh_result_after_lease_expiry_is_discarded() {
    let now = fixed_now();
    let (oauth, gate) = super::test_auth::FakeOAuth::gated(vec![Ok(tokens(
        "expired-lease-access",
        Some("expired-lease-refresh"),
        Some(now + Duration::hours(1)),
        Some(now + Duration::hours(2)),
    ))]);
    let mut fixture = sqlite_fixture(vec![]).await;
    fixture.oauth = Arc::new(oauth);
    install(
        &fixture,
        &tokens(
            "old",
            Some("old-refresh"),
            Some(now + Duration::seconds(1)),
            Some(now + Duration::hours(1)),
        ),
    )
    .await;
    fixture.clock.advance(Duration::seconds(1));
    let refresher = service(&fixture);
    let task = tokio::spawn(async move {
        refresher.access(ALICE_USER_ID, ALICE_GITHUB_ID).await
    });
    gate.started.acquire().await.unwrap().forget();
    fixture.clock.advance(Duration::seconds(46));
    gate.release.notify_waiters();

    assert!(matches!(task.await.unwrap(), Err(TeamAuthError::Unauthorized)));
    let row = team_github_credential::Entity::find_by_id(ALICE_USER_ID)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "reauth_required");
    assert!(row.encrypted_payload.is_none());
}

#[tokio::test]
async fn two_service_instances_share_one_database_refresh() {
    let now = fixed_now();
    let (oauth, gate) = super::test_auth::FakeOAuth::gated(vec![Ok(tokens(
        "invented-rotated-access",
        Some("rotated-refresh"),
        Some(now + Duration::hours(1)),
        Some(now + Duration::hours(2)),
    ))]);
    let mut fixture = sqlite_fixture(vec![]).await;
    fixture.oauth = Arc::new(oauth);
    install(
        &fixture,
        &tokens(
            "old",
            Some("shared-refresh"),
            Some(now + Duration::seconds(1)),
            Some(now + Duration::hours(1)),
        ),
    )
    .await;
    fixture.clock.advance(Duration::seconds(1));
    let first_service = service(&fixture);
    let second_service = service(&fixture);
    let first = tokio::spawn(async move {
        first_service.access(ALICE_USER_ID, ALICE_GITHUB_ID).await
    });
    gate.started.acquire().await.unwrap().forget();
    let second = tokio::spawn(async move {
        second_service.access(ALICE_USER_ID, ALICE_GITHUB_ID).await
    });
    gate.release.notify_waiters();
    let first = first.await.unwrap().unwrap();
    let second = second.await.unwrap().unwrap();
    assert_eq!(fixture.oauth.refresh_calls(), 1);
    assert_eq!(first.generation, 2);
    assert_eq!(second.generation, 2);
    assert_eq!(first.access_token, "invented-rotated-access");
    assert_eq!(second.access_token, "invented-rotated-access");
}

#[test]
fn team_auth_database_cleanup_guard_requires_exact_name() {
    let expected = "workos_team_auth_test_0123456789abcdef0123456789abcdef";
    assert!(super::test_auth::validate_database_target(expected, expected).is_ok());
    assert!(super::test_auth::validate_database_target(expected, "postgres").is_err());
}

#[tokio::test]
#[ignore = "requires local Postgres role air1 with CREATEDB on localhost:5432"]
async fn credential_service_postgres_replacement_invalidation_and_concurrency() {
    let (name, admin, db) = super::test_auth::create_postgres_fixture()
        .await
        .expect("create PostgreSQL fixture");
    let result: Result<(), String> = async {
        let now = fixed_now();
        let (oauth, gate) = super::test_auth::FakeOAuth::gated(vec![Ok(tokens(
            "pg-rotated",
            Some("pg-refresh-2"),
            Some(now + Duration::hours(1)),
            Some(now + Duration::hours(2)),
        ))]);
        let fixture = super::test_auth::Fixture {
            db: db.clone(),
            vault: Arc::new(
                SecretVault::from_hex_key(
                    "2222222222222222222222222222222222222222222222222222222222222222",
                )
                .unwrap(),
            ),
            oauth: Arc::new(oauth),
            clock: Arc::new(super::test_auth::TestClock::new(now)),
        };
        let first = tokens(
            "pg-first",
            Some("pg-refresh"),
            Some(now + Duration::seconds(1)),
            Some(now + Duration::hours(1)),
        );
        if install(&fixture, &first).await != 1 {
            return Err("wrong PostgreSQL generation".into());
        }
        fixture.clock.advance(Duration::seconds(1));
        let first_service = service(&fixture);
        let second_service = service(&fixture);
        let first_task = tokio::spawn(async move {
            first_service.access(ALICE_USER_ID, ALICE_GITHUB_ID).await
        });
        gate.started.acquire().await.unwrap().forget();
        let second_task = tokio::spawn(async move {
            second_service.access(ALICE_USER_ID, ALICE_GITHUB_ID).await
        });
        gate.release.notify_waiters();
        let first_grant = first_task.await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        let second_grant = second_task
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        if fixture.oauth.refresh_calls() != 1
            || first_grant.generation != 2
            || second_grant.generation != 2
        {
            return Err("PostgreSQL two-instance refresh was not serialized".into());
        }
        if !service(&fixture)
            .invalidate_if_generation(ALICE_USER_ID, ALICE_GITHUB_ID, 2)
            .await
            .map_err(|e| e.to_string())?
        {
            return Err("PostgreSQL invalidation lost".into());
        }
        let second = tokens("pg-second", None, None, None);
        if install(&fixture, &second).await != 3 {
            return Err("PostgreSQL replacement not atomic".into());
        }
        Ok(())
    }.await;
    if let Err(error) = result {
        db.close().await.expect("close failed fixture");
        admin.close().await.expect("close admin after failure");
        panic!("{error}; preserved database {name}");
    }
    super::test_auth::drop_postgres_fixture(&name, admin, db)
        .await
        .expect("drop successful PostgreSQL fixture");
}
