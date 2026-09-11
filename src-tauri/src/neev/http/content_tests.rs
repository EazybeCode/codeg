use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{Path, State},
    http::{header, Request, StatusCode},
    response::IntoResponse,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sea_orm::DatabaseConnection;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::neev::{
    auth::session,
    catalog::nodes,
    test_support as h,
    types::{NodeInput, NodeKind},
};

use super::cookies::CookieNames;

struct Harness {
    server: axum_test::TestServer,
    cookies: String,
    member_cookies: String,
    csrf: String,
    node: i32,
    org_id: i32,
    database: DatabaseConnection,
}

impl Harness {
    async fn new() -> Self {
        let org = h::OrgHarness::new().await;
        let node = nodes::create(
            &org.state,
            org.owner_context(),
            NodeInput {
                parent_id: None,
                kind: NodeKind::Project,
                name: "Native content".into(),
            },
        )
        .await
        .unwrap()
        .id;
        let identity = session::issue_identity(
            &org.database,
            org.owner_context().identity.user_id,
            1_700_000_000,
        )
        .await
        .unwrap();
        let identity = session::identity(&org.database, &identity.session, 1_700_000_000)
            .await
            .unwrap();
        let scoped =
            session::select_organization(&org.database, &identity, org.org_id(), 1_700_000_000)
                .await
                .unwrap();
        let member_identity = session::issue_identity(
            &org.database,
            org.member_context().identity.user_id,
            1_700_000_000,
        )
        .await
        .unwrap();
        let member_identity =
            session::identity(&org.database, &member_identity.session, 1_700_000_000)
                .await
                .unwrap();
        let member_scoped = session::select_organization(
            &org.database,
            &member_identity,
            org.org_id(),
            1_700_000_000,
        )
        .await
        .unwrap();
        let names = CookieNames::for_preview(true);
        Self {
            server: axum_test::TestServer::new(super::router(org.state.clone())).unwrap(),
            cookies: format!(
                "{}={}; {}={}",
                names.session, scoped.session, names.csrf, scoped.csrf
            ),
            member_cookies: format!(
                "{}={}; {}={}",
                names.session, member_scoped.session, names.csrf, member_scoped.csrf
            ),
            csrf: scoped.csrf,
            node,
            org_id: org.org_id(),
            database: org.database.clone(),
        }
    }

    async fn mutation(&self, method: &str, path: &str, body: Value) -> axum_test::TestResponse {
        let request = match method {
            "POST" => self.server.post(path),
            "PATCH" => self.server.patch(path),
            _ => panic!("unsupported method"),
        };
        request
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, &self.cookies)
            .add_header("x-neev-csrf", &self.csrf)
            .json(&body)
            .await
    }

    async fn create(&self, kind: &str, origin: Option<&str>) -> Value {
        let mut body = json!({
            "title": format!("Native {kind}"),
            "requestId": uuid::Uuid::new_v4().to_string(),
        });
        if let Some(origin) = origin {
            body["originChatId"] = json!(origin);
        }
        let response = self
            .mutation(
                "POST",
                &format!("/api/neev/nodes/{}/{kind}s", self.node),
                body,
            )
            .await;
        response.assert_status(StatusCode::CREATED);
        response.json()
    }
}

#[tokio::test]
async fn content_routes_publish_strict_safe_wire_dtos() {
    let h = Harness::new().await;
    h.server
        .get(&format!("/api/neev/nodes/{}/chats", h.node))
        .await
        .assert_status_unauthorized();

    let supplied_origin = h
        .mutation(
            "POST",
            &format!("/api/neev/nodes/{}/chats", h.node),
            json!({
                "title":"Injected",
                "requestId":uuid::Uuid::new_v4().to_string(),
                "originChatId":null
            }),
        )
        .await;
    supplied_origin.assert_status_bad_request();

    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap();
    assert_eq!(chat["orgId"], h.org_id);
    assert_eq!(chat["nodeId"], h.node);
    assert_eq!(chat["kind"], "chat");
    assert_eq!(chat["authRevision"], "1");
    assert_eq!(chat["contentRevision"], "0");
    assert_eq!(chat["visibility"], "private");
    assert!(chat.get("originKind").is_none());

    h.server
        .get(&format!("/api/neev/resources/{chat_id}"))
        .add_header(header::COOKIE, &h.member_cookies)
        .await
        .assert_status_not_found();
    let hidden_cursor = URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&(h.org_id, h.node, "chat", false, chat_id)).unwrap());
    h.server
        .get(&format!(
            "/api/neev/nodes/{}/chats?after={hidden_cursor}",
            h.node
        ))
        .add_header(header::COOKIE, &h.member_cookies)
        .await
        .assert_status_not_found();

    let document = h.create("document", Some(chat_id)).await;
    let document_id = document["id"].as_str().unwrap();
    assert_eq!(document["originChatId"], chat_id);
    assert_eq!(document["visibility"], "private");

    let listed = h
        .server
        .get(&format!("/api/neev/nodes/{}/documents", h.node))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    listed.assert_status_ok();
    assert_eq!(listed.json::<Value>()["items"].as_array().unwrap().len(), 1);

    h.mutation(
        "PATCH",
        &format!("/api/neev/resources/{chat_id}"),
        json!({
            "expectedAuthRevision":"1","title":"Bad","nodeId":h.node,
            "visibility":"private","archived":false,"creatorId":999
        }),
    )
    .await
    .assert_status_bad_request();

    h.mutation(
        "POST",
        &format!("/api/neev/chats/{chat_id}/messages"),
        json!({
            "requestId":uuid::Uuid::new_v4().to_string(),
            "expectedAuthRevision":"1","body":"Hello","authorId":999
        }),
    )
    .await
    .assert_status_bad_request();

    let message = h
        .mutation(
            "POST",
            &format!("/api/neev/chats/{chat_id}/messages"),
            json!({
                "requestId":uuid::Uuid::new_v4().to_string(),
                "expectedAuthRevision":"1","body":"Hello"
            }),
        )
        .await;
    message.assert_status(StatusCode::CREATED);
    let message = message.json::<Value>();
    assert_eq!(message["sequence"], "1");
    assert_eq!(message["body"], "Hello");

    h.mutation(
        "POST",
        &format!("/api/neev/documents/{document_id}/revisions"),
        json!({
            "requestId":uuid::Uuid::new_v4().to_string(),
            "expectedContentRevision":"0","expectedAuthRevision":"1",
            "markdown":"# Bad","blobKey":"injected"
        }),
    )
    .await
    .assert_status_bad_request();

    h.mutation(
        "POST",
        &format!("/api/neev/documents/{document_id}/revisions"),
        json!({
            "requestId":uuid::Uuid::new_v4().to_string(),
            "expectedContentRevision":"0","expectedAuthRevision":"1",
            "markdown":"x".repeat(1048577)
        }),
    )
    .await
    .assert_status_bad_request();

    let saved = h
        .mutation(
            "POST",
            &format!("/api/neev/documents/{document_id}/revisions"),
            json!({
                "requestId":uuid::Uuid::new_v4().to_string(),
                "expectedContentRevision":"0","expectedAuthRevision":"1",
                "markdown":"# Safe"
            }),
        )
        .await;
    saved.assert_status(StatusCode::CREATED);
    let saved = saved.json::<Value>();
    assert_eq!(saved["revision"], "1");
    assert_eq!(saved["byteLength"], "6");
    assert!(saved.get("blobKey").is_none());

    let history = h
        .server
        .get(&format!("/api/neev/documents/{document_id}/revisions"))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    history.assert_status_ok();
    assert_eq!(
        history.json::<Value>()["items"].as_array().unwrap().len(),
        1
    );

    let content = h
        .server
        .get(&format!(
            "/api/neev/documents/{document_id}/content?revision=1"
        ))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    content.assert_status_ok();
    assert_eq!(content.json::<Value>()["markdown"], "# Safe");

    let download = h
        .server
        .get(&format!(
            "/api/neev/documents/{document_id}/download?revision=1"
        ))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    download.assert_status_ok();
    assert_eq!(
        download.header(header::CONTENT_TYPE),
        "text/markdown;charset=utf-8"
    );
    assert_eq!(
        download.header(header::CONTENT_DISPOSITION),
        "attachment; filename=\"document.md\""
    );
    assert_eq!(download.header("x-content-type-options"), "nosniff");
    assert_eq!(download.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(download.text(), "# Safe");

    let restored = h
        .mutation(
            "POST",
            &format!("/api/neev/documents/{document_id}/restore"),
            json!({
                "requestId":uuid::Uuid::new_v4().to_string(),
                "expectedContentRevision":"1","expectedAuthRevision":"1",
                "sourceRevision":"1"
            }),
        )
        .await;
    restored.assert_status(StatusCode::CREATED);
    assert_eq!(restored.json::<Value>()["restoredFrom"], "1");

    h::execute(
        &h.database,
        "UPDATE neev_document_revisions SET blob_key='not-a-uuid' WHERE org_id=$1 AND document_id=$2 AND revision=2",
        vec![h.org_id.into(), document_id.into()],
    )
    .await;
    let unavailable = h
        .server
        .get(&format!(
            "/api/neev/documents/{document_id}/content?revision=2"
        ))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    unavailable.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    unavailable.assert_json(&json!({"code":"storage_unavailable"}));

    h.server
        .get(&format!("/api/neev/resources/{}", uuid::Uuid::new_v4()))
        .add_header(header::COOKIE, &h.cookies)
        .await
        .assert_status_not_found();
}

#[tokio::test]
async fn content_mutations_enforce_origin_csrf_bounds_archive_and_conflicts() {
    let h = Harness::new().await;
    let path = format!("/api/neev/nodes/{}/chats", h.node);
    let body = json!({"title":"Chat","requestId":uuid::Uuid::new_v4().to_string()});
    h.server
        .post(&path)
        .add_header(header::ORIGIN, "https://evil.example")
        .add_header(header::COOKIE, &h.cookies)
        .add_header("x-neev-csrf", &h.csrf)
        .json(&body)
        .await
        .assert_status_forbidden();
    h.server
        .post(&path)
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &h.cookies)
        .add_header("x-neev-csrf", "wrong")
        .json(&body)
        .await
        .assert_status_unauthorized();

    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap();
    h.mutation(
        "POST",
        &format!("/api/neev/chats/{chat_id}/messages"),
        json!({
            "requestId":uuid::Uuid::new_v4().to_string(),
            "expectedAuthRevision":"1","body":"x".repeat(32769)
        }),
    )
    .await
    .assert_status_bad_request();
    h.mutation(
        "POST",
        &format!("/api/neev/chats/{chat_id}/messages"),
        json!({
            "requestId":uuid::Uuid::new_v4().to_string(),
            "expectedAuthRevision":"2","body":"stale"
        }),
    )
    .await
    .assert_status(StatusCode::CONFLICT);
    h.mutation(
        "PATCH",
        &format!("/api/neev/resources/{chat_id}"),
        json!({
            "expectedAuthRevision":"1","title":"Archived","nodeId":h.node,
            "visibility":"private","archived":true
        }),
    )
    .await
    .assert_status_ok();
    let archived = h
        .mutation(
            "POST",
            &format!("/api/neev/chats/{chat_id}/messages"),
            json!({
                "requestId":uuid::Uuid::new_v4().to_string(),
                "expectedAuthRevision":"2","body":"denied"
            }),
        )
        .await;
    archived.assert_status(StatusCode::CONFLICT);
    archived.assert_json(&json!({"code":"archived"}));

    h.server
        .get(&format!("/api/neev/nodes/{}/chats?after=private", h.node))
        .add_header(header::COOKIE, &h.cookies)
        .await
        .assert_status_not_found();

    h::execute(
        &h.database,
        "UPDATE neev_content_usage SET chats=10000 WHERE org_id=$1",
        vec![h.org_id.into()],
    )
    .await;
    let quota = h
        .mutation(
            "POST",
            &format!("/api/neev/nodes/{}/chats", h.node),
            json!({"title":"Over quota","requestId":uuid::Uuid::new_v4().to_string()}),
        )
        .await;
    quota.assert_status(StatusCode::TOO_MANY_REQUESTS);
    quota.assert_json(&json!({"code":"content_quota"}));
}

#[tokio::test]
async fn content_body_collection_reuses_the_thirty_second_deadline() {
    let org = h::OrgHarness::new().await;
    let mut state = Arc::try_unwrap(org.state).ok().unwrap();
    state.auth_limit = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let state = std::sync::Arc::new(state);
    tokio::time::pause();
    let pending_body =
        Body::from_stream(futures::stream::pending::<Result<Bytes, std::io::Error>>());
    let request = Request::builder()
        .method("POST")
        .uri("/api/neev/nodes/1/chats")
        .header(header::ORIGIN, "http://127.0.0.1:3100")
        .header(header::CONTENT_TYPE, "application/json")
        .body(pending_body)
        .unwrap();
    let pending = tokio::spawn(super::content::create_chat(
        State(state.clone()),
        Path(1),
        request,
    ));
    while state.auth_limit.available_permits() != 0 {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        super::content::create_chat(
            State(state.clone()),
            Path(1),
            Request::builder()
                .method("POST")
                .uri("/api/neev/nodes/1/chats")
                .header(header::ORIGIN, "http://127.0.0.1:3100")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await,
        Err(super::ApiError::Limited)
    ));
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    tokio::task::yield_now().await;
    let response = pending.await.unwrap().unwrap_err().into_response();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(
        serde_json::from_slice::<Value>(&to_bytes(response.into_body(), 1024).await.unwrap())
            .unwrap(),
        json!({"code":"request_timeout"})
    );
}
