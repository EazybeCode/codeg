use std::{collections::HashMap, sync::Arc};

use crate::team_access::{
    core::TeamClock,
    credentials::CredentialService,
    github::{
        GithubEvidenceError, GithubIdentity, GithubMembershipEvidence, GithubOrganizationChoice,
    },
    oauth::{GithubOAuthClient, GithubTokenSet, OAuthError},
    onboarding::OnboardingService,
    principal::PrincipalService,
    secrets::SecretVault,
    test_auth::{fixed_now, FakeGithub, FakeOAuth, TestClock},
};

use super::{config::TeamConfig, state::TeamState};

pub(super) struct TeamHttpFixture {
    pub(super) state: TeamState,
    pub(super) oauth: Arc<FakeOAuth>,
    _static_root: tempfile::TempDir,
}

pub(super) async fn team_state(
    exchanges: Vec<Result<GithubTokenSet, OAuthError>>,
    identities: Vec<Result<GithubIdentity, GithubEvidenceError>>,
    organizations: Vec<Result<Vec<GithubOrganizationChoice>, GithubEvidenceError>>,
    memberships: Vec<Result<GithubMembershipEvidence, GithubEvidenceError>>,
) -> TeamHttpFixture {
    let static_root = tempfile::tempdir().expect("team static root");
    for page in ["login", "onboarding", "workspace", "settings"] {
        std::fs::write(
            static_root.path().join(format!("{page}.html")),
            format!("<!doctype html><title>Eazybe WorkOS</title><main>{page}</main>"),
        )
        .expect("static fixture page");
    }
    let mut values = HashMap::from([
        (
            "WORKOS_APP_ORIGIN".to_owned(),
            "https://workos.example".to_owned(),
        ),
        (
            "GITHUB_CALLBACK_URL".to_owned(),
            "https://workos.example/auth/github/callback".to_owned(),
        ),
        ("GITHUB_CLIENT_ID".to_owned(), "test-client".to_owned()),
        ("GITHUB_CLIENT_SECRET".to_owned(), "test-secret".to_owned()),
        (
            "WORKOS_ENC_KEY".to_owned(),
            "2222222222222222222222222222222222222222222222222222222222222222".to_owned(),
        ),
        (
            "DATABASE_URL".to_owned(),
            "postgres://test@localhost/workos".to_owned(),
        ),
        ("CODEG_HOST".to_owned(), "127.0.0.1".to_owned()),
        ("CODEG_PORT".to_owned(), "3080".to_owned()),
        (
            "CODEG_STATIC_DIR".to_owned(),
            static_root.path().display().to_string(),
        ),
    ]);
    values.insert("WORKOS_ENABLE_CATALOG_RUNTIME".to_owned(), "false".to_owned());
    let config = TeamConfig::from_values(&values).expect("team HTTP config");
    let db = crate::db::test_helpers::fresh_in_memory_db().await.conn;
    let vault = Arc::new(
        SecretVault::from_hex_key(&values["WORKOS_ENC_KEY"]).expect("team HTTP vault"),
    );
    let fake_oauth = Arc::new(FakeOAuth::with_exchange(exchanges));
    let github = Arc::new(FakeGithub::new(identities, organizations, memberships));
    let clock: Arc<dyn TeamClock> = Arc::new(TestClock::new(fixed_now()));
    let credentials = Arc::new(CredentialService::new(
        db.clone(),
        vault.clone(),
        fake_oauth.clone(),
        clock.clone(),
    ));
    let onboarding = Arc::new(OnboardingService::new(
        db.clone(),
        vault.clone(),
        fake_oauth.clone(),
        github.clone(),
        credentials.clone(),
        clock.clone(),
    ));
    let principals = Arc::new(PrincipalService::new(
        db.clone(),
        credentials.clone(),
        github,
        clock.clone(),
    ));
    let authorization = Arc::new(
        GithubOAuthClient::new(
            config.github_client_id.clone(),
            config.github_client_secret.clone(),
            config.callback.clone(),
        )
        .expect("authorization URL client"),
    );
    TeamHttpFixture {
        state: TeamState::new(
            db,
            config,
            vault,
            authorization,
            onboarding,
            credentials,
            principals,
            clock,
        ),
        oauth: fake_oauth,
        _static_root: static_root,
    }
}
