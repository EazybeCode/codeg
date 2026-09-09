use std::{collections::HashMap, sync::Arc};

use axum::http::header;
use axum_test::{TestResponse, TestServer};

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

pub(super) struct PendingBrowser {
    pub(super) cookies: String,
    pub(super) csrf: String,
}

pub(super) struct SessionBrowser {
    pub(super) cookies: String,
    pub(super) session: String,
    pub(super) csrf: String,
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

pub(super) async fn begin_pending(
    server: &TestServer,
    code: &str,
    existing_cookies: Option<&str>,
) -> (TestResponse, PendingBrowser) {
    let mut login = server.get("/auth/github/login");
    if let Some(cookies) = existing_cookies {
        login = login.add_header(header::COOKIE, cookies);
    }
    let login = login.await;
    let browser = cookie_value(&login, "workos_team_oauth_browser");
    let state = query_value(
        login.header(header::LOCATION).to_str().expect("login location"),
        "state",
    );
    let mut callback_cookies = format!("workos_team_oauth_browser={browser}");
    if let Some(cookies) = existing_cookies {
        callback_cookies.push_str("; ");
        callback_cookies.push_str(cookies);
    }
    let callback = server
        .get(&format!(
            "/auth/github/callback?state={state}&code={code}"
        ))
        .add_header(header::COOKIE, callback_cookies)
        .await;
    let pending = cookie_value(&callback, "workos_team_pending");
    let csrf = cookie_value(&callback, "workos_team_pending_csrf");
    (
        login,
        PendingBrowser {
            cookies: format!(
                "workos_team_pending={pending}; workos_team_pending_csrf={csrf}"
            ),
            csrf,
        },
    )
}

pub(super) async fn select_pending(
    server: &TestServer,
    pending: &PendingBrowser,
    github_org_id: &str,
    previous: Option<&SessionBrowser>,
) -> TestResponse {
    let mut cookies = pending.cookies.clone();
    if let Some(previous) = previous {
        cookies.push_str("; ");
        cookies.push_str(&previous.cookies);
    }
    server
        .post("/api/team/onboarding/select")
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("X-WorkOS-CSRF", &pending.csrf)
        .add_header(header::COOKIE, cookies)
        .json(&serde_json::json!({"githubOrgId":github_org_id}))
        .await
}

pub(super) fn session_browser(response: &TestResponse) -> SessionBrowser {
    let session = cookie_value(response, "workos_team_session");
    let csrf = cookie_value(response, "workos_team_csrf");
    SessionBrowser {
        cookies: format!("workos_team_session={session}; workos_team_csrf={csrf}"),
        session,
        csrf,
    }
}

pub(super) fn set_cookies(response: &TestResponse) -> Vec<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().expect("ASCII cookie").to_owned())
        .collect()
}

pub(super) fn cookie_value(response: &TestResponse, name: &str) -> String {
    set_cookies(response)
        .into_iter()
        .find_map(|cookie| {
            cookie
                .strip_prefix(&format!("{name}="))
                .and_then(|value| value.split(';').next())
                .map(str::to_owned)
        })
        .expect("named response cookie")
}

pub(super) fn query_value(url: &str, name: &str) -> String {
    url.split_once('?')
        .and_then(|(_, query)| {
            query.split('&').find_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                (key == name).then(|| value.to_owned())
            })
        })
        .expect("query value")
}
