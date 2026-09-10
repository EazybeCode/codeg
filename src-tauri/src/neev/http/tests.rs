use axum::{body::Bytes, http::header};
use serde_json::json;

use crate::neev::{auth::session, test_support as h};

use super::cookies::CookieNames;

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
async fn capabilities_are_native_disabled_and_old_routes_do_not_exist() {
    let server = h::http_server().await;
    let capabilities = server.get("/api/workos/capabilities").await;
    capabilities.assert_status_ok();
    capabilities.assert_json(&json!({
        "version": 1,
        "identityMode": "native",
        "runtimeEnabled": false,
        "providers": {"google":false,"email":false,"github":false}
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
    let callback = server.get("/auth/google/callback?code=ignored").await;
    callback.assert_status_bad_request();
    assert!(callback
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .is_none());
}

#[tokio::test]
async fn allowlisted_confirmation_is_no_store_without_a_spa_fallback() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("email-confirm.html"),
        b"confirm safely",
    )
    .unwrap();
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
    response.assert_json(&json!({"userId":user,"csrf":issued.csrf}));
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
    let (org, member) = h::organization(&database, user, "admin").await;
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
    server.get("/api/neev/session").add_header(header::COOKIE, cookies).await.assert_json(&json!({
        "userId":user,"csrf":scoped.csrf,"organization":{"orgId":org,"memberId":member,"role":"admin"}
    }));
}

use axum::http::StatusCode;
