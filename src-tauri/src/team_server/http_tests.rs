use std::sync::Arc;

use axum::{
    body::Bytes,
    http::{header, Method, StatusCode},
};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DbBackend, EntityTrait, IntoActiveModel, PaginatorTrait,
    Set, Statement, TransactionTrait,
};
use axum_test::TestServer;

use super::{
    router::build_router,
    test_server::{cookie_value, query_value, set_cookies, team_state},
};
use crate::team_access::{
    github::{GithubIdentity, GithubMembershipEvidence, GithubOrganizationChoice},
    session::TeamRole,
    test_auth::{
        create_postgres_principal_fixture, drop_postgres_fixture, tokens,
        wait_for_postgres_lock, ALICE_A_TOKEN, fixed_now,
    },
};

#[tokio::test]
async fn manifest_isolated_public_and_static_errors_do_not_leak_operator_routes() {
    let fixture = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    let server = TestServer::new(build_router(Arc::new(fixture.state))).expect("test server");

    let capabilities = server.get("/api/workos/capabilities").await;
    capabilities.assert_status_ok();
    capabilities.assert_json(&serde_json::json!({
        "version": 1,
        "mode": "team",
        "runtimeEnabled": false,
    }));
    assert_eq!(capabilities.header(header::CACHE_CONTROL), "no-store");

    server.get("/healthz").await.assert_json(&serde_json::json!({"status":"ok"}));
    let root = server.get("/").await;
    root.assert_status_see_other();
    assert_eq!(root.header(header::LOCATION), "/workspace");
    assert!(root.headers().get(header::SET_COOKIE).is_none());
    server
        .post("/api/acp_connect")
        .add_header(header::AUTHORIZATION, "Bearer invented-operator")
        .await
        .assert_status_not_found();
    server.get("/api/team/session?secret=echo-me").await.assert_status_unauthorized();
}

#[tokio::test]
async fn login_ignores_client_authority_and_callback_requires_unique_browser_binding() {
    let fixture = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    let oauth = fixture.oauth.clone();
    let server = TestServer::new(build_router(Arc::new(fixture.state))).expect("test server");

    let login = server
        .get("/auth/github/login?redirect=https://attacker.invalid&actor=99&org=10201")
        .await;
    login.assert_status_see_other();
    let location_header = login.header(header::LOCATION);
    let location = location_header.to_str().unwrap();
    assert!(location.starts_with("https://github.com/login/oauth/authorize?"));
    assert!(!location.contains("attacker"));
    assert!(!location.contains("actor"));
    let attempt_cookie = cookie_value(&login, "workos_team_oauth_browser");
    assert!(!attempt_cookie.is_empty());

    let exchanges_before = oauth.exchange_calls().len();
    let unbound = server.get("/auth/github/callback?code=unbound-code").await;
    unbound.assert_status_see_other();
    assert_eq!(unbound.header(header::LOCATION), "/auth/github/login");
    assert_eq!(oauth.exchange_calls().len(), exchanges_before);

    let duplicate = server
        .get("/auth/github/callback?state=a&state=b&code=forged")
        .add_header(header::COOKIE, format!("workos_team_oauth_browser={attempt_cookie}"))
        .await;
    duplicate.assert_status_see_other();
    assert_eq!(duplicate.header(header::LOCATION), "/login?reason=invalid_login");
    assert_eq!(oauth.exchange_calls().len(), exchanges_before);
    assert!(set_cookies(&duplicate)
        .iter()
        .all(|cookie| !cookie.starts_with("workos_team_session=")));
}

#[tokio::test]
async fn provider_denial_is_single_use_browser_bound_and_recovers_with_a_fresh_attempt() {
    let fixture = team_state(
        vec![Ok(tokens("recovery-access", None, None, None))],
        vec![Ok(GithubIdentity {
            github_user_id: 30_301,
            login: "recovered-user".into(),
            name: None,
            avatar_url: None,
        })],
        Vec::new(),
        Vec::new(),
    )
    .await;
    let db = fixture.state.db.clone();
    let oauth = fixture.oauth.clone();
    let server = TestServer::new(build_router(Arc::new(fixture.state))).expect("test server");

    let login = server.get("/auth/github/login").await;
    let browser = cookie_value(&login, "workos_team_oauth_browser");
    let state = query_value(login.header(header::LOCATION).to_str().unwrap(), "state");
    let ambiguous = server
        .get(&format!(
            "/auth/github/callback?state={state}&code=ambiguous&error=access_denied"
        ))
        .add_header(
            header::COOKIE,
            format!("workos_team_oauth_browser={browser}"),
        )
        .await;
    assert_eq!(ambiguous.header(header::LOCATION), "/login?reason=invalid_login");
    assert_eq!(
        crate::db::entities::team_oauth_attempt::Entity::find()
            .count(&db)
            .await
            .unwrap(),
        1
    );
    let forged_state = "b".repeat(64);
    let forged = server
        .get(&format!(
            "/auth/github/callback?state={forged_state}&error=access_denied"
        ))
        .add_header(
            header::COOKIE,
            format!("workos_team_oauth_browser={browser}"),
        )
        .await;
    assert_eq!(forged.header(header::LOCATION), "/login?reason=invalid_login");
    assert_eq!(
        crate::db::entities::team_oauth_attempt::Entity::find()
            .count(&db)
            .await
            .unwrap(),
        1,
        "forged state consumed the genuine browser attempt"
    );

    let denied = server
        .get(&format!(
            "/auth/github/callback?state={state}&error=access_denied"
        ))
        .add_header(
            header::COOKIE,
            format!("workos_team_oauth_browser={browser}"),
        )
        .await;
    assert_eq!(denied.header(header::LOCATION), "/login?reason=access_denied");
    assert_eq!(
        crate::db::entities::team_oauth_attempt::Entity::find()
            .count(&db)
            .await
            .unwrap(),
        0
    );
    let replay = server
        .get(&format!(
            "/auth/github/callback?state={state}&error=access_denied"
        ))
        .add_header(
            header::COOKIE,
            format!("workos_team_oauth_browser={browser}"),
        )
        .await;
    assert_eq!(replay.header(header::LOCATION), "/login?reason=invalid_login");
    assert!(oauth.exchange_calls().is_empty());
    assert_eq!(
        crate::db::entities::team_pending_onboarding::Entity::find()
            .count(&db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        crate::db::entities::team_session::Entity::find()
            .count(&db)
            .await
            .unwrap(),
        0
    );

    let recovery_login = server.get("/auth/github/login").await;
    let recovery_browser = cookie_value(&recovery_login, "workos_team_oauth_browser");
    let recovery_state = query_value(
        recovery_login.header(header::LOCATION).to_str().unwrap(),
        "state",
    );
    let recovered = server
        .get(&format!(
            "/auth/github/callback?state={recovery_state}&code=recovery-code"
        ))
        .add_header(
            header::COOKIE,
            format!("workos_team_oauth_browser={recovery_browser}"),
        )
        .await;
    assert_eq!(recovered.header(header::LOCATION), "/onboarding");
    assert_eq!(oauth.exchange_calls().len(), 1);
}

#[tokio::test]
async fn writes_require_exact_origin_csrf_json_and_canonical_github_id() {
    let organization = GithubOrganizationChoice {
        github_org_id: 10_201,
        login: "alpha-company".into(),
        avatar_url: None,
        role: TeamRole::Member,
    };
    let fixture = team_state(
        vec![Ok(tokens("access-token", None, None, None))],
        vec![Ok(GithubIdentity {
            github_user_id: 30_301,
            login: "new-user".into(),
            name: Some("New User".into()),
            avatar_url: None,
        })],
        vec![Ok(vec![organization.clone()]), Ok(vec![organization])],
        Vec::new(),
    )
    .await;
    let server = TestServer::new(build_router(Arc::new(fixture.state))).expect("test server");

    let login = server.get("/auth/github/login").await;
    let attempt_cookie = cookie_value(&login, "workos_team_oauth_browser");
    let state = query_value(
        login.header(header::LOCATION).to_str().unwrap(),
        "state",
    );
    let callback = server
        .get(&format!("/auth/github/callback?state={state}&code=valid-code"))
        .add_header(
            header::COOKIE,
            format!("workos_team_oauth_browser={attempt_cookie}"),
        )
        .await;
    callback.assert_status_see_other();
    let pending = cookie_value(&callback, "workos_team_pending");
    let pending_csrf = cookie_value(&callback, "workos_team_pending_csrf");
    let pending_cookies = format!(
        "workos_team_pending={pending}; workos_team_pending_csrf={pending_csrf}"
    );

    let wrong_csrf = server
        .get("/api/team/onboarding")
        .add_header(
            header::COOKIE,
            format!(
                "workos_team_pending={pending}; workos_team_pending_csrf={}",
                "c".repeat(64)
            ),
        )
        .await;
    wrong_csrf.assert_status_unauthorized();
    let wrong_body = wrong_csrf.text();
    assert!(!wrong_body.contains("csrf_hash"));
    assert!(!wrong_body.contains("encrypted_payload"));
    assert!(!wrong_body.contains(&pending));

    server
        .post("/api/team/onboarding/select")
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("X-WorkOS-CSRF", &pending_csrf)
        .add_header("X-WorkOS-CSRF", &pending_csrf)
        .add_header(header::COOKIE, &pending_cookies)
        .json(&serde_json::json!({"githubOrgId":"10201"}))
        .await
        .assert_status_unauthorized();
    server
        .post("/api/team/onboarding/select")
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("X-WorkOS-CSRF", &pending_csrf)
        .add_header(header::COOKIE, &pending_cookies)
        .json(&serde_json::json!({"githubOrgId":"10201","forgedUserId":301}))
        .await
        .assert_status_bad_request();
    server
        .post("/api/team/onboarding/select")
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("X-WorkOS-CSRF", &pending_csrf)
        .add_header(header::COOKIE, &pending_cookies)
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(Bytes::from(vec![b' '; 16 * 1024 + 1]))
        .await
        .assert_status_bad_request();

    for origin in [
        "https://attacker.invalid",
        "https://user@workos.example",
        "https://workos.example/path",
        "https://workos.example?query",
        "https://workos.example#fragment",
        "https://workos.example, https://attacker.invalid",
    ] {
        server
            .post("/api/team/onboarding/select")
            .add_header(header::ORIGIN, origin)
            .add_header(header::CONTENT_TYPE, "application/json")
            .json(&serde_json::json!({"githubOrgId":"10201"}))
            .await
            .assert_status_forbidden();
    }
    server
        .post("/api/team/onboarding/select")
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header(header::CONTENT_TYPE, "application/json")
        .json(&serde_json::json!({"githubOrgId":"10201"}))
        .await
        .assert_status_forbidden();

    for github_org_id in ["0", "01", "9007199254740992", "9223372036854775808"] {
        let response = server
            .post("/api/team/onboarding/select")
            .add_header(header::ORIGIN, "https://workos.example")
            .add_header("X-WorkOS-CSRF", &pending_csrf)
            .add_header(header::COOKIE, &pending_cookies)
            .json(&serde_json::json!({"githubOrgId":github_org_id}))
            .await;
        if github_org_id == "9007199254740992" {
            response.assert_status_forbidden();
        } else {
            response.assert_status_bad_request();
        }
    }
}

#[tokio::test]
async fn onboarding_bootstrap_selection_session_and_local_logout_are_browser_bound() {
    let organization = GithubOrganizationChoice {
        github_org_id: 10_201,
        login: "alpha-company".into(),
        avatar_url: Some("https://avatars.example/alpha".into()),
        role: TeamRole::Admin,
    };
    let fixture = team_state(
        vec![Ok(tokens("durable-access", None, None, None))],
        vec![Ok(GithubIdentity {
            github_user_id: 9_007_199_254_740_992,
            login: "new-user".into(),
            name: Some("New User".into()),
            avatar_url: Some("https://avatars.example/user".into()),
        })],
        vec![Ok(vec![organization.clone()]), Ok(vec![organization.clone()])],
        vec![Ok(GithubMembershipEvidence {
            github_user_id: 9_007_199_254_740_992,
            organization,
        })],
    )
    .await;
    let server = TestServer::new(build_router(Arc::new(fixture.state))).expect("test server");

    let login = server.get("/auth/github/login").await;
    let attempt = cookie_value(&login, "workos_team_oauth_browser");
    let state = query_value(login.header(header::LOCATION).to_str().unwrap(), "state");
    let callback = server
        .get(&format!("/auth/github/callback?state={state}&code=good-code"))
        .add_header(
            header::COOKIE,
            format!("workos_team_oauth_browser={attempt}"),
        )
        .await;
    let pending = cookie_value(&callback, "workos_team_pending");
    let pending_csrf = cookie_value(&callback, "workos_team_pending_csrf");
    for cookie in set_cookies(&callback) {
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Secure"));
        assert!(!cookie.contains("Domain="));
    }
    let pending_cookies = format!(
        "workos_team_pending={pending}; workos_team_pending_csrf={pending_csrf}"
    );
    let choices = server
        .get("/api/team/onboarding")
        .add_header(header::COOKIE, &pending_cookies)
        .await;
    choices.assert_status_ok();
    let choices_json: serde_json::Value = choices.json();
    assert_eq!(
        choices_json["identity"]["githubUserId"],
        "9007199254740992"
    );
    assert_eq!(choices_json["organizations"][0]["githubOrgId"], "10201");
    assert_eq!(choices_json["csrf"], pending_csrf);
    let choices_text = choices.text();
    assert!(!choices_text.contains("csrf_hash"));
    assert!(!choices_text.contains("encrypted_payload"));

    let selected = server
        .post("/api/team/onboarding/select")
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("X-WorkOS-CSRF", &pending_csrf)
        .add_header(header::COOKIE, &pending_cookies)
        .json(&serde_json::json!({"githubOrgId":"10201"}))
        .await;
    selected.assert_status_ok();
    selected.assert_json(&serde_json::json!({"redirect":"/workspace"}));
    let session = cookie_value(&selected, "workos_team_session");
    let csrf = cookie_value(&selected, "workos_team_csrf");
    assert!(!session.is_empty());
    assert!(!csrf.is_empty());
    let session_cookies = format!("workos_team_session={session}; workos_team_csrf={csrf}");

    let bootstrap = server
        .get("/api/team/session")
        .add_header(header::COOKIE, &session_cookies)
        .await;
    bootstrap.assert_status_ok();
    let bootstrap_json: serde_json::Value = bootstrap.json();
    assert_eq!(bootstrap_json["user"]["login"], "new-user");
    assert_eq!(bootstrap_json["organization"]["login"], "alpha-company");
    assert_eq!(bootstrap_json["role"], "admin");
    assert_eq!(bootstrap_json["csrf"], csrf);
    assert_ne!(bootstrap_json["scope"], session);
    assert!(bootstrap_json["user"]["id"].is_i64());

    for invalid in [
        server
            .post("/api/team/logout")
            .add_header(header::ORIGIN, "https://workos.example")
            .add_header("X-WorkOS-CSRF", &csrf)
            .add_header(header::COOKIE, &session_cookies)
            .text("{}")
            .await,
        server
            .post("/api/team/logout")
            .add_header(header::ORIGIN, "https://workos.example")
            .add_header("X-WorkOS-CSRF", &csrf)
            .add_header(header::COOKIE, &session_cookies)
            .add_header(header::CONTENT_TYPE, "application/json")
            .add_header(header::CONTENT_TYPE, "application/json")
            .bytes(Bytes::from_static(b"{}"))
            .await,
        server
            .post("/api/team/logout")
            .add_header(header::ORIGIN, "https://workos.example")
            .add_header("X-WorkOS-CSRF", &csrf)
            .add_header(header::COOKIE, &session_cookies)
            .add_header(header::CONTENT_TYPE, "application/json")
            .bytes(Bytes::from(vec![b' '; 16 * 1024 + 1]))
            .await,
    ] {
        invalid.assert_status_bad_request();
        server
            .get("/api/team/session")
            .add_header(header::COOKIE, &session_cookies)
            .await
            .assert_status_ok();
    }

    let logout = server
        .post("/api/team/logout")
        .add_header(header::ORIGIN, "https://workos.example")
        .add_header("X-WorkOS-CSRF", &csrf)
        .add_header(header::COOKIE, &session_cookies)
        .json(&serde_json::json!({}))
        .await;
    logout.assert_status(StatusCode::NO_CONTENT);
    assert!(set_cookies(&logout)
        .iter()
        .any(|cookie| cookie.starts_with("workos_team_session=") && cookie.contains("Max-Age=0")));
    server
        .get("/api/team/session")
        .add_header(header::COOKIE, &session_cookies)
        .await
        .assert_status_unauthorized();

    let login_page = server.get("/login").await;
    login_page.assert_status_ok();
    assert!(login_page.text().contains("Eazybe WorkOS"));
}

#[tokio::test]
async fn unsupported_methods_and_unknown_protected_paths_are_not_found() {
    let fixture = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    let db = fixture.state.db.clone();
    let server = TestServer::new(build_router(Arc::new(fixture.state))).expect("test server");
    for path in ["/api/nope", "/auth/nope", "/ws/events", "/download/file", "/settings/nope"] {
        server.get(path).await.assert_status_not_found();
    }
    server
        .post("/healthz")
        .await
        .assert_status(StatusCode::METHOD_NOT_ALLOWED);
    server.post("/").await.assert_status(StatusCode::METHOD_NOT_ALLOWED);
    server
        .method(Method::HEAD, "/auth/github/login")
        .await
        .assert_status(StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        crate::db::entities::team_oauth_attempt::Entity::find()
            .count(&db)
            .await
            .unwrap(),
        0,
        "unsupported HEAD must not persist an OAuth attempt"
    );
}

const LOGOUT_CSRF: &str =
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

#[tokio::test]
async fn local_logout_removes_only_the_cookie_selected_session_when_membership_is_missing() {
    let fixture = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    let db = fixture.state.db.clone();
    let session = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    let hash = crate::team_access::test_auth::token_hash(session);
    crate::db::entities::team_session::ActiveModel {
        token_hash: Set(hash.clone()),
        user_id: Set(999),
        org_id: Set(998),
        membership_id: Set(997),
        authorization_revision: Set(1),
        credential_generation: Set(1),
        csrf_hash: Set(
            crate::team_access::secrets::hash_browser_secret(LOGOUT_CSRF).unwrap(),
        ),
        created_at: Set(fixed_now() - chrono::Duration::days(40)),
        expires_at: Set(fixed_now() - chrono::Duration::days(10)),
    }
    .insert(&db)
    .await
    .expect("orphaned expired session fixture");

    super::handlers::session::synchronized_logout(&db, session, LOGOUT_CSRF)
        .await
        .expect("orphaned expired session can be terminated");
    assert!(crate::db::entities::team_session::Entity::find_by_id(hash)
        .one(&db)
        .await
        .unwrap()
        .is_none());
}

async fn prepare_postgres_logout_session(
    db: &sea_orm::DatabaseConnection,
) -> Result<(), String> {
    let hash = crate::team_access::test_auth::token_hash(ALICE_A_TOKEN);
    let row = crate::db::entities::team_session::Entity::find_by_id(&hash)
        .one(db)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "missing PostgreSQL logout session".to_owned())?;
    let mut active = row.into_active_model();
    active.csrf_hash = Set(
        crate::team_access::secrets::hash_browser_secret(LOGOUT_CSRF)
            .map_err(|error| error.to_string())?,
    );
    active
        .update(db)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn session_headers() -> axum::http::HeaderMap {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        header::COOKIE,
        format!("workos_team_session={ALICE_A_TOKEN}")
            .parse()
            .unwrap(),
    );
    headers
}

/// Requires the local disposable PostgreSQL guard used by team_access tests.
#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn postgres_logout_waits_behind_an_authorized_mutation_guard() {
    let (name, admin, fixture) = create_postgres_principal_fixture(Vec::new())
        .await
        .expect("PostgreSQL fixture");
    let db = fixture.db.clone();
    let service = fixture.service();
    let result: Result<(), String> = async {
        prepare_postgres_logout_session(&db).await?;
        let context = service
            .resolve(
                &session_headers(),
                crate::team_access::principal::ProofRequirement::Cached,
            )
            .await
            .map_err(|error| error.to_string())?;
        let mutation = db.begin().await.map_err(|error| error.to_string())?;
        service
            .recheck_in_transaction(&mutation, &context)
            .await
            .map_err(|error| error.to_string())?;
        let logout = {
            let db = db.clone();
            tokio::spawn(async move {
                super::handlers::session::synchronized_logout(&db, ALICE_A_TOKEN, LOGOUT_CSRF)
                    .await
            })
        };
        wait_for_postgres_lock(&admin, &name, "organization_membership").await?;
        assert!(!logout.is_finished());
        mutation.commit().await.map_err(|error| error.to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(5), logout)
            .await
            .map_err(|_| "logout remained blocked".to_owned())?
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        assert!(crate::db::entities::team_session::Entity::find_by_id(
            crate::team_access::test_auth::token_hash(ALICE_A_TOKEN)
        )
        .one(&db)
        .await
        .map_err(|error| error.to_string())?
        .is_none());
        Ok(())
    }
    .await;
    drop(service);
    drop(fixture);
    drop_postgres_fixture(&name, admin, db)
        .await
        .expect("exact PostgreSQL cleanup");
    result.unwrap();
}

/// Requires the local disposable PostgreSQL guard used by team_access tests.
#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn postgres_guard_waiting_behind_logout_rejects_the_deleted_session() {
    let (name, admin, fixture) = create_postgres_principal_fixture(Vec::new())
        .await
        .expect("PostgreSQL fixture");
    let db = fixture.db.clone();
    let service = fixture.service();
    let result: Result<(), String> = async {
        prepare_postgres_logout_session(&db).await?;
        let context = service
            .resolve(
                &session_headers(),
                crate::team_access::principal::ProofRequirement::Cached,
            )
            .await
            .map_err(|error| error.to_string())?;
        let session_hash = crate::team_access::test_auth::token_hash(ALICE_A_TOKEN);
        let blocker = db.begin().await.map_err(|error| error.to_string())?;
        blocker
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT token_hash FROM team_session WHERE token_hash = $1 FOR UPDATE".to_owned(),
                [session_hash.into()],
            ))
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "session blocker found no row".to_owned())?;
        let logout = {
            let db = db.clone();
            tokio::spawn(async move {
                super::handlers::session::synchronized_logout(&db, ALICE_A_TOKEN, LOGOUT_CSRF)
                    .await
            })
        };
        wait_for_postgres_lock(&admin, &name, "team_session").await?;
        let guard = {
            let db = db.clone();
            let service = service.clone();
            tokio::spawn(async move {
                let transaction = db.begin().await.unwrap();
                let result = service.recheck_in_transaction(&transaction, &context).await;
                let _ = transaction.rollback().await;
                result
            })
        };
        wait_for_postgres_lock(&admin, &name, "organization_membership").await?;
        blocker.commit().await.map_err(|error| error.to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(5), logout)
            .await
            .map_err(|_| "logout remained blocked".to_owned())?
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let guarded = tokio::time::timeout(std::time::Duration::from_secs(5), guard)
            .await
            .map_err(|_| "guard remained blocked".to_owned())?
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            guarded,
            Err(crate::team_access::core::TeamAuthError::Unauthorized)
        ));
        Ok(())
    }
    .await;
    drop(service);
    drop(fixture);
    drop_postgres_fixture(&name, admin, db)
        .await
        .expect("exact PostgreSQL cleanup");
    result.unwrap();
}
