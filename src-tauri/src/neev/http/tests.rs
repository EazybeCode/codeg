use axum::{
    body::{to_bytes, Body, Bytes},
    extract::State,
    http::{header, Request},
    response::IntoResponse,
};
use serde_json::json;

use crate::neev::{
    auth::{identity, oauth, secrets, session},
    test_support as h,
    types::{Provider, SignInIntent, VerifiedIdentity},
};

use super::cookies::CookieNames;

#[tokio::test]
async fn pending_json_body_times_out_and_releases_native_request_capacity() {
    let database = h::database().await;
    let mut state = crate::neev::startup::ServerState::new(
        database,
        h::preview_config(),
        std::sync::Arc::new(h::TestClock::new(1_700_000_000)),
    )
    .unwrap();
    state.auth_limit = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let state = std::sync::Arc::new(state);
    tokio::time::pause();
    let pending_body =
        Body::from_stream(futures::stream::pending::<Result<Bytes, std::io::Error>>());
    let request = Request::builder()
        .method("POST")
        .uri("/api/neev/auth/google/login")
        .header(header::ORIGIN, "http://127.0.0.1:3100")
        .header(header::CONTENT_TYPE, "application/json")
        .body(pending_body)
        .unwrap();
    let pending = tokio::spawn(super::auth::google_login(State(state.clone()), request));
    while state.auth_limit.available_permits() != 0 {
        tokio::task::yield_now().await;
    }

    let while_pending = super::auth::google_login(
        State(state.clone()),
        Request::builder()
            .method("POST")
            .uri("/api/neev/auth/google/login")
            .header(header::ORIGIN, "http://127.0.0.1:3100")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert!(matches!(while_pending, Err(super::ApiError::Limited)));

    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    tokio::task::yield_now().await;
    assert!(
        pending.is_finished(),
        "pending body retained its request permit"
    );
    let response = pending.await.unwrap().unwrap_err().into_response();
    assert_eq!(response.status(), axum::http::StatusCode::REQUEST_TIMEOUT);
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"code":"request_timeout"})
    );

    let recovered = super::auth::google_login(
        State(state),
        Request::builder()
            .method("POST")
            .uri("/api/neev/auth/google/login")
            .header(header::ORIGIN, "http://127.0.0.1:3100")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert!(matches!(recovered, Err(super::ApiError::Unauthorized)));
}

#[tokio::test]
async fn google_callback_browser_errors_use_only_fixed_private_recovery_redirects() {
    let server = h::http_server().await;
    let path = "/auth/google/callback?code=synthetic-secret&error_description=provider-secret&redirect=https://evil.test";
    let browser = server
        .get(path)
        .add_header(header::ACCEPT, "text/html,application/xhtml+xml")
        .await;
    browser.assert_status(StatusCode::SEE_OTHER);
    assert_eq!(
        browser.header(header::LOCATION),
        "/login?auth_error=sign_in_failed"
    );
    assert_eq!(browser.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(browser.header("referrer-policy"), "no-referrer");
    assert!(!browser.text().contains("synthetic-secret"));
    for accept in [
        "application/json",
        "*/*",
        "text/html;q=0",
        "text/html;q=invalid",
    ] {
        let api = server.get(path).add_header(header::ACCEPT, accept).await;
        api.assert_status_bad_request();
        api.assert_json(&json!({"code":"invalid"}));
        assert!(api.headers().get(header::LOCATION).is_none());
    }
}

#[tokio::test]
async fn preauth_bootstrap_uses_distinct_cookies_and_preserves_browser_binding() {
    let server = h::http_server().await;
    let names = CookieNames::for_preview(true);
    let browser =
        crate::neev::auth::secrets::random_token(crate::neev::auth::secrets::TokenPurpose::Browser)
            .unwrap();
    let authenticated_csrf =
        crate::neev::auth::secrets::random_token(crate::neev::auth::secrets::TokenPurpose::Csrf)
            .unwrap();
    let response = server
        .get("/api/neev/preauth")
        .add_header(
            header::COOKIE,
            format!(
                "{}={browser}; {}={authenticated_csrf}",
                names.browser, names.csrf
            ),
        )
        .await;
    response.assert_status_ok();
    assert!(response.json::<serde_json::Value>()["csrf"]
        .as_str()
        .unwrap()
        .starts_with("n1.csrf."));
    let set_cookies: Vec<_> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect();
    assert_eq!(set_cookies.len(), 1);
    assert!(set_cookies[0].starts_with(names.preauth_csrf));
    assert!(!set_cookies[0].starts_with(names.csrf));
    assert!(!set_cookies[0].starts_with(names.browser));
}

#[tokio::test]
async fn hierarchy_http_does_not_call_the_configured_external_identity_boundary() {
    let issuer = h::OAuthIssuerCounter::start().await;
    let database = h::database().await;
    let clock = std::sync::Arc::new(h::TestClock::new(1_700_000_000));
    let config = h::preview_config();
    let google = crate::neev::auth::google::Google::for_test(
        "catalog-network-client".into(),
        "catalog-network-secret".into(),
        config.google_callback.clone(),
        issuer.base_url(),
    )
    .unwrap();
    let oauth = oauth::Service::with_google(
        database.clone(),
        config.encryption_key,
        clock.clone(),
        google,
    );
    let state = std::sync::Arc::new(crate::neev::startup::ServerState::with_oauth(
        database.clone(),
        config,
        clock,
        oauth,
    ));

    let browser = secrets::random_token(secrets::TokenPurpose::Browser).unwrap();
    let authorization = state
        .oauth
        .begin(Provider::Google, SignInIntent::Login, &browser)
        .await
        .unwrap();
    let oauth_state = url::Url::parse(&authorization)
        .unwrap()
        .query_pairs()
        .find(|(name, _)| name == "state")
        .unwrap()
        .1
        .into_owned();
    assert!(state
        .oauth
        .complete(
            Provider::Google,
            &oauth_state,
            oauth::CallbackResult::Code("invalid-code".into()),
            &browser,
        )
        .await
        .is_err());
    let connected_count = issuer.requests();
    assert!(
        connected_count > 0,
        "the injected HTTP counter must observe a real OAuth request"
    );

    let owner = h::native_account(&database, "network-owner", "network-owner@example.test").await;
    let (org_id, _) = h::organization(&database, owner, "admin").await;
    let identity = session::issue_identity(&database, owner, 1_700_000_000)
        .await
        .unwrap();
    let identity_context = session::identity(&database, &identity.session, 1_700_000_000)
        .await
        .unwrap();
    let scoped = session::select_organization(&database, &identity_context, org_id, 1_700_000_000)
        .await
        .unwrap();
    let names = CookieNames::for_preview(true);
    let cookies = format!(
        "{}={}; {}={}",
        names.session, scoped.session, names.csrf, scoped.csrf
    );
    let server = axum_test::TestServer::new(crate::neev::http::router(state)).unwrap();
    let root = catalog_mutation(
        &server,
        "/api/neev/nodes",
        &cookies,
        &scoped.csrf,
        json!({"parentId":null,"kind":"project","name":"Offline"}),
    )
    .await;
    root.assert_status(StatusCode::CREATED);
    let root_id = root.json::<serde_json::Value>()["id"].as_i64().unwrap();
    let folder = catalog_mutation(
        &server,
        "/api/neev/nodes",
        &cookies,
        &scoped.csrf,
        json!({"parentId":root_id,"kind":"folder","name":"Nested"}),
    )
    .await;
    folder.assert_status(StatusCode::CREATED);
    let folder_id = folder.json::<serde_json::Value>()["id"].as_i64().unwrap();
    server
        .patch(&format!("/api/neev/nodes/{folder_id}"))
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &scoped.csrf)
        .json(&json!({
            "expectedRevision":"1","parentId":root_id,
            "kind":"folder","name":"Renamed"
        }))
        .await
        .assert_status_ok();
    catalog_mutation(
        &server,
        "/api/neev/nodes",
        &cookies,
        &scoped.csrf,
        json!({"parentId":root_id,"kind":"working_directory","name":"Unavailable"}),
    )
    .await
    .assert_status(StatusCode::SERVICE_UNAVAILABLE);
    server
        .get("/api/neev/nodes")
        .add_header(header::COOKIE, &cookies)
        .await
        .assert_status_ok();
    server
        .get(&format!("/api/neev/nodes/{root_id}/chats"))
        .add_header(header::COOKIE, &cookies)
        .await
        .assert_status_ok();
    assert_eq!(issuer.requests(), connected_count);
}

#[tokio::test]
async fn capabilities_are_native_disabled_and_old_routes_do_not_exist() {
    let server = h::http_server().await;
    let capabilities = server.get("/api/workos/capabilities").await;
    capabilities.assert_status_ok();
    capabilities.assert_json(&json!({
        "version": 1,
        "identityMode": "native",
        "runtimeEnabled": false,
        "realtime": true,
        "providers": {"google":false,"email":false,"github":false},
        "catalog": {
            "nodeKinds":["project","folder"],
            "repositoryDefaults":false,
            "chats":true,
            "documents":true
        }
    }));
    assert_eq!(capabilities.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(capabilities.header("referrer-policy"), "no-referrer");
    server.get("/healthz").await.assert_status_ok();
    server.get("/api/settings").await.assert_status_not_found();
    server
        .get("/api/team/session")
        .await
        .assert_status_not_found();
    server
        .post("/api/acp_connect")
        .await
        .assert_status_not_found();
    server
        .post("/api/neev/email/login")
        .await
        .assert_status_not_found();
    let callback = server.get("/auth/google/callback?code=ignored").await;
    callback.assert_status_bad_request();
    assert!(callback
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .is_none());
}

#[tokio::test]
async fn native_catalog_http_uses_strict_revisioned_dtos_and_honest_boundaries() {
    let h = h::OrgHarness::new().await;
    let server = axum_test::TestServer::new(crate::neev::http::router(h.state.clone())).unwrap();
    let owner = h.owner_context();
    let identity = session::issue_identity(&h.database, owner.identity.user_id, 1_700_000_000)
        .await
        .unwrap();
    let identity_context = session::identity(&h.database, &identity.session, 1_700_000_000)
        .await
        .unwrap();
    let scoped =
        session::select_organization(&h.database, &identity_context, h.org_id(), 1_700_000_000)
            .await
            .unwrap();
    let names = CookieNames::for_preview(true);
    let cookies = format!(
        "{}={}; {}={}",
        names.session, scoped.session, names.csrf, scoped.csrf
    );

    let root = catalog_mutation(
        &server,
        "/api/neev/nodes",
        &cookies,
        &scoped.csrf,
        json!({"parentId":null,"kind":"project","name":"Marketing"}),
    )
    .await;
    root.assert_status(StatusCode::CREATED);
    let root_body = root.json::<serde_json::Value>();
    let root_id = root_body["id"].as_i64().unwrap();
    assert_eq!(
        root_body,
        json!({
            "id":root_id,"orgId":h.org_id(),"parentId":null,
            "kind":"project","name":"Marketing","revision":"1"
        })
    );

    let folder = catalog_mutation(
        &server,
        "/api/neev/nodes",
        &cookies,
        &scoped.csrf,
        json!({"parentId":root_id,"kind":"folder","name":"Campaigns"}),
    )
    .await;
    folder.assert_status(StatusCode::CREATED);
    let folder_body = folder.json::<serde_json::Value>();
    let folder_id = folder_body["id"].as_i64().unwrap();
    assert_eq!(folder_body["revision"], "1");

    let listed = server
        .get("/api/neev/nodes")
        .add_header(header::COOKIE, &cookies)
        .await;
    listed.assert_status_ok();
    listed.assert_json(&json!({
        "nodes":[root_body,folder_body],
        "nextAfter":null
    }));

    let updated = server
        .patch(&format!("/api/neev/nodes/{folder_id}"))
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &scoped.csrf)
        .json(&json!({
            "expectedRevision":"1","parentId":root_id,
            "kind":"folder","name":"Launches"
        }))
        .await;
    updated.assert_status_ok();
    assert_eq!(updated.json::<serde_json::Value>()["revision"], "2");
    server
        .patch(&format!("/api/neev/nodes/{folder_id}"))
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &scoped.csrf)
        .json(&json!({
            "expectedRevision":"2","parentId":root_id,
            "kind":"folder","name":"Repo","repositoryId":null
        }))
        .await
        .assert_status(StatusCode::SERVICE_UNAVAILABLE);

    for invalid in [
        json!({"expectedRevision":1,"parentId":root_id,"kind":"folder","name":"Bad"}),
        json!({"expectedRevision":"01","parentId":root_id,"kind":"folder","name":"Bad"}),
        json!({"expectedRevision":"2","parentId":root_id,"kind":"folder","name":"Bad","extra":true}),
    ] {
        server
            .patch(&format!("/api/neev/nodes/{folder_id}"))
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, &cookies)
            .add_header("x-neev-csrf", &scoped.csrf)
            .json(&invalid)
            .await
            .assert_status_bad_request();
    }

    for unsupported in [
        json!({"parentId":root_id,"kind":"working_directory","name":"Checkout"}),
        json!({"parentId":root_id,"kind":"folder","name":"Repo","repositoryId":null}),
        json!({"parentId":root_id,"kind":"folder","name":"Repo","repositoryUrl":null,"defaultBranch":null}),
    ] {
        catalog_mutation(
            &server,
            "/api/neev/nodes",
            &cookies,
            &scoped.csrf,
            unsupported,
        )
        .await
        .assert_status(StatusCode::SERVICE_UNAVAILABLE);
    }

    server
        .get(&format!("/api/neev/nodes/{folder_id}/chats"))
        .add_header(header::COOKIE, &cookies)
        .await
        .assert_json(&json!({"items":[],"nextAfter":null}));
    server
        .post(&format!("/api/neev/nodes/{folder_id}/chats"))
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &scoped.csrf)
        .json(&json!({}))
        .await
        .assert_status_bad_request();
    server
        .post("/api/neev/documents")
        .await
        .assert_status_not_found();
    server
        .post("/api/neev/prompts")
        .await
        .assert_status_not_found();

    let foreign = crate::neev::catalog::nodes::create(
        h.state(),
        h.other_org_context(),
        crate::neev::types::NodeInput {
            parent_id: None,
            kind: crate::neev::types::NodeKind::Project,
            name: "Foreign".into(),
        },
    )
    .await
    .unwrap();
    server
        .get(&format!("/api/neev/nodes?after={}", foreign.id))
        .add_header(header::COOKIE, &cookies)
        .await
        .assert_status_forbidden();
    server
        .get(&format!("/api/neev/nodes/{}/chats", foreign.id))
        .add_header(header::COOKIE, &cookies)
        .await
        .assert_status_not_found();
    server
        .get("/api/neev/nodes/2147483647/chats")
        .add_header(header::COOKIE, &cookies)
        .await
        .assert_status_not_found();
}

async fn catalog_mutation(
    server: &axum_test::TestServer,
    path: &str,
    cookies: &str,
    csrf: &str,
    body: serde_json::Value,
) -> axum_test::TestResponse {
    server
        .post(path)
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, cookies)
        .add_header("x-neev-csrf", csrf)
        .json(&body)
        .await
}

#[tokio::test]
async fn allowlisted_confirmation_is_no_store_without_a_spa_fallback() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("neev-auth")).unwrap();
    std::fs::write(
        directory.path().join("neev-auth/confirm.html"),
        b"confirm safely",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("neev-auth/confirm.js"),
        b"history.replaceState(null, '', location.pathname)",
    )
    .unwrap();
    std::fs::write(directory.path().join("neev-auth/confirm.css"), b"body{}").unwrap();
    let database = h::database().await;
    let clock = std::sync::Arc::new(h::TestClock::new(1000));
    let mut config = h::preview_config();
    config.static_root = directory.path().to_owned();
    let state = std::sync::Arc::new(
        crate::neev::startup::ServerState::new(database, config, clock).unwrap(),
    );
    let server = axum_test::TestServer::new(crate::neev::http::router(state)).unwrap();
    let response = server.get("/auth/email/confirm").await;
    response.assert_status_ok();
    assert_eq!(response.text(), "confirm safely");
    assert_eq!(response.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(response.header("referrer-policy"), "no-referrer");
    assert_eq!(
        response.header("content-security-policy"),
        "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'"
    );
    let script = server.get("/neev-auth/confirm.js").await;
    script.assert_status_ok();
    assert_eq!(
        script.header(header::CONTENT_TYPE),
        "text/javascript; charset=utf-8"
    );
    server
        .get("/neev-auth/confirm.css")
        .await
        .assert_status_ok();
    server
        .get("/not-allowlisted")
        .await
        .assert_status_not_found();
}

#[tokio::test]
async fn bootstrap_requires_one_native_session_and_matching_csrf_cookie() {
    let (server, database, _) = h::http_fixture().await;
    let user = h::native_account(&database, "owner", "support@neev.so").await;
    let issued = session::issue_identity(&database, user, 1000)
        .await
        .unwrap();
    let names = CookieNames::for_preview(true);
    let cookies = format!(
        "{}={}; {}={}",
        names.session, issued.session, names.csrf, issued.csrf
    );
    let response = server
        .get("/api/neev/session")
        .add_header(header::COOKIE, &cookies)
        .await;
    response.assert_status_ok();
    response.assert_json(&json!({
        "user":{"id":user,"displayName":"Test supporter","avatarUrl":null},
        "organization":null,
        "role":null,
        "scope":"identity",
        "csrf":issued.csrf
    }));
    assert_eq!(response.header(header::CACHE_CONTROL), "no-store");

    server
        .get("/api/neev/session")
        .add_header(
            header::COOKIE,
            format!("{cookies}; {}={}", names.session, issued.session),
        )
        .await
        .assert_status_unauthorized();
    let wrong =
        crate::neev::auth::secrets::random_token(crate::neev::auth::secrets::TokenPurpose::Csrf)
            .unwrap();
    server
        .get("/api/neev/session")
        .add_header(
            header::COOKIE,
            format!(
                "{}={}; {}={wrong}",
                names.session, issued.session, names.csrf
            ),
        )
        .await
        .assert_status_unauthorized();
}

#[tokio::test]
async fn native_static_export_assets_navigation_and_exact_inline_hashes() {
    let directory = tempfile::tempdir().unwrap();
    for path in [
        "_next/static/chunks",
        "_next/static/media",
        "workspace",
        "settings",
        "neev-auth",
    ] {
        std::fs::create_dir_all(directory.path().join(path)).unwrap();
    }
    let html = "<html><script src=\"/_next/static/chunks/app.js\"></script><script> window.fixture = 1;\n</script></html>";
    for (path, bytes) in [
        ("login.html", html),
        ("workspace.html", html),
        ("_next/static/chunks/app.js", "window.external = true"),
        ("_next/static/chunks/app.css", "body{margin:0}"),
        ("_next/static/media/font.woff2", "font-fixture"),
        ("workspace.txt", "native-flight"),
        ("workspace/__next.workspace.__PAGE__.txt", "native-segment"),
        ("workspace/__next._tree.txt", "native-tree"),
        ("settings/__next.settings.agents.txt", "legacy-segment"),
        ("settings/agents.html", "legacy-page"),
        ("icon.svg", "<svg></svg>"),
        (
            "neev-auth/invite.html",
            "<script src=\"/neev-auth/invite.js\"></script>",
        ),
    ] {
        std::fs::write(directory.path().join(path), bytes).unwrap();
    }
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.js"), "outside-root").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.path().join("secret.js"),
        directory.path().join("_next/static/chunks/escape.js"),
    )
    .unwrap();
    let mut config = h::preview_config();
    config.static_root = directory.path().to_owned();
    let state = std::sync::Arc::new(
        crate::neev::startup::ServerState::new(
            h::database().await,
            config,
            std::sync::Arc::new(h::TestClock::new(1000)),
        )
        .unwrap(),
    );
    let server = axum_test::TestServer::new(crate::neev::http::router(state)).unwrap();
    let response = server.get("/login").await;
    response.assert_status_ok();
    let csp = response
        .header("content-security-policy")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(csp.contains("'sha256-tOhhWU95qU4WAXDN1Rc8acgSz5+gwkrZbXdkR/yHwlQ='"));
    assert_eq!(csp.matches("sha256-").count(), 1);
    assert!(!csp
        .split("style-src")
        .next()
        .unwrap()
        .contains("'unsafe-inline'"));
    assert!(!csp.contains("unsafe-eval"));
    assert!(csp.contains("img-src 'self' data:"));
    for (path, content_type) in [
        (
            "/_next/static/chunks/app.js",
            "text/javascript; charset=utf-8",
        ),
        ("/_next/static/chunks/app.css", "text/css; charset=utf-8"),
        ("/_next/static/media/font.woff2", "font/woff2"),
        ("/workspace.txt", "text/x-component; charset=utf-8"),
        (
            "/workspace/__next.workspace.__PAGE__.txt",
            "text/x-component; charset=utf-8",
        ),
        (
            "/workspace/__next._tree.txt",
            "text/x-component; charset=utf-8",
        ),
        ("/icon.svg", "image/svg+xml"),
    ] {
        let asset = server.get(path).await;
        asset.assert_status_ok();
        assert_eq!(asset.header(header::CONTENT_TYPE), content_type);
        assert_eq!(asset.header("x-content-type-options"), "nosniff");
        // A build-id-named chunk may be kept; anything whose name is stable
        // across builds may not, or a deploy would leave readers on the old app.
        // These were all `no-store`, which is why the whole bundle came down
        // again on every load.
        let expected = if path.starts_with("/_next/static/") {
            crate::neev::http::IMMUTABLE_ASSET
        } else {
            "no-store"
        };
        assert_eq!(
            asset.header(header::CACHE_CONTROL),
            expected,
            "wrong cache policy for {path}"
        );
        assert_eq!(asset.header("referrer-policy"), "no-referrer");
    }
    for path in [
        "/settings/agents",
        "/settings/agents.html",
        "/settings/__next.settings.agents.txt",
        "/_next/static/chunks/missing.js",
        "/_next/static/chunks/escape.js",
        "/_next/static/chunks/%2e%2e/%2e%2e/secret.js",
        "/api/team/session",
        "/push",
    ] {
        server.get(path).await.assert_status_not_found();
    }
    let fragment = server.get("/auth/invite").await;
    assert_eq!(fragment.header("content-security-policy"), "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'");
}

/// Direct sign-in: the extension redirects the provider's loopback callback to
/// `/agents/signin#…`. The exported page must be served (it was a 404), with
/// the same exact-hash app CSP as every other exported page, and only its own
/// flight files beside it.
#[tokio::test]
async fn direct_signin_return_page_is_served_with_the_app_csp() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("agents/signin")).unwrap();
    let html = "<html><script src=\"/_next/static/chunks/app.js\"></script><script> window.fixture = 1;\n</script></html>";
    for (path, bytes) in [
        ("agents/signin.html", html),
        ("agents/signin.txt", "signin-flight"),
        ("agents/signin/__next._tree.txt", "signin-tree"),
        (
            "agents/signin/__next.agents.signin.__PAGE__.txt",
            "signin-segment",
        ),
        ("agents/other.html", "not-exported-route"),
        ("agents/signin/secret.txt", "not-a-segment"),
    ] {
        std::fs::write(directory.path().join(path), bytes).unwrap();
    }
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "outside-root").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        directory.path().join("agents/signin/__next._full.txt"),
    )
    .unwrap();
    let mut config = h::preview_config();
    config.static_root = directory.path().to_owned();
    let state = std::sync::Arc::new(
        crate::neev::startup::ServerState::new(
            h::database().await,
            config,
            std::sync::Arc::new(h::TestClock::new(1000)),
        )
        .unwrap(),
    );
    let server = axum_test::TestServer::new(crate::neev::http::router(state)).unwrap();

    let page = server.get("/agents/signin").await;
    page.assert_status_ok();
    assert_eq!(
        page.header(header::CONTENT_TYPE),
        "text/html; charset=utf-8"
    );
    assert_eq!(page.header("x-content-type-options"), "nosniff");
    assert_eq!(page.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(page.header("referrer-policy"), "no-referrer");
    let csp = page
        .header("content-security-policy")
        .to_str()
        .unwrap()
        .to_owned();
    // Same exact-hash policy as `/login`: the one inline script, nothing looser.
    assert!(csp.contains("'sha256-tOhhWU95qU4WAXDN1Rc8acgSz5+gwkrZbXdkR/yHwlQ='"));
    assert_eq!(csp.matches("sha256-").count(), 1);
    assert!(csp.starts_with("default-src 'none'; script-src 'self' "));
    assert!(!csp.contains("unsafe-eval"));
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(csp.contains("base-uri 'none'"));

    for path in [
        "/agents/signin.txt",
        "/agents/signin/__next._tree.txt",
        "/agents/signin/__next.agents.signin.__PAGE__.txt",
    ] {
        let asset = server.get(path).await;
        asset.assert_status_ok();
        assert_eq!(
            asset.header(header::CONTENT_TYPE),
            "text/x-component; charset=utf-8",
            "{path}"
        );
        assert_eq!(asset.header(header::CACHE_CONTROL), "no-store", "{path}");
    }

    for path in [
        // Only the exact page path; its `.html` is never served raw.
        "/agents/signin.html",
        "/agents/signin/",
        "/agents",
        "/agents/other",
        "/agents/other.html",
        // Only the page's own flight files.
        "/agents/signin/secret.txt",
        // A symlink out of the export root is refused.
        "/agents/signin/__next._full.txt",
        // Traversal stays blocked (encoded: a literal `..` is resolved by the
        // client before it is sent).
        "/agents/signin/%2e%2e/%2e%2e/secret.txt",
        "/agents/%2e%2e/login.html",
    ] {
        server.get(path).await.assert_status_not_found();
    }
    // A method other than GET does not reach the page.
    server
        .post("/agents/signin")
        .await
        .assert_status(axum::http::StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn logout_requires_origin_json_and_three_way_csrf_then_clears_state() {
    let (server, database, _) = h::http_fixture().await;
    let user = h::native_account(&database, "owner", "support@neev.so").await;
    let issued = session::issue_identity(&database, user, 1000)
        .await
        .unwrap();
    let names = CookieNames::for_preview(true);
    let cookies = format!(
        "{}={}; {}={}",
        names.session, issued.session, names.csrf, issued.csrf
    );

    server
        .post("/api/neev/logout")
        .add_header(header::ORIGIN, "http://attacker.test")
        .add_header(header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &issued.csrf)
        .json(&json!({}))
        .await
        .assert_status_forbidden();
    server
        .post("/api/neev/logout")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &cookies)
        .json(&json!({}))
        .await
        .assert_status_unauthorized();
    server
        .post("/api/neev/logout")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &issued.csrf)
        .json(&json!({}))
        .await
        .assert_status_forbidden();
    server
        .post("/api/neev/logout")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &issued.csrf)
        .await
        .assert_status_bad_request();
    server
        .post("/api/neev/logout")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &issued.csrf)
        .add_header("x-neev-csrf", &issued.csrf)
        .json(&json!({}))
        .await
        .assert_status_unauthorized();
    server
        .post("/api/neev/logout")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::CONTENT_TYPE, "application/json")
        .add_header(header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &issued.csrf)
        .bytes(Bytes::from(vec![b' '; 1025]))
        .await
        .assert_status_bad_request();

    let response = server
        .post("/api/neev/logout")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &issued.csrf)
        .json(&json!({}))
        .await;
    response.assert_status(StatusCode::NO_CONTENT);
    let set_cookies: Vec<_> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .collect();
    assert_eq!(set_cookies.len(), 4);
    assert!(set_cookies
        .iter()
        .all(|value| value.to_str().unwrap().contains("Max-Age=0")));
    server
        .get("/api/neev/session")
        .add_header(header::COOKIE, &cookies)
        .await
        .assert_status_unauthorized();
}

#[tokio::test]
async fn scoped_bootstrap_reports_org_but_identity_session_never_invents_one() {
    let (server, database, _) = h::http_fixture().await;
    let user = h::native_account(&database, "owner", "support@neev.so").await;
    let (org, _) = h::organization(&database, user, "admin").await;
    let identity = session::issue_identity(&database, user, 1000)
        .await
        .unwrap();
    let identity_context = session::identity(&database, &identity.session, 1000)
        .await
        .unwrap();
    let scoped = session::select_organization(&database, &identity_context, org, 1000)
        .await
        .unwrap();
    let names = CookieNames::for_preview(true);
    let cookies = format!(
        "{}={}; {}={}",
        names.session, scoped.session, names.csrf, scoped.csrf
    );
    server
        .get("/api/neev/session")
        .add_header(header::COOKIE, cookies)
        .await
        .assert_json(&json!({
            "user":{"id":user,"displayName":"Test supporter","avatarUrl":null},
            "organization":{"id":org,"name":"Test org","avatarUrl":null},
            "role":"admin",
            "scope":"organization",
            "csrf":scoped.csrf
        }));
}

#[tokio::test]
async fn organization_http_create_list_and_select_use_strict_safe_dtos() {
    let (server, database, _) = h::http_fixture().await;
    let user = h::native_account(&database, "http-org", "http-org@example.test").await;
    let issued = session::issue_identity(&database, user, 1000)
        .await
        .unwrap();
    let names = CookieNames::for_preview(true);
    let cookies = format!(
        "{}={}; {}={}",
        names.session, issued.session, names.csrf, issued.csrf
    );
    let request_id = uuid::Uuid::new_v4().to_string();
    server
        .post("/api/neev/organizations")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &issued.csrf)
        .json(&json!({"requestId":request_id,"name":"HTTP Company","extra":true}))
        .await
        .assert_status_bad_request();
    let created = server
        .post("/api/neev/organizations")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &issued.csrf)
        .json(&json!({"requestId":request_id,"name":"HTTP Company"}))
        .await;
    created.assert_status(StatusCode::CREATED);
    let body = created.json::<serde_json::Value>();
    let org_id = body["organization"]["id"].as_i64().unwrap();
    assert_eq!(body["organization"]["name"], "HTTP Company");
    assert_eq!(body["organization"]["avatarUrl"], serde_json::Value::Null);
    assert_eq!(body["role"], "admin");
    assert_eq!(body["brandingRevision"], "1");
    let listed = server
        .get("/api/neev/organizations")
        .add_header(header::COOKIE, &cookies)
        .await;
    listed.assert_status_ok();
    listed.assert_json(&json!({
        "organizations":[{"id":org_id,"name":"HTTP Company","role":"admin"}],
        "nextOffset":null
    }));
    let selected = server
        .post("/api/neev/organizations/select")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &cookies)
        .add_header("x-neev-csrf", &issued.csrf)
        .json(&json!({"organizationId":org_id}))
        .await;
    selected.assert_status_ok();
    selected.assert_json(&json!({
        "organization":{"id":org_id,"name":"HTTP Company","avatarUrl":null},
        "role":"admin"
    }));
    assert_eq!(
        selected
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .count(),
        2
    );
}

#[tokio::test]
async fn invitation_http_never_returns_raw_token_and_requires_post_csrf_preview() {
    let h = h::OrgHarness::new().await;
    let server = axum_test::TestServer::new(crate::neev::http::router(h.state.clone())).unwrap();
    let owner = h.owner();
    let identity = session::issue_identity(&h.database, owner.identity.user_id, 1_700_000_000)
        .await
        .unwrap();
    let identity_context = session::identity(&h.database, &identity.session, 1_700_000_000)
        .await
        .unwrap();
    let scoped =
        session::select_organization(&h.database, &identity_context, h.org_id(), 1_700_000_000)
            .await
            .unwrap();
    let names = CookieNames::for_preview(true);
    let owner_cookies = format!(
        "{}={}; {}={}",
        names.session, scoped.session, names.csrf, scoped.csrf
    );
    let sent = server
        .post("/api/neev/organization/invitations")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &owner_cookies)
        .add_header("x-neev-csrf", &scoped.csrf)
        .json(&json!({
            "email":"invitee@example.test",
            "requestId":uuid::Uuid::new_v4().to_string()
        }))
        .await;
    sent.assert_status(StatusCode::CREATED);
    let sent_text = sent.text();
    assert!(sent_text.contains("invitationId"));
    assert!(!sent_text.contains("n1.invite."));
    server
        .post("/api/neev/organization/invitations")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &owner_cookies)
        .add_header("x-neev-csrf", &scoped.csrf)
        .json(&json!({
            "email":"invitee@example.test",
            "role":"owner",
            "requestId":uuid::Uuid::new_v4().to_string()
        }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(h.capture.len().await, 1);
    let token = h.capture.latest_fragment().await;

    let invitee_user = h.invitee().user_id;
    let invitee = session::issue_identity(&h.database, invitee_user, 1_700_000_000)
        .await
        .unwrap();
    let invitee_cookies = format!(
        "{}={}; {}={}",
        names.session, invitee.session, names.csrf, invitee.csrf
    );
    server
        .get(&format!("/api/neev/invitations/preview?token={token}"))
        .add_header(header::COOKIE, &invitee_cookies)
        .await
        .assert_status(StatusCode::METHOD_NOT_ALLOWED);
    server
        .post("/api/neev/invitations/preview")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &invitee_cookies)
        .json(&json!({"token":token}))
        .await
        .assert_status_unauthorized();
    let preview = server
        .post("/api/neev/invitations/preview")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &invitee_cookies)
        .add_header("x-neev-csrf", &invitee.csrf)
        .json(&json!({"token":token}))
        .await;
    preview.assert_status_ok();
    preview.assert_json(&json!({"organization":{"id":h.org_id(),"name":"Test org"}}));
}

#[tokio::test]
async fn stale_scoped_authority_denies_bootstrap_and_linked_methods() {
    for mutation in [
        "UPDATE neev_accounts SET disabled=true",
        "UPDATE neev_memberships SET active=false,revision=revision+1",
        "UPDATE neev_memberships SET role='member',revision=revision+1",
        "UPDATE neev_organizations SET revision=revision+1",
    ] {
        let (server, database, _) = h::http_fixture().await;
        let user = h::native_account(&database, "stale-http", "stale-http@example.test").await;
        let (org, _) = h::organization(&database, user, "admin").await;
        let identity = session::issue_identity(&database, user, 1000)
            .await
            .unwrap();
        let context = session::identity(&database, &identity.session, 1000)
            .await
            .unwrap();
        let scoped = session::select_organization(&database, &context, org, 1000)
            .await
            .unwrap();
        h::execute(&database, mutation, vec![]).await;
        let names = CookieNames::for_preview(true);
        let cookies = format!(
            "{}={}; {}={}",
            names.session, scoped.session, names.csrf, scoped.csrf
        );
        let expected = if mutation.contains("accounts") {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::FORBIDDEN
        };
        server
            .get("/api/neev/session")
            .add_header(header::COOKIE, &cookies)
            .await
            .assert_status(expected);
        server
            .get("/api/neev/auth/methods")
            .add_header(header::COOKIE, &cookies)
            .await
            .assert_status(expected);
    }
}

#[tokio::test]
async fn linking_a_method_preserves_a_valid_scoped_session_and_refetches_methods() {
    let (server, database, _) = h::http_fixture().await;
    let user = h::native_account(&database, "link-method", "link-method@example.test").await;
    let (org, _) = h::organization(&database, user, "admin").await;
    let identity_session = session::issue_identity(&database, user, 1000)
        .await
        .unwrap();
    let identity_context = session::identity(&database, &identity_session.session, 1000)
        .await
        .unwrap();
    let scoped = session::select_organization(&database, &identity_context, org, 1000)
        .await
        .unwrap();
    let scoped_context = session::organization(&database, &scoped.session, 1000)
        .await
        .unwrap();
    identity::resolve(
        &database,
        VerifiedIdentity {
            provider: Provider::Email,
            issuer: "https://app.neev.so".into(),
            subject: "linked-method@example.test".into(),
            verified_email: Some("linked-method@example.test".into()),
            display_name: "Linked method".into(),
            avatar_url: None,
        },
        SignInIntent::Link {
            user_id: user,
            session_id: scoped_context.identity.session_id,
            auth_revision: scoped_context.identity.auth_revision,
        },
        1001,
    )
    .await
    .unwrap();
    let names = CookieNames::for_preview(true);
    server
        .get("/api/neev/auth/methods")
        .add_header(
            header::COOKIE,
            format!(
                "{}={}; {}={}",
                names.session, scoped.session, names.csrf, scoped.csrf
            ),
        )
        .await
        .assert_json(&json!({"methods":["email","google"]}));
}

#[tokio::test]
async fn invitation_page_is_fragment_only_and_never_echoes_or_stores_a_token() {
    let database = h::database().await;
    let clock = std::sync::Arc::new(h::TestClock::new(1000));
    let mut config = h::preview_config();
    config.static_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("public");
    let state = std::sync::Arc::new(
        crate::neev::startup::ServerState::new(database, config, clock).unwrap(),
    );
    let server = axum_test::TestServer::new(crate::neev::http::router(state)).unwrap();
    let page = server.get("/auth/invite").await;
    page.assert_status_ok();
    assert_eq!(page.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(page.header("referrer-policy"), "no-referrer");
    assert!(!page.text().contains("n1.invite."));
    server
        .get("/auth/invite?token=n1.invite.not-allowed")
        .await
        .assert_status_bad_request();
    let script = server.get("/neev-auth/invite.js").await;
    script.assert_status_ok();
}

#[tokio::test]
async fn linked_methods_are_distinct_lexical_and_hide_identity_fields() {
    let (server, database, _) = h::http_fixture().await;
    let user = h::native_account(&database, "methods", "methods@example.test").await;
    h::execute(
        &database,
        "INSERT INTO neev_identities(user_id,provider,issuer,subject,created_at) VALUES($1,'google','https://accounts.google.com','methods-second',1000),($1,'email','https://app.neev.so','methods@example.test',1000)",
        vec![user.into()],
    )
    .await;
    let issued = session::issue_identity(&database, user, 1000)
        .await
        .unwrap();
    let names = CookieNames::for_preview(true);
    let response = server
        .get("/api/neev/auth/methods")
        .add_header(
            header::COOKIE,
            format!(
                "{}={}; {}={}",
                names.session, issued.session, names.csrf, issued.csrf
            ),
        )
        .await;
    response.assert_status_ok();
    response.assert_json(&json!({"methods":["email","google"]}));
    let text = response.text();
    assert!(!text.contains("issuer"));
    assert!(!text.contains("subject"));
    assert!(!text.contains("methods@example.test"));
    assert_eq!(response.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(response.header("referrer-policy"), "no-referrer");
}

use axum::http::StatusCode;
