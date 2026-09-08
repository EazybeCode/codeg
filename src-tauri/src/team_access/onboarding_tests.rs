use std::sync::Arc;

use chrono::Duration;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter,
    PaginatorTrait, Set, Statement, TransactionTrait,
};
use sha2::{Digest, Sha256};

use super::{
    core::TeamAuthError,
    credentials::CredentialService,
    github::{
        GithubEvidenceError, GithubIdentity, GithubMembershipEvidence,
        GithubOrganizationChoice,
    },
    oauth::{GithubTokenSet, OAuthError},
    onboarding::OnboardingService,
    secrets::{SecretBinding, SecretVault},
    session::TeamRole,
    test_auth::{
        create_postgres_fixture, drop_postgres_fixture, fixed_now, seed_accounts, tokens,
        FakeGithub, FakeOAuth, TestClock, ALICE_GITHUB_ID, ALICE_USER_ID,
    },
};
use crate::db::entities::{
    org, organization_membership, team_github_credential, team_oauth_attempt,
    team_pending_onboarding, team_session, user,
};

const GITHUB_USER_ID: i64 = 30_301;
const GITHUB_ORG_A: i64 = 10_201;
const GITHUB_ORG_B: i64 = 10_202;

struct Fixture {
    db: sea_orm::DatabaseConnection,
    vault: Arc<SecretVault>,
    oauth: Arc<FakeOAuth>,
    github: Arc<FakeGithub>,
    clock: Arc<TestClock>,
}

impl Fixture {
    fn service(&self) -> OnboardingService {
        let credentials = Arc::new(CredentialService::new(
            self.db.clone(),
            self.vault.clone(),
            self.oauth.clone(),
            self.clock.clone(),
        ));
        OnboardingService::new(
            self.db.clone(),
            self.vault.clone(),
            self.oauth.clone(),
            self.github.clone(),
            credentials,
            self.clock.clone(),
        )
    }
}

fn identity(id: i64, login: &str) -> GithubIdentity {
    GithubIdentity {
        github_user_id: id,
        login: login.into(),
        name: Some(format!("{login} name")),
        avatar_url: Some("https://avatars.githubusercontent.com/u/30301".into()),
    }
}

fn organization(id: i64, login: &str, role: TeamRole) -> GithubOrganizationChoice {
    GithubOrganizationChoice {
        github_org_id: id,
        login: login.into(),
        avatar_url: Some(format!("https://avatars.githubusercontent.com/{login}")),
        role,
    }
}

fn evidence(
    user_id: i64,
    org_id: i64,
    login: &str,
    role: TeamRole,
) -> GithubMembershipEvidence {
    GithubMembershipEvidence {
        github_user_id: user_id,
        organization: organization(org_id, login, role),
    }
}

fn durable_tokens(access: &str) -> GithubTokenSet {
    tokens(access, None, None, None)
}

async fn fixture(
    exchanges: Vec<Result<GithubTokenSet, OAuthError>>,
    identities: Vec<Result<GithubIdentity, GithubEvidenceError>>,
    organizations: Vec<Result<Vec<GithubOrganizationChoice>, GithubEvidenceError>>,
    memberships: Vec<Result<GithubMembershipEvidence, GithubEvidenceError>>,
    seeded: bool,
) -> Fixture {
    let db = crate::db::test_helpers::fresh_in_memory_db().await.conn;
    if seeded {
        seed_accounts(&db).await.expect("seed onboarding fixture");
    }
    Fixture {
        db,
        vault: Arc::new(
            SecretVault::from_hex_key(
                "2222222222222222222222222222222222222222222222222222222222222222",
            )
            .expect("onboarding test vault"),
        ),
        oauth: Arc::new(FakeOAuth::with_exchange(exchanges)),
        github: Arc::new(FakeGithub::new(identities, organizations, memberships)),
        clock: Arc::new(TestClock::new(fixed_now())),
    }
}

async fn pending(service: &OnboardingService, code: &str) -> super::onboarding::PendingStart {
    let attempt = service.begin().await.expect("begin onboarding");
    service
        .complete(&attempt.state, &attempt.browser_secret, code)
        .await
        .expect("complete OAuth")
}

fn hash(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

fn valid_secret(secret: &str) -> bool {
    secret.len() == 64
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[tokio::test]
async fn begin_persists_only_independent_hashes_and_a_bound_encrypted_verifier() {
    let fixture = fixture(Vec::new(), Vec::new(), Vec::new(), Vec::new(), false).await;
    let started = fixture.service().begin().await.unwrap();

    assert!(valid_secret(&started.state));
    assert!(valid_secret(&started.browser_secret));
    assert!(valid_secret(&started.verifier));
    assert_ne!(started.state, started.browser_secret);
    assert_ne!(started.state, started.verifier);
    assert_ne!(started.browser_secret, started.verifier);
    assert_eq!(started.expires_at, fixed_now() + Duration::minutes(10));

    let row = team_oauth_attempt::Entity::find()
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state_hash, hash(&started.state));
    assert_eq!(row.browser_hash, hash(&started.browser_secret));
    assert!(!row.encrypted_verifier.contains(&started.verifier));
    assert_eq!(
        fixture
            .vault
            .open(
                SecretBinding::OAuthAttempt {
                    state_hash: &row.state_hash,
                    browser_hash: &row.browser_hash,
                },
                &row.encrypted_verifier,
            )
            .unwrap(),
        started.verifier.as_bytes()
    );
}

#[tokio::test]
async fn complete_rejects_wrong_replayed_and_expired_browser_state_before_exchange() {
    let fixture = fixture(
        vec![Ok(durable_tokens("access-one"))],
        vec![Ok(identity(GITHUB_USER_ID, "new-user"))],
        Vec::new(),
        Vec::new(),
        false,
    )
    .await;
    let service = fixture.service();
    let started = service.begin().await.unwrap();
    let wrong = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    assert!(matches!(
        service.complete(&started.state, wrong, "code-one").await,
        Err(TeamAuthError::Unauthorized)
    ));
    assert!(fixture.oauth.exchange_calls().is_empty());
    let completed = service
        .complete(&started.state, &started.browser_secret, "code-one")
        .await
        .unwrap();
    assert!(valid_secret(&completed.browser_secret));
    assert!(valid_secret(&completed.csrf_secret));
    assert_eq!(completed.expires_at, fixed_now() + Duration::minutes(10));
    assert!(matches!(
        service
            .complete(&started.state, &started.browser_secret, "code-one")
            .await,
        Err(TeamAuthError::Unauthorized)
    ));
    assert_eq!(fixture.oauth.exchange_calls().len(), 1);

    let expired = service.begin().await.unwrap();
    fixture.clock.advance(Duration::minutes(10));
    assert!(matches!(
        service
            .complete(&expired.state, &expired.browser_secret, "code-two")
            .await,
        Err(TeamAuthError::Unauthorized)
    ));
    assert_eq!(fixture.oauth.exchange_calls().len(), 1);
}

#[tokio::test]
async fn an_uncertain_exchange_consumes_the_attempt_and_never_retries_the_code() {
    let fixture = fixture(
        vec![Err(OAuthError::Unavailable)],
        Vec::new(),
        Vec::new(),
        Vec::new(),
        false,
    )
    .await;
    let service = fixture.service();
    let started = service.begin().await.unwrap();

    assert!(matches!(
        service
            .complete(&started.state, &started.browser_secret, "ambiguous-code")
            .await,
        Err(TeamAuthError::Unavailable)
    ));
    assert!(matches!(
        service
            .complete(&started.state, &started.browser_secret, "ambiguous-code")
            .await,
        Err(TeamAuthError::Unauthorized)
    ));
    assert_eq!(fixture.oauth.exchange_calls().len(), 1);
    assert_eq!(
        team_oauth_attempt::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn maximum_supported_pending_payload_round_trips_without_creating_authority() {
    let at = fixed_now();
    let mut maximum_identity = identity(GITHUB_USER_ID, "new-user");
    maximum_identity.name = Some("n".repeat(256));
    maximum_identity.avatar_url = Some(format!("https://example.invalid/{}", "a".repeat(2_100)));
    let fixture = fixture(
        vec![Ok(tokens(
            &"a".repeat(4_096),
            Some(&"b".repeat(4_096)),
            Some(at + Duration::hours(1)),
            Some(at + Duration::days(30)),
        ))],
        vec![Ok(maximum_identity)],
        vec![Ok(vec![organization(
            GITHUB_ORG_A,
            "alpha-company",
            TeamRole::Member,
        )])],
        Vec::new(),
        false,
    )
    .await;
    let service = fixture.service();
    let completed = pending(&service, "maximum-code").await;

    let pending_row = team_pending_onboarding::Entity::find()
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert!(!pending_row.encrypted_payload.contains(&"a".repeat(128)));
    assert!(!pending_row.encrypted_payload.contains(&"b".repeat(128)));
    let choices = service.choices(&completed.browser_secret).await.unwrap();
    assert_eq!(choices.identity.github_user_id, GITHUB_USER_ID);
    assert_eq!(choices.identity.name.as_deref(), Some(&*"n".repeat(256)));
    assert_eq!(choices.identity.avatar_url, None);
    assert_eq!(choices.organizations.len(), 1);
    assert_eq!(user::Entity::find().count(&fixture.db).await.unwrap(), 0);
    assert_eq!(org::Entity::find().count(&fixture.db).await.unwrap(), 0);
    assert_eq!(
        organization_membership::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(team_session::Entity::find().count(&fixture.db).await.unwrap(), 0);
    assert_eq!(
        team_github_credential::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn choices_returns_zero_one_or_many_explicit_options_without_auto_selection() {
    for expected in [
        vec![],
        vec![organization(GITHUB_ORG_A, "alpha-company", TeamRole::Member)],
        vec![
            organization(GITHUB_ORG_A, "alpha-company", TeamRole::Member),
            organization(GITHUB_ORG_B, "beta-company", TeamRole::Admin),
            organization(10_203, "gamma-company", TeamRole::Member),
        ],
    ] {
        let fixture = fixture(
            vec![Ok(durable_tokens("choice-access"))],
            vec![Ok(identity(GITHUB_USER_ID, "new-user"))],
            vec![Ok(expected.clone())],
            Vec::new(),
            false,
        )
        .await;
        let service = fixture.service();
        let completed = pending(&service, "choice-code").await;
        let choices = service.choices(&completed.browser_secret).await.unwrap();
        assert_eq!(choices.organizations, expected);
        assert_eq!(
            fixture.github.organization_calls(),
            vec![("choice-access".into(), GITHUB_USER_ID)]
        );
        assert_eq!(user::Entity::find().count(&fixture.db).await.unwrap(), 0);
        assert_eq!(team_session::Entity::find().count(&fixture.db).await.unwrap(), 0);
    }
}

#[tokio::test]
async fn select_creates_a_member_membership_credential_and_bound_session_atomically() {
    let choice = organization(GITHUB_ORG_A, "alpha-company", TeamRole::Member);
    let fixture = fixture(
        vec![Ok(durable_tokens("new-access"))],
        vec![Ok(identity(GITHUB_USER_ID, "new-user"))],
        vec![Ok(vec![choice.clone()])],
        vec![Ok(evidence(
            GITHUB_USER_ID,
            GITHUB_ORG_A,
            "alpha-company",
            TeamRole::Member,
        ))],
        false,
    )
    .await;
    let service = fixture.service();
    let completed = pending(&service, "select-code").await;
    assert_eq!(user::Entity::find().count(&fixture.db).await.unwrap(), 0);
    assert_eq!(team_session::Entity::find().count(&fixture.db).await.unwrap(), 0);

    let issued = service
        .select(
            &completed.browser_secret,
            &completed.csrf_secret,
            GITHUB_ORG_A,
            None,
        )
        .await
        .unwrap();
    let account = user::Entity::find_by_id(issued.user_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let created_org = org::Entity::find_by_id(issued.org_id)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let membership = organization_membership::Entity::find()
        .filter(organization_membership::Column::UserId.eq(issued.user_id))
        .filter(organization_membership::Column::OrgId.eq(issued.org_id))
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let session = team_session::Entity::find_by_id(hash(&issued.session_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.github_user_id, GITHUB_USER_ID);
    assert_eq!(account.role, "member");
    assert_eq!(account.org_id, created_org.id);
    assert_eq!(membership.role, "member");
    assert_eq!(membership.authorization_revision, 1);
    assert_eq!(membership.credential_generation, 1);
    assert_eq!(membership.verified_at, Some(fixed_now()));
    assert_eq!(session.org_id, issued.org_id);
    assert_eq!(session.membership_id, membership.id);
    assert_eq!(session.authorization_revision, membership.authorization_revision);
    assert_eq!(session.credential_generation, 1);
    assert_eq!(session.csrf_hash, hash(&issued.csrf_secret));
    assert_eq!(issued.expires_at, fixed_now() + Duration::days(30));
    assert!(matches!(
        service
            .select(
                &completed.browser_secret,
                &completed.csrf_secret,
                GITHUB_ORG_A,
                None,
            )
            .await,
        Err(TeamAuthError::Unauthorized | TeamAuthError::Conflict)
    ));
}

#[tokio::test]
async fn select_uses_current_provider_login_and_rejects_forged_or_mismatched_evidence() {
    let choice = organization(GITHUB_ORG_A, "renamed-company", TeamRole::Member);
    let fixture = fixture(
        vec![Ok(durable_tokens("fresh-access"))],
        vec![Ok(identity(GITHUB_USER_ID, "new-user"))],
        vec![Ok(vec![choice.clone()]), Ok(vec![choice.clone()])],
        vec![Ok(evidence(
            GITHUB_USER_ID,
            GITHUB_ORG_B,
            "renamed-company",
            TeamRole::Member,
        ))],
        false,
    )
    .await;
    let service = fixture.service();
    let completed = pending(&service, "forgery-code").await;

    assert!(matches!(
        service
            .select(
                &completed.browser_secret,
                &completed.csrf_secret,
                GITHUB_ORG_B,
                None,
            )
            .await,
        Err(TeamAuthError::Forbidden)
    ));
    assert!(fixture.github.membership_calls().is_empty());
    assert!(matches!(
        service
            .select(
                &completed.browser_secret,
                &completed.csrf_secret,
                GITHUB_ORG_A,
                None,
            )
            .await,
        Err(TeamAuthError::Forbidden)
    ));
    assert_eq!(
        fixture.github.membership_calls(),
        vec![(
            "fresh-access".into(),
            GITHUB_USER_ID,
            GITHUB_ORG_A,
            "renamed-company".into()
        )]
    );
    assert_eq!(
        team_pending_onboarding::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn existing_identity_and_org_keep_legacy_scope_branding_and_local_block() {
    let choice_a = organization(GITHUB_ORG_A, "alice-renamed-org", TeamRole::Member);
    let blocked = organization(GITHUB_ORG_B, "alice-b", TeamRole::Member);
    let fixture = fixture(
        vec![
            Ok(durable_tokens("existing-access")),
            Ok(durable_tokens("blocked-access")),
        ],
        vec![
            Ok(GithubIdentity {
                github_user_id: ALICE_GITHUB_ID,
                login: "alice-renamed".into(),
                name: Some("Alice Current".into()),
                avatar_url: Some("https://avatars.githubusercontent.com/u/20301?v=2".into()),
            }),
            Ok(identity(ALICE_GITHUB_ID, "alice-renamed")),
        ],
        vec![Ok(vec![choice_a.clone()]), Ok(vec![blocked.clone()])],
        vec![
            Ok(evidence(
                ALICE_GITHUB_ID,
                GITHUB_ORG_A,
                "alice-renamed-org",
                TeamRole::Member,
            )),
            Ok(evidence(
                ALICE_GITHUB_ID,
                GITHUB_ORG_B,
                "alice-b",
                TeamRole::Member,
            )),
        ],
        true,
    )
    .await;
    let mut branded = org::Entity::find_by_id(201)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    branded.name = Set(Some("Hand-set brand".into()));
    branded.avatar_url = Set(Some("https://brand.invalid/logo".into()));
    branded.update(&fixture.db).await.unwrap();
    let service = fixture.service();
    let pending_a = pending(&service, "existing-code").await;
    service
        .select(
            &pending_a.browser_secret,
            &pending_a.csrf_secret,
            GITHUB_ORG_A,
            None,
        )
        .await
        .unwrap();

    let account = user::Entity::find_by_id(ALICE_USER_ID)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let branded = org::Entity::find_by_id(201)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let membership = organization_membership::Entity::find_by_id(401)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.login, "alice-renamed");
    assert_eq!(account.name.as_deref(), Some("Alice Current"));
    assert_eq!(account.org_id, 203);
    assert_eq!(account.role, "owner");
    assert_eq!(account.gh_token_enc.as_deref(), Some("legacy-must-not-be-read"));
    assert_eq!(branded.login, "alice-renamed-org");
    assert_eq!(branded.name.as_deref(), Some("Hand-set brand"));
    assert_eq!(branded.avatar_url.as_deref(), Some("https://brand.invalid/logo"));
    assert_eq!(membership.role, "member");
    assert_eq!(membership.authorization_revision, 8);
    assert_eq!(membership.credential_generation, 1);

    let pending_blocked = pending(&service, "blocked-code").await;
    assert!(matches!(
        service
            .select(
                &pending_blocked.browser_secret,
                &pending_blocked.csrf_secret,
                GITHUB_ORG_B,
                None,
            )
            .await,
        Err(TeamAuthError::Forbidden)
    ));
    assert!(team_pending_onboarding::Entity::find_by_id(hash(&pending_blocked.browser_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        organization_membership::Entity::find_by_id(402)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .state,
        "blocked"
    );
}

#[tokio::test]
async fn provider_wait_expiry_rolls_back_and_leaves_pending_retryable() {
    let choice = organization(GITHUB_ORG_A, "alpha-company", TeamRole::Member);
    let fixture = fixture(
        vec![Ok(durable_tokens("wait-access"))],
        vec![Ok(identity(GITHUB_USER_ID, "new-user"))],
        vec![Ok(vec![choice.clone()])],
        vec![Ok(evidence(
            GITHUB_USER_ID,
            GITHUB_ORG_A,
            "alpha-company",
            TeamRole::Member,
        ))],
        false,
    )
    .await;
    let gate = fixture.github.gate_membership();
    let service = Arc::new(fixture.service());
    let completed = pending(&service, "wait-code").await;
    let selecting = {
        let service = service.clone();
        let browser = completed.browser_secret.clone();
        let csrf = completed.csrf_secret.clone();
        tokio::spawn(async move { service.select(&browser, &csrf, GITHUB_ORG_A, None).await })
    };
    gate.started.acquire().await.unwrap().forget();
    fixture.clock.advance(Duration::seconds(60));
    gate.release.notify_waiters();

    assert!(matches!(selecting.await.unwrap(), Err(TeamAuthError::Unavailable)));
    assert_eq!(user::Entity::find().count(&fixture.db).await.unwrap(), 0);
    assert_eq!(team_session::Entity::find().count(&fixture.db).await.unwrap(), 0);
    assert!(team_pending_onboarding::Entity::find_by_id(hash(&completed.browser_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn previous_session_rotation_is_exact_and_failed_selection_preserves_sessions() {
    let choice = organization(GITHUB_ORG_A, "alpha-company", TeamRole::Member);
    let fixture = fixture(
        vec![
            Ok(durable_tokens("rotation-one")),
            Ok(durable_tokens("rotation-two")),
        ],
        vec![
            Ok(identity(GITHUB_USER_ID, "first-user")),
            Ok(identity(GITHUB_USER_ID, "first-user")),
        ],
        vec![Ok(vec![choice.clone()]), Ok(vec![choice.clone()])],
        vec![
            Ok(evidence(
                GITHUB_USER_ID,
                GITHUB_ORG_A,
                "alpha-company",
                TeamRole::Member,
            )),
            Ok(evidence(
                GITHUB_USER_ID,
                GITHUB_ORG_A,
                "alpha-company",
                TeamRole::Member,
            )),
        ],
        false,
    )
    .await;
    let service = fixture.service();
    let first_pending = pending(&service, "rotation-code-one").await;
    let first = service
        .select(
            &first_pending.browser_secret,
            &first_pending.csrf_secret,
            GITHUB_ORG_A,
            None,
        )
        .await
        .unwrap();
    let first_row = team_session::Entity::find_by_id(hash(&first.session_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let mut other_device = first_row.clone().into_active_model();
    let other_secret = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    other_device.token_hash = Set(hash(other_secret));
    other_device.insert(&fixture.db).await.unwrap();

    let second_pending = pending(&service, "rotation-code-two").await;
    assert!(matches!(
        service
            .select(
                &second_pending.browser_secret,
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                GITHUB_ORG_A,
                Some(&first.session_secret),
            )
            .await,
        Err(TeamAuthError::Unauthorized)
    ));
    assert!(team_session::Entity::find_by_id(hash(&first.session_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_some());
    assert!(matches!(
        service
            .select(
                &second_pending.browser_secret,
                &second_pending.csrf_secret,
                GITHUB_ORG_A,
                Some("not-a-session-secret"),
            )
            .await,
        Err(TeamAuthError::Unauthorized)
    ));
    let second = service
        .select(
            &second_pending.browser_secret,
            &second_pending.csrf_secret,
            GITHUB_ORG_A,
            Some(&first.session_secret),
        )
        .await
        .unwrap();
    assert!(team_session::Entity::find_by_id(hash(&first.session_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_none());
    assert!(team_session::Entity::find_by_id(hash(other_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_some());
    assert!(team_session::Entity::find_by_id(hash(&second.session_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn one_stable_user_selects_two_orgs_without_rewriting_legacy_or_existing_session_scope() {
    let org_c_id = 10_204;
    let choice_a = organization(GITHUB_ORG_A, "alice-a", TeamRole::Admin);
    let choice_c = organization(org_c_id, "alice-current", TeamRole::Member);
    let fixture = fixture(
        vec![
            Ok(durable_tokens("multi-org-one")),
            Ok(durable_tokens("multi-org-two")),
        ],
        vec![
            Ok(identity(ALICE_GITHUB_ID, "alice")),
            Ok(identity(ALICE_GITHUB_ID, "alice")),
        ],
        vec![Ok(vec![choice_a.clone()]), Ok(vec![choice_c.clone()])],
        vec![
            Ok(evidence(ALICE_GITHUB_ID, GITHUB_ORG_A, "alice-a", TeamRole::Admin)),
            Ok(evidence(
                ALICE_GITHUB_ID,
                org_c_id,
                "alice-current",
                TeamRole::Member,
            )),
        ],
        true,
    )
    .await;
    let service = fixture.service();
    let pending_a = pending(&service, "multi-org-code-one").await;
    let issued_a = service
        .select(
            &pending_a.browser_secret,
            &pending_a.csrf_secret,
            GITHUB_ORG_A,
            None,
        )
        .await
        .unwrap();
    let pending_c = pending(&service, "multi-org-code-two").await;
    let issued_c = service
        .select(
            &pending_c.browser_secret,
            &pending_c.csrf_secret,
            org_c_id,
            None,
        )
        .await
        .unwrap();

    let account = user::Entity::find_by_id(ALICE_USER_ID)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let session_a = team_session::Entity::find_by_id(hash(&issued_a.session_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let session_c = team_session::Entity::find_by_id(hash(&issued_c.session_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.org_id, 203);
    assert_eq!(account.role, "owner");
    assert_eq!(session_a.org_id, 201);
    assert_eq!(session_c.org_id, 204);
    assert_eq!(session_a.credential_generation, 2);
    assert_eq!(session_c.credential_generation, 2);
    assert_eq!(session_a.authorization_revision, 7);
}

#[tokio::test]
async fn previous_session_can_switch_accounts_without_revoking_another_device() {
    let second_user_id = 30_302;
    let choice_a = organization(GITHUB_ORG_A, "alpha-company", TeamRole::Member);
    let choice_b = organization(GITHUB_ORG_B, "beta-company", TeamRole::Member);
    let fixture = fixture(
        vec![
            Ok(durable_tokens("account-one")),
            Ok(durable_tokens("account-two")),
        ],
        vec![
            Ok(identity(GITHUB_USER_ID, "first-user")),
            Ok(identity(second_user_id, "second-user")),
        ],
        vec![Ok(vec![choice_a.clone()]), Ok(vec![choice_b.clone()])],
        vec![
            Ok(evidence(
                GITHUB_USER_ID,
                GITHUB_ORG_A,
                "alpha-company",
                TeamRole::Member,
            )),
            Ok(evidence(
                second_user_id,
                GITHUB_ORG_B,
                "beta-company",
                TeamRole::Member,
            )),
        ],
        false,
    )
    .await;
    let service = fixture.service();
    let pending_one = pending(&service, "account-code-one").await;
    let issued_one = service
        .select(
            &pending_one.browser_secret,
            &pending_one.csrf_secret,
            GITHUB_ORG_A,
            None,
        )
        .await
        .unwrap();
    let first_row = team_session::Entity::find_by_id(hash(&issued_one.session_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let other_device_secret =
        "edededededededededededededededededededededededededededededededed";
    let mut other_device = first_row.into_active_model();
    other_device.token_hash = Set(hash(other_device_secret));
    other_device.insert(&fixture.db).await.unwrap();

    let pending_two = pending(&service, "account-code-two").await;
    let issued_two = service
        .select(
            &pending_two.browser_secret,
            &pending_two.csrf_secret,
            GITHUB_ORG_B,
            Some(&issued_one.session_secret),
        )
        .await
        .unwrap();
    let second_row = team_session::Entity::find_by_id(hash(&issued_two.session_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    let other_device = team_session::Entity::find_by_id(hash(other_device_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(issued_one.user_id, issued_two.user_id);
    assert!(team_session::Entity::find_by_id(hash(&issued_one.session_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_none());
    assert_eq!(other_device.user_id, issued_one.user_id);
    assert_eq!(other_device.org_id, issued_one.org_id);
    assert_eq!(second_row.user_id, issued_two.user_id);
    assert_eq!(second_row.org_id, issued_two.org_id);
}

#[tokio::test]
async fn selection_observes_a_local_block_applied_while_provider_verification_waits() {
    let choice = organization(GITHUB_ORG_B, "alice-b", TeamRole::Member);
    let fixture = fixture(
        vec![Ok(durable_tokens("block-race"))],
        vec![Ok(identity(ALICE_GITHUB_ID, "alice"))],
        vec![Ok(vec![choice.clone()]), Ok(vec![choice.clone()])],
        vec![
            Ok(evidence(
                ALICE_GITHUB_ID,
                GITHUB_ORG_B,
                "alice-b",
                TeamRole::Member,
            )),
            Ok(evidence(
                ALICE_GITHUB_ID,
                GITHUB_ORG_B,
                "alice-b",
                TeamRole::Member,
            )),
        ],
        true,
    )
    .await;
    let mut initially_active = organization_membership::Entity::find_by_id(402)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    initially_active.state = Set("active".into());
    initially_active.update(&fixture.db).await.unwrap();
    let gate = fixture.github.gate_membership();
    let service = Arc::new(fixture.service());
    let pending = pending(&service, "block-race-code").await;
    let selecting = {
        let service = service.clone();
        let browser = pending.browser_secret.clone();
        let csrf = pending.csrf_secret.clone();
        tokio::spawn(async move { service.select(&browser, &csrf, GITHUB_ORG_B, None).await })
    };
    gate.started.acquire().await.unwrap().forget();
    let mut blocked = organization_membership::Entity::find_by_id(402)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    blocked.state = Set("blocked".into());
    blocked.update(&fixture.db).await.unwrap();
    gate.release.notify_waiters();

    assert!(matches!(selecting.await.unwrap(), Err(TeamAuthError::Forbidden)));
    assert!(team_pending_onboarding::Entity::find_by_id(hash(&pending.browser_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        organization_membership::Entity::find_by_id(402)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .state,
        "blocked"
    );
    fixture.github.clear_membership_gate();
    let mut lifted = organization_membership::Entity::find_by_id(402)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    lifted.state = Set("active".into());
    lifted.update(&fixture.db).await.unwrap();
    service
        .select(
            &pending.browser_secret,
            &pending.csrf_secret,
            GITHUB_ORG_B,
            None,
        )
        .await
        .expect("rolled-back pending remains retryable after an authorized block lift");
}

#[tokio::test]
async fn membership_revision_changes_only_for_revocation_or_role_change() {
    let choice = organization(GITHUB_ORG_A, "alice-a", TeamRole::Admin);
    let fixture = fixture(
        vec![
            Ok(durable_tokens("revision-one")),
            Ok(durable_tokens("revision-two")),
        ],
        vec![
            Ok(identity(ALICE_GITHUB_ID, "alice")),
            Ok(identity(ALICE_GITHUB_ID, "alice")),
        ],
        vec![Ok(vec![choice.clone()]), Ok(vec![choice.clone()])],
        vec![
            Ok(evidence(ALICE_GITHUB_ID, GITHUB_ORG_A, "alice-a", TeamRole::Admin)),
            Ok(evidence(ALICE_GITHUB_ID, GITHUB_ORG_A, "alice-a", TeamRole::Admin)),
        ],
        true,
    )
    .await;
    let service = fixture.service();
    let first_pending = pending(&service, "revision-code-one").await;
    service
        .select(
            &first_pending.browser_secret,
            &first_pending.csrf_secret,
            GITHUB_ORG_A,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        organization_membership::Entity::find_by_id(401)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .authorization_revision,
        7
    );
    let mut revoked = organization_membership::Entity::find_by_id(401)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    revoked.state = Set("revoked".into());
    revoked.update(&fixture.db).await.unwrap();
    let second_pending = pending(&service, "revision-code-two").await;
    service
        .select(
            &second_pending.browser_secret,
            &second_pending.csrf_secret,
            GITHUB_ORG_A,
            None,
        )
        .await
        .unwrap();
    let reactivated = organization_membership::Entity::find_by_id(401)
        .one(&fixture.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reactivated.state, "active");
    assert_eq!(reactivated.authorization_revision, 8);
}

#[tokio::test]
async fn pending_provider_errors_map_without_consuming_selection_authority() {
    let fixture = fixture(
        vec![Ok(durable_tokens("provider-errors"))],
        vec![Ok(identity(GITHUB_USER_ID, "provider-user"))],
        vec![
            Err(GithubEvidenceError::ReauthenticationRequired),
            Err(GithubEvidenceError::Unavailable),
            Err(GithubEvidenceError::NotMember),
        ],
        Vec::new(),
        false,
    )
    .await;
    let service = fixture.service();
    let pending = pending(&service, "provider-error-code").await;
    assert!(matches!(
        service.choices(&pending.browser_secret).await,
        Err(TeamAuthError::Unauthorized)
    ));
    assert!(matches!(
        service.choices(&pending.browser_secret).await,
        Err(TeamAuthError::Unavailable)
    ));
    assert!(matches!(
        service
            .select(
                &pending.browser_secret,
                &pending.csrf_secret,
                GITHUB_ORG_A,
                None,
            )
            .await,
        Err(TeamAuthError::Forbidden)
    ));
    assert!(team_pending_onboarding::Entity::find_by_id(hash(&pending.browser_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn oversized_organization_avatars_are_dropped_in_choices_and_new_rows() {
    let mut oversized = organization(GITHUB_ORG_A, "avatar-company", TeamRole::Member);
    oversized.avatar_url = Some(format!("https://example.invalid/{}", "z".repeat(2_100)));
    let fixture = fixture(
        vec![Ok(durable_tokens("avatar-access"))],
        vec![Ok(identity(GITHUB_USER_ID, "avatar-user"))],
        vec![Ok(vec![oversized.clone()]), Ok(vec![oversized])],
        vec![Ok(evidence(
            GITHUB_USER_ID,
            GITHUB_ORG_A,
            "avatar-company",
            TeamRole::Member,
        ))],
        false,
    )
    .await;
    let service = fixture.service();
    let pending = pending(&service, "avatar-code").await;
    assert_eq!(
        service.choices(&pending.browser_secret).await.unwrap().organizations[0].avatar_url,
        None
    );
    let issued = service
        .select(
            &pending.browser_secret,
            &pending.csrf_secret,
            GITHUB_ORG_A,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        org::Entity::find_by_id(issued.org_id)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .avatar_url,
        None
    );
}

#[tokio::test]
async fn cleanup_removes_only_expired_attempts_and_pending_records() {
    let fixture = fixture(
        vec![Ok(durable_tokens("cleanup-access"))],
        vec![Ok(identity(GITHUB_USER_ID, "cleanup-user"))],
        Vec::new(),
        Vec::new(),
        true,
    )
    .await;
    let service = fixture.service();
    let expired_pending = pending(&service, "cleanup-code").await;
    let expired_attempt = service.begin().await.unwrap();
    fixture.clock.advance(Duration::minutes(10));
    let live_attempt = service.begin().await.unwrap();
    let sessions_before = team_session::Entity::find().count(&fixture.db).await.unwrap();
    let credentials_before = team_github_credential::Entity::find()
        .count(&fixture.db)
        .await
        .unwrap();

    service.cleanup_expired().await.unwrap();

    assert!(team_pending_onboarding::Entity::find_by_id(hash(&expired_pending.browser_secret))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_none());
    assert!(team_oauth_attempt::Entity::find_by_id(hash(&expired_attempt.state))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_none());
    assert!(team_oauth_attempt::Entity::find_by_id(hash(&live_attempt.state))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_some());
    assert_eq!(team_session::Entity::find().count(&fixture.db).await.unwrap(), sessions_before);
    assert_eq!(
        team_github_credential::Entity::find()
            .count(&fixture.db)
            .await
            .unwrap(),
        credentials_before
    );
}

#[tokio::test]
async fn secret_values_never_appear_in_errors() {
    let fixture = fixture(Vec::new(), Vec::new(), Vec::new(), Vec::new(), false).await;
    let browser = "abababababababababababababababababababababababababababababababab";
    let csrf = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
    let error = fixture
        .service()
        .select(browser, csrf, GITHUB_ORG_A, None)
        .await
        .err()
        .expect("missing pending must fail")
        .to_string();
    assert!(!error.contains(browser));
    assert!(!error.contains(csrf));
}

/// `CARGO_INCREMENTAL=0 cargo test --no-default-features --lib onboarding_postgres_selection_rollback -- --ignored --test-threads=1 --nocapture`
#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn onboarding_postgres_selection_rollback() {
    let (name, admin, db) = create_postgres_fixture().await.expect("PostgreSQL fixture");
    let result = async {
        let vault = Arc::new(
            SecretVault::from_hex_key(
                "3333333333333333333333333333333333333333333333333333333333333333",
            )
            .unwrap(),
        );
        let oauth = Arc::new(FakeOAuth::with_exchange(vec![Ok(durable_tokens("pg-access"))]));
        let github = Arc::new(FakeGithub::new(
            vec![Ok(identity(ALICE_GITHUB_ID, "alice"))],
            vec![Ok(vec![organization(GITHUB_ORG_B, "alice-b", TeamRole::Member)])],
            vec![Ok(evidence(
                ALICE_GITHUB_ID,
                GITHUB_ORG_B,
                "alice-b",
                TeamRole::Member,
            ))],
        ));
        let clock = Arc::new(TestClock::new(fixed_now()));
        let credentials = Arc::new(CredentialService::new(
            db.clone(),
            vault.clone(),
            oauth.clone(),
            clock.clone(),
        ));
        let service = OnboardingService::new(
            db.clone(), vault, oauth, github, credentials, clock,
        );
        let pending = pending(&service, "pg-rollback-code").await;
        let before = team_github_credential::Entity::find().count(&db).await.unwrap();
        assert!(matches!(
            service
                .select(
                    &pending.browser_secret,
                    &pending.csrf_secret,
                    GITHUB_ORG_B,
                    None,
                )
                .await,
            Err(TeamAuthError::Forbidden)
        ));
        assert!(team_pending_onboarding::Entity::find_by_id(hash(&pending.browser_secret))
            .one(&db)
            .await
            .unwrap()
            .is_some());
        assert_eq!(team_github_credential::Entity::find().count(&db).await.unwrap(), before);
        Ok::<(), String>(())
    }
    .await;
    drop_postgres_fixture(&name, admin, db)
        .await
        .expect("exact PostgreSQL cleanup");
    result.unwrap();
}

/// `CARGO_INCREMENTAL=0 cargo test --no-default-features --lib onboarding_postgres_attempt_expiry_while_consuming -- --ignored --test-threads=1 --nocapture`
#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn onboarding_postgres_attempt_expiry_while_consuming() {
    let (name, admin, db) = create_postgres_fixture().await.expect("PostgreSQL fixture");
    let result = async {
        let vault = Arc::new(
            SecretVault::from_hex_key(
                "5555555555555555555555555555555555555555555555555555555555555555",
            )
            .unwrap(),
        );
        let oauth = Arc::new(FakeOAuth::with_exchange(vec![Ok(durable_tokens(
            "must-not-exchange",
        ))]));
        let github = Arc::new(FakeGithub::new(Vec::new(), Vec::new(), Vec::new()));
        let clock = Arc::new(TestClock::new(fixed_now()));
        let credentials = Arc::new(CredentialService::new(
            db.clone(),
            vault.clone(),
            oauth.clone(),
            clock.clone(),
        ));
        let service = Arc::new(OnboardingService::new(
            db.clone(), vault, oauth.clone(), github, credentials, clock.clone(),
        ));
        let attempt = service.begin().await.unwrap();
        let lock = db.begin().await.unwrap();
        lock.query_one(Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "SELECT state_hash FROM team_oauth_attempt WHERE state_hash = $1 FOR UPDATE",
            [hash(&attempt.state).into()],
        ))
        .await
        .unwrap()
        .expect("lock attempt row");
        let completing = {
            let service = service.clone();
            let state = attempt.state.clone();
            let browser = attempt.browser_secret.clone();
            tokio::spawn(async move { service.complete(&state, &browser, "waited-code").await })
        };
        tokio::task::yield_now().await;
        clock.advance(Duration::minutes(10));
        lock.commit().await.unwrap();
        assert!(matches!(
            completing.await.unwrap(),
            Err(TeamAuthError::Unauthorized)
        ));
        assert!(oauth.exchange_calls().is_empty());
        assert_eq!(
            team_pending_onboarding::Entity::find().count(&db).await.unwrap(),
            0
        );
        Ok::<(), String>(())
    }
    .await;
    drop_postgres_fixture(&name, admin, db)
        .await
        .expect("exact PostgreSQL cleanup");
    result.unwrap();
}

/// `CARGO_INCREMENTAL=0 cargo test --no-default-features --lib onboarding_postgres_concurrent_consume -- --ignored --test-threads=1 --nocapture`
#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn onboarding_postgres_concurrent_consume() {
    let (name, admin, db) = create_postgres_fixture().await.expect("PostgreSQL fixture");
    let result = async {
        let vault = Arc::new(
            SecretVault::from_hex_key(
                "4444444444444444444444444444444444444444444444444444444444444444",
            )
            .unwrap(),
        );
        let oauth = Arc::new(FakeOAuth::with_exchange(vec![Ok(durable_tokens("race-access"))]));
        let choice = organization(GITHUB_ORG_A, "alice-a", TeamRole::Admin);
        let github = Arc::new(FakeGithub::new(
            vec![Ok(identity(ALICE_GITHUB_ID, "alice"))],
            vec![Ok(vec![choice.clone()]), Ok(vec![choice.clone()])],
            vec![
                Ok(evidence(ALICE_GITHUB_ID, GITHUB_ORG_A, "alice-a", TeamRole::Admin)),
                Ok(evidence(ALICE_GITHUB_ID, GITHUB_ORG_A, "alice-a", TeamRole::Admin)),
            ],
        ));
        let gate = github.gate_membership();
        let clock = Arc::new(TestClock::new(fixed_now()));
        let credentials = Arc::new(CredentialService::new(
            db.clone(),
            vault.clone(),
            oauth.clone(),
            clock.clone(),
        ));
        let service = Arc::new(OnboardingService::new(
            db.clone(), vault, oauth, github, credentials, clock,
        ));
        let pending = pending(&service, "pg-race-code").await;
        let one = {
            let service = service.clone();
            let browser = pending.browser_secret.clone();
            let csrf = pending.csrf_secret.clone();
            tokio::spawn(async move { service.select(&browser, &csrf, GITHUB_ORG_A, None).await })
        };
        let two = {
            let service = service.clone();
            let browser = pending.browser_secret.clone();
            let csrf = pending.csrf_secret.clone();
            tokio::spawn(async move { service.select(&browser, &csrf, GITHUB_ORG_A, None).await })
        };
        for _ in 0..2 {
            gate.started.acquire().await.unwrap().forget();
        }
        gate.release.notify_waiters();
        let results = [one.await.unwrap(), two.await.unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        assert_eq!(team_session::Entity::find().count(&db).await.unwrap(), 6);
        Ok::<(), String>(())
    }
    .await;
    drop_postgres_fixture(&name, admin, db)
        .await
        .expect("exact PostgreSQL cleanup");
    result.unwrap();
}
