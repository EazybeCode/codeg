use std::{
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, Method, Response, StatusCode, Uri},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use axum_test::{TestResponse, TestServer};
use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

use super::{
    router::build_router,
    state::TeamState,
    test_server::{team_state, SessionBrowser},
};
use crate::{
    db::entities::team_session,
    team_access::{
        github::{
            GithubEvidenceError, GithubIdentity, GithubMembershipEvidence,
            GithubMembershipSource, GithubOrganizationChoice,
        },
        principal::PrincipalService,
        secrets::hash_browser_secret,
        session::TeamRole,
        test_auth::{
            principal_fixture, token_hash, ALICE_A_TOKEN, ALICE_B_TOKEN, BOB_A_TOKEN,
            CAROL_B_TOKEN,
        },
    },
};

const ORIGIN: &str = "https://workos.example";
const ALICE_A_CSRF: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const ALICE_B_CSRF: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const BOB_A_CSRF: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const CAROL_B_CSRF: &str = "4444444444444444444444444444444444444444444444444444444444444444";
const ANONYMOUS_CSRF: &str = "5555555555555555555555555555555555555555555555555555555555555555";

#[derive(Default)]
struct SyntheticMembership;

#[async_trait]
impl GithubMembershipSource for SyntheticMembership {
    async fn identity(
        &self,
        _access_token: &str,
    ) -> Result<GithubIdentity, GithubEvidenceError> {
        Err(GithubEvidenceError::Unavailable)
    }

    async fn active_organizations(
        &self,
        _access_token: &str,
        _expected_user_id: i64,
    ) -> Result<Vec<GithubOrganizationChoice>, GithubEvidenceError> {
        Err(GithubEvidenceError::Unavailable)
    }

    async fn verify_membership(
        &self,
        access_token: &str,
        expected_user_id: i64,
        expected_org_id: i64,
        organization_login: &str,
    ) -> Result<GithubMembershipEvidence, GithubEvidenceError> {
        let role = match (
            access_token,
            expected_user_id,
            expected_org_id,
            organization_login,
        ) {
            ("alice-access", 20_301, 10_201, "alice-a") => TeamRole::Admin,
            ("alice-access", 20_301, 10_202, "alice-b") => TeamRole::Member,
            ("bob-access", 20_302, 10_201, "alice-a") => TeamRole::Member,
            ("carol-access", 20_303, 10_202, "alice-b") => TeamRole::Admin,
            _ => return Err(GithubEvidenceError::NotMember),
        };
        Ok(GithubMembershipEvidence {
            github_user_id: expected_user_id,
            organization: GithubOrganizationChoice {
                github_org_id: expected_org_id,
                login: organization_login.to_owned(),
                avatar_url: None,
                role,
            },
        })
    }
}

struct RepositoryProofServer {
    origin: String,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl RepositoryProofServer {
    async fn start() -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/{*path}", get(repository_proof))
            .with_state(Arc::clone(&requests));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("repository proof listener");
        let address = listener.local_addr().expect("repository proof address");
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = receiver.await;
                })
                .await
                .expect("repository proof server");
        });
        Self {
            origin: format!("http://{address}"),
            requests,
            shutdown: Some(shutdown),
            task,
        }
    }

    fn origin(&self) -> &str {
        &self.origin
    }

    fn requests(&self) -> Vec<(String, String)> {
        self.requests.lock().expect("proof request log").clone()
    }
}

impl Drop for RepositoryProofServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

async fn repository_proof(
    State(requests): State<Arc<Mutex<Vec<(String, String)>>>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response<Body> {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let path = uri.path().to_owned();
    requests
        .lock()
        .expect("proof request log")
        .push((authorization.clone(), path.clone()));
    if !matches!(
        authorization.as_str(),
        "Bearer alice-access" | "Bearer bob-access" | "Bearer carol-access"
    ) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let response = match path.as_str() {
        "/repos/atlas/repo" => serde_json::json!({"id":8080,"full_name":"atlas/repo"}),
        "/repos/atlas/repo/branches/main" => serde_json::json!({"name":"main"}),
        "/repos/beacon/repo" => serde_json::json!({"id":9090,"full_name":"beacon/repo"}),
        "/repos/beacon/repo/branches/main" => serde_json::json!({"name":"main"}),
        "/repos/outage/repo" => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    Json(response).into_response()
}

struct CatalogFixture {
    server: TestServer,
    alice_a: SessionBrowser,
    alice_b: SessionBrowser,
    bob_a: SessionBrowser,
    carol_b: SessionBrowser,
    provider: RepositoryProofServer,
}

fn browser(token: &str, csrf: &str) -> SessionBrowser {
    SessionBrowser {
        cookies: format!("workos_team_session={token}; workos_team_csrf={csrf}"),
        session: token.to_owned(),
        csrf: csrf.to_owned(),
    }
}

async fn state_with_repository_origin(origin: &str) -> TeamState {
    let principal = principal_fixture(Vec::new()).await;
    for (token, csrf) in [
        (ALICE_A_TOKEN, ALICE_A_CSRF),
        (ALICE_B_TOKEN, ALICE_B_CSRF),
        (BOB_A_TOKEN, BOB_A_CSRF),
        (CAROL_B_TOKEN, CAROL_B_CSRF),
    ] {
        let row = team_session::Entity::find_by_id(token_hash(token))
            .one(&principal.db)
            .await
            .expect("session lookup")
            .expect("seeded session");
        let mut active = row.into_active_model();
        active.csrf_hash = Set(hash_browser_secret(csrf).expect("valid CSRF secret"));
        active.update(&principal.db).await.expect("update CSRF hash");
    }
    let principals = Arc::new(PrincipalService::new(
        principal.db.clone(),
        principal.credentials.clone(),
        Arc::new(SyntheticMembership),
        principal.clock.clone(),
    ));
    let shell = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    TeamState::new(
        principal.db,
        shell.state.config,
        principal.vault,
        shell.state.oauth,
        shell.state.onboarding,
        principal.credentials,
        principals,
        principal.clock,
    )
    .with_catalog_repository_origin_for_test(origin)
}

async fn fixture() -> CatalogFixture {
    let provider = RepositoryProofServer::start().await;
    let state = state_with_repository_origin(provider.origin()).await;
    CatalogFixture {
        server: TestServer::new(build_router(Arc::new(state))).expect("catalog HTTP server"),
        alice_a: browser(ALICE_A_TOKEN, ALICE_A_CSRF),
        alice_b: browser(ALICE_B_TOKEN, ALICE_B_CSRF),
        bob_a: browser(BOB_A_TOKEN, BOB_A_CSRF),
        carol_b: browser(CAROL_B_TOKEN, CAROL_B_CSRF),
        provider,
    }
}

async fn write_json(
    server: &TestServer,
    method: Method,
    path: &str,
    session: Option<&SessionBrowser>,
    body: &serde_json::Value,
) -> TestResponse {
    let mut request = server
        .method(method, path)
        .add_header(header::ORIGIN, ORIGIN);
    if let Some(session) = session {
        request = request
            .add_header(header::COOKIE, &session.cookies)
            .add_header("x-workos-csrf", &session.csrf);
    } else {
        request = request
            .add_header(header::AUTHORIZATION, "Bearer invented-shared-secret")
            .add_header(
                header::COOKIE,
                format!("workos_team_csrf={ANONYMOUS_CSRF}"),
            )
            .add_header("x-workos-csrf", ANONYMOUS_CSRF);
    }
    request.json(body).await
}

fn write_cases() -> Vec<(Method, &'static str, serde_json::Value)> {
    vec![
        (
            Method::POST,
            "/api/team/catalog/nodes",
            serde_json::json!({
                "parentId":null,"kind":"project","name":"Atlas",
                "repositoryDefault":{"repositoryId":1,"startingBranch":"main"}
            }),
        ),
        (
            Method::PATCH,
            "/api/team/catalog/nodes/1",
            serde_json::json!({
                "expectedRevision":"0","parentId":null,"name":"Atlas",
                "repositoryDefault":{"repositoryId":1,"startingBranch":"main"}
            }),
        ),
        (
            Method::POST,
            "/api/team/catalog/repositories",
            serde_json::json!({"fullName":"atlas/repo","startingBranch":"main"}),
        ),
        (
            Method::POST,
            "/api/team/chats",
            serde_json::json!({
                "nodeId":1,"repositoryOverride":null,"title":"Private",
                "requestId":"123e4567-e89b-12d3-a456-426614174000",
                "visibility":"private"
            }),
        ),
        (
            Method::PATCH,
            "/api/team/chats/1/visibility",
            serde_json::json!({"expectedRevision":"0","visibility":"organization"}),
        ),
    ]
}

fn assert_private(response: &TestResponse) {
    assert_eq!(response.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(response.header("referrer-policy"), "no-referrer");
}

#[tokio::test]
async fn http_catalog_manifest_protects_reads_denies_head_and_exposes_no_legacy_paths() {
    let fixture = fixture().await;
    for path in [
        "/api/team/catalog/nodes",
        "/api/team/catalog/repositories",
        "/api/team/chats",
        "/api/team/chats/1",
    ] {
        let response = fixture.server.get(path).await;
        response.assert_status_unauthorized();
        assert_private(&response);
        fixture
            .server
            .get(path)
            .add_header(header::AUTHORIZATION, "Bearer invented-shared-secret")
            .await
            .assert_status_unauthorized();
    }
    for path in [
        "/api/team/catalog/nodes",
        "/api/team/catalog/nodes/1",
        "/api/team/catalog/repositories",
        "/api/team/chats",
        "/api/team/chats/1",
        "/api/team/chats/1/visibility",
    ] {
        let response = fixture.server.method(Method::HEAD, path).await;
        response.assert_status(StatusCode::METHOD_NOT_ALLOWED);
        assert_private(&response);
    }
    for path in [
        "/api/team/catalog/repositories/1",
        "/api/team/catalog/chats",
        "/api/team/catalog/chats/1",
        "/api/team/nodes",
    ] {
        fixture.server.get(path).await.assert_status_not_found();
    }
}

#[tokio::test]
async fn http_catalog_every_write_rejects_missing_session_and_origin_or_csrf() {
    let fixture = fixture().await;
    for (method, path, body) in write_cases() {
        let response = write_json(&fixture.server, method.clone(), path, None, &body).await;
        response.assert_status_unauthorized();
        assert_private(&response);

        fixture
            .server
            .method(method.clone(), path)
            .add_header(header::ORIGIN, "https://evil.example")
            .json(&body)
            .await
            .assert_status_forbidden();

        fixture
            .server
            .method(method.clone(), path)
            .add_header(header::COOKIE, &fixture.alice_a.cookies)
            .add_header("x-workos-csrf", &fixture.alice_a.csrf)
            .json(&body)
            .await
            .assert_status_forbidden();

        fixture
            .server
            .method(method, path)
            .add_header(header::ORIGIN, ORIGIN)
            .add_header(
                header::COOKIE,
                format!("workos_team_session={}", fixture.alice_a.session),
            )
            .json(&body)
            .await
            .assert_status_unauthorized();
    }
}

#[tokio::test]
async fn http_catalog_rejects_ambiguous_headers_cookies_content_type_and_oversize_body() {
    let fixture = fixture().await;
    let body = serde_json::json!({"fullName":"atlas/repo","startingBranch":"main"});

    fixture
        .server
        .post("/api/team/catalog/repositories")
        .add_header(header::ORIGIN, ORIGIN)
        .add_header(header::ORIGIN, ORIGIN)
        .add_header(header::COOKIE, &fixture.alice_a.cookies)
        .add_header("x-workos-csrf", &fixture.alice_a.csrf)
        .json(&body)
        .await
        .assert_status_forbidden();
    fixture
        .server
        .post("/api/team/catalog/repositories")
        .add_header(header::ORIGIN, ORIGIN)
        .add_header(header::COOKIE, &fixture.alice_a.cookies)
        .add_header("x-workos-csrf", &fixture.alice_a.csrf)
        .add_header("x-workos-csrf", &fixture.alice_a.csrf)
        .json(&body)
        .await
        .assert_status_unauthorized();
    fixture
        .server
        .post("/api/team/catalog/repositories")
        .add_header(header::ORIGIN, ORIGIN)
        .add_header(header::COOKIE, &fixture.alice_a.cookies)
        .add_header("x-workos-csrf", &fixture.alice_a.csrf)
        .add_header(header::CONTENT_TYPE, "application/json")
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(Bytes::from_static(
            br#"{"fullName":"atlas/repo","startingBranch":"main"}"#,
        ))
        .await
        .assert_status_bad_request();
    fixture
        .server
        .post("/api/team/catalog/repositories")
        .add_header(header::ORIGIN, ORIGIN)
        .add_header(header::COOKIE, &fixture.alice_a.cookies)
        .add_header(header::COOKIE, &fixture.alice_a.cookies)
        .add_header("x-workos-csrf", &fixture.alice_a.csrf)
        .json(&body)
        .await
        .assert_status_unauthorized();
    fixture
        .server
        .post("/api/team/catalog/repositories")
        .add_header(header::ORIGIN, ORIGIN)
        .add_header(header::COOKIE, &fixture.alice_a.cookies)
        .add_header("x-workos-csrf", &fixture.alice_a.csrf)
        .add_header(header::CONTENT_TYPE, "text/plain")
        .bytes(Bytes::from_static(br#"{"fullName":"atlas/repo","startingBranch":"main"}"#))
        .await
        .assert_status_bad_request();
    let response = fixture
        .server
        .post("/api/team/catalog/repositories")
        .add_header(header::ORIGIN, ORIGIN)
        .add_header(header::COOKIE, &fixture.alice_a.cookies)
        .add_header("x-workos-csrf", &fixture.alice_a.csrf)
        .add_header(header::CONTENT_TYPE, "application/json")
        .bytes(Bytes::from(vec![b' '; 32 * 1024 + 1]))
        .await;
    response.assert_status_bad_request();
    assert_private(&response);
}

#[tokio::test]
async fn http_catalog_strict_dtos_reject_forged_authority_and_nested_unknown_fields() {
    let fixture = fixture().await;
    for (method, path, mut body) in write_cases() {
        let object = body.as_object_mut().expect("object body");
        object.insert("orgId".into(), serde_json::json!(202));
        object.insert("userId".into(), serde_json::json!(303));
        object.insert("role".into(), serde_json::json!("admin"));
        write_json(
            &fixture.server,
            method,
            path,
            Some(&fixture.alice_a),
            &body,
        )
        .await
        .assert_status_bad_request();
    }

    for (method, path, body) in [
        (
            Method::POST,
            "/api/team/catalog/nodes",
            serde_json::json!({
                "parentId":null,"kind":"project","name":"Atlas",
                "repositoryDefault":{"repositoryId":1,"startingBranch":"main","orgId":201}
            }),
        ),
        (
            Method::PATCH,
            "/api/team/catalog/nodes/1",
            serde_json::json!({
                "expectedRevision":"0","parentId":null,"name":"Atlas",
                "repositoryDefault":{"repositoryId":1,"startingBranch":"main","role":"admin"}
            }),
        ),
        (
            Method::POST,
            "/api/team/chats",
            serde_json::json!({
                "nodeId":1,
                "repositoryOverride":{"repositoryId":1,"startingBranch":"main","userId":301},
                "title":"Private","requestId":"123e4567-e89b-12d3-a456-426614174000",
                "visibility":"private"
            }),
        ),
    ] {
        write_json(
            &fixture.server,
            method,
            path,
            Some(&fixture.alice_a),
            &body,
        )
        .await
        .assert_status_bad_request();
    }
}

#[tokio::test]
async fn http_catalog_strict_ids_and_queries_reject_ambiguity_with_not_found_controls() {
    let fixture = fixture().await;
    for path in [
        "/api/team/chats/0",
        "/api/team/chats/-1",
        "/api/team/chats/01",
        "/api/team/chats/+1",
        "/api/team/chats/2147483648",
    ] {
        fixture
            .server
            .get(path)
            .add_header(header::COOKIE, &fixture.alice_a.cookies)
            .await
            .assert_status_bad_request();
    }
    for query in [
        "nodeId=0",
        "afterId=-1",
        "nodeId=01",
        "nodeId=1&nodeId=2",
        "afterId=1&afterId=2",
        "nodeId=1&unknown=2",
        "nodeId=",
        "nodeId=%2B1",
    ] {
        fixture
            .server
            .get(&format!("/api/team/chats?{query}"))
            .add_header(header::COOKIE, &fixture.alice_a.cookies)
            .await
            .assert_status_bad_request();
    }

    let missing = fixture
        .server
        .get("/api/team/chats/999999")
        .add_header(header::COOKIE, &fixture.alice_a.cookies)
        .await;
    missing.assert_status_not_found();
    assert_private(&missing);
    fixture
        .server
        .get("/api/team/chats?nodeId=999999")
        .add_header(header::COOKIE, &fixture.alice_a.cookies)
        .await
        .assert_status_not_found();

    write_json(
        &fixture.server,
        Method::PATCH,
        "/api/team/catalog/nodes/01",
        Some(&fixture.alice_a),
        &serde_json::json!({
            "expectedRevision":"0","parentId":null,"name":"Missing",
            "repositoryDefault":null
        }),
    )
    .await
    .assert_status_bad_request();
    write_json(
        &fixture.server,
        Method::PATCH,
        "/api/team/catalog/nodes/999999",
        Some(&fixture.alice_a),
        &serde_json::json!({
            "expectedRevision":"0","parentId":null,"name":"Missing",
            "repositoryDefault":null
        }),
    )
    .await
    .assert_status_not_found();
    write_json(
        &fixture.server,
        Method::PATCH,
        "/api/team/chats/01/visibility",
        Some(&fixture.alice_a),
        &serde_json::json!({"expectedRevision":"0","visibility":"private"}),
    )
    .await
    .assert_status_bad_request();
    write_json(
        &fixture.server,
        Method::PATCH,
        "/api/team/chats/999999/visibility",
        Some(&fixture.alice_a),
        &serde_json::json!({"expectedRevision":"0","visibility":"private"}),
    )
    .await
    .assert_status_not_found();
}

#[tokio::test]
async fn http_catalog_real_router_shares_only_explicit_audience_across_two_organizations() {
    let fixture = fixture().await;

    let repository = write_json(
        &fixture.server,
        Method::POST,
        "/api/team/catalog/repositories",
        Some(&fixture.alice_a),
        &serde_json::json!({"fullName":"atlas/repo","startingBranch":"main"}),
    )
    .await;
    repository.assert_status_ok();
    let repository: serde_json::Value = repository.json();
    assert_eq!(repository["githubRepositoryId"], "8080");
    assert_eq!(repository.as_object().expect("repository DTO").len(), 3);
    let repository_id = repository["id"].as_i64().expect("repository id");

    let project = write_json(
        &fixture.server,
        Method::POST,
        "/api/team/catalog/nodes",
        Some(&fixture.alice_a),
        &serde_json::json!({
            "parentId":null,"kind":"project","name":"Atlas",
            "repositoryDefault":{"repositoryId":repository_id,"startingBranch":"main"}
        }),
    )
    .await;
    project.assert_status_ok();
    let project: serde_json::Value = project.json();
    let project_id = project["id"].as_i64().expect("project id");

    let project = write_json(
        &fixture.server,
        Method::PATCH,
        &format!("/api/team/catalog/nodes/{project_id}"),
        Some(&fixture.alice_a),
        &serde_json::json!({
            "expectedRevision":"0","parentId":null,"name":"Atlas renamed",
            "repositoryDefault":{"repositoryId":repository_id,"startingBranch":"main"}
        }),
    )
    .await;
    project.assert_status_ok();
    assert_eq!(project.json::<serde_json::Value>()["revision"], "1");

    let nodes = fixture
        .server
        .get("/api/team/catalog/nodes")
        .add_header(header::COOKIE, &fixture.bob_a.cookies)
        .await;
    nodes.assert_status_ok();
    let nodes: serde_json::Value = nodes.json();
    assert_eq!(nodes.as_object().expect("list envelope").len(), 1);
    assert_eq!(nodes["items"].as_array().expect("node items").len(), 1);

    let alice_private = write_json(
        &fixture.server,
        Method::POST,
        "/api/team/chats",
        Some(&fixture.alice_a),
        &serde_json::json!({
            "nodeId":project_id,"repositoryOverride":null,"title":"Alice private",
            "requestId":"123e4567-e89b-12d3-a456-426614174001"
        }),
    )
    .await;
    alice_private.assert_status_ok();
    let alice_private: serde_json::Value = alice_private.json();
    assert_eq!(alice_private["visibility"], "private");
    let alice_chat_id = alice_private["id"].as_i64().expect("Alice chat id");

    fixture
        .server
        .get(&format!("/api/team/chats/{alice_chat_id}"))
        .add_header(header::COOKIE, &fixture.bob_a.cookies)
        .await
        .assert_status_not_found();
    let bob_before = fixture
        .server
        .get("/api/team/chats")
        .add_header(header::COOKIE, &fixture.bob_a.cookies)
        .await;
    bob_before.assert_status_ok();
    assert!(bob_before.json::<serde_json::Value>()["items"]
        .as_array()
        .expect("chat items")
        .is_empty());

    let bob_private = write_json(
        &fixture.server,
        Method::POST,
        "/api/team/chats",
        Some(&fixture.bob_a),
        &serde_json::json!({
            "nodeId":project_id,"repositoryOverride":null,"title":"Bob private",
            "requestId":"123e4567-e89b-12d3-a456-426614174002"
        }),
    )
    .await;
    bob_private.assert_status_ok();
    let bob_private: serde_json::Value = bob_private.json();
    let bob_chat_id = bob_private["id"].as_i64().expect("Bob chat id");
    fixture
        .server
        .get(&format!("/api/team/chats/{bob_chat_id}"))
        .add_header(header::COOKIE, &fixture.alice_a.cookies)
        .await
        .assert_status_not_found();

    let shared = write_json(
        &fixture.server,
        Method::PATCH,
        &format!("/api/team/chats/{alice_chat_id}/visibility"),
        Some(&fixture.alice_a),
        &serde_json::json!({"expectedRevision":"0","visibility":"organization"}),
    )
    .await;
    shared.assert_status_ok();
    let shared: serde_json::Value = shared.json();
    assert_eq!(shared["accessRevision"], "1");
    assert_eq!(shared["visibility"], "organization");

    let stale = write_json(
        &fixture.server,
        Method::PATCH,
        &format!("/api/team/chats/{alice_chat_id}/visibility"),
        Some(&fixture.alice_a),
        &serde_json::json!({"expectedRevision":"0","visibility":"private"}),
    )
    .await;
    stale.assert_status_conflict();
    assert_private(&stale);
    let bob_read = fixture
        .server
        .get(&format!("/api/team/chats/{alice_chat_id}"))
        .add_header(header::COOKIE, &fixture.bob_a.cookies)
        .await;
    bob_read.assert_status_ok();
    assert_eq!(bob_read.json::<serde_json::Value>()["id"], alice_chat_id);
    write_json(
        &fixture.server,
        Method::PATCH,
        &format!("/api/team/chats/{alice_chat_id}/visibility"),
        Some(&fixture.bob_a),
        &serde_json::json!({"expectedRevision":"1","visibility":"private"}),
    )
    .await
    .assert_status_not_found();
    fixture
        .server
        .get(&format!("/api/team/chats/{alice_chat_id}"))
        .add_header(header::COOKIE, &fixture.alice_b.cookies)
        .await
        .assert_status_not_found();

    let beacon_repository = write_json(
        &fixture.server,
        Method::POST,
        "/api/team/catalog/repositories",
        Some(&fixture.carol_b),
        &serde_json::json!({"fullName":"beacon/repo","startingBranch":"main"}),
    )
    .await;
    beacon_repository.assert_status_ok();
    let beacon_repository: serde_json::Value = beacon_repository.json();
    let beacon_repository_id = beacon_repository["id"].as_i64().expect("beacon repository id");
    let beacon_project = write_json(
        &fixture.server,
        Method::POST,
        "/api/team/catalog/nodes",
        Some(&fixture.carol_b),
        &serde_json::json!({
            "parentId":null,"kind":"project","name":"Beacon",
            "repositoryDefault":{"repositoryId":beacon_repository_id,"startingBranch":"main"}
        }),
    )
    .await;
    beacon_project.assert_status_ok();
    let beacon_project_id = beacon_project.json::<serde_json::Value>()["id"]
        .as_i64()
        .expect("beacon project id");
    let beacon_chat = write_json(
        &fixture.server,
        Method::POST,
        "/api/team/chats",
        Some(&fixture.carol_b),
        &serde_json::json!({
            "nodeId":beacon_project_id,"repositoryOverride":null,"title":"Beacon shared",
            "requestId":"123e4567-e89b-12d3-a456-426614174003",
            "visibility":"organization"
        }),
    )
    .await;
    beacon_chat.assert_status_ok();
    let beacon_chat_id = beacon_chat.json::<serde_json::Value>()["id"]
        .as_i64()
        .expect("beacon chat id");
    fixture
        .server
        .get(&format!("/api/team/chats/{beacon_chat_id}"))
        .add_header(header::COOKIE, &fixture.alice_b.cookies)
        .await
        .assert_status_ok();
    for outsider in [&fixture.alice_a, &fixture.bob_a] {
        fixture
            .server
            .get(&format!("/api/team/chats/{beacon_chat_id}"))
            .add_header(header::COOKIE, &outsider.cookies)
            .await
            .assert_status_not_found();
    }

    let repositories = fixture
        .server
        .get("/api/team/catalog/repositories")
        .add_header(header::COOKIE, &fixture.alice_a.cookies)
        .await;
    repositories.assert_status_ok();
    assert_eq!(
        repositories.json::<serde_json::Value>()["items"]
            .as_array()
            .expect("repository items")
            .len(),
        1
    );

    let filtered = fixture
        .server
        .get(&format!(
            "/api/team/chats?nodeId={project_id}&afterId={alice_chat_id}"
        ))
        .add_header(header::COOKIE, &fixture.bob_a.cookies)
        .await;
    filtered.assert_status_ok();
    let filtered: serde_json::Value = filtered.json();
    assert_eq!(filtered.as_object().expect("filtered list envelope").len(), 1);
    assert_eq!(
        filtered["items"].as_array().expect("filtered chat items")[0]["id"],
        bob_chat_id
    );

    let proof_requests = fixture.provider.requests();
    for token in [
        "Bearer alice-access",
        "Bearer bob-access",
        "Bearer carol-access",
    ] {
        assert!(
            proof_requests
                .iter()
                .any(|(authorization, _)| authorization == token),
            "missing repository proof with {token}"
        );
    }
}

#[tokio::test]
async fn http_catalog_safe_provider_errors_preserve_status_and_private_headers() {
    let fixture = fixture().await;
    let unavailable = write_json(
        &fixture.server,
        Method::POST,
        "/api/team/catalog/repositories",
        Some(&fixture.alice_a),
        &serde_json::json!({"fullName":"outage/repo","startingBranch":"main"}),
    )
    .await;
    unavailable.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    assert_private(&unavailable);

    let hidden = write_json(
        &fixture.server,
        Method::POST,
        "/api/team/catalog/repositories",
        Some(&fixture.alice_a),
        &serde_json::json!({"fullName":"missing/repo","startingBranch":"main"}),
    )
    .await;
    hidden.assert_status_not_found();
    assert_private(&hidden);
}

#[tokio::test]
async fn http_catalog_and_auth_router_buckets_exhaust_independently() {
    let provider = RepositoryProofServer::start().await;
    let auth_exhausted = state_with_repository_origin(provider.origin()).await;
    for _ in 0..100 {
        drop(
            auth_exhausted
                .auth_limiter
                .try_acquire()
                .expect("auth bucket token"),
        );
    }
    let server = TestServer::new(build_router(Arc::new(auth_exhausted))).expect("auth limit server");
    server
        .get("/api/team/session")
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS);
    server
        .get("/api/team/catalog/nodes")
        .await
        .assert_status_unauthorized();

    let catalog_exhausted = state_with_repository_origin(provider.origin()).await;
    for _ in 0..200 {
        drop(
            catalog_exhausted
                .catalog_limiter
                .try_acquire()
                .expect("catalog bucket token"),
        );
    }
    let server =
        TestServer::new(build_router(Arc::new(catalog_exhausted))).expect("catalog limit server");
    server
        .get("/api/team/catalog/nodes")
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS);
    server
        .get("/api/team/session")
        .await
        .assert_status_unauthorized();
}
