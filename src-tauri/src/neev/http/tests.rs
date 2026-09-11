use axum::{body::Bytes, http::header};
use serde_json::json;

use crate::neev::{
    auth::{identity, session},
    test_support as h,
    types::{Provider, SignInIntent, VerifiedIdentity},
};

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
