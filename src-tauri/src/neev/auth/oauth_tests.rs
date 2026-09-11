use super::{google, identity, oauth, secrets, session};
use crate::neev::test_support;
use crate::neev::{
    error::AuthError,
    types::{Provider, SignInIntent},
};
use sea_orm::{ConnectionTrait, QueryResult, Statement};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

#[derive(Default)]
struct MockIssuerState {
    base_url: Mutex<String>,
    jwks: Mutex<serde_json::Value>,
    token_response: Mutex<serde_json::Value>,
    paths: Mutex<Vec<String>>,
    token_forms: Mutex<Vec<HashMap<String, String>>>,
    fail_discovery: AtomicBool,
    token_barriers: Mutex<Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>>,
}

struct MockIssuer {
    base_url: String,
    state: Arc<MockIssuerState>,
    task: tokio::task::JoinHandle<()>,
}

impl MockIssuer {
    async fn start(jwks: serde_json::Value) -> Self {
        use axum::{
            extract::{Form, State},
            http::Uri,
            response::IntoResponse,
            routing::{get, post},
            Json, Router,
        };

        async fn discovery(State(state): State<Arc<MockIssuerState>>) -> impl IntoResponse {
            state
                .paths
                .lock()
                .unwrap()
                .push("/.well-known/openid-configuration".into());
            if state.fail_discovery.load(Ordering::SeqCst) {
                return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            let base = state.base_url.lock().unwrap().clone();
            Json(serde_json::json!({
                "issuer":"https://accounts.google.com",
                "authorization_endpoint":format!("{base}/authorize"),
                "token_endpoint":format!("{base}/token"),
                "jwks_uri":format!("{base}/jwks"),
                "response_types_supported":["code"],
                "id_token_signing_alg_values_supported":["RS256"],
                "code_challenge_methods_supported":["S256"]
            }))
            .into_response()
        }
        async fn jwks_handler(State(state): State<Arc<MockIssuerState>>) -> impl IntoResponse {
            state.paths.lock().unwrap().push("/jwks".into());
            Json(state.jwks.lock().unwrap().clone())
        }
        async fn token(
            State(state): State<Arc<MockIssuerState>>,
            Form(form): Form<HashMap<String, String>>,
        ) -> impl IntoResponse {
            state.paths.lock().unwrap().push("/token".into());
            state.token_forms.lock().unwrap().push(form);
            let barriers = state.token_barriers.lock().unwrap().clone();
            if let Some((entered, release)) = barriers {
                entered.wait().await;
                release.wait().await;
            }
            Json(state.token_response.lock().unwrap().clone())
        }
        async fn unexpected(
            State(state): State<Arc<MockIssuerState>>,
            uri: Uri,
        ) -> impl IntoResponse {
            state.paths.lock().unwrap().push(uri.path().to_owned());
            axum::http::StatusCode::NOT_FOUND
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(MockIssuerState {
            base_url: Mutex::new(base_url.clone()),
            jwks: Mutex::new(jwks),
            ..Default::default()
        });
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks_handler))
            .route("/token", post(token))
            .fallback(unexpected)
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base_url,
            state,
            task,
        }
    }
}

impl Drop for MockIssuer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[test]
fn google_verifier_rejects_untrusted_claims() {
    for mutation in [
        "issuer",
        "audience",
        "nonce",
        "expiry",
        "signature",
        "algorithm",
        "subject",
    ] {
        let fixture = test_support::signed_google_fixture(mutation);
        assert!(
            google::verify_fixture(&fixture).is_err(),
            "accepted {mutation} mutation"
        );
    }
}

#[tokio::test]
async fn google_protocol_uses_discovery_pkce_minimal_scopes_and_no_roster_calls() {
    let (_initial_id_token, jwks) =
        test_support::valid_google_token("placeholder-nonce", "google-client", 1_700_000_000);
    let issuer = MockIssuer::start(jwks).await;
    let google = google::Google::for_test(
        "google-client".into(),
        "google-secret".into(),
        url::Url::parse("https://app.neev.so/auth/google/callback").unwrap(),
        &issuer.base_url,
    )
    .unwrap();
    let prepared = google.authorization("n1.oauth.test-state").unwrap();
    let authorization = url::Url::parse(&prepared.url).unwrap();
    let query: HashMap<_, _> = authorization.query_pairs().into_owned().collect();
    assert_eq!(authorization.path(), "/authorize");
    assert_eq!(query.get("scope").unwrap(), "openid email profile");
    assert_eq!(query.get("response_type").unwrap(), "code");
    assert_eq!(query.get("code_challenge_method").unwrap(), "S256");
    assert_eq!(query.get("state").unwrap(), "n1.oauth.test-state");
    assert!(!query.get("code_challenge").unwrap().is_empty());

    let (id_token, _) =
        test_support::valid_google_token(&prepared.nonce, "google-client", 1_700_000_000);
    *issuer.state.token_response.lock().unwrap() = serde_json::json!({
        "access_token":"discard-this-token",
        "token_type":"Bearer",
        "expires_in":300,
        "id_token":id_token
    });
    let proof = google
        .exchange(
            "authorization-code",
            &prepared.pkce_verifier,
            &prepared.nonce,
            1_700_000_000,
        )
        .await
        .unwrap();
    assert_eq!(proof.subject, "google-123");
    assert_eq!(proof.verified_email.as_deref(), Some("support@neev.so"));
    let forms = issuer.state.token_forms.lock().unwrap();
    assert_eq!(forms.len(), 1);
    assert_eq!(forms[0].get("code").unwrap(), "authorization-code");
    assert_eq!(
        forms[0].get("code_verifier").unwrap(),
        &prepared.pkce_verifier
    );
    assert_eq!(forms[0].get("grant_type").unwrap(), "authorization_code");
    assert_eq!(forms[0].len(), 6);
    assert_eq!(
        *issuer.state.paths.lock().unwrap(),
        ["/.well-known/openid-configuration", "/jwks", "/token"]
    );
}

fn response_cookies(response: &axum_test::TestResponse) -> String {
    response
        .headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().split(';').next().unwrap())
        .collect::<Vec<_>>()
        .join("; ")
}

#[tokio::test]
async fn google_http_callback_accepts_metadata_claims_once_and_does_not_leak_query() {
    let (_token, jwks) = test_support::valid_google_token("unused", "google-client", 1_700_000_000);
    let issuer = MockIssuer::start(jwks).await;
    let database = test_support::database().await;
    let clock = Arc::new(test_support::TestClock::new(1_700_000_000));
    let google = google::Google::for_test(
        "google-client".into(),
        "google-secret".into(),
        url::Url::parse("http://127.0.0.1:3100/auth/google/callback").unwrap(),
        &issuer.base_url,
    )
    .unwrap();
    let service = oauth::Service::with_google(database.clone(), [7; 32], clock.clone(), google);
    let state = Arc::new(crate::neev::startup::ServerState::with_oauth(
        database,
        test_support::preview_config(),
        clock,
        service,
    ));
    let server = axum_test::TestServer::new(crate::neev::http::router(state)).unwrap();

    let preauth = server.get("/api/neev/preauth").await;
    let csrf = preauth.json::<serde_json::Value>()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let cookies = response_cookies(&preauth);
    let begun = server
        .post("/api/neev/auth/google/login")
        .add_header(axum::http::header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(axum::http::header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &csrf)
        .json(&serde_json::json!({}))
        .await;
    begun.assert_status_ok();
    let refreshed = response_cookies(&begun);
    assert!(refreshed.contains("neev_preview_browser="));
    assert!(refreshed.contains("neev_preview_preauth_csrf="));
    assert!(refreshed.contains(cookies.split("; ").next().unwrap()));
    let authorization_url = begun.json::<serde_json::Value>()["authorizationUrl"]
        .as_str()
        .unwrap()
        .to_owned();
    let authorization = url::Url::parse(&authorization_url).unwrap();
    let query: HashMap<_, _> = authorization.query_pairs().into_owned().collect();
    let callback_state = query["state"].clone();
    let nonce = query["nonce"].clone();
    let (id_token, _) = test_support::valid_google_token(&nonce, "google-client", 1_700_000_000);
    *issuer.state.token_response.lock().unwrap() = serde_json::json!({
        "access_token":"discard-this-token",
        "token_type":"Bearer",
        "id_token":id_token
    });

    let callback = server
        .get(&format!(
            "/auth/google/callback?state={}&code=provider-code&scope=openid%20email%20profile&authuser=0&hd=example.test&prompt=consent&future_google_extension=opaque-value",
            urlencoding::encode(&callback_state)
        ))
        .add_header(axum::http::header::COOKIE, &cookies)
        .add_header(axum::http::header::ACCEPT, "text/html")
        .await;
    callback.assert_status(axum::http::StatusCode::SEE_OTHER);
    assert_eq!(callback.header(axum::http::header::LOCATION), "/onboarding");
    assert_eq!(
        callback
            .headers()
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .count(),
        2
    );
    assert_eq!(callback.header("referrer-policy"), "no-referrer");
    assert!(!format!("{:?}", callback.headers()).contains("provider-code"));
    server
        .get("/auth/google/callback?state=malformed&code=provider-code&future_google_extension=opaque-value")
        .add_header(axum::http::header::COOKIE, &cookies)
        .await
        .assert_status_unauthorized();
    server
        .get(&format!(
            "/auth/google/callback?state={}&code=provider-code",
            urlencoding::encode(&callback_state)
        ))
        .add_header(axum::http::header::COOKIE, &cookies)
        .await
        .assert_status_unauthorized();
}

#[tokio::test]
async fn google_http_callback_conflicts_preserve_api_errors_and_offer_safe_browser_recovery() {
    use axum::http::{header, StatusCode};
    let now = 1_700_000_000;
    let database = test_support::database().await;
    test_support::native_account(&database, "original-account", "support@neev.so").await;
    let (service, issuer) = oauth_service_fixture(database.clone(), now).await;
    let state = Arc::new(crate::neev::startup::ServerState::with_oauth(
        database.clone(),
        test_support::preview_config(),
        Arc::new(test_support::TestClock::new(now)),
        Arc::try_unwrap(service).ok().unwrap(),
    ));
    let server = axum_test::TestServer::new(crate::neev::http::router(state.clone())).unwrap();
    let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
    for accept in ["application/json", "text/html"] {
        let begun = state
            .oauth
            .begin(Provider::Google, SignInIntent::Login, &browser)
            .await
            .unwrap();
        let values = authorization_values(&begun);
        let (token, _) = test_support::valid_google_token(&values["nonce"], "google-client", now);
        *issuer.state.token_response.lock().unwrap() = serde_json::json!({
            "access_token":"discard-provider-secret", "token_type":"Bearer", "id_token":token
        });
        let callback = server
            .get(&format!(
                "/auth/google/callback?state={}&code=synthetic-code&redirect=https://evil.test",
                urlencoding::encode(&values["state"])
            ))
            .add_header(header::COOKIE, format!("neev_preview_browser={browser}"))
            .add_header(header::ACCEPT, accept)
            .await;
        if accept == "text/html" {
            callback.assert_status(StatusCode::SEE_OTHER);
            assert_eq!(
                callback.header(header::LOCATION),
                "/login?auth_error=account_conflict"
            );
        } else {
            callback.assert_status(StatusCode::CONFLICT);
            callback.assert_json(&serde_json::json!({"code":"conflict"}));
            assert!(callback.headers().get(header::LOCATION).is_none());
        }
        assert_eq!(callback.header(header::CACHE_CONTROL), "no-store");
        assert_eq!(callback.header("referrer-policy"), "no-referrer");
        assert!(callback.headers().get(header::SET_COOKIE).is_none());
        let visible = format!("{:?} {}", callback.headers(), callback.text());
        for secret in [
            &values["state"],
            "synthetic-code",
            "discard-provider-secret",
            "support@neev.so",
            "evil.test",
        ] {
            assert!(!visible.contains(secret));
        }
    }
    assert_eq!(test_support::count(&database, "neev_accounts").await, 1);
    assert_eq!(test_support::count(&database, "neev_identities").await, 1);
    assert_eq!(test_support::count(&database, "neev_sessions").await, 0);
}

#[tokio::test]
async fn oauth_begin_reserves_browser_quota_without_provider_traffic() {
    let database = test_support::database().await;
    let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
    let (service, issuer) = oauth_service_fixture(database.clone(), 1_700_000_000).await;
    issuer.state.fail_discovery.store(true, Ordering::SeqCst);

    for _ in 0..5 {
        service
            .begin(Provider::Google, SignInIntent::Login, &browser)
            .await
            .unwrap();
    }
    assert_eq!(
        service
            .begin(Provider::Google, SignInIntent::Login, &browser)
            .await
            .err(),
        Some(AuthError::Limited)
    );
    assert!(issuer.state.paths.lock().unwrap().is_empty());
    assert_eq!(
        test_support::count(&database, "neev_oauth_attempts").await,
        5
    );
}

#[tokio::test]
async fn oauth_attempt_is_browser_provider_and_replay_bound_and_scrubs_ciphertext() {
    let database = test_support::database().await;
    let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
    let wrong_browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
    let state = oauth::reserve_attempt(
        &database,
        &[9; 32],
        Provider::Google,
        SignInIntent::Login,
        &browser,
        "pkce-verifier",
        "oidc-nonce",
        1000,
    )
    .await
    .unwrap();

    assert_eq!(
        oauth::claim_attempt(
            &database,
            &[9; 32],
            Provider::Github,
            &state,
            &browser,
            1001,
        )
        .await
        .err(),
        Some(AuthError::Unauthorized)
    );
    assert_eq!(
        oauth::claim_attempt(
            &database,
            &[9; 32],
            Provider::Google,
            &state,
            &wrong_browser,
            1001,
        )
        .await
        .err(),
        Some(AuthError::Unauthorized)
    );
    let claimed = oauth::claim_attempt(
        &database,
        &[9; 32],
        Provider::Google,
        &state,
        &browser,
        1001,
    )
    .await
    .unwrap();
    assert_eq!(claimed.pkce_verifier, "pkce-verifier");
    assert_eq!(claimed.nonce, "oidc-nonce");
    assert!(matches!(claimed.intent, SignInIntent::Login));
    assert_eq!(
        oauth::claim_attempt(
            &database,
            &[9; 32],
            Provider::Google,
            &state,
            &browser,
            1002,
        )
        .await
        .err(),
        Some(AuthError::Unauthorized)
    );

    let row: QueryResult = database
        .query_one(Statement::from_string(
            database.get_database_backend(),
            "SELECT exchange_enc,consumed_at FROM neev_oauth_attempts".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<String>("", "exchange_enc").unwrap(), "");
    assert_eq!(row.try_get::<i64>("", "consumed_at").unwrap(), 1001);
}

#[tokio::test]
async fn oauth_attempt_limits_are_durable_and_consumed_rows_still_count() {
    let database = test_support::database().await;
    let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
    for _ in 0..5 {
        let state = oauth::reserve_attempt(
            &database,
            &[8; 32],
            Provider::Google,
            SignInIntent::Login,
            &browser,
            "pkce",
            "nonce",
            1000,
        )
        .await
        .unwrap();
        oauth::claim_attempt(
            &database,
            &[8; 32],
            Provider::Google,
            &state,
            &browser,
            1001,
        )
        .await
        .unwrap();
    }
    assert_eq!(
        oauth::reserve_attempt(
            &database,
            &[8; 32],
            Provider::Google,
            SignInIntent::Login,
            &browser,
            "pkce",
            "nonce",
            1002,
        )
        .await
        .err(),
        Some(AuthError::Limited)
    );
    assert!(oauth::reserve_attempt(
        &database,
        &[8; 32],
        Provider::Google,
        SignInIntent::Login,
        &browser,
        "pkce",
        "nonce",
        1901,
    )
    .await
    .is_ok());
}

#[tokio::test]
async fn oauth_global_and_active_attempt_ceilings_bound_database_growth() {
    let global_database = test_support::database().await;
    for _ in 0..60 {
        let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
        oauth::reserve_attempt(
            &global_database,
            &[6; 32],
            Provider::Google,
            SignInIntent::Login,
            &browser,
            "pkce",
            "nonce",
            1000,
        )
        .await
        .unwrap();
    }
    let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
    assert_eq!(
        oauth::reserve_attempt(
            &global_database,
            &[6; 32],
            Provider::Google,
            SignInIntent::Login,
            &browser,
            "pkce",
            "nonce",
            1000,
        )
        .await
        .err(),
        Some(AuthError::Limited)
    );

    let active_database = test_support::database().await;
    test_support::execute(
        &active_database,
        "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n<1000) INSERT INTO neev_oauth_attempts(state_hash,browser_hash,provider,intent,exchange_enc,expires_at) SELECT printf('%064x',n),printf('%064x',n+2000),'google','login','retained',1100 FROM seq",
        vec![],
    )
    .await;
    let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
    assert_eq!(
        oauth::reserve_attempt(
            &active_database,
            &[6; 32],
            Provider::Google,
            SignInIntent::Login,
            &browser,
            "pkce",
            "nonce",
            1000,
        )
        .await
        .err(),
        Some(AuthError::Limited)
    );
}

async fn oauth_service_fixture(
    database: sea_orm::DatabaseConnection,
    now: i64,
) -> (Arc<oauth::Service>, MockIssuer) {
    let (_token, jwks) = test_support::valid_google_token("unused", "google-client", now);
    let issuer = MockIssuer::start(jwks).await;
    let google = google::Google::for_test(
        "google-client".into(),
        "google-secret".into(),
        url::Url::parse("http://127.0.0.1:3100/auth/google/callback").unwrap(),
        &issuer.base_url,
    )
    .unwrap();
    let clock = Arc::new(test_support::TestClock::new(now));
    (
        Arc::new(oauth::Service::with_google(
            database, [5; 32], clock, google,
        )),
        issuer,
    )
}

fn authorization_values(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

#[tokio::test]
async fn provider_denial_claims_state_and_matching_email_never_merges_accounts() {
    let database = test_support::database().await;
    let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
    let (service, issuer) = oauth_service_fixture(database.clone(), 1_700_000_000).await;
    let denied_url = service
        .begin(Provider::Google, SignInIntent::Login, &browser)
        .await
        .unwrap();
    let denied_state = authorization_values(&denied_url)["state"].clone();
    assert_eq!(
        service
            .complete(
                Provider::Google,
                &denied_state,
                oauth::CallbackResult::Denied,
                &browser,
            )
            .await
            .err(),
        Some(AuthError::Unauthorized)
    );
    assert_eq!(
        service
            .complete(
                Provider::Google,
                &denied_state,
                oauth::CallbackResult::Code("later-code".into()),
                &browser,
            )
            .await
            .err(),
        Some(AuthError::Unauthorized)
    );

    test_support::native_account(&database, "existing-subject", "support@neev.so").await;
    let login_url = service
        .begin(Provider::Google, SignInIntent::Login, &browser)
        .await
        .unwrap();
    let values = authorization_values(&login_url);
    let (token, _) =
        test_support::valid_google_token(&values["nonce"], "google-client", 1_700_000_000);
    *issuer.state.token_response.lock().unwrap() = serde_json::json!({
        "access_token":"discard",
        "token_type":"Bearer",
        "id_token":token
    });
    assert_eq!(
        service
            .complete(
                Provider::Google,
                &values["state"],
                oauth::CallbackResult::Code("code".into()),
                &browser,
            )
            .await
            .err(),
        Some(AuthError::Conflict)
    );
    assert_eq!(test_support::count(&database, "neev_accounts").await, 1);
    assert_eq!(test_support::count(&database, "neev_identities").await, 1);
}

#[tokio::test]
#[ignore = "requires explicit isolated PostgreSQL"]
async fn postgres_oauth_link_rechecks_mutations_committed_while_token_exchange_waits() {
    use sea_orm::{ConnectOptions, Database};
    use std::time::Duration;

    for mutation in ["membership", "session"] {
        let (database, admin, schema) = test_support::postgres_database().await;
        let user =
            test_support::native_account(&database, "existing-subject", "support@neev.so").await;
        let identity_secrets = session::issue_identity(&database, user, 1_700_000_000)
            .await
            .unwrap();
        let identity_context =
            session::identity(&database, &identity_secrets.session, 1_700_000_001)
                .await
                .unwrap();
        let (context, update, values, expected) = if mutation == "membership" {
            let (org, _) = test_support::organization(&database, user, "admin").await;
            let scoped =
                session::select_organization(&database, &identity_context, org, 1_700_000_002)
                    .await
                    .unwrap();
            let context = session::identity(&database, &scoped.session, 1_700_000_003)
                .await
                .unwrap();
            (
                context,
                "UPDATE neev_memberships SET active=false,revision=revision+1 WHERE org_id=$1 AND user_id=$2",
                vec![org.into(), user.into()],
                AuthError::Forbidden,
            )
        } else {
            let session_id = identity_context.session_id.clone();
            (
                identity_context,
                "UPDATE neev_sessions SET revoked_at=$1 WHERE id=$2",
                vec![1_700_000_004_i64.into(), session_id.into()],
                AuthError::Unauthorized,
            )
        };
        let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
        let (service, issuer) = oauth_service_fixture(database.clone(), 1_700_000_004).await;
        let url = service
            .begin(
                Provider::Google,
                SignInIntent::Link {
                    user_id: context.user_id,
                    session_id: context.session_id,
                    auth_revision: context.auth_revision,
                },
                &browser,
            )
            .await
            .unwrap();
        let authorization = authorization_values(&url);
        let (token, _) = test_support::valid_google_token(
            &authorization["nonce"],
            "google-client",
            1_700_000_004,
        );
        *issuer.state.token_response.lock().unwrap() = serde_json::json!({
            "access_token":"discard",
            "token_type":"Bearer",
            "id_token":token
        });
        let entered = Arc::new(tokio::sync::Barrier::new(2));
        let release = Arc::new(tokio::sync::Barrier::new(2));
        *issuer.state.token_barriers.lock().unwrap() = Some((entered.clone(), release.clone()));

        let completing = {
            let service = service.clone();
            let state = authorization["state"].clone();
            let browser = browser.clone();
            tokio::spawn(async move {
                service
                    .complete(
                        Provider::Google,
                        &state,
                        oauth::CallbackResult::Code("code".into()),
                        &browser,
                    )
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(3), entered.wait())
            .await
            .expect("token endpoint should become observably blocked");

        let mut options = ConnectOptions::new(
            std::env::var("NEEV_TEST_DATABASE_URL")
                .expect("isolated NEEV_TEST_DATABASE_URL required"),
        );
        options
            .max_connections(1)
            .sqlx_logging(false)
            .set_schema_search_path(&schema);
        let writer = Database::connect(options).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            let tx = crate::neev::db::begin(&writer).await.unwrap();
            crate::neev::db::execute(&tx, update, values).await.unwrap();
            tx.commit().await.unwrap();
        })
        .await
        .expect("authorization mutation must commit while provider response is blocked");
        release.wait().await;

        assert_eq!(completing.await.unwrap().err(), Some(expected));
        assert_eq!(test_support::count(&database, "neev_identities").await, 1);
        writer.close().await.unwrap();
        drop(service);
        drop(issuer);
        test_support::cleanup_postgres(database, admin, schema).await;
    }
}

#[tokio::test]
async fn oauth_link_preserves_session_and_rejects_prior_revocation() {
    for revoke in [false, true] {
        let database = test_support::database().await;
        let user =
            test_support::native_account(&database, "existing-subject", "support@neev.so").await;
        let issued = session::issue_identity(&database, user, 1_700_000_000)
            .await
            .unwrap();
        let context = session::identity(&database, &issued.session, 1_700_000_001)
            .await
            .unwrap();
        let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
        let (service, issuer) = oauth_service_fixture(database.clone(), 1_700_000_002).await;
        let url = service
            .begin(
                Provider::Google,
                SignInIntent::Link {
                    user_id: context.user_id,
                    session_id: context.session_id.clone(),
                    auth_revision: context.auth_revision,
                },
                &browser,
            )
            .await
            .unwrap();
        let values = authorization_values(&url);
        let (token, _) =
            test_support::valid_google_token(&values["nonce"], "google-client", 1_700_000_002);
        *issuer.state.token_response.lock().unwrap() = serde_json::json!({
            "access_token":"discard",
            "token_type":"Bearer",
            "id_token":token
        });
        if revoke {
            test_support::execute(
                &database,
                "UPDATE neev_sessions SET revoked_at=$1 WHERE id=$2",
                vec![1_700_000_002_i64.into(), context.session_id.clone().into()],
            )
            .await;
        }
        let result = service
            .complete(
                Provider::Google,
                &values["state"],
                oauth::CallbackResult::Code("code".into()),
                &browser,
            )
            .await;
        if revoke {
            assert_eq!(result.err(), Some(AuthError::Unauthorized));
            assert_eq!(test_support::count(&database, "neev_identities").await, 1);
        } else {
            assert!(matches!(result, Ok(oauth::Completion::Linked)));
            assert!(session::identity(&database, &issued.session, 1_700_000_003)
                .await
                .is_ok());
            assert_eq!(test_support::count(&database, "neev_identities").await, 2);
            assert_eq!(test_support::count(&database, "neev_sessions").await, 1);
        }
    }
}

#[tokio::test]
async fn unconfigured_google_is_explicitly_unavailable_and_github_stays_unmounted() {
    let server = test_support::http_server().await;
    let preauth = server.get("/api/neev/preauth").await;
    let csrf = preauth.json::<serde_json::Value>()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let cookies = response_cookies(&preauth);
    server
        .post("/api/neev/auth/google/login")
        .add_header(axum::http::header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(axum::http::header::COOKIE, cookies)
        .add_header("x-neev-csrf", csrf)
        .json(&serde_json::json!({}))
        .await
        .assert_status(axum::http::StatusCode::SERVICE_UNAVAILABLE);
    server
        .post("/api/neev/auth/github/login")
        .await
        .assert_status_not_found();
    server
        .get("/api/workos/capabilities")
        .await
        .assert_json(&serde_json::json!({
            "version":1,
            "identityMode":"native",
            "runtimeEnabled":false,
            "providers":{"google":false,"email":false,"github":false},
            "catalog":{
                "nodeKinds":["project","folder"],
                "repositoryDefaults":false,
                "chats":false
            }
        }));
}

#[tokio::test]
async fn linking_rechecks_scoped_membership_and_rolls_back_identity_insert() {
    let database = test_support::database().await;
    let user = test_support::native_account(&database, "owner", "owner@neev.so").await;
    let (org, _) = test_support::organization(&database, user, "admin").await;
    let issued = session::issue_identity(&database, user, 1000)
        .await
        .unwrap();
    let identity_context = session::identity(&database, &issued.session, 1001)
        .await
        .unwrap();
    let scoped = session::select_organization(&database, &identity_context, org, 1002)
        .await
        .unwrap();
    let scoped_context = session::identity(&database, &scoped.session, 1003)
        .await
        .unwrap();
    test_support::execute(
        &database,
        "UPDATE neev_memberships SET active=false,revision=revision+1 WHERE org_id=$1 AND user_id=$2",
        vec![org.into(), user.into()],
    )
    .await;

    assert_eq!(
        identity::resolve(
            &database,
            test_support::google("new-subject", "owner@neev.so"),
            SignInIntent::Link {
                user_id: scoped_context.user_id,
                session_id: scoped_context.session_id,
                auth_revision: scoped_context.auth_revision,
            },
            1004,
        )
        .await
        .err(),
        Some(AuthError::Forbidden)
    );
    assert_eq!(test_support::count(&database, "neev_identities").await, 1);
}

#[tokio::test]
#[ignore = "requires explicit isolated PostgreSQL"]
async fn postgres_oauth_reservation_serializes_browser_quota() {
    let (database, admin, schema) = test_support::postgres_database().await;
    let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
    for _ in 0..4 {
        oauth::reserve_attempt(
            &database,
            &[7; 32],
            Provider::Google,
            SignInIntent::Login,
            &browser,
            "pkce",
            "nonce",
            1000,
        )
        .await
        .unwrap();
    }
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let db = database.clone();
        let browser = browser.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            oauth::reserve_attempt(
                &db,
                &[7; 32],
                Provider::Google,
                SignInIntent::Login,
                &browser,
                "pkce",
                "nonce",
                1000,
            )
            .await
        }));
    }
    barrier.wait().await;
    let results = futures::future::join_all(tasks).await;
    assert_eq!(
        results
            .iter()
            .filter(|result| result.as_ref().unwrap().is_ok())
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| result.as_ref().unwrap().as_ref().err() == Some(&AuthError::Limited))
            .count(),
        1
    );
    test_support::cleanup_postgres(database, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires explicit isolated PostgreSQL"]
async fn postgres_duplicate_callback_claims_once_before_provider_exchange() {
    let (database, admin, schema) = test_support::postgres_database().await;
    let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
    let (service, issuer) = oauth_service_fixture(database.clone(), 1_700_000_000).await;
    let url = service
        .begin(Provider::Google, SignInIntent::Login, &browser)
        .await
        .unwrap();
    let values = authorization_values(&url);
    let (token, _) =
        test_support::valid_google_token(&values["nonce"], "google-client", 1_700_000_000);
    *issuer.state.token_response.lock().unwrap() = serde_json::json!({
        "access_token":"discard",
        "token_type":"Bearer",
        "id_token":token
    });
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let service = service.clone();
        let barrier = barrier.clone();
        let state = values["state"].clone();
        let browser = browser.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            service
                .complete(
                    Provider::Google,
                    &state,
                    oauth::CallbackResult::Code("same-code".into()),
                    &browser,
                )
                .await
        }));
    }
    barrier.wait().await;
    let results = futures::future::join_all(tasks).await;
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result.as_ref().unwrap(), Ok(oauth::Completion::Login(_))))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(
                |result| result.as_ref().unwrap().as_ref().err() == Some(&AuthError::Unauthorized)
            )
            .count(),
        1
    );
    assert_eq!(issuer.state.token_forms.lock().unwrap().len(), 1);
    drop(service);
    drop(issuer);
    test_support::cleanup_postgres(database, admin, schema).await;
}
