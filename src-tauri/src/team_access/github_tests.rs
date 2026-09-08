use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::Response,
    routing::any,
    Router,
};
use futures_util::stream;
use serde_json::{json, Value};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

use super::{
    github::{
        GithubEvidenceError, GithubMembershipClient, GithubMembershipSource,
    },
    session::TeamRole,
};

const TOKEN: &str = "team-github-fixture-token";

#[derive(Clone, Debug, Eq, PartialEq)]
struct RecordedRequest {
    method: Method,
    path: String,
    query: Option<String>,
    authorization: Option<String>,
    user_agent: Option<String>,
    accept: Option<String>,
    api_version: Option<String>,
}

enum FixtureBody {
    Bytes(Vec<u8>),
    Chunks(Vec<Vec<u8>>),
    Delayed(Duration, Vec<u8>),
}

struct FixtureResponse {
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: FixtureBody,
}

impl FixtureResponse {
    fn json(status: StatusCode, value: Value) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: FixtureBody::Bytes(serde_json::to_vec(&value).unwrap()),
        }
    }

    fn raw(status: StatusCode, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![],
            body: FixtureBody::Bytes(bytes.into()),
        }
    }

    fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

struct FixtureState {
    responses: Mutex<VecDeque<FixtureResponse>>,
    requests: Mutex<Vec<RecordedRequest>>,
}

struct FixtureServer {
    origin: String,
    state: Arc<FixtureState>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl FixtureServer {
    async fn start(responses: Vec<FixtureResponse>) -> Self {
        let state = Arc::new(FixtureState {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .fallback(any(fixture_handler))
            .with_state(Arc::clone(&state));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = receiver.await;
                })
                .await
                .unwrap();
        });
        Self {
            origin: format!("http://{address}"),
            state,
            shutdown: Some(shutdown),
            task,
        }
    }

    fn client(&self) -> GithubMembershipClient {
        GithubMembershipClient::new_for_test(&self.origin, Duration::from_secs(2)).unwrap()
    }

    fn client_with_timeout(&self, timeout: Duration) -> GithubMembershipClient {
        GithubMembershipClient::new_for_test(&self.origin, timeout).unwrap()
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.state.requests.lock().unwrap().clone()
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

async fn fixture_handler(
    State(state): State<Arc<FixtureState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    fn header(headers: &HeaderMap, name: &str) -> Option<String> {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }

    state.requests.lock().unwrap().push(RecordedRequest {
        method,
        path: uri.path().to_owned(),
        query: uri.query().map(str::to_owned),
        authorization: header(&headers, "authorization"),
        user_agent: header(&headers, "user-agent"),
        accept: header(&headers, "accept"),
        api_version: header(&headers, "x-github-api-version"),
    });
    let scripted = state
        .responses
        .lock()
        .unwrap()
        .pop_front()
        .expect("fixture received an unexpected request");
    let mut builder = Response::builder().status(scripted.status);
    for (name, value) in scripted.headers {
        builder = builder.header(name, value);
    }
    let body = match scripted.body {
        FixtureBody::Bytes(bytes) => Body::from(bytes),
        FixtureBody::Chunks(chunks) => Body::from_stream(stream::iter(
            chunks
                .into_iter()
                .map(Ok::<Vec<u8>, std::io::Error>),
        )),
        FixtureBody::Delayed(delay, bytes) => {
            tokio::time::sleep(delay).await;
            Body::from(bytes)
        }
    };
    builder.body(body).unwrap()
}

fn user(id: i64, login: &str) -> Value {
    json!({
        "id": id,
        "login": login,
        "name": "Alice Example",
        "avatar_url": "https://avatars.githubusercontent.com/u/301",
        "type": "User"
    })
}

fn membership(user_id: i64, org_id: i64, org_login: &str, state: &str, role: &str) -> Value {
    json!({
        "state": state,
        "role": role,
        "user": {"id": user_id, "login": "alice", "type": "User"},
        "organization": {
            "id": org_id,
            "login": org_login,
            "avatar_url": format!("https://avatars.githubusercontent.com/u/{org_id}")
        }
    })
}

#[tokio::test]
async fn identity_returns_validated_metadata_and_required_headers() {
    let server = FixtureServer::start(vec![FixtureResponse::json(
        StatusCode::OK,
        user(301, "alice"),
    )])
    .await;
    let identity = server.client().identity(TOKEN).await.unwrap();

    assert_eq!(identity.github_user_id, 301);
    assert_eq!(identity.login, "alice");
    assert_eq!(identity.name.as_deref(), Some("Alice Example"));
    assert_eq!(
        identity.avatar_url.as_deref(),
        Some("https://avatars.githubusercontent.com/u/301")
    );
    let requests = server.requests();
    assert_eq!(requests[0].method, Method::GET);
    assert_eq!(requests[0].path, "/user");
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some("Bearer team-github-fixture-token")
    );
    assert_eq!(requests[0].user_agent.as_deref(), Some("EazyWorkOS"));
    assert_eq!(
        requests[0].accept.as_deref(),
        Some("application/vnd.github+json")
    );
    assert_eq!(requests[0].api_version.as_deref(), Some("2022-11-28"));
}

#[tokio::test]
async fn identity_accepts_missing_optional_metadata_and_discards_unsafe_values() {
    let mut unsafe_metadata = user(301, "alice");
    unsafe_metadata["name"] = json!(format!("{}\n", "x".repeat(256)));
    unsafe_metadata["avatar_url"] = json!("https://evil.example/avatar.png");
    let server = FixtureServer::start(vec![
        FixtureResponse::json(
            StatusCode::OK,
            json!({"id": 301, "login": "alice", "type": "User"}),
        ),
        FixtureResponse::json(
            StatusCode::OK,
            json!({"id": 301, "login": "alice", "name": null, "avatar_url": null, "type": "User"}),
        ),
        FixtureResponse::json(StatusCode::OK, unsafe_metadata),
    ])
    .await;

    for _ in 0..3 {
        let identity = server.client().identity(TOKEN).await.unwrap();
        assert_eq!(
            (identity.github_user_id, identity.login.as_str()),
            (301, "alice")
        );
        assert_eq!(identity.name, None);
        assert_eq!(identity.avatar_url, None);
    }
}

#[tokio::test]
async fn identity_rejects_invalid_authority_shapes() {
    let cases = vec![
        json!({"id": 0, "login": "alice", "type": "User"}),
        json!({"id": -1, "login": "alice", "type": "User"}),
        json!({"login": "alice", "type": "User"}),
        json!({"id": 301, "type": "User"}),
        json!({"id": 301, "login": "alice", "type": "Bot"}),
        json!({"id": 301, "login": "-alice", "type": "User"}),
        json!({"id": 301, "login": "alice--ops", "type": "User"}),
    ];
    let mut responses: Vec<_> = cases
        .into_iter()
        .map(|body| FixtureResponse::json(StatusCode::OK, body))
        .collect();
    responses.push(FixtureResponse::raw(StatusCode::OK, b"not json".to_vec()));
    let server = FixtureServer::start(responses).await;

    for _ in 0..8 {
        assert_eq!(
            server.client().identity(TOKEN).await,
            Err(GithubEvidenceError::Unavailable)
        );
    }
}

#[tokio::test]
async fn individual_membership_maps_member_and_admin_roles() {
    let server = FixtureServer::start(vec![
        FixtureResponse::json(
            StatusCode::OK,
            membership(301, 202, "beta-company", "active", "member"),
        ),
        FixtureResponse::json(
            StatusCode::OK,
            membership(301, 202, "Beta-Company", "active", "admin"),
        ),
    ])
    .await;

    let member = server
        .client()
        .verify_membership(TOKEN, 301, 202, "beta-company")
        .await
        .unwrap();
    assert_eq!(member.github_user_id, 301);
    assert_eq!(member.organization.github_org_id, 202);
    assert_eq!(member.organization.role, TeamRole::Member);
    let admin = server
        .client()
        .verify_membership(TOKEN, 301, 202, "beta-company")
        .await
        .unwrap();
    assert_eq!(admin.organization.role, TeamRole::Admin);
    assert_eq!(server.requests()[0].path, "/user/memberships/orgs/beta-company");
}

#[tokio::test]
async fn individual_membership_rejects_stable_id_and_login_mismatches() {
    let server = FixtureServer::start(vec![
        FixtureResponse::json(StatusCode::OK, membership(999, 202, "beta-company", "active", "member")),
        FixtureResponse::json(StatusCode::OK, membership(301, 999, "beta-company", "active", "member")),
        FixtureResponse::json(StatusCode::OK, membership(301, 202, "other-company", "active", "member")),
    ]).await;

    for _ in 0..3 {
        assert_eq!(
            server.client().verify_membership(TOKEN, 301, 202, "beta-company").await,
            Err(GithubEvidenceError::NotMember)
        );
    }
}

#[tokio::test]
async fn individual_membership_rejects_pending_and_invalid_wire_values() {
    let server = FixtureServer::start(vec![
        FixtureResponse::json(StatusCode::OK, membership(301, 202, "beta-company", "pending", "member")),
        FixtureResponse::json(StatusCode::OK, membership(301, 202, "beta-company", "active", "owner")),
        FixtureResponse::json(StatusCode::OK, membership(301, 202, "beta-company", "mystery", "member")),
        FixtureResponse::json(StatusCode::OK, membership(301, 202, "beta-company", "active", "admin")).with_header("content-type", "application/json"),
    ]).await;

    assert_eq!(server.client().verify_membership(TOKEN, 301, 202, "beta-company").await, Err(GithubEvidenceError::NotMember));
    for _ in 0..2 {
        assert_eq!(server.client().verify_membership(TOKEN, 301, 202, "beta-company").await, Err(GithubEvidenceError::Unavailable));
    }
    // The final valid fixture proves the preceding failures were validation,
    // rather than an accidentally non-functional fixture server.
    assert!(server.client().verify_membership(TOKEN, 301, 202, "beta-company").await.is_ok());
}

#[tokio::test]
async fn active_organizations_returns_empty_without_personal_fallback() {
    let server = FixtureServer::start(vec![FixtureResponse::json(StatusCode::OK, json!([]))]).await;
    assert_eq!(server.client().active_organizations(TOKEN, 301).await.unwrap(), vec![]);
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn active_organizations_paginates_locally_sorts_and_excludes_pending() {
    let page_one: Vec<_> = (0..100)
        .map(|offset| membership(301, 1_000 + offset, &format!("org-{offset}"), "active", "member"))
        .collect();
    let page_two = json!([
        membership(301, 202, "beta-company", "pending", "member"),
        membership(301, 102, "alpha-company", "active", "admin")
    ]);
    let server = FixtureServer::start(vec![
        FixtureResponse::json(StatusCode::OK, json!(page_one)).with_header(
            "link",
            "<http://127.0.0.1:9/steal-token>; rel=\"next\"",
        ),
        FixtureResponse::json(StatusCode::OK, page_two),
    ]).await;

    let organizations = server.client().active_organizations(TOKEN, 301).await.unwrap();
    assert_eq!(organizations.len(), 101);
    assert_eq!(organizations[0].github_org_id, 102);
    assert_eq!(organizations[0].role, TeamRole::Admin);
    assert_eq!(organizations[1].github_org_id, 1_000);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, "/user/memberships/orgs");
    assert_eq!(requests[0].query.as_deref(), Some("state=active&per_page=100&page=1"));
    assert_eq!(requests[1].query.as_deref(), Some("state=active&per_page=100&page=2"));
}

#[tokio::test]
async fn active_organizations_rejects_duplicates_and_contradictory_rows() {
    let exact_duplicate = membership(301, 202, "beta-company", "active", "member");
    let server = FixtureServer::start(vec![
        FixtureResponse::json(StatusCode::OK, json!([exact_duplicate.clone(), exact_duplicate])),
        FixtureResponse::json(StatusCode::OK, json!([
            membership(301, 202, "beta-company", "active", "member"),
            membership(301, 202, "other-company", "active", "admin")
        ])),
    ]).await;

    for _ in 0..2 {
        assert_eq!(server.client().active_organizations(TOKEN, 301).await, Err(GithubEvidenceError::Unavailable));
    }
}

#[tokio::test]
async fn active_organizations_distinguishes_wrong_user_from_invalid_rows() {
    let mut invalid_user = membership(301, 202, "beta-company", "active", "member");
    invalid_user["user"]["type"] = json!("Bot");
    let server = FixtureServer::start(vec![
        FixtureResponse::json(StatusCode::OK, json!([membership(999, 202, "beta-company", "active", "member")])),
        FixtureResponse::json(StatusCode::OK, json!([invalid_user])),
        FixtureResponse::json(StatusCode::OK, json!([membership(301, 202, "beta-company", "active", "owner")])),
        FixtureResponse::json(StatusCode::OK, json!([membership(301, 202, "beta-company", "mystery", "member")])),
    ]).await;

    assert_eq!(server.client().active_organizations(TOKEN, 301).await, Err(GithubEvidenceError::NotMember));
    for _ in 0..3 {
        assert_eq!(server.client().active_organizations(TOKEN, 301).await, Err(GithubEvidenceError::Unavailable));
    }
}

#[tokio::test]
async fn one_hundred_full_pages_fail_instead_of_returning_truncated_success() {
    let responses = (0..100)
        .map(|page| {
            let rows: Vec<_> = (0..100)
                .map(|offset| {
                    let id = 10_000 + page * 100 + offset;
                    membership(301, id, &format!("org-{id}"), "active", "member")
                })
                .collect();
            FixtureResponse::json(StatusCode::OK, json!(rows))
        })
        .collect();
    let server = FixtureServer::start(responses).await;

    assert_eq!(server.client().active_organizations(TOKEN, 301).await, Err(GithubEvidenceError::Unavailable));
    assert_eq!(server.requests().len(), 100);
}

#[tokio::test]
async fn a_page_larger_than_the_requested_page_size_is_unavailable() {
    let oversized_page: Vec<_> = (0..101)
        .map(|offset| {
            membership(
                301,
                20_000 + offset,
                &format!("oversized-{offset}"),
                "active",
                "member",
            )
        })
        .collect();
    let server = FixtureServer::start(vec![
        FixtureResponse::json(StatusCode::OK, json!(oversized_page)),
        FixtureResponse::json(StatusCode::OK, json!([])),
    ])
    .await;

    assert_eq!(
        server.client().active_organizations(TOKEN, 301).await,
        Err(GithubEvidenceError::Unavailable)
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn statuses_map_without_turning_provider_failures_into_denials() {
    let server = FixtureServer::start(vec![
        FixtureResponse::raw(StatusCode::UNAUTHORIZED, ""),
        FixtureResponse::raw(StatusCode::FORBIDDEN, ""),
        FixtureResponse::raw(StatusCode::NOT_FOUND, ""),
        FixtureResponse::raw(StatusCode::FORBIDDEN, "").with_header("x-ratelimit-remaining", "0"),
        FixtureResponse::raw(StatusCode::FORBIDDEN, "").with_header("retry-after", "1"),
        FixtureResponse::raw(StatusCode::TOO_MANY_REQUESTS, ""),
        FixtureResponse::raw(StatusCode::INTERNAL_SERVER_ERROR, ""),
        FixtureResponse::raw(StatusCode::CREATED, "{}"),
    ]).await;

    assert_eq!(server.client().identity(TOKEN).await, Err(GithubEvidenceError::ReauthenticationRequired));
    assert_eq!(server.client().verify_membership(TOKEN, 301, 202, "beta-company").await, Err(GithubEvidenceError::NotMember));
    assert_eq!(server.client().verify_membership(TOKEN, 301, 202, "beta-company").await, Err(GithubEvidenceError::NotMember));
    for _ in 0..5 {
        assert_eq!(server.client().identity(TOKEN).await, Err(GithubEvidenceError::Unavailable));
    }
}

#[tokio::test]
async fn listing_non_rate_limited_forbidden_and_not_found_are_unavailable() {
    let server = FixtureServer::start(vec![
        FixtureResponse::raw(StatusCode::FORBIDDEN, ""),
        FixtureResponse::raw(StatusCode::NOT_FOUND, ""),
    ]).await;
    for _ in 0..2 {
        assert_eq!(server.client().active_organizations(TOKEN, 301).await, Err(GithubEvidenceError::Unavailable));
    }
}

#[tokio::test]
async fn redirects_are_rejected_without_contacting_the_target() {
    let target = FixtureServer::start(vec![FixtureResponse::json(StatusCode::OK, user(301, "alice"))]).await;
    let redirect = format!("{}/stolen", target.origin);
    let source = FixtureServer::start(vec![
        FixtureResponse::raw(StatusCode::FOUND, "").with_header("location", redirect),
    ]).await;

    assert_eq!(source.client().identity(TOKEN).await, Err(GithubEvidenceError::Unavailable));
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(target.requests(), vec![]);
}

#[tokio::test]
async fn invalid_inputs_fail_before_http_and_errors_do_not_leak_tokens() {
    let server = FixtureServer::start(vec![]).await;
    for token in ["", "  ", "abc\rdef", "abc\ndef"] {
        let error = server.client().identity(token).await.unwrap_err();
        assert_eq!(error, GithubEvidenceError::ReauthenticationRequired);
        if !token.is_empty() {
            assert!(!format!("{error:?} {error}").contains(token));
        }
    }
    for (user_id, org_id, login) in [(0, 202, "beta-company"), (301, 0, "beta-company"), (301, 202, "bad--login")] {
        assert_eq!(server.client().verify_membership(TOKEN, user_id, org_id, login).await, Err(GithubEvidenceError::NotMember));
    }
    assert_eq!(server.client().active_organizations(TOKEN, 0).await, Err(GithubEvidenceError::NotMember));
    assert_eq!(server.requests(), vec![]);
}

#[tokio::test]
async fn decoded_body_cap_rejects_oversized_chunked_response() {
    let chunks = (0..17).map(|_| vec![b'x'; 65_536]).collect();
    let server = FixtureServer::start(vec![FixtureResponse {
        status: StatusCode::OK,
        headers: vec![("content-type".into(), "application/json".into())],
        body: FixtureBody::Chunks(chunks),
    }]).await;

    assert_eq!(server.client().identity(TOKEN).await, Err(GithubEvidenceError::Unavailable));
}

#[tokio::test]
async fn complete_operation_timeout_is_unavailable() {
    let server = FixtureServer::start(vec![FixtureResponse {
        status: StatusCode::OK,
        headers: vec![("content-type".into(), "application/json".into())],
        body: FixtureBody::Delayed(Duration::from_millis(100), serde_json::to_vec(&user(301, "alice")).unwrap()),
    }]).await;

    assert_eq!(
        server.client_with_timeout(Duration::from_millis(20)).identity(TOKEN).await,
        Err(GithubEvidenceError::Unavailable)
    );
}
