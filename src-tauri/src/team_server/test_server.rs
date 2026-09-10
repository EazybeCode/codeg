use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::{
    extract::{Form, Path, Query, State, WebSocketUpgrade},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use axum_test::{TestResponse, TestServer};
use chrono::Duration as ChronoDuration;
use serde::Deserialize;
use serde_json::json;
use tokio::{
    net::TcpListener,
    sync::{oneshot, Notify},
    task::JoinHandle,
};

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

pub(super) const BROWSER_CLIENT_ID: &str = "browser-fixture-client";
pub(super) const BROWSER_CLIENT_SECRET: &str = "browser-fixture-secret";

const ALICE_USER_ID: i64 = 51_001;
const BOB_USER_ID: i64 = 51_002;
const CAROL_USER_ID: i64 = 51_003;
pub(super) const ORG_A_ID: i64 = 61_001;
pub(super) const ORG_B_ID: i64 = 61_002;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserPersona {
    Alice,
    Bob,
    Carol,
    Outage,
}

impl BrowserPersona {
    fn from_choice(value: &str) -> Option<Self> {
        match value {
            "alice" => Some(Self::Alice),
            "bob" => Some(Self::Bob),
            "carol" => Some(Self::Carol),
            "outage" => Some(Self::Outage),
            _ => None,
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::Alice => "alice",
            Self::Bob => "bob",
            Self::Carol => "carol",
            Self::Outage => "outage",
        }
    }
}

struct AuthorizationGrant {
    persona: BrowserPersona,
    challenge: String,
}

#[derive(Default)]
struct ProviderCounts {
    authorize: AtomicUsize,
    exchange: AtomicUsize,
    identity: AtomicUsize,
    listing: AtomicUsize,
    proof: AtomicUsize,
}

struct BrowserProviderState {
    expected_callback: String,
    clock: Arc<TestClock>,
    grants: Mutex<HashMap<String, AuthorizationGrant>>,
    tokens: Mutex<HashMap<String, BrowserPersona>>,
    sequence: AtomicUsize,
    zero_next_listing: AtomicBool,
    fail_next_proof: AtomicBool,
    counts: ProviderCounts,
    session_delay: Arc<SessionResponseGate>,
    settings_delay: Arc<SessionResponseGate>,
}

/// Controls a single delayed browser-fixture response. The response is produced
/// by the real team router first, then held so the browser can rotate authority
/// and prove that an obsolete body never paints.
pub(super) struct SessionResponseGate {
    armed: AtomicBool,
    started: AtomicBool,
    released: AtomicBool,
    changed: Notify,
}

impl SessionResponseGate {
    fn new() -> Self {
        Self {
            armed: AtomicBool::new(false),
            started: AtomicBool::new(false),
            released: AtomicBool::new(false),
            changed: Notify::new(),
        }
    }

    fn arm(&self) -> bool {
        if self
            .armed
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        self.started.store(false, Ordering::SeqCst);
        self.released.store(false, Ordering::SeqCst);
        true
    }

    pub(super) fn take(&self) -> bool {
        self.armed.swap(false, Ordering::SeqCst)
    }

    pub(super) fn mark_started(&self) {
        self.started.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    pub(super) async fn wait_for_release(&self) {
        while !self.released.load(Ordering::SeqCst) {
            self.changed.notified().await;
        }
    }

    pub(super) fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
    }
}

pub(super) struct BrowserProviderServer {
    origin: String,
    state: Arc<BrowserProviderState>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl BrowserProviderServer {
    pub(super) async fn start(clock: Arc<TestClock>, expected_callback: String) -> Self {
        let state = Arc::new(BrowserProviderState {
            expected_callback,
            clock,
            grants: Mutex::new(HashMap::new()),
            tokens: Mutex::new(HashMap::new()),
            sequence: AtomicUsize::new(1),
            zero_next_listing: AtomicBool::new(false),
            fail_next_proof: AtomicBool::new(false),
            counts: ProviderCounts::default(),
            session_delay: Arc::new(SessionResponseGate::new()),
            settings_delay: Arc::new(SessionResponseGate::new()),
        });
        let app = Router::new()
            .route("/login/oauth/authorize", get(provider_authorize))
            .route("/fixture/authorize", get(provider_choice))
            .route("/login/oauth/access_token", post(provider_exchange))
            .route("/user", get(provider_identity))
            .route("/user/memberships/orgs", get(provider_organizations))
            .route(
                "/user/memberships/orgs/{login}",
                get(provider_membership),
            )
            .route(
                "/repos/{owner}/{repository}",
                get(provider_repository),
            )
            .route(
                "/repos/{owner}/{repository}/branches/{branch}",
                get(provider_repository_branch),
            )
            .route("/fixture/zero-next", post(provider_zero_next))
            .route("/fixture/fail-next-proof", post(provider_fail_next_proof))
            .route("/fixture/advance-clock", post(provider_advance_clock))
            .route(
                "/fixture/session-delay/arm",
                post(provider_arm_session_delay),
            )
            .route(
                "/fixture/session-delay/release",
                post(provider_release_session_delay),
            )
            .route(
                "/fixture/settings-delay/arm",
                post(provider_arm_settings_delay),
            )
            .route(
                "/fixture/settings-delay/release",
                post(provider_release_settings_delay),
            )
            .route("/fixture/status", get(provider_status))
            .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
            .with_state(Arc::clone(&state));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake GitHub listener");
        let address = listener.local_addr().expect("fake GitHub address");
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = receiver.await;
                })
                .await
                .expect("fake GitHub server");
        });
        Self {
            origin: format!("http://{address}"),
            state,
            shutdown: Some(shutdown),
            task,
        }
    }

    pub(super) fn origin(&self) -> &str {
        &self.origin
    }

    pub(super) fn session_delay(&self) -> Arc<SessionResponseGate> {
        Arc::clone(&self.state.session_delay)
    }

    pub(super) fn settings_delay(&self) -> Arc<SessionResponseGate> {
        Arc::clone(&self.state.settings_delay)
    }

    pub(super) async fn shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if tokio::time::timeout(Duration::from_secs(2), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
        }
    }
}

impl Drop for BrowserProviderServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

#[derive(Deserialize)]
struct AuthorizationQuery {
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
    code_challenge_method: String,
}

async fn provider_authorize(
    State(state): State<Arc<BrowserProviderState>>,
    Query(query): Query<AuthorizationQuery>,
) -> Response {
    state.counts.authorize.fetch_add(1, Ordering::SeqCst);
    if query.client_id != BROWSER_CLIENT_ID
        || query.redirect_uri != state.expected_callback
        || query.code_challenge_method != "S256"
        || !valid_secret(&query.state)
        || !valid_challenge(&query.code_challenge)
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let hidden = format!(
        "<input type=\"hidden\" name=\"state\" value=\"{}\"><input type=\"hidden\" name=\"redirect_uri\" value=\"{}\"><input type=\"hidden\" name=\"code_challenge\" value=\"{}\">",
        html_attribute(&query.state),
        html_attribute(&query.redirect_uri),
        html_attribute(&query.code_challenge),
    );
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Fake GitHub consent</title></head><body><main><h1>Fake GitHub consent</h1><p>Invented local evidence only.</p><form action=\"/fixture/authorize\" method=\"get\">{hidden}<button name=\"persona\" value=\"alice\">Continue as Alice Atlas</button><button name=\"persona\" value=\"bob\">Continue as Bob Birch</button><button name=\"persona\" value=\"carol\">Continue as Carol Coast</button><button name=\"persona\" value=\"outage\">Simulate provider outage</button><button name=\"persona\" value=\"deny\">Deny access</button></form></main></body></html>"
    ))
    .into_response()
}

#[derive(Deserialize)]
struct ProviderChoiceQuery {
    persona: String,
    state: String,
    redirect_uri: String,
    code_challenge: String,
}

async fn provider_choice(
    State(state): State<Arc<BrowserProviderState>>,
    Query(query): Query<ProviderChoiceQuery>,
) -> Response {
    if query.redirect_uri != state.expected_callback
        || !valid_secret(&query.state)
        || !valid_challenge(&query.code_challenge)
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut callback = match reqwest::Url::parse(&query.redirect_uri) {
        Ok(callback) => callback,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if query.persona == "deny" {
        callback
            .query_pairs_mut()
            .append_pair("state", &query.state)
            .append_pair("error", "access_denied");
        return redirect_response(callback);
    }
    let Some(persona) = BrowserPersona::from_choice(&query.persona) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let sequence = state.sequence.fetch_add(1, Ordering::SeqCst);
    let code = format!("fixture-code-{}-{sequence}", persona.slug());
    state.grants.lock().expect("provider grants").insert(
        code.clone(),
        AuthorizationGrant {
            persona,
            challenge: query.code_challenge,
        },
    );
    callback
        .query_pairs_mut()
        .append_pair("state", &query.state)
        .append_pair("code", &code);
    redirect_response(callback)
}

fn redirect_response(location: reqwest::Url) -> Response {
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, location.as_str())
        .body(axum::body::Body::empty())
        .expect("provider redirect")
}

async fn provider_exchange(
    State(state): State<Arc<BrowserProviderState>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    state.counts.exchange.fetch_add(1, Ordering::SeqCst);
    if form.get("client_id").map(String::as_str) != Some(BROWSER_CLIENT_ID)
        || form.get("client_secret").map(String::as_str) != Some(BROWSER_CLIENT_SECRET)
        || form.get("redirect_uri").map(String::as_str) != Some(state.expected_callback.as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(code) = form.get("code") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(verifier) = form.get("code_verifier") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(grant) = state.grants.lock().expect("provider grants").remove(code) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if crate::team_access::secrets::pkce_challenge(verifier)
        .ok()
        .as_deref()
        != Some(grant.challenge.as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if grant.persona == BrowserPersona::Outage {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let token = format!(
        "fixture-access-{}-{}",
        grant.persona.slug(),
        state.sequence.fetch_add(1, Ordering::SeqCst)
    );
    state
        .tokens
        .lock()
        .expect("provider tokens")
        .insert(token.clone(), grant.persona);
    Json(json!({
        "access_token": token,
        "token_type": "bearer",
        "expires_in": 28_800,
        "refresh_token": format!("fixture-refresh-{}", grant.persona.slug()),
        "refresh_token_expires_in": 15_897_600,
        "scope": "read:user"
    }))
    .into_response()
}

async fn provider_identity(
    State(state): State<Arc<BrowserProviderState>>,
    headers: HeaderMap,
) -> Response {
    state.counts.identity.fetch_add(1, Ordering::SeqCst);
    let Some(persona) = authenticated_persona(&state, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (id, login, name) = persona_identity(persona);
    Json(json!({
        "id": id,
        "login": login,
        "name": name,
        "avatar_url": null,
        "type": "User"
    }))
    .into_response()
}

async fn provider_organizations(
    State(state): State<Arc<BrowserProviderState>>,
    headers: HeaderMap,
) -> Response {
    state.counts.listing.fetch_add(1, Ordering::SeqCst);
    let Some(persona) = authenticated_persona(&state, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if state.zero_next_listing.swap(false, Ordering::SeqCst) {
        return Json(json!([])).into_response();
    }
    let organizations: Vec<_> = persona_organizations(persona)
        .into_iter()
        .map(|(org_id, login, role)| membership_json(persona, org_id, login, role))
        .collect();
    Json(json!(organizations)).into_response()
}

async fn provider_membership(
    State(state): State<Arc<BrowserProviderState>>,
    Path(login): Path<String>,
    headers: HeaderMap,
) -> Response {
    state.counts.proof.fetch_add(1, Ordering::SeqCst);
    let Some(persona) = authenticated_persona(&state, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if state.fail_next_proof.swap(false, Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Some((org_id, canonical, role)) = persona_organizations(persona)
        .into_iter()
        .find(|(_, candidate, _)| candidate.eq_ignore_ascii_case(&login))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Json(membership_json(persona, org_id, canonical, role)).into_response()
}

async fn provider_repository(
    State(state): State<Arc<BrowserProviderState>>,
    Path((owner, repository)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if authenticated_persona(&state, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if owner != "atlas" || repository != "repo" {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(json!({"id": 8080, "full_name": "atlas/repo"})).into_response()
}

async fn provider_repository_branch(
    State(state): State<Arc<BrowserProviderState>>,
    Path((owner, repository, branch)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    if authenticated_persona(&state, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if owner != "atlas" || repository != "repo" || branch != "main" {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(json!({"name": "main"})).into_response()
}

fn authenticated_persona(
    state: &BrowserProviderState,
    headers: &HeaderMap,
) -> Option<BrowserPersona> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?;
    state
        .tokens
        .lock()
        .expect("provider tokens")
        .get(token)
        .copied()
}

fn persona_identity(persona: BrowserPersona) -> (i64, &'static str, &'static str) {
    match persona {
        BrowserPersona::Alice => (ALICE_USER_ID, "alice-atlas", "Alice Atlas"),
        BrowserPersona::Bob => (BOB_USER_ID, "bob-birch", "Bob Birch"),
        BrowserPersona::Carol => (CAROL_USER_ID, "carol-coast", "Carol Coast"),
        BrowserPersona::Outage => unreachable!("outage persona has no token"),
    }
}

fn persona_organizations(
    persona: BrowserPersona,
) -> Vec<(i64, &'static str, &'static str)> {
    match persona {
        BrowserPersona::Alice => vec![
            (ORG_A_ID, "atlas-labs", "admin"),
            (ORG_B_ID, "beacon-works", "admin"),
        ],
        BrowserPersona::Bob => vec![(ORG_A_ID, "atlas-labs", "member")],
        BrowserPersona::Carol => vec![(ORG_B_ID, "beacon-works", "admin")],
        BrowserPersona::Outage => Vec::new(),
    }
}

fn membership_json(
    persona: BrowserPersona,
    org_id: i64,
    org_login: &str,
    role: &str,
) -> serde_json::Value {
    let (user_id, user_login, _) = persona_identity(persona);
    json!({
        "state": "active",
        "role": role,
        "user": {"id": user_id, "login": user_login, "type": "User"},
        "organization": {"id": org_id, "login": org_login, "avatar_url": null}
    })
}

async fn provider_zero_next(State(state): State<Arc<BrowserProviderState>>) -> StatusCode {
    state.zero_next_listing.store(true, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}

async fn provider_fail_next_proof(
    State(state): State<Arc<BrowserProviderState>>,
) -> StatusCode {
    state.fail_next_proof.store(true, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
struct AdvanceClockQuery {
    seconds: i64,
}

async fn provider_advance_clock(
    State(state): State<Arc<BrowserProviderState>>,
    Query(query): Query<AdvanceClockQuery>,
) -> StatusCode {
    if !(1..=3_600).contains(&query.seconds) {
        return StatusCode::BAD_REQUEST;
    }
    state.clock.advance(ChronoDuration::seconds(query.seconds));
    StatusCode::NO_CONTENT
}

async fn provider_arm_session_delay(
    State(state): State<Arc<BrowserProviderState>>,
) -> StatusCode {
    if state.session_delay.arm() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::CONFLICT
    }
}

async fn provider_release_session_delay(
    State(state): State<Arc<BrowserProviderState>>,
) -> StatusCode {
    state.session_delay.release();
    StatusCode::NO_CONTENT
}

async fn provider_arm_settings_delay(
    State(state): State<Arc<BrowserProviderState>>,
) -> StatusCode {
    if state.settings_delay.arm() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::CONFLICT
    }
}

async fn provider_release_settings_delay(
    State(state): State<Arc<BrowserProviderState>>,
) -> StatusCode {
    state.settings_delay.release();
    StatusCode::NO_CONTENT
}

async fn provider_status(
    State(state): State<Arc<BrowserProviderState>>,
) -> Json<serde_json::Value> {
    Json(json!({
        "authorize": state.counts.authorize.load(Ordering::SeqCst),
        "exchange": state.counts.exchange.load(Ordering::SeqCst),
        "identity": state.counts.identity.load(Ordering::SeqCst),
        "listing": state.counts.listing.load(Ordering::SeqCst),
        "proof": state.counts.proof.load(Ordering::SeqCst),
        "sessionDelayStarted": state.session_delay.started.load(Ordering::SeqCst),
        "settingsDelayStarted": state.settings_delay.started.load(Ordering::SeqCst)
    }))
}

fn valid_secret(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_challenge(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn html_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[derive(Default)]
struct CanaryState {
    http_requests: AtomicUsize,
    websocket_upgrades: AtomicUsize,
}

pub(super) struct BrowserCanaryServer {
    origin: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl BrowserCanaryServer {
    pub(super) async fn start() -> Self {
        let state = Arc::new(CanaryState::default());
        let app = Router::new()
            .route("/custom-css-canary", get(canary_http))
            .route("/legacy-http", get(canary_http))
            .route("/legacy-ws", get(canary_websocket))
            .route("/fixture/status", get(canary_status))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("canary listener");
        let address = listener.local_addr().expect("canary address");
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = receiver.await;
                })
                .await
                .expect("canary server");
        });
        Self {
            origin: format!("http://{address}"),
            shutdown: Some(shutdown),
            task,
        }
    }

    pub(super) fn origin(&self) -> &str {
        &self.origin
    }

    pub(super) async fn shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if tokio::time::timeout(Duration::from_secs(2), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
        }
    }
}

impl Drop for BrowserCanaryServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

async fn canary_http(State(state): State<Arc<CanaryState>>) -> StatusCode {
    state.http_requests.fetch_add(1, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}

async fn canary_websocket(
    State(state): State<Arc<CanaryState>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    state.websocket_upgrades.fetch_add(1, Ordering::SeqCst);

    upgrade.on_upgrade(|_socket| async move {})
}

async fn canary_status(State(state): State<Arc<CanaryState>>) -> Json<serde_json::Value> {
    Json(json!({
        "httpRequests": state.http_requests.load(Ordering::SeqCst),
        "websocketUpgrades": state.websocket_upgrades.load(Ordering::SeqCst)
    }))
}
