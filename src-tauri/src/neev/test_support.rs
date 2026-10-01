use super::{
    error::AuthError,
    mail::{MailMessage, MailSender},
    types::{Provider, VerifiedIdentity},
};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use openidconnect::{
    core::{
        CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
        CoreRsaPrivateSigningKey,
    },
    Audience, EndUserEmail, IssuerUrl, JsonWebKeyId, Nonce, PrivateSigningKey, StandardClaims,
    SubjectIdentifier,
};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::sync::Mutex;

pub(super) struct TestClock(pub(super) AtomicI64);

impl TestClock {
    pub(super) fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }
}

impl super::clock::Clock for TestClock {
    fn now_unix(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

pub(super) struct MailCapture {
    messages: Mutex<Vec<MailMessage>>,
    fail: AtomicBool,
}

pub(super) struct OAuthIssuerCounter {
    base_url: String,
    requests: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}

impl OAuthIssuerCounter {
    pub(super) async fn start() -> Self {
        use axum::{
            extract::State,
            http::Uri,
            response::IntoResponse,
            routing::{get, post},
            Json, Router,
        };

        #[derive(Clone)]
        struct CounterState {
            base_url: String,
            requests: Arc<AtomicUsize>,
        }

        async fn discovery(State(state): State<CounterState>) -> impl IntoResponse {
            state.requests.fetch_add(1, Ordering::SeqCst);
            Json(serde_json::json!({
                "issuer":"https://accounts.google.com",
                "authorization_endpoint":format!("{}/authorize", state.base_url),
                "token_endpoint":format!("{}/token", state.base_url),
                "jwks_uri":format!("{}/jwks", state.base_url),
                "response_types_supported":["code"],
                "id_token_signing_alg_values_supported":["RS256"],
                "code_challenge_methods_supported":["S256"]
            }))
        }

        async fn response(State(state): State<CounterState>) -> impl IntoResponse {
            state.requests.fetch_add(1, Ordering::SeqCst);
            Json(serde_json::json!({"keys":[]}))
        }

        async fn unexpected(State(state): State<CounterState>, _uri: Uri) -> impl IntoResponse {
            state.requests.fetch_add(1, Ordering::SeqCst);
            axum::http::StatusCode::NOT_FOUND
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let state = CounterState {
            base_url: base_url.clone(),
            requests: requests.clone(),
        };
        let router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(response))
            .route("/token", post(response))
            .fallback(unexpected)
            .with_state(state);
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            base_url,
            requests,
            server,
        }
    }

    pub(super) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(super) fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for OAuthIssuerCounter {
    fn drop(&mut self) {
        self.server.abort();
    }
}

pub(super) struct OrgHarness {
    pub(super) state: Arc<super::startup::ServerState>,
    pub(super) database: DatabaseConnection,
    pub(super) capture: Arc<MailCapture>,
    owner: super::types::OrgContext,
    member: super::types::OrgContext,
    other_org_owner: super::types::OrgContext,
    inviter: super::types::OrgContext,
    inviter_member_id: i32,
    invitee: super::types::IdentityContext,
    wrong_recipient: super::types::IdentityContext,
    org_id: i32,
}

impl OrgHarness {
    pub(super) async fn new() -> Self {
        Self::build_on(database().await, preview_config()).await
    }

    /// Same fixture with the integration-gated gateway event path enabled, for
    /// the N2 transaction-coupled revocation-event tests.
    pub(super) async fn new_with_gateway_events() -> Self {
        let mut config = preview_config();
        config.gateway_events = true;
        Self::build_on(database().await, config).await
    }

    /// Events-enabled fixture on a caller-provided (already initialized)
    /// connection, so a guarded-PostgreSQL peer can drive the same accept flow.
    pub(super) async fn new_with_gateway_events_on(database: DatabaseConnection) -> Self {
        let mut config = preview_config();
        config.gateway_events = true;
        Self::build_on(database, config).await
    }

    async fn build_on(database: DatabaseConnection, config: super::config::Config) -> Self {
        let clock = Arc::new(TestClock::new(1_700_000_000));
        let capture = Arc::new(MailCapture::new());
        let mail = super::mail::Service::new(database.clone(), capture.clone());
        let state = Arc::new(
            super::startup::ServerState::with_mail(database.clone(), config, clock, mail).unwrap(),
        );
        let owner_user = native_account(&database, "org-owner", "owner@example.test").await;
        let inviter_user = native_account(&database, "org-inviter", "inviter@example.test").await;
        let member_user = native_account(&database, "org-member", "member@example.test").await;
        let invitee_user = native_account(&database, "org-invitee", "invitee@example.test").await;
        let wrong_user = native_account(&database, "org-wrong", "wrong@example.test").await;
        let other_owner_user =
            native_account(&database, "org-other", "other-owner@example.test").await;
        let (org_id, _) = organization(&database, owner_user, "admin").await;
        let inviter_member_id = add_membership(&database, org_id, inviter_user, "admin").await;
        add_membership(&database, org_id, member_user, "member").await;
        let (other_org, _) = organization(&database, other_owner_user, "admin").await;
        let owner = scoped_context(&database, owner_user, org_id, 1_700_000_000).await;
        let inviter = scoped_context(&database, inviter_user, org_id, 1_700_000_000).await;
        let member = scoped_context(&database, member_user, org_id, 1_700_000_000).await;
        let other_org_owner =
            scoped_context(&database, other_owner_user, other_org, 1_700_000_000).await;
        let invitee = identity_context(&database, invitee_user, 1_700_000_000).await;
        let wrong_recipient = identity_context(&database, wrong_user, 1_700_000_000).await;
        Self {
            state,
            database,
            capture,
            owner,
            member,
            other_org_owner,
            inviter,
            inviter_member_id,
            invitee,
            wrong_recipient,
            org_id,
        }
    }

    pub(super) async fn invite(&self, email: &str) -> String {
        super::org::invites::send(
            &self.state,
            &self.inviter,
            email,
            super::types::Role::Member,
            &uuid::Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
        self.capture.latest_fragment().await
    }

    pub(super) fn wrong_recipient(&self) -> super::types::IdentityContext {
        self.wrong_recipient.clone()
    }

    pub(super) fn invitee(&self) -> super::types::IdentityContext {
        self.invitee.clone()
    }

    pub(super) async fn revoke_inviter_membership(&self) {
        super::org::remove(&self.state, &self.owner, self.inviter_member_id)
            .await
            .unwrap();
    }

    pub(super) async fn accept(
        &self,
        identity: super::types::IdentityContext,
        token: &str,
    ) -> Result<i32, AuthError> {
        super::org::invites::accept(&self.state, &identity, token, self.org_id).await
    }

    pub(super) fn owner(&self) -> super::types::OrgContext {
        self.owner.clone()
    }

    pub(super) fn state(&self) -> &super::startup::ServerState {
        &self.state
    }

    pub(super) fn owner_context(&self) -> &super::types::OrgContext {
        &self.owner
    }

    pub(super) fn member_context(&self) -> &super::types::OrgContext {
        &self.member
    }

    pub(super) fn other_org_context(&self) -> &super::types::OrgContext {
        &self.other_org_owner
    }

    pub(super) fn member(&self) -> super::types::OrgContext {
        self.member.clone()
    }

    pub(super) fn other_org_owner(&self) -> super::types::OrgContext {
        self.other_org_owner.clone()
    }

    pub(super) fn org_id(&self) -> i32 {
        self.org_id
    }
}

impl MailCapture {
    pub(super) fn new() -> Self {
        Self {
            messages: Mutex::new(Vec::new()),
            fail: AtomicBool::new(false),
        }
    }

    pub(super) fn fail(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }

    pub(super) async fn len(&self) -> usize {
        self.messages.lock().await.len()
    }

    pub(super) async fn recipient(&self, index: usize) -> String {
        self.messages.lock().await[index].recipient.clone()
    }

    pub(super) async fn text(&self, index: usize) -> String {
        self.messages.lock().await[index].text.clone()
    }

    pub(super) async fn latest_fragment(&self) -> String {
        self.messages
            .lock()
            .await
            .last()
            .and_then(|message| message.text.split("#token=").nth(1))
            .and_then(|tail| tail.split_whitespace().next())
            .expect("captured magic fragment")
            .to_owned()
    }
}

#[async_trait]
impl MailSender for MailCapture {
    async fn send(&self, message: MailMessage) -> Result<(), AuthError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(AuthError::Unavailable);
        }
        self.messages.lock().await.push(message);
        Ok(())
    }
}

struct Browser {
    client: reqwest::Client,
    cookies: BTreeMap<String, String>,
}

impl Browser {
    fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            cookies: BTreeMap::new(),
        }
    }

    async fn send(&mut self, mut request: reqwest::RequestBuilder) -> reqwest::Response {
        if !self.cookies.is_empty() {
            let value = self
                .cookies
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; ");
            request = request.header(reqwest::header::COOKIE, value);
        }
        let response = request.send().await.unwrap();
        for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
            let value = value.to_str().unwrap();
            let pair = value.split(';').next().unwrap();
            let (name, value) = pair.split_once('=').unwrap();
            if value.is_empty() {
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.to_owned(), value.to_owned());
            }
        }
        response
    }
}

pub(super) struct HarnessResponse(reqwest::StatusCode);

impl HarnessResponse {
    pub(super) fn assert_status_ok(self) {
        assert_eq!(self.0, reqwest::StatusCode::OK);
    }

    pub(super) fn assert_status_unauthorized(self) {
        assert_eq!(self.0, reqwest::StatusCode::UNAUTHORIZED);
    }
}

pub(super) struct HarnessJsonResponse {
    pub(super) status: reqwest::StatusCode,
    pub(super) body: serde_json::Value,
}

pub(super) struct MagicHarness {
    base: String,
    origin: String,
    first: Browser,
    other: Browser,
    pub(super) capture: Arc<MailCapture>,
    pub(super) database: DatabaseConnection,
    _static_root: tempfile::TempDir,
    server: tokio::task::JoinHandle<()>,
}

impl MagicHarness {
    pub(super) async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}");
        let database = database().await;
        let clock = Arc::new(TestClock::new(1_700_000_000));
        let capture = Arc::new(MailCapture::new());
        let mail = super::mail::Service::new(database.clone(), capture.clone());
        let service = super::auth::magic::Service::new(
            database.clone(),
            [61; 32],
            url::Url::parse(&format!("{base}/")).unwrap(),
            clock.clone(),
            mail,
        );
        let static_root = tempfile::tempdir().unwrap();
        std::fs::create_dir(static_root.path().join("neev-auth")).unwrap();
        std::fs::write(
            static_root.path().join("neev-auth/confirm.html"),
            "scanner-safe confirmation",
        )
        .unwrap();
        std::fs::write(static_root.path().join("neev-auth/confirm.js"), "").unwrap();
        std::fs::write(static_root.path().join("neev-auth/confirm.css"), "").unwrap();
        let mut config = preview_config();
        config.origin = url::Url::parse(&format!("{base}/")).unwrap();
        config.origin_header = base.clone();
        config.static_root = static_root.path().to_owned();
        let state = Arc::new(
            super::startup::ServerState::with_magic(database.clone(), config, clock, service)
                .unwrap(),
        );
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                super::http::router(state)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            base,
            origin: format!("http://{address}"),
            first: Browser::new(),
            other: Browser::new(),
            capture,
            database,
            _static_root: static_root,
            server,
        }
    }

    async fn bootstrap(base: &str, browser: &mut Browser) -> String {
        let response = browser
            .send(browser.client.get(format!("{base}/api/neev/preauth")))
            .await;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        response.json::<serde_json::Value>().await.unwrap()["csrf"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    pub(super) async fn request(&mut self, email: &str) -> String {
        let response = self.request_response(email).await;
        assert_eq!(response.status, reqwest::StatusCode::ACCEPTED);
        assert_eq!(response.body["status"], "accepted");
        self.capture.latest_fragment().await
    }

    pub(super) async fn request_response(&mut self, email: &str) -> HarnessJsonResponse {
        let csrf = Self::bootstrap(&self.base, &mut self.first).await;
        let request = self
            .first
            .client
            .post(format!("{}/api/neev/email/login", self.base))
            .header(reqwest::header::ORIGIN, &self.origin)
            .header("x-neev-csrf", csrf)
            .json(&serde_json::json!({"email":email}));
        let response = self.first.send(request).await;
        let status = response.status();
        let body = response.json().await.unwrap();
        HarnessJsonResponse { status, body }
    }

    pub(super) async fn get_confirmation(&mut self) -> HarnessResponse {
        let request = self
            .first
            .client
            .get(format!("{}/auth/email/confirm", self.base));
        HarnessResponse(self.first.send(request).await.status())
    }

    pub(super) async fn confirm_from_other_browser(&mut self, token: &str) -> HarnessResponse {
        let csrf = Self::bootstrap(&self.base, &mut self.other).await;
        let request = self
            .other
            .client
            .post(format!("{}/api/neev/email/confirm", self.base))
            .header(reqwest::header::ORIGIN, &self.origin)
            .header("x-neev-csrf", csrf)
            .json(&serde_json::json!({"token":token}));
        HarnessResponse(self.other.send(request).await.status())
    }

    pub(super) async fn confirm(&mut self, token: &str) -> HarnessResponse {
        let csrf = Self::bootstrap(&self.base, &mut self.first).await;
        let request = self
            .first
            .client
            .post(format!("{}/api/neev/email/confirm", self.base))
            .header(reqwest::header::ORIGIN, &self.origin)
            .header("x-neev-csrf", csrf)
            .json(&serde_json::json!({"token":token}));
        HarnessResponse(self.first.send(request).await.status())
    }
}

impl Drop for MagicHarness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn isolated_postgres_database_name(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql") || parsed.fragment().is_some() {
        return None;
    }
    let encoded = parsed.path().strip_prefix('/')?;
    if encoded.is_empty() || encoded.contains('/') {
        return None;
    }
    let decoded = urlencoding::decode(encoded).ok()?.into_owned();
    (decoded == "neev_identity_test").then_some(decoded)
}

pub(super) async fn empty_database() -> DatabaseConnection {
    let mut options = ConnectOptions::new("sqlite::memory:");
    options.max_connections(1).sqlx_logging(false);
    Database::connect(options).await.unwrap()
}
pub(super) async fn database() -> DatabaseConnection {
    let db = empty_database().await;
    super::db::initialize(&db).await.unwrap();
    db
}
pub(super) fn google(subject: &str, email: &str) -> VerifiedIdentity {
    VerifiedIdentity {
        provider: Provider::Google,
        issuer: "https://accounts.google.com".into(),
        subject: subject.into(),
        verified_email: Some(email.into()),
        display_name: "Test supporter".into(),
        avatar_url: None,
    }
}

const GOOGLE_TEST_RSA_KEY: &str = r#"-----BEGIN RSA PRIVATE KEY-----
MIIEpgIBAAKCAQEA3rfpPUJ+4mv7yjLZjLxihkVxtajKtPbYgDOlvRHsun5LwwBc
3WbPrXkhO6Vk7ibDAJS0M93hoESyt3qkrDEu94+S6k6EEyUYNUyV7LtI2RSfjgtf
P1jfGQkg0AqaOVxgzOaRieoejFRtDqu17tU+YrPqaPH9tqD9SxPBrdtCsflKUT+R
BR/Hoh8I/b52bj/RTCNi90sE8X/FwfuuhE9+XlbT6A4lxZiArnKHBT//nEE9mx4I
4GEkB3jdOBY9/gXT2T2kf9Lxo3KFcC9PbWLDmaQ7Db1P7wFRzDGtOwk0HZtKzBal
wYXSMM0GFlAc73zH+cBcRmy7tKxJzJQIFNDhMwIDAQABAoIBAQCdcVNvm+dItt9b
NBE4fSAFHWhh6/3Iacvo7RnYC6jWuVtjcYk0dm6Pilak7Knp9PrbGUQ+ejb67jWM
oSVyGJ7EIPnRq0mQoyN24joGDOtVkk+swXtGs9OaxiotB6G4tQY1MzVlUlzgBPrH
aTmGqbadXeRy5cbeaPZGMaQz/BEs7+Uvm6e+iWhG7iugzW6AUOTOr9KpDCiKPb1T
7D49lPx3uumvm4FHQ6ZCzmU86VBYTbPN/+BkNuktLSbJIpLtJwnqV8ki9fWzrgL6
tvsrUcbhp36SjovhK9xR6pKnpVVJuA4u2AaeUhjnUz/S/Eh+Ul4HfVr4AE7a/PqV
DrOeys65AoGBAPA7s6mHCeQH09wGylsh137Zu8MAu6dk/UAOlRgkKtWJRI2sGPIb
05uWtAGT6NvucP8Ky4J83uLKOw9OUroGlCmfKGYvvC7h2iBo9CqK46hr1R90jewl
y15LNcJ5GJeeU4fFOQuE0AWhCzPIGKMgtdwuoaf2trkwbgTnVcaa6fTVAoGBAO1V
7suz5kDJQ9+EQwi0tVgUdBKEXfIkyROlkRcUv9Tzd1TecJuaUGveFHe7VmRZKkB6
MCzREKwHQlMXhotsGOCvoJOkXcX6ze3yCkrDAYAdTTsHO/a/vFEcQbhRXAJV4qt0
ary+Qv2Mz6KKRPYdsLYbR3LDenL6vLi2wQGvT6HnAoGBAMU5fmpzft9CveApgwAG
bJCr2MZjV6w58OAiGK1VJAVjSHC3FVPPHI7jzqxs7gOBYitAhYt/UM0IEiyitLAc
ro7tJ5GdJvWmXhm1kJ9Mn1KqFLJaUhnJfYjQXjZmjJYNWa5hcE/QnafBaxz77Z4f
QK6fagAnQCQRaoGEGpuOmvxxAoGBAIcoaudiufunIpoFrk8k6ikQ7NEmxIGDZWdk
4G2uiVhylkCktKhIhwgG3s0YyKg5BxDVBN5g6Qeu0pyTRzHbC4fZPK8X6bc3j1K0
hhvxVcdp6HN3OTTaGBpw5xRacPV7h6xwjeCS0X+nUw+TgpZObjYm7tdybydPl23x
1ZRqDAV9AoGBALpdhBOIiZJOkDcY2i44V30LD1DwWgRpEhNyWkiK9/rvgyiGZqRO
1JfBU8K9dfA5Rto9E5q9gosd2vQXmh3pAP/eD2etlZNOimT11rq6CrM9JIysUmTe
Wv3mn3vHNuQ4BT5pzeeaaQawdujfHXV57pcc97az9wQ3hCfXU1oemYqo
-----END RSA PRIVATE KEY-----"#;

pub(super) fn signed_google_fixture(mutation: &str) -> super::auth::google::SignedGoogleFixture {
    let now = 1_700_000_000;
    let client_id = "google-client".to_owned();
    let expected_nonce = "expected-nonce".to_owned();
    let key = CoreRsaPrivateSigningKey::from_pem(
        GOOGLE_TEST_RSA_KEY,
        Some(JsonWebKeyId::new("test-key".to_owned())),
    )
    .unwrap();
    let issuer = if mutation == "issuer" {
        "https://attacker.example"
    } else {
        "https://accounts.google.com"
    };
    let audience = if mutation == "audience" {
        "other-client"
    } else {
        &client_id
    };
    let nonce = if mutation == "nonce" {
        "other-nonce"
    } else {
        &expected_nonce
    };
    let subject = if mutation == "subject" {
        ""
    } else {
        "google-123"
    };
    let expiration = if mutation == "expiry" {
        now - 1
    } else {
        now + 300
    };
    let claims = CoreIdTokenClaims::new(
        IssuerUrl::new(issuer.to_owned()).unwrap(),
        vec![Audience::new(audience.to_owned())],
        Utc.timestamp_opt(expiration, 0).single().unwrap(),
        Utc.timestamp_opt(now - 1, 0).single().unwrap(),
        StandardClaims::new(SubjectIdentifier::new(subject.to_owned()))
            .set_email(Some(EndUserEmail::new("support@neev.so".to_owned())))
            .set_email_verified(Some(true)),
        Default::default(),
    )
    .set_nonce(Some(Nonce::new(nonce.to_owned())));
    let algorithm = if mutation == "algorithm" {
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha384
    } else {
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256
    };
    let mut id_token = CoreIdToken::new(claims, &key, algorithm, None, None)
        .unwrap()
        .to_string();
    if mutation == "signature" {
        let signature_start = id_token.rfind('.').unwrap() + 1;
        let replacement = if id_token.as_bytes()[signature_start] == b'A' {
            "B"
        } else {
            "A"
        };
        id_token.replace_range(signature_start..signature_start + 1, replacement);
    }
    super::auth::google::SignedGoogleFixture {
        jwks: CoreJsonWebKeySet::new(vec![key.as_verification_key()]),
        id_token,
        client_id,
        nonce: expected_nonce,
        now,
    }
}

pub(super) fn valid_google_token(
    nonce: &str,
    client_id: &str,
    now: i64,
) -> (String, serde_json::Value) {
    browser_google_token(nonce, client_id, now, "google-123", "support@neev.so")
}

pub(super) fn browser_google_token(
    nonce: &str,
    client_id: &str,
    now: i64,
    subject: &str,
    email: &str,
) -> (String, serde_json::Value) {
    let key = CoreRsaPrivateSigningKey::from_pem(
        GOOGLE_TEST_RSA_KEY,
        Some(JsonWebKeyId::new("test-key".to_owned())),
    )
    .unwrap();
    let claims = CoreIdTokenClaims::new(
        IssuerUrl::new("https://accounts.google.com".to_owned()).unwrap(),
        vec![Audience::new(client_id.to_owned())],
        Utc.timestamp_opt(now + 300, 0).single().unwrap(),
        Utc.timestamp_opt(now - 1, 0).single().unwrap(),
        StandardClaims::new(SubjectIdentifier::new(subject.to_owned()))
            .set_email(Some(EndUserEmail::new(email.to_owned())))
            .set_email_verified(Some(true)),
        Default::default(),
    )
    .set_nonce(Some(Nonce::new(nonce.to_owned())));
    let token = CoreIdToken::new(
        claims,
        &key,
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
        None,
        None,
    )
    .unwrap()
    .to_string();
    (
        token,
        serde_json::to_value(CoreJsonWebKeySet::new(vec![key.as_verification_key()])).unwrap(),
    )
}
pub(super) async fn execute(db: &DatabaseConnection, sql: &str, values: Vec<Value>) {
    db.execute(Statement::from_sql_and_values(
        db.get_database_backend(),
        sql,
        values,
    ))
    .await
    .unwrap();
}
pub(super) async fn count(db: &DatabaseConnection, table: &str) -> i64 {
    db.query_one(Statement::from_string(
        db.get_database_backend(),
        format!("SELECT COUNT(*) AS n FROM {table}"),
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "n")
    .unwrap()
}

pub(super) async fn native_account(db: &DatabaseConnection, subject: &str, email: &str) -> i32 {
    super::auth::identity::resolve(
        db,
        google(subject, email),
        super::types::SignInIntent::Login,
        1000,
    )
    .await
    .unwrap()
}

pub(super) async fn organization(db: &DatabaseConnection, user: i32, role: &str) -> (i32, i32) {
    let row = db.query_one(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO neev_organizations(owner_user_id,name,revision,disabled,created_at) VALUES($1,'Test org',1,false,1000) RETURNING id",
        vec![user.into()],
    )).await.unwrap().unwrap();
    let org: i32 = row.try_get("", "id").unwrap();
    let row = db.query_one(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO neev_memberships(org_id,user_id,role,revision,active) VALUES($1,$2,$3,1,true) RETURNING id",
        vec![org.into(), user.into(), role.into()],
    )).await.unwrap().unwrap();
    let member: i32 = row.try_get("", "id").unwrap();
    (org, member)
}

pub(super) async fn add_membership(
    db: &DatabaseConnection,
    org: i32,
    user: i32,
    role: &str,
) -> i32 {
    let row = db
        .query_one(Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO neev_memberships(org_id,user_id,role,revision,active) VALUES($1,$2,$3,1,true) RETURNING id",
            vec![org.into(), user.into(), role.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    row.try_get("", "id").unwrap()
}

pub(super) async fn identity_context(
    db: &DatabaseConnection,
    user: i32,
    now: i64,
) -> super::types::IdentityContext {
    let issued = super::auth::session::issue_identity(db, user, now)
        .await
        .unwrap();
    super::auth::session::identity(db, &issued.session, now)
        .await
        .unwrap()
}

pub(super) async fn scoped_context(
    db: &DatabaseConnection,
    user: i32,
    org: i32,
    now: i64,
) -> super::types::OrgContext {
    let issued = super::auth::session::issue_identity(db, user, now)
        .await
        .unwrap();
    let identity = super::auth::session::identity(db, &issued.session, now)
        .await
        .unwrap();
    let scoped = super::auth::session::select_organization(db, &identity, org, now)
        .await
        .unwrap();
    super::auth::session::organization(db, &scoped.session, now)
        .await
        .unwrap()
}

pub(super) fn owned_document_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    root
}

pub(super) fn preview_config() -> super::config::Config {
    let document_root = owned_document_root();
    let document_root_path = document_root.path().canonicalize().unwrap();
    let origin = url::Url::parse("http://127.0.0.1:3100/").unwrap();
    super::config::Config {
        origin: origin.clone(),
        origin_header: "http://127.0.0.1:3100".into(),
        google_callback: origin.join("auth/google/callback").unwrap(),
        github_callback: origin.join("auth/github/callback").unwrap(),
        database_url: "postgresql:///unused_test_database".into(),
        encryption_key: [7; 32],
        bind: "127.0.0.1:0".parse().unwrap(),
        trusted_proxy_ips: Vec::new(),
        static_root: std::env::current_dir().unwrap(),
        document_root: document_root_path,
        preview: true,
        providers: super::config::ProviderConfig {
            google: None,
            github: None,
            email: None,
        },
        gateway_events: false,
        runtime_enabled: false,
        realtime_enabled: true,
        engine: None,
        knowledge_root: None,
        _document_root_guard: Some(document_root),
        _knowledge_root_guard: None,
    }
}

/// `preview_config` with an owned, isolated knowledge root.
pub(super) fn knowledge_config() -> super::config::Config {
    let mut config = preview_config();
    let root = owned_document_root();
    config.knowledge_root = Some(root.path().canonicalize().unwrap());
    config._knowledge_root_guard = Some(root);
    config
}

pub(super) async fn http_fixture() -> (axum_test::TestServer, DatabaseConnection, Arc<TestClock>) {
    let database = database().await;
    let clock = Arc::new(TestClock::new(1000));
    let state = Arc::new(
        super::startup::ServerState::new(database.clone(), preview_config(), clock.clone())
            .unwrap(),
    );
    let server = axum_test::TestServer::new(super::http::router(state)).unwrap();
    (server, database, clock)
}

pub(super) async fn http_server() -> axum_test::TestServer {
    http_fixture().await.0
}
pub(super) async fn session(
    db: &DatabaseConnection,
    user: i32,
    name: &str,
    authenticated: i64,
    revision: i64,
) {
    execute(db,"INSERT INTO neev_sessions(id,token_hash,csrf_hash,user_id,account_revision,authenticated_at,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)",vec![name.into(),"1".repeat(64).into(),"2".repeat(64).into(),user.into(),revision.into(),authenticated.into(),(authenticated+3600).into()]).await;
}

/// Explicitly isolated PostgreSQL test schema; caller must close before cleanup.
pub(super) async fn postgres_database() -> (DatabaseConnection, DatabaseConnection, String) {
    let (db, admin, schema) = postgres_database_uninitialized().await;
    super::db::initialize(&db).await.unwrap();
    (db, admin, schema)
}

pub(super) async fn postgres_database_uninitialized(
) -> (DatabaseConnection, DatabaseConnection, String) {
    let url =
        std::env::var("NEEV_TEST_DATABASE_URL").expect("isolated NEEV_TEST_DATABASE_URL required");
    assert_eq!(
        isolated_postgres_database_name(&url).as_deref(),
        Some("neev_identity_test"),
        "refuse non-test PostgreSQL database"
    );
    let mut opts = ConnectOptions::new(url.clone());
    opts.max_connections(1).sqlx_logging(false);
    let admin = Database::connect(opts).await.unwrap();
    let schema = format!("neev_test_{}", uuid::Uuid::new_v4().simple());
    execute(&admin, &format!("CREATE SCHEMA {schema}"), vec![]).await;
    let mut opts = ConnectOptions::new(url);
    opts.max_connections(8)
        .sqlx_logging(false)
        .set_schema_search_path(&schema);
    let db = Database::connect(opts).await.unwrap();
    (db, admin, schema)
}
pub(super) async fn cleanup_postgres(
    db: DatabaseConnection,
    admin: DatabaseConnection,
    schema: String,
) {
    db.close().await.unwrap();
    assert!(
        schema.starts_with("neev_test_")
            && schema
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    );
    execute(&admin, &format!("DROP SCHEMA {schema} CASCADE"), vec![]).await;
    admin.close().await.unwrap();
}

pub(super) async fn wait_for_postgres_lock(admin: &DatabaseConnection, query_pattern: &str) {
    wait_for_postgres_locks(admin, query_pattern, 1).await;
}

pub(super) async fn wait_for_postgres_locks(
    admin: &DatabaseConnection,
    query_pattern: &str,
    expected: i64,
) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let row = admin
                .query_one(Statement::from_sql_and_values(
                    admin.get_database_backend(),
                    "SELECT COUNT(*) AS waiting FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE $1",
                    vec![query_pattern.into()],
                ))
                .await
                .unwrap()
                .unwrap();
            if row.try_get::<i64>("", "waiting").unwrap() >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("expected PostgreSQL query to become observably lock-waiting");
}

#[cfg(test)]
mod tests {
    use super::{isolated_postgres_database_name, preview_config};

    #[test]
    fn postgres_guard_requires_the_exact_decoded_database_name() {
        assert_eq!(
            isolated_postgres_database_name(
                "postgresql:///neev_identity_test?host=/tmp/neev-pg&user=air1"
            ),
            Some("neev_identity_test".into())
        );
        assert_eq!(
            isolated_postgres_database_name(
                "postgresql:///%6Eeev_identity_test?host=/tmp/neev-pg&user=air1"
            ),
            Some("neev_identity_test".into())
        );
        assert_eq!(
            isolated_postgres_database_name(
                "postgresql:///wrong_database?note=/neev_identity_test?host=/tmp/neev-pg"
            ),
            None
        );
    }

    #[test]
    fn preview_configs_own_distinct_document_roots() {
        let first = preview_config();
        let first_root = first.document_root.clone();
        let second = preview_config();
        let second_root = second.document_root.clone();
        assert_ne!(first_root, second_root);
        assert!(first_root.is_dir());
        assert!(second_root.is_dir());
        drop(first);
        assert!(!first_root.exists());
        assert!(second_root.is_dir());
        drop(second);
        assert!(!second_root.exists());
    }
}
