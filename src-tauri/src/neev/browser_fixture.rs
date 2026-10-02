//! Synthetic local issuer and actual native HTTP routers; never compiled into a server.
use super::{
    auth::{google, magic, oauth},
    clock::{Clock, SystemClock},
    config::Config,
    content::BlobStore,
    http, mail,
    startup::ServerState,
    test_support,
};
use axum::{
    extract::{Form, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{self, Read},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, task::JoinHandle};

const CLIENT: &str = "synthetic-browser-client";
const SECRET: &str = "synthetic-browser-secret";
const PEOPLE: [(&str, &str); 3] = [
    ("synthetic-founder", "support@neev.so"),
    ("synthetic-recipient", "recipient2@example.test"),
    ("synthetic-outsider", "outsider@example.test"),
];

struct Attempt {
    callback: String,
    state: String,
    nonce: String,
    challenge: String,
    created: Instant,
}
struct Grant {
    attempt: Attempt,
    person: usize,
}
struct IssuerState {
    origin: String,
    callbacks: [String; 2],
    attempts: Mutex<HashMap<String, Attempt>>,
    grants: Mutex<HashMap<String, Grant>>,
    capture: Arc<test_support::MailCapture>,
    paths: Mutex<Vec<String>>,
}

fn page(body: String) -> Response {
    let mut response = Html(format!("<!doctype html><html lang=en><meta name=viewport content='width=device-width,initial-scale=1'><title>Synthetic Neev fixture</title><h1>Synthetic Neev fixture</h1><p>Local test identities and captured mail only. No Google account or delivered email.</p>{body}</html>")).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    response.headers_mut().insert(
        "content-security-policy",
        "default-src 'none'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
            .parse()
            .unwrap(),
    );
    response
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

async fn authorize(
    State(s): State<Arc<IssuerState>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    // Native credentials must not reach this separate host. Unrelated localhost
    // preferences can already exist in the user's browser and are harmless here.
    let native_names = [
        http::cookies::CookieNames::for_preview(true),
        http::cookies::CookieNames::for_preview(false),
    ];
    if headers.get_all(header::COOKIE).iter().any(|raw| {
        raw.as_bytes().split(|byte| *byte == b';').any(|part| {
            let name = part
                .split(|byte| *byte == b'=')
                .next()
                .unwrap_or_default()
                .trim_ascii();
            native_names.iter().any(|names| {
                [names.session, names.csrf, names.browser, names.preauth_csrf]
                    .iter()
                    .any(|native| name == native.as_bytes())
            })
        })
    }) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(callback) = q.get("redirect_uri").filter(|v| s.callbacks.contains(v)) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if q.get("client_id").map(String::as_str) != Some(CLIENT)
        || q.get("response_type").map(String::as_str) != Some("code")
        || q.get("scope").map(String::as_str) != Some("openid email profile")
        || q.get("code_challenge_method").map(String::as_str) != Some("S256")
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let field = |name| {
        q.get(name)
            .filter(|v| !v.is_empty() && v.len() <= 512)
            .cloned()
    };
    let (Some(state), Some(nonce), Some(challenge)) =
        (field("state"), field("nonce"), field("code_challenge"))
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let id = uuid::Uuid::new_v4().to_string();
    let mut attempts = s.attempts.lock().unwrap();
    attempts.retain(|_, a| a.created.elapsed() < Duration::from_secs(600));
    if attempts.len() >= 100 {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    attempts.insert(
        id.clone(),
        Attempt {
            callback: callback.clone(),
            state,
            nonce,
            challenge,
            created: Instant::now(),
        },
    );
    s.paths.lock().unwrap().push("/authorize".into());
    Redirect::to(&format!("/choose/{id}")).into_response()
}

async fn choose(State(s): State<Arc<IssuerState>>, Path(id): Path<String>) -> Response {
    let callback_origin = {
        let attempts = s.attempts.lock().unwrap();
        let Some(attempt) = attempts.get(&id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        // authorize stores only exact registered callbacks. Permit that origin
        // through the callback-to-workspace redirect chain, never query input.
        url::Url::parse(&attempt.callback)
            .unwrap()
            .origin()
            .ascii_serialization()
    };
    let buttons = PEOPLE
        .iter()
        .enumerate()
        .map(|(i, (_, email))| {
            format!("<p><button name=person value={i}>Continue as {email}</button></p>")
        })
        .collect::<String>();
    let mut response = page(format!(
        "<form method=post action='/choose/{}'>{buttons}</form>",
        escape(&id)
    ));
    response.headers_mut().insert(
        "content-security-policy",
        format!("default-src 'none'; form-action 'self' {callback_origin}; frame-ancestors 'none'; base-uri 'none'")
            .parse()
            .unwrap(),
    );
    response
}

async fn approve(
    State(s): State<Arc<IssuerState>>,
    Path(id): Path<String>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let Some(person) = form
        .get("person")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v < PEOPLE.len())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(attempt) = s.attempts.lock().unwrap().remove(&id) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if attempt.created.elapsed() >= Duration::from_secs(600) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let code = uuid::Uuid::new_v4().to_string();
    let mut callback = url::Url::parse(&attempt.callback).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &attempt.state)
        .append_pair("code", &code);
    let mut grants = s.grants.lock().unwrap();
    grants.retain(|_, g| g.attempt.created.elapsed() < Duration::from_secs(600));
    if grants.len() >= 100 {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    grants.insert(code, Grant { attempt, person });
    Redirect::to(callback.as_str()).into_response()
}

async fn discovery(State(s): State<Arc<IssuerState>>) -> Json<serde_json::Value> {
    s.paths.lock().unwrap().push("/discovery".into());
    Json(
        serde_json::json!({"issuer":"https://accounts.google.com", "authorization_endpoint":format!("{}/authorize",s.origin), "token_endpoint":format!("{}/token",s.origin), "jwks_uri":format!("{}/jwks",s.origin), "response_types_supported":["code"], "id_token_signing_alg_values_supported":["RS256"], "code_challenge_methods_supported":["S256"]}),
    )
}
async fn jwks(State(s): State<Arc<IssuerState>>) -> Json<serde_json::Value> {
    s.paths.lock().unwrap().push("/jwks".into());
    Json(test_support::valid_google_token("unused", CLIENT, SystemClock.now_unix()).1)
}
async fn token(
    State(s): State<Arc<IssuerState>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    s.paths.lock().unwrap().push("/token".into());
    let Some(grant) = form
        .get("code")
        .and_then(|code| s.grants.lock().unwrap().remove(code))
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let challenge = form
        .get("code_verifier")
        .map(|v| URL_SAFE_NO_PAD.encode(Sha256::digest(v.as_bytes())));
    if form.get("client_id").map(String::as_str) != Some(CLIENT)
        || form.get("client_secret").map(String::as_str) != Some(SECRET)
        || form.get("grant_type").map(String::as_str) != Some("authorization_code")
        || form.get("redirect_uri") != Some(&grant.attempt.callback)
        || challenge.as_ref() != Some(&grant.attempt.challenge)
        || grant.attempt.created.elapsed() >= Duration::from_secs(600)
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let (subject, email) = PEOPLE[grant.person];
    let (id_token, _) = test_support::browser_google_token(
        &grant.attempt.nonce,
        CLIENT,
        SystemClock.now_unix(),
        subject,
        email,
    );
    Json(serde_json::json!({"access_token":"synthetic-discarded", "token_type":"Bearer", "expires_in":300,"id_token":id_token})).into_response()
}

async fn inbox(State(s): State<Arc<IssuerState>>) -> Response {
    let mut body = String::from("<h2>Captured synthetic mail</h2><p>Reload after requesting mail. Alternate-origin invitation links test cookie isolation; they do not prove nonrewritten delivery.</p>");
    for i in 0..s.capture.len().await {
        let recipient = s.capture.recipient(i).await;
        let text = s.capture.text(i).await;
        if let Some(link) = text.split_whitespace().find_map(|part| {
            url::Url::parse(part)
                .ok()
                .filter(|u| u.fragment().is_some())
        }) {
            let valid = s
                .callbacks
                .iter()
                .any(|c| url::Url::parse(c).unwrap().origin() == link.origin());
            if !valid {
                continue;
            }
            body.push_str(&format!(
                "<section><h3>Mail {} to {}</h3>",
                i + 1,
                escape(&recipient)
            ));
            for (j, callback) in s.callbacks.iter().enumerate() {
                let mut local = url::Url::parse(callback).unwrap();
                local.set_path(link.path());
                local.set_query(None);
                local.set_fragment(link.fragment());
                body.push_str(&format!(
                    "<p><a rel=noreferrer href='{}'>Open captured link on participant {}</a></p>",
                    escape(local.as_str()),
                    j + 1
                ));
            }
            body.push_str("</section>");
        }
    }
    page(body)
}

async fn google_launcher() -> Response {
    let mut response = page("<h2>Synthetic Google protocol launcher</h2><p>This test-only page uses real native begin/callback/session routes. The production Google button correctly rejects local issuers and is not certified by this fixture.</p><button id=begin-login>Begin synthetic Google login</button><button id=begin-link>Link synthetic Google to current account</button><p id=status role=status aria-live=polite></p><p><a href=/login>Back to native sign-in</a></p><script src=/__synthetic/google.js defer></script>".into());
    response.headers_mut().insert("content-security-policy", "default-src 'none'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'".parse().unwrap());
    response
}

async fn google_launcher_script(State(s): State<Arc<IssuerState>>) -> Response {
    let script =
        include_str!("browser_fixture_google.js").replace("__NEEV_SYNTHETIC_ISSUER__", &s.origin);
    let mut response = script.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "text/javascript; charset=utf-8".parse().unwrap(),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
}

struct Fixture {
    origins: [String; 2],
    issuer: Arc<IssuerState>,
    tasks: Vec<JoinHandle<()>>,
    stop: tokio::sync::watch::Sender<bool>,
    database: sea_orm::DatabaseConnection,
    admin: sea_orm::DatabaseConnection,
    schema: String,
    document_root: tempfile::TempDir,
}
impl Fixture {
    async fn start() -> Self {
        // Bind first; failure cannot leave a database schema behind.
        let a = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let b = TcpListener::bind("[::1]:0").await.unwrap();
        let issuer_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origins = [
            format!("http://{}", a.local_addr().unwrap()),
            format!("http://{}", b.local_addr().unwrap()),
        ];
        let issuer_origin = format!(
            "http://localhost:{}",
            issuer_listener.local_addr().unwrap().port()
        );
        let capture = Arc::new(test_support::MailCapture::new());
        let issuer = Arc::new(IssuerState {
            origin: issuer_origin,
            callbacks: origins.clone().map(|o| format!("{o}/auth/google/callback")),
            attempts: Mutex::new(HashMap::new()),
            grants: Mutex::new(HashMap::new()),
            capture: capture.clone(),
            paths: Mutex::new(Vec::new()),
        });
        let router = Router::new()
            .route("/authorize", get(authorize))
            .route("/choose/{id}", get(choose).post(approve))
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/token", post(token))
            .route("/inbox", get(inbox))
            .with_state(issuer.clone());
        let mut key = [0u8; 32];
        OsRng.try_fill_bytes(&mut key).unwrap();
        let key_hex = key
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let document_root = test_support::owned_document_root();
        let document_root_path = document_root.path().canonicalize().unwrap();
        let blobs = Arc::new(BlobStore::open(&document_root_path).unwrap());
        let mut prepared = Vec::new();
        for (index, listener) in [a, b].into_iter().enumerate() {
            let mut values = HashMap::from([
                ("NEEV_APP_ORIGIN".into(), origins[index].clone()),
                (
                    "NEEV_DATABASE_URL".into(),
                    std::env::var("NEEV_TEST_DATABASE_URL").unwrap(),
                ),
                (
                    "NEEV_BIND".into(),
                    listener.local_addr().unwrap().to_string(),
                ),
                ("NEEV_ENC_KEY".into(), key_hex.clone()),
                ("NEEV_ALLOW_LOOPBACK_PREVIEW".into(), "true".into()),
            ]);
            values.insert(
                "NEEV_STATIC_ROOT".into(),
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../out")
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string(),
            );
            values.insert(
                "NEEV_DOCUMENT_ROOT".into(),
                document_root_path.display().to_string(),
            );
            let config = Config::from_values(&values).unwrap();
            let clock = Arc::new(SystemClock);
            let google = google::Google::for_test(
                CLIENT.into(),
                SECRET.into(),
                config.google_callback.clone(),
                &issuer.origin,
            )
            .unwrap();
            prepared.push((listener, config, clock, google));
        }
        // Resolve fallible local configuration before creating owned DB resources.
        let (database, admin, schema) = test_support::postgres_database().await;
        let (stop, mut issuer_stop) = tokio::sync::watch::channel(false);
        let mut tasks = vec![tokio::spawn(async move {
            axum::serve(issuer_listener, router)
                .with_graceful_shutdown(async move {
                    let _ = issuer_stop.changed().await;
                })
                .await
                .unwrap()
        })];
        for (listener, config, clock, google) in prepared {
            let oauth = oauth::Service::with_google(
                database.clone(),
                config.encryption_key,
                clock.clone(),
                google,
            );
            let mail = mail::Service::new(database.clone(), capture.clone());
            let magic = magic::Service::new(
                database.clone(),
                config.encryption_key,
                config.origin.clone(),
                clock.clone(),
                mail.clone(),
            );
            let state = Arc::new(ServerState {
                database: database.clone(),
                config,
                engine: None,
                blobs: blobs.clone(),
                clock,
                auth_limit: Arc::new(tokio::sync::Semaphore::new(32)),
                agents: crate::neev::agents::execution::disabled_runtime(),
                agent_jobs: crate::neev::agents::execution::new_job_registry(),
                realtime: Arc::new(crate::neev::content::realtime::MessageHub::default()),
                agent_runs: crate::neev::agents::runs::new_registry(),
                supervisor: std::sync::Arc::new(
                    crate::neev::agents::supervisor::Supervisor::Unavailable,
                ),
                agent_logins: crate::neev::agents::supervisor::new_login_registry(),
                agent_catalogues: crate::neev::agents::discovery::new_cache(),
                knowledge: None,
                knowledge_asks: crate::neev::knowledge::ask::AskRegistry::default(),
                oauth,
                magic: Some(magic),
                mail: Some(mail),
            });
            let mut app_stop = stop.subscribe();
            let launcher = Router::new()
                .route("/__synthetic/google", get(google_launcher))
                .route("/__synthetic/google.js", get(google_launcher_script))
                .with_state(issuer.clone());
            let app = http::router(state).merge(launcher);
            tasks.push(tokio::spawn(async move {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .with_graceful_shutdown(async move {
                    let _ = app_stop.changed().await;
                })
                .await
                .unwrap()
            }));
        }
        Self {
            origins,
            issuer,
            tasks,
            stop,
            database,
            admin,
            schema,
            document_root,
        }
    }
    async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        let mut server_failed = false;
        for mut task in self.tasks.drain(..) {
            match tokio::time::timeout(Duration::from_secs(15), &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => server_failed = true,
                Err(_) => {
                    task.abort();
                    if let Err(error) = task.await {
                        // Cancellation from our bounded fallback is expected; a
                        // panic racing with abort must still fail acceptance.
                        server_failed |= !error.is_cancelled();
                    }
                }
            }
        }
        test_support::cleanup_postgres(
            self.database.clone(),
            self.admin.clone(),
            self.schema.clone(),
        )
        .await;
        assert!(
            !server_failed,
            "synthetic fixture server task failed after cleanup"
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[tokio::test]
#[ignore = "owned interactive browser fixture; launch separately with exact name"]
async fn browser_acceptance_driver() {
    let fixture = Fixture::start().await;
    println!("OWNED SYNTHETIC SCHEMA: {}", fixture.schema);
    println!(
        "OWNED SYNTHETIC DOCUMENT ROOT: {}",
        fixture
            .document_root
            .path()
            .canonicalize()
            .unwrap()
            .display()
    );
    println!("SYNTHETIC PARTICIPANT 1: {}", fixture.origins[0]);
    println!("SYNTHETIC PARTICIPANT 2: {}", fixture.origins[1]);
    println!("SYNTHETIC CAPTURE INBOX: {}/inbox", fixture.issuer.origin);
    println!("Use /__synthetic/google on each app host for synthetic Google; production login button rejects local issuers.");
    println!("Type stop then Enter to close owned listeners/schema; automatic timeout 45 minutes.");
    let (send_stop, stop) = tokio::sync::oneshot::channel();
    // A detached std thread does not keep Tokio runtime shutdown waiting on stdin.
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let _ = send_stop.send(());
    });
    tokio::select! { _ = stop => {}, _ = tokio::time::sleep(Duration::from_secs(2700)) => {} }
    fixture.shutdown().await;
    println!("Owned fixture listeners stopped and exact synthetic schema removed.");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn authorize_allows_unrelated_cookies_but_denies_native_credentials() {
    let fixture = Fixture::start().await;
    let origin = fixture.origins[0].clone();
    let result = tokio::spawn(async move {
        let mut participant = HttpParticipant::new(origin);
        let preauth = participant
            .request(reqwest::Method::GET, "/api/neev/preauth", None)
            .await;
        participant.csrf = preauth.json::<serde_json::Value>().await.unwrap()["csrf"]
            .as_str()
            .unwrap()
            .into();
        let begin = participant
            .request(
                reqwest::Method::POST,
                "/api/neev/auth/google/login",
                Some(serde_json::json!({})),
            )
            .await;
        let authorization = begin.json::<serde_json::Value>().await.unwrap()["authorizationUrl"]
            .as_str()
            .unwrap()
            .to_owned();
        for cookies in [
            vec!["ext_name=unrelated; codeg.language_mode=system; codeg.locale=en"],
            vec!["codeg.locale=en", "codeg.language_mode=system"],
            vec!["neev_preview_session_preference=dark; preference=neev_preview_session"],
        ] {
            let mut headers = HeaderMap::new();
            for cookie in cookies {
                headers.append(header::COOKIE, cookie.parse().unwrap());
            }
            let response = participant
                .client
                .get(&authorization)
                .headers(headers)
                .send()
                .await
                .map_err(|_| "synthetic authorization request failed")
                .unwrap();
            assert_eq!(response.status(), 303);
            let chooser = url::Url::parse(&authorization)
                .unwrap()
                .join(response.headers()["location"].to_str().unwrap())
                .unwrap();
            let response = participant
                .client
                .get(chooser)
                .send()
                .await
                .map_err(|_| "synthetic chooser request failed")
                .unwrap();
            assert_eq!(response.status(), 200);
            assert!(response
                .text()
                .await
                .unwrap()
                .contains("Continue as support@neev.so"));
        }
        for name in [
            "neev_preview_session",
            "neev_preview_csrf",
            "neev_preview_browser",
            "neev_preview_preauth_csrf",
            "__Host-neev_session",
            "__Host-neev_csrf",
            "__Host-neev_browser",
            "__Host-neev_preauth_csrf",
        ] {
            for multiple_headers in [false, true] {
                let mut headers = HeaderMap::new();
                if multiple_headers {
                    headers.append(header::COOKIE, "codeg.locale=en".parse().unwrap());
                }
                headers.append(
                    header::COOKIE,
                    format!("codeg.language_mode=system; {name}=synthetic; ext_name=unrelated")
                        .parse()
                        .unwrap(),
                );
                let response = participant
                    .client
                    .get(&authorization)
                    .headers(headers)
                    .send()
                    .await
                    .map_err(|_| "synthetic authorization request failed")
                    .unwrap();
                assert_eq!(
                    response.status(),
                    400,
                    "native cookie must be denied: {name}"
                );
                assert!(response.headers().get(header::LOCATION).is_none());
            }
        }
    })
    .await;
    fixture.shutdown().await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn shutdown_reports_panicked_server_after_exact_resource_cleanup() {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    let mut fixture = Fixture::start().await;
    let database = fixture.database.clone();
    let admin = fixture.admin.clone();
    let schema = fixture.schema.clone();
    let origins = fixture.origins.clone();
    let observer = Database::connect(std::env::var("NEEV_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    fixture.tasks.insert(
        0,
        tokio::spawn(async {
            panic!("synthetic-private-task-detail");
        }),
    );
    let mut stop = fixture.stop.subscribe();
    let (completed, completion) = tokio::sync::oneshot::channel();
    fixture.tasks.push(tokio::spawn(async move {
        stop.changed().await.unwrap();
        completed.send(()).unwrap();
    }));

    let result = tokio::spawn(fixture.shutdown()).await;
    let row = observer
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) AS remaining FROM pg_namespace WHERE nspname = $1",
            vec![schema.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    observer.close().await.unwrap();
    assert_eq!(row.try_get::<i64>("", "remaining").unwrap(), 0);
    assert!(database.ping().await.is_err());
    assert!(admin.ping().await.is_err());
    completion.await.expect("later server task must finish");
    for origin in origins {
        let address = origin.strip_prefix("http://").unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }
    let failure = result.expect_err("a panicked server must fail fixture shutdown");
    let payload = failure.into_panic();
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .expect("sanitized shutdown failure");
    assert_eq!(
        message,
        "synthetic fixture server task failed after cleanup"
    );
}

#[tokio::test]
async fn native_production_router_has_no_synthetic_google_launcher() {
    let server = test_support::http_server().await;
    for path in [
        "/__synthetic/google",
        "/__synthetic/google.js",
        "/authorize",
        "/inbox",
    ] {
        server.get(path).await.assert_status_not_found();
    }
}

struct HttpParticipant {
    client: reqwest::Client,
    origin: String,
    cookies: HashMap<String, String>,
    csrf: String,
}
impl HttpParticipant {
    fn new(origin: String) -> Self {
        Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            origin,
            cookies: HashMap::new(),
            csrf: String::new(),
        }
    }
    async fn request(
        &mut self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> reqwest::Response {
        let mut req = self
            .client
            .request(method, format!("{}{path}", self.origin));
        if !self.cookies.is_empty() {
            req = req.header(
                "cookie",
                self.cookies
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            );
        }
        if let Some(body) = body {
            req = req
                .header("origin", &self.origin)
                .header("x-neev-csrf", &self.csrf)
                .json(&body);
        }
        let response = req
            .send()
            .await
            .map_err(|_| "synthetic native request failed")
            .unwrap();
        for header in response.headers().get_all("set-cookie") {
            let (name, value) = header
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .split_once('=')
                .unwrap();
            if value.is_empty() {
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.into(), value.into());
            }
        }
        response
    }
    async fn bootstrap(&mut self) -> serde_json::Value {
        let response = self
            .request(reqwest::Method::GET, "/api/neev/session", None)
            .await;
        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.unwrap();
        self.csrf = body["csrf"].as_str().unwrap().into();
        body
    }
    async fn login(&mut self, person: usize, linking: bool) -> u16 {
        let response = self
            .request(reqwest::Method::GET, "/api/neev/preauth", None)
            .await;
        assert_eq!(response.status(), 200);
        let preauth: serde_json::Value = response.json().await.unwrap();
        if !linking {
            self.csrf = preauth["csrf"].as_str().unwrap().into();
        }
        let path = if linking {
            "/api/neev/auth/google/link"
        } else {
            "/api/neev/auth/google/login"
        };
        let response = self
            .request(reqwest::Method::POST, path, Some(serde_json::json!({})))
            .await;
        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.unwrap();
        let authorization = url::Url::parse(body["authorizationUrl"].as_str().unwrap()).unwrap();
        // No native cookies are sent to this separate synthetic issuer.
        let response = self
            .client
            .get(authorization.clone())
            .send()
            .await
            .map_err(|_| "synthetic authorization request failed")
            .unwrap();
        assert_eq!(response.status(), 303);
        let choose = authorization
            .join(response.headers()["location"].to_str().unwrap())
            .unwrap();
        let mut chooser_request = choose.clone();
        // Untrusted chooser query parameters cannot expand the stored attempt's
        // allowed form destination or replace it with another origin.
        chooser_request
            .query_pairs_mut()
            .append_pair(
                "redirect_uri",
                "https://unowned.invalid/auth/google/callback",
            )
            .append_pair("origin", "https://unowned.invalid");
        let response = self
            .client
            .get(chooser_request)
            .send()
            .await
            .map_err(|_| "synthetic chooser request failed")
            .unwrap();
        assert_eq!(response.status(), 200);
        let policy = response.headers()["content-security-policy"]
            .to_str()
            .unwrap();
        let directives = policy
            .split(';')
            .map(|directive| {
                let mut parts = directive.split_whitespace();
                (parts.next().unwrap(), parts.collect::<Vec<_>>())
            })
            .collect::<HashMap<_, _>>();
        assert_eq!(directives.len(), 4);
        assert_eq!(
            directives["form-action"],
            vec!["'self'", self.origin.as_str()]
        );
        for directive in ["default-src", "frame-ancestors", "base-uri"] {
            assert_eq!(directives[directive], vec!["'none'"]);
        }
        let inbox = self
            .client
            .get(authorization.join("/inbox").unwrap())
            .send()
            .await
            .map_err(|_| "synthetic inbox request failed")
            .unwrap();
        assert_eq!(inbox.status(), 200);
        assert_eq!(
            inbox.headers()["content-security-policy"],
            "default-src 'none'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
        );
        let response = self
            .client
            .post(choose)
            .form(&[("person", person.to_string())])
            .send()
            .await
            .map_err(|_| "synthetic approval request failed")
            .unwrap();
        assert_eq!(response.status(), 303);
        let callback = url::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
        assert_eq!(callback.origin().ascii_serialization(), self.origin);
        let path = format!("{}?{}", callback.path(), callback.query().unwrap());
        self.request(reqwest::Method::GET, &path, None)
            .await
            .status()
            .as_u16()
    }
}

#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL; included by full native gate"]
async fn native_browser_http_round_trip_tenant_denial_and_link_conflict() {
    let fixture = Fixture::start().await;
    // Always stop tasks/close pools before rethrowing an assertion failure.
    let origins = fixture.origins.clone();
    let capture = fixture.issuer.capture.clone();
    let result = tokio::spawn(async move {
        let mut founder = HttpParticipant::new(origins[0].clone());
        let mut recipient = HttpParticipant::new(origins[1].clone());
        assert_eq!(founder.login(0,false).await,303);
        assert_eq!(recipient.login(1,false).await,303);
        let a = founder.bootstrap().await;
        let b = recipient.bootstrap().await;
        assert_ne!(a["user"]["id"],b["user"]["id"]);
        assert_eq!(a["scope"],"identity");
        let response = founder.request(reqwest::Method::POST,"/api/neev/organizations",Some(serde_json::json!({"requestId":uuid::Uuid::new_v4().to_string(),"name":"Synthetic company one"}))).await;
        assert_eq!(response.status(),201);
        let company: serde_json::Value = response.json().await.unwrap();
        let id = company["organization"]["id"].as_i64().unwrap();
        let response = founder.request(reqwest::Method::POST,"/api/neev/organizations/select",Some(serde_json::json!({"organizationId":id}))).await;
        assert_eq!(response.status(),200);
        founder.bootstrap().await;
        let denied = recipient.request(reqwest::Method::POST,"/api/neev/organizations/select",Some(serde_json::json!({"organizationId":id}))).await;
        assert_eq!(denied.status(),403);
        let mut parent = serde_json::Value::Null;
        for (kind,name) in [("project","Project"),("folder","Folder"),("folder","Subfolder")] {
            let response = founder.request(reqwest::Method::POST,"/api/neev/nodes",Some(serde_json::json!({"parentId":parent,"kind":kind,"name":name}))).await;
            assert_eq!(response.status(),201);
            let node: serde_json::Value = response.json().await.unwrap(); parent = node["id"].clone();
        }
        let nodes = founder.request(reqwest::Method::GET,"/api/neev/nodes",None).await;
        assert_eq!(nodes.status(),200);
        let nodes: serde_json::Value = nodes.json().await.unwrap();
        assert_eq!(nodes["nodes"].as_array().unwrap().len(),3);
        assert_eq!(founder.login(1,true).await,409);
        assert_eq!(founder.bootstrap().await["user"]["id"],a["user"]["id"]);
        let response = founder.request(reqwest::Method::POST,"/api/neev/organization/invitations",Some(serde_json::json!({"email":"recipient2@example.test","role":"member","requestId":uuid::Uuid::new_v4().to_string()}))).await;
        assert_eq!(response.status(),201);
        let invitation = capture.latest_fragment().await;
        let response = recipient.request(reqwest::Method::POST,"/api/neev/invitations/preview",Some(serde_json::json!({"token":invitation}))).await;
        assert_eq!(response.status(),200);
        // A preview neither admits nor scopes the recipient.
        assert_eq!(recipient.bootstrap().await["scope"],"identity");
        let denied = recipient.request(reqwest::Method::POST,"/api/neev/organizations/select",Some(serde_json::json!({"organizationId":id}))).await;
        assert_eq!(denied.status(),403);
        let response = recipient.request(reqwest::Method::POST,"/api/neev/invitations/accept",Some(serde_json::json!({"token":invitation,"confirmedOrgId":id}))).await;
        assert_eq!(response.status(),200);
        let response = recipient.request(reqwest::Method::POST,"/api/neev/organizations/select",Some(serde_json::json!({"organizationId":id}))).await;
        assert_eq!(response.status(),200);
        assert_eq!(recipient.bootstrap().await["role"],"member");
        for route in ["/api/team/session","/api/settings","/api/chat/send"] { assert_eq!(founder.request(reqwest::Method::GET,route,None).await.status(),404); }
        let response = founder.request(reqwest::Method::POST,"/api/neev/logout",Some(serde_json::json!({}))).await;
        assert_eq!(response.status(),204);
        assert_eq!(founder.request(reqwest::Method::GET,"/api/neev/session",None).await.status(),401);
        let response = founder.request(reqwest::Method::GET,"/api/neev/preauth",None).await;
        let preauth: serde_json::Value = response.json().await.unwrap();
        founder.csrf = preauth["csrf"].as_str().unwrap().into();
        let response = founder.request(reqwest::Method::POST,"/api/neev/email/login",Some(serde_json::json!({"email":"outsider@example.test"}))).await;
        assert_eq!(response.status(),202);
        let magic = capture.latest_fragment().await;
        let scanner = reqwest::Client::builder().no_proxy().build().unwrap().get(format!("{}/auth/email/confirm",founder.origin)).send().await.map_err(|_| "scanner GET failed").unwrap();
        assert_eq!(scanner.status(),200);
        assert_eq!(founder.request(reqwest::Method::GET,"/api/neev/session",None).await.status(),401);
        let response = founder.request(reqwest::Method::POST,"/api/neev/email/confirm",Some(serde_json::json!({"token":magic}))).await;
        assert_eq!(response.status(),200);
        let preauth_csrf = founder.csrf.clone();
        let email_user = founder.bootstrap().await;
        assert_ne!(email_user["user"]["id"],a["user"]["id"]);
        assert_ne!(email_user["user"]["id"],b["user"]["id"]);
        assert_eq!(email_user["scope"],"identity");
        founder.csrf = preauth_csrf;
        let replay = founder.request(reqwest::Method::POST,"/api/neev/email/confirm",Some(serde_json::json!({"token":magic}))).await;
        assert_eq!(replay.status(),401);
    }).await;
    assert!(fixture.issuer.paths.lock().unwrap().iter().all(|p| [
        "/authorize",
        "/discovery",
        "/jwks",
        "/token"
    ]
    .contains(&p.as_str())));
    fixture.shutdown().await;
    result.unwrap();
}

const FIXTURE_CHILD_OUTPUT_LIMIT: u64 = 16 * 1024;

fn read_fixture_child_output(path: &std::path::Path) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    std::fs::File::open(path)?
        .take(FIXTURE_CHILD_OUTPUT_LIMIT + 1)
        .read_to_end(&mut output)?;
    if output.len() as u64 > FIXTURE_CHILD_OUTPUT_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fixture child output exceeded limit",
        ));
    }
    Ok(output)
}

struct ReapedFixtureChild(Child);

impl ReapedFixtureChild {
    fn kill_and_wait(&mut self) -> io::Result<ExitStatus> {
        if self.0.try_wait()?.is_none() {
            self.0.kill()?;
        }
        self.0.wait()
    }
}

impl Drop for ReapedFixtureChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

async fn bounded_fixture_child(
    mut command: Command,
    deadline: Duration,
) -> io::Result<Option<Output>> {
    let output_root = tempfile::tempdir()?;
    let stdout_path = output_root.path().join("stdout");
    let stderr_path = output_root.path().join("stderr");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(&stdout_path)?))
        .stderr(Stdio::from(std::fs::File::create(&stderr_path)?));
    let mut child = ReapedFixtureChild(command.spawn()?);
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if started.elapsed() >= deadline {
            child.kill_and_wait()?;
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let stdout = read_fixture_child_output(&stdout_path)?;
    let stderr = read_fixture_child_output(&stderr_path)?;
    Ok(Some(Output {
        status,
        stdout,
        stderr,
    }))
}

fn stalled_fixture_child_command(
    started: &std::path::Path,
    outlived_cleanup: &std::path::Path,
    owned_root: &std::path::Path,
) -> Command {
    let node = std::env::var_os("NEEV_TEST_NODE").unwrap_or_else(|| "node".into());
    let mut command = Command::new(node);
    command
        .args([
            "-e",
            "const fs=require('node:fs');fs.writeFileSync(process.env.NEEV_STALLED_CHILD_STARTED,'started');setTimeout(()=>{if(!fs.existsSync(process.env.NEEV_STALLED_CHILD_OWNED_ROOT))fs.writeFileSync(process.env.NEEV_STALLED_CHILD_OUTLIVED_CLEANUP,'child outlived fixture cleanup')},5000)",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NEEV_STALLED_CHILD_STARTED", started)
        .env("NEEV_STALLED_CHILD_OUTLIVED_CLEANUP", outlived_cleanup)
        .env("NEEV_STALLED_CHILD_OWNED_ROOT", owned_root);
    command
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL; included by full native gate"]
async fn stalled_preflight_child_is_reaped_before_exact_fixture_cleanup() {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    let fixture = Fixture::start().await;
    let owned_document_root = fixture.document_root.path().to_path_buf();
    let owned_schema = fixture.schema.clone();
    let observer = Database::connect(std::env::var("NEEV_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let marker_root = tempfile::tempdir().unwrap();
    let started = marker_root.path().join("started");
    let outlived_cleanup = marker_root.path().join("outlived-cleanup");
    let cancelled_started = marker_root.path().join("cancelled-started");
    let cancelled_outlived_cleanup = marker_root.path().join("cancelled-outlived-cleanup");
    let command = stalled_fixture_child_command(&started, &outlived_cleanup, &owned_document_root);

    let outcome = bounded_fixture_child(command, Duration::from_secs(2))
        .await
        .unwrap();
    assert!(
        outcome.is_none(),
        "the lifecycle control must reach its deadline"
    );
    assert!(started.exists(), "the stalled child must actually start");

    let cancelled_command = stalled_fixture_child_command(
        &cancelled_started,
        &cancelled_outlived_cleanup,
        &owned_document_root,
    );
    let cancelled_child = tokio::spawn(bounded_fixture_child(
        cancelled_command,
        Duration::from_secs(30),
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        while !cancelled_started.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    cancelled_child.abort();
    assert!(cancelled_child.await.unwrap_err().is_cancelled());

    fixture.shutdown().await;
    assert!(!owned_document_root.exists());
    let row = observer
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) AS remaining FROM pg_namespace WHERE nspname = $1",
            vec![owned_schema.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    observer.close().await.unwrap();
    assert_eq!(row.try_get::<i64>("", "remaining").unwrap(), 0);
    // Both synthetic children would write after five seconds if either escaped
    // its owner; wait beyond that point before accepting cleanup ordering.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        !outlived_cleanup.exists(),
        "stalled preflight child continued after exact fixture cleanup"
    );
    assert!(
        !cancelled_outlived_cleanup.exists(),
        "cancelled preflight child continued after exact fixture cleanup"
    );
}

#[tokio::test]
#[ignore = "requires explicitly isolated PostgreSQL; included by full native gate"]
async fn native_browser_http_content_collaboration_conflict_restore_and_revocation() {
    let fixture = Fixture::start().await;
    // Always stop tasks/close pools before rethrowing an assertion failure.
    let origins = fixture.origins.clone();
    let capture = fixture.issuer.capture.clone();
    let owned_document_root = fixture.document_root.path().to_path_buf();
    let result = tokio::spawn(async move {
        let mut founder = HttpParticipant::new(origins[0].clone());
        let mut member = HttpParticipant::new(origins[1].clone());
        assert_eq!(founder.login(0, false).await, 303);
        assert_eq!(member.login(1, false).await, 303);
        let founder_session = founder.bootstrap().await;
        let member_session = member.bootstrap().await;

        let response = founder
            .request(
                reqwest::Method::POST,
                "/api/neev/organizations",
                Some(serde_json::json!({
                    "requestId": uuid::Uuid::new_v4().to_string(),
                    "name": "Synthetic native content"
                })),
            )
            .await;
        assert_eq!(response.status(), 201);
        let organization: serde_json::Value = response.json().await.unwrap();
        let organization_id = organization["organization"]["id"].as_i64().unwrap();
        assert_eq!(
            founder
                .request(
                    reqwest::Method::POST,
                    "/api/neev/organizations/select",
                    Some(serde_json::json!({"organizationId": organization_id})),
                )
                .await
                .status(),
            200
        );
        founder.bootstrap().await;
        assert_eq!(
            founder
                .request(
                    reqwest::Method::POST,
                    "/api/neev/organization/invitations",
                    Some(serde_json::json!({
                        "email": "recipient2@example.test",
                        "role": "member",
                        "requestId": uuid::Uuid::new_v4().to_string()
                    })),
                )
                .await
                .status(),
            201
        );
        let invitation = capture.latest_fragment().await;
        assert_eq!(
            member
                .request(
                    reqwest::Method::POST,
                    "/api/neev/invitations/accept",
                    Some(serde_json::json!({
                        "token": invitation,
                        "confirmedOrgId": organization_id
                    })),
                )
                .await
                .status(),
            200
        );
        assert_eq!(
            member
                .request(
                    reqwest::Method::POST,
                    "/api/neev/organizations/select",
                    Some(serde_json::json!({"organizationId": organization_id})),
                )
                .await
                .status(),
            200
        );
        member.bootstrap().await;

        let response = founder
            .request(
                reqwest::Method::POST,
                "/api/neev/nodes",
                Some(serde_json::json!({
                    "parentId": null,
                    "kind": "project",
                    "name": "Shared project"
                })),
            )
            .await;
        assert_eq!(response.status(), 201);
        let node: serde_json::Value = response.json().await.unwrap();
        let node_id = node["id"].as_i64().unwrap();

        let response = founder
            .request(
                reqwest::Method::POST,
                &format!("/api/neev/nodes/{node_id}/chats"),
                Some(serde_json::json!({
                    "title": "Private planning",
                    "requestId": uuid::Uuid::new_v4().to_string()
                })),
            )
            .await;
        assert_eq!(response.status(), 201);
        let chat: serde_json::Value = response.json().await.unwrap();
        let chat_id = chat["id"].as_str().unwrap().to_owned();
        assert_eq!(
            member
                .request(
                    reqwest::Method::GET,
                    &format!("/api/neev/resources/{chat_id}"),
                    None,
                )
                .await
                .status(),
            404
        );
        let response = founder
            .request(
                reqwest::Method::PATCH,
                &format!("/api/neev/resources/{chat_id}"),
                Some(serde_json::json!({
                    "expectedAuthRevision": "1",
                    "title": "Organization planning",
                    "nodeId": node_id,
                    "visibility": "organization",
                    "archived": false
                })),
            )
            .await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["authRevision"],
            "2"
        );

        let first_message = founder
            .request(
                reqwest::Method::POST,
                &format!("/api/neev/chats/{chat_id}/messages"),
                Some(serde_json::json!({
                    "requestId": uuid::Uuid::new_v4().to_string(),
                    "expectedAuthRevision": "2",
                    "body": "Founder message"
                })),
            )
            .await;
        assert_eq!(first_message.status(), 201);
        assert_eq!(
            first_message.json::<serde_json::Value>().await.unwrap()["authorId"],
            founder_session["user"]["id"]
        );
        let shared_message = member
            .request(
                reqwest::Method::POST,
                &format!("/api/neev/chats/{chat_id}/messages"),
                Some(serde_json::json!({
                    "requestId": uuid::Uuid::new_v4().to_string(),
                    "expectedAuthRevision": "2",
                    "body": "Member message"
                })),
            )
            .await;
        assert_eq!(shared_message.status(), 201);
        assert_eq!(
            shared_message.json::<serde_json::Value>().await.unwrap()["authorId"],
            member_session["user"]["id"]
        );
        let messages = founder
            .request(
                reqwest::Method::GET,
                &format!("/api/neev/chats/{chat_id}/messages"),
                None,
            )
            .await;
        assert_eq!(messages.status(), 200);
        let messages: serde_json::Value = messages.json().await.unwrap();
        assert_eq!(messages["items"].as_array().unwrap().len(), 2);
        assert_eq!(
            messages["items"][0]["authorId"],
            founder_session["user"]["id"]
        );
        assert_eq!(
            messages["items"][1]["authorId"],
            member_session["user"]["id"]
        );

        let response = founder
            .request(
                reqwest::Method::POST,
                &format!("/api/neev/nodes/{node_id}/documents"),
                Some(serde_json::json!({
                    "title": "Shared brief",
                    "originChatId": chat_id,
                    "requestId": uuid::Uuid::new_v4().to_string()
                })),
            )
            .await;
        assert_eq!(response.status(), 201);
        let document: serde_json::Value = response.json().await.unwrap();
        let document_id = document["id"].as_str().unwrap().to_owned();
        let first_revision = founder
            .request(
                reqwest::Method::POST,
                &format!("/api/neev/documents/{document_id}/revisions"),
                Some(serde_json::json!({
                    "requestId": uuid::Uuid::new_v4().to_string(),
                    "expectedContentRevision": "0",
                    "expectedAuthRevision": "1",
                    "markdown": "# Shared through one owned store"
                })),
            )
            .await;
        assert_eq!(first_revision.status(), 201);
        assert_eq!(
            first_revision.json::<serde_json::Value>().await.unwrap()["revision"],
            "1"
        );

        // This write-on-listener-one/read-on-listener-two control fails when the
        // two routers share PostgreSQL metadata but open distinct blob roots.
        let shared_content = member
            .request(
                reqwest::Method::GET,
                &format!("/api/neev/documents/{document_id}/content?revision=1"),
                None,
            )
            .await;
        assert_eq!(shared_content.status(), 200);
        assert_eq!(
            shared_content.json::<serde_json::Value>().await.unwrap()["markdown"],
            "# Shared through one owned store"
        );

        let second_revision = member
            .request(
                reqwest::Method::POST,
                &format!("/api/neev/documents/{document_id}/revisions"),
                Some(serde_json::json!({
                    "requestId": uuid::Uuid::new_v4().to_string(),
                    "expectedContentRevision": "1",
                    "expectedAuthRevision": "1",
                    "markdown": "# Member edit"
                })),
            )
            .await;
        assert_eq!(second_revision.status(), 201);
        assert_eq!(
            second_revision.json::<serde_json::Value>().await.unwrap()["parentRevision"],
            "1"
        );
        let stale_save_response = founder
            .request(
                reqwest::Method::POST,
                &format!("/api/neev/documents/{document_id}/revisions"),
                Some(serde_json::json!({
                    "requestId": uuid::Uuid::new_v4().to_string(),
                    "expectedContentRevision": "1",
                    "expectedAuthRevision": "1",
                    "markdown": "# Preserved stale draft"
                })),
            )
            .await;
        assert_eq!(stale_save_response.status(), 409);
        let restored_revision = founder
            .request(
                reqwest::Method::POST,
                &format!("/api/neev/documents/{document_id}/restore"),
                Some(serde_json::json!({
                    "requestId": uuid::Uuid::new_v4().to_string(),
                    "expectedContentRevision": "2",
                    "expectedAuthRevision": "1",
                    "sourceRevision": "1"
                })),
            )
            .await;
        assert_eq!(restored_revision.status(), 201);
        let restored_revision: serde_json::Value = restored_revision.json().await.unwrap();
        assert_eq!(restored_revision["revision"], "3");
        assert_eq!(restored_revision["parentRevision"], "2");
        assert_eq!(restored_revision["restoredFrom"], "1");

        let session_name = http::cookies::CookieNames::for_preview(true).session;
        let native_session = member.cookies.get(session_name).unwrap().clone();
        let preflight_origin = member.origin.clone();
        let node = std::env::var_os("NEEV_TEST_NODE").unwrap_or_else(|| "node".into());
        let mut command = Command::new(node);
        command
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../scripts/neev-content-preflight.mjs"),
            )
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("NEEV_PREFLIGHT_ORIGIN", preflight_origin)
            .env("NEEV_PREFLIGHT_SESSION", native_session);
        let preflight = bounded_fixture_child(command, Duration::from_secs(20))
            .await
            .unwrap()
            .expect("fixture-owned native content preflight timed out after child termination");
        assert!(
            preflight.status.success(),
            "fixture-owned native content preflight failed: {}",
            String::from_utf8_lossy(&preflight.stderr)
        );
        assert!(preflight.stderr.is_empty());
        let summary: serde_json::Value = serde_json::from_slice(&preflight.stdout).unwrap();
        assert_eq!(
            summary,
            serde_json::json!({
                "identityMode": "native",
                "runtimeEnabled": false,
                "organizationScoped": true,
                "nodes": 1,
                "chats": 1,
                "documents": 1
            })
        );

        let response = founder
            .request(
                reqwest::Method::PATCH,
                &format!("/api/neev/resources/{chat_id}"),
                Some(serde_json::json!({
                    "expectedAuthRevision": "2",
                    "title": "Private again",
                    "nodeId": node_id,
                    "visibility": "private",
                    "archived": false
                })),
            )
            .await;
        assert_eq!(response.status(), 200);
        let hidden_resource_response = member
            .request(
                reqwest::Method::GET,
                &format!("/api/neev/documents/{document_id}/content"),
                None,
            )
            .await;
        assert_eq!(hidden_resource_response.status(), 404);

        let response = founder
            .request(
                reqwest::Method::PATCH,
                &format!("/api/neev/resources/{chat_id}"),
                Some(serde_json::json!({
                    "expectedAuthRevision": "3",
                    "title": "Organization again",
                    "nodeId": node_id,
                    "visibility": "organization",
                    "archived": false
                })),
            )
            .await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["authRevision"],
            "4"
        );
        assert_eq!(
            member
                .request(
                    reqwest::Method::GET,
                    &format!("/api/neev/documents/{document_id}/content"),
                    None,
                )
                .await
                .status(),
            200
        );

        let response = founder
            .request(reqwest::Method::GET, "/api/neev/organization/members", None)
            .await;
        assert_eq!(response.status(), 200);
        let members: serde_json::Value = response.json().await.unwrap();
        let member_id = members["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["user"]["id"] == member_session["user"]["id"])
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        let response = founder
            .request(
                reqwest::Method::DELETE,
                &format!("/api/neev/organization/members/{member_id}"),
                Some(serde_json::json!({})),
            )
            .await;
        assert_eq!(response.status(), 204);
        assert_eq!(
            member
                .request(
                    reqwest::Method::GET,
                    &format!("/api/neev/resources/{chat_id}"),
                    None,
                )
                .await
                .status(),
            403
        );
    });
    let mut result = result;
    let (result, timed_out, action_task_stopped) =
        match tokio::time::timeout(Duration::from_secs(120), &mut result).await {
            Ok(result) => (Some(result), false, true),
            Err(_) => {
                result.abort();
                let action_task_stopped = result.await.is_err_and(|error| error.is_cancelled());
                (None, true, action_task_stopped)
            }
        };
    fixture.shutdown().await;
    assert!(!owned_document_root.exists());
    assert!(
        action_task_stopped,
        "bounded native content action task did not stop"
    );
    assert!(!timed_out, "bounded native content fixture timed out");
    result.unwrap().unwrap();
}
