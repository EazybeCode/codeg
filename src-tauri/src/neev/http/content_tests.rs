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

fn assert_safe_invalid(response: axum_test::TestResponse) {
    response.assert_status_bad_request();
    response.assert_json(&json!({"code":"invalid"}));
    assert_eq!(response.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(response.header("referrer-policy"), "no-referrer");
}

struct Harness {
    server: axum_test::TestServer,
    cookies: String,
    member_cookies: String,
    member_csrf: String,
    foreign_cookies: String,
    foreign_csrf: String,
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
        let foreign_identity = session::issue_identity(
            &org.database,
            org.other_org_context().identity.user_id,
            1_700_000_000,
        )
        .await
        .unwrap();
        let foreign_identity =
            session::identity(&org.database, &foreign_identity.session, 1_700_000_000)
                .await
                .unwrap();
        let foreign_scoped = session::select_organization(
            &org.database,
            &foreign_identity,
            org.other_org_context().org_id,
            1_700_000_000,
        )
        .await
        .unwrap();
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
            member_csrf: member_scoped.csrf,
            foreign_cookies: format!(
                "{}={}; {}={}",
                names.session, foreign_scoped.session, names.csrf, foreign_scoped.csrf
            ),
            foreign_csrf: foreign_scoped.csrf,
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
async fn rich_routes_round_trip_export_history_restore_and_reject_corruption() {
    let h = Harness::new().await;
    let resource = h.create("document", None).await;
    let id = resource["id"].as_str().unwrap();
    let path = format!("/api/neev/documents/{id}");
    let document = json!({"version":1,"blocks":[{"type":"paragraph","id":"00000000-0000-0000-0000-000000000000","content":[{"text":"Hello","styles":[],"href":null}],"children":[]}]});
    let input = json!({"requestId":uuid::Uuid::new_v4().to_string(),"expectedContentRevision":"0","expectedAuthRevision":"1","document":document});
    let saved = h
        .mutation("POST", &format!("{path}/revisions"), input.clone())
        .await;
    saved.assert_status(StatusCode::CREATED);
    let revision: Value = saved.json();
    assert_eq!(revision["format"], "blocks");
    assert_eq!(revision["formatVersion"], 1);
    assert_eq!(revision["byteLength"], "156");
    assert_eq!(
        revision["sha256"],
        "8f05bf0f0f2475feff5738cafac55f3e4aea98a3be5ddd4a59d23697aef8e6ea"
    );
    h.mutation("POST", &format!("{path}/revisions"), input.clone())
        .await
        .assert_json(&revision);
    let content = h
        .server
        .get(&format!("{path}/content"))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    content.assert_status_ok();
    content.assert_json(&json!({"revision":revision,"markdown":"Hello\n","document":document}));
    let download = h
        .server
        .get(&format!("{path}/download"))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    download.assert_status_ok();
    assert_eq!(download.text(), "Hello\n");
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
    assert_eq!(download.header("referrer-policy"), "no-referrer");
    h.mutation("POST", &format!("{path}/revisions"), json!({"requestId":uuid::Uuid::new_v4().to_string(),"expectedContentRevision":"1","expectedAuthRevision":"1","markdown":"legacy\r\n"})).await.assert_status(StatusCode::CREATED);
    let restored = h.mutation("POST", &format!("{path}/restore"), json!({"requestId":uuid::Uuid::new_v4().to_string(),"expectedContentRevision":"2","expectedAuthRevision":"1","sourceRevision":"1"})).await;
    restored.assert_status(StatusCode::CREATED);
    let restored: Value = restored.json();
    assert_eq!(restored["format"], "blocks");
    assert_eq!(restored["formatVersion"], 1);
    assert_eq!(restored["restoredFrom"], "1");
    assert_eq!(restored["parentRevision"], "2");
    assert_eq!(restored["sha256"], revision["sha256"]);
    let history = h
        .server
        .get(&format!("{path}/revisions"))
        .add_header(header::COOKIE, &h.cookies)
        .await
        .json::<Value>();
    assert_eq!(history["items"][0]["format"], "blocks");
    assert_eq!(history["items"][1]["format"], "markdown");
    assert_eq!(history["items"][2]["format"], "blocks");
    assert!(history["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["formatVersion"] == 1));
    assert_eq!(
        h.server
            .get(&format!("{path}/download?revision=2"))
            .add_header(header::COOKIE, &h.cookies)
            .await
            .text(),
        "legacy\r\n"
    );
    // Metadata is authoritative: a valid UTF-8 Markdown body labeled blocks is storage corruption.
    h::execute(&h.database,"UPDATE neev_document_formats SET format='blocks' WHERE org_id=$1 AND document_id=$2 AND revision=2",vec![h.org_id.into(),id.into()]).await;
    for endpoint in ["content?revision=2", "download?revision=2"] {
        let response = h
            .server
            .get(&format!("{path}/{endpoint}"))
            .add_header(header::COOKIE, &h.cookies)
            .await;
        response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
        response.assert_json(&json!({"code":"storage_unavailable"}));
        assert_eq!(response.header(header::CACHE_CONTROL), "no-store");
    }
}

#[tokio::test]
async fn rich_routes_enforce_typed_bodies_limits_and_existing_access_guards() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap();
    let document = json!({"version":1,"blocks":[{"type":"paragraph","id":"00000000-0000-0000-0000-000000000000","content":[{"text":"Hello","styles":[],"href":null}],"children":[]}]});
    for origin in [None, Some(chat_id)] {
        let resource = h.create("document", origin).await;
        let id = resource["id"].as_str().unwrap();
        let path = format!("/api/neev/documents/{id}");
        let input = json!({"requestId":uuid::Uuid::new_v4().to_string(),"expectedContentRevision":"0","expectedAuthRevision":"1","document":document});
        for (cookies, csrf) in [(&h.cookies, "wrong"), (&h.cookies, &h.csrf)] {
            let origin = if csrf == "wrong" {
                "http://127.0.0.1:3100"
            } else {
                "https://evil.example"
            };
            h.server
                .post(&format!("{path}/revisions"))
                .add_header(header::ORIGIN, origin)
                .add_header(header::COOKIE, cookies)
                .add_header("x-neev-csrf", csrf)
                .json(&input)
                .await
                .assert_status(if csrf == "wrong" {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::FORBIDDEN
                });
        }
        for (cookies, csrf) in [
            (&h.member_cookies, &h.member_csrf),
            (&h.foreign_cookies, &h.foreign_csrf),
        ] {
            h.server
                .post(&format!("{path}/revisions"))
                .add_header(header::ORIGIN, "http://127.0.0.1:3100")
                .add_header(header::COOKIE, cookies)
                .add_header("x-neev-csrf", csrf)
                .json(&input)
                .await
                .assert_status_not_found();
            for endpoint in ["content", "download", "revisions"] {
                h.server
                    .get(&format!("{path}/{endpoint}"))
                    .add_header(header::COOKIE, cookies)
                    .await
                    .assert_status_not_found();
            }
        }
        let mut both = input.clone();
        both["markdown"] = json!("both");
        let mut neither = input.clone();
        neither.as_object_mut().unwrap().remove("document");
        let mut unknown = input.clone();
        unknown["blobKey"] = json!("injected");
        let mut unsupported = input.clone();
        unsupported["document"]["blocks"][0]["type"] = json!("image");
        let mut version = input.clone();
        version["document"]["version"] = json!(2);
        let mut oversized = input.clone();
        oversized["document"]["blocks"][0]["content"][0]["text"] = json!("x".repeat(1048576));
        let mut expanded_export = input.clone();
        // Stored JSON fits, but each literal star requires a Markdown escape:
        // 524288 * 2 + the paragraph newline is one byte beyond the export cap.
        expanded_export["document"]["blocks"][0]["content"][0]["text"] = json!("*".repeat(524288));
        assert!(
            serde_json::to_vec(&expanded_export["document"])
                .unwrap()
                .len()
                < 1048576
        );
        let mut body_bound = input.clone();
        body_bound["document"]["blocks"][0]["content"][0]["text"] =
            json!("x".repeat(6 * 1048576 + 4096));
        for bad in [
            both,
            neither,
            unknown,
            unsupported,
            version,
            oversized,
            expanded_export,
            body_bound,
        ] {
            assert_safe_invalid(h.mutation("POST", &format!("{path}/revisions"), bad).await);
        }
        h.server
            .get(&format!("{path}/revisions"))
            .add_header(header::COOKIE, &h.cookies)
            .await
            .assert_json(&json!({"items":[],"nextAfter":null}));
        h.mutation("POST", &format!("{path}/revisions"), input.clone())
            .await
            .assert_status(StatusCode::CREATED);
        if origin.is_some() {
            h.mutation(
                "PATCH",
                &format!("/api/neev/resources/{chat_id}"),
                json!({
                    "expectedAuthRevision":"1","title":"Shared source","nodeId":h.node,
                    "visibility":"organization","archived":false
                }),
            )
            .await
            .assert_status_ok();
            h.server
                .get(&format!("{path}/content"))
                .add_header(header::COOKIE, &h.member_cookies)
                .await
                .assert_status_ok();
            h.mutation(
                "PATCH",
                &format!("/api/neev/resources/{chat_id}"),
                json!({
                    "expectedAuthRevision":"2","title":"Private source","nodeId":h.node,
                    "visibility":"private","archived":false
                }),
            )
            .await
            .assert_status_ok();
        }
        // Exercise denied reads after real rich content exists, including a
        // linked document whose source-chat audience was just revoked.
        for cookies in [&h.member_cookies, &h.foreign_cookies] {
            for endpoint in ["content", "download", "revisions"] {
                h.server
                    .get(&format!("{path}/{endpoint}"))
                    .add_header(header::COOKIE, cookies)
                    .await
                    .assert_status_not_found();
            }
        }
        let mut stale = input.clone();
        stale["requestId"] = json!(uuid::Uuid::new_v4().to_string());
        h.mutation("POST", &format!("{path}/revisions"), stale)
            .await
            .assert_status(StatusCode::CONFLICT);
    }
    let resource = h.create("document", None).await;
    let id = resource["id"].as_str().unwrap();
    h::execute(
        &h.database,
        "UPDATE neev_content_usage SET charged_bytes=1073741824 WHERE org_id=$1",
        vec![h.org_id.into()],
    )
    .await;
    let quota = h.mutation("POST",&format!("/api/neev/documents/{id}/revisions"),json!({"requestId":uuid::Uuid::new_v4().to_string(),"expectedContentRevision":"0","expectedAuthRevision":"1","document":document})).await;
    quota.assert_status(StatusCode::TOO_MANY_REQUESTS);
    quota.assert_json(&json!({"code":"content_quota"}));
}

#[tokio::test]
async fn malformed_content_extractors_use_only_private_json_errors() {
    let h = Harness::new().await;

    for path in [
        "/api/neev/nodes/not-an-id/chats",
        "/api/neev/nodes/not-an-id/documents",
        "/api/neev/resources/%FF",
        "/api/neev/chats/%FF/messages",
        "/api/neev/documents/%FF/revisions",
        "/api/neev/documents/%FF/content",
        "/api/neev/documents/%FF/download",
    ] {
        assert_safe_invalid(h.server.get(path).await);
    }

    for path in [
        "/api/neev/nodes/not-an-id/chats",
        "/api/neev/nodes/not-an-id/documents",
        "/api/neev/chats/%FF/messages",
        "/api/neev/documents/%FF/revisions",
        "/api/neev/documents/%FF/restore",
    ] {
        assert_safe_invalid(h.server.post(path).await);
    }
    assert_safe_invalid(h.server.patch("/api/neev/resources/%FF").await);

    for path in [
        "/api/neev/nodes/1/chats?archived=submitted-secret",
        "/api/neev/nodes/1/documents?archived=submitted-secret",
        "/api/neev/chats/00000000-0000-4000-8000-000000000000/messages?after=1&after=2",
        "/api/neev/documents/00000000-0000-4000-8000-000000000000/revisions?after=1&after=2",
        "/api/neev/documents/00000000-0000-4000-8000-000000000000/content?revision=1&revision=2",
        "/api/neev/documents/00000000-0000-4000-8000-000000000000/download?revision=1&revision=2",
    ] {
        assert_safe_invalid(h.server.get(path).await);
    }

    for path in [
        "/api/neev/nodes/1/chats?submittedSecret=value",
        "/api/neev/nodes/1/documents?submittedSecret=value",
        "/api/neev/chats/00000000-0000-4000-8000-000000000000/messages?submittedSecret=value",
        "/api/neev/documents/00000000-0000-4000-8000-000000000000/revisions?submittedSecret=value",
        "/api/neev/documents/00000000-0000-4000-8000-000000000000/content?submittedSecret=value",
        "/api/neev/documents/00000000-0000-4000-8000-000000000000/download?submittedSecret=value",
    ] {
        assert_safe_invalid(h.server.get(path).await);
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
    assert_eq!(saved["format"], "markdown");
    assert_eq!(saved["formatVersion"], 1);
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
    assert_eq!(content.json::<Value>().get("document"), Some(&Value::Null));

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
        "UPDATE neev_document_revisions SET blob_key='00000000-0000-4000-8000-000000000099' WHERE org_id=$1 AND document_id=$2 AND revision=2",
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

    let unavailable = h
        .server
        .get(&format!(
            "/api/neev/documents/{document_id}/download?revision=2"
        ))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    unavailable.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    unavailable.assert_json(&json!({"code":"storage_unavailable"}));
    let unavailable = h
        .mutation(
            "POST",
            &format!("/api/neev/documents/{document_id}/restore"),
            json!({
                "requestId":uuid::Uuid::new_v4().to_string(), "sourceRevision":"2",
                "expectedContentRevision":"2", "expectedAuthRevision":"1"
            }),
        )
        .await;
    unavailable.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    unavailable.assert_json(&json!({"code":"storage_unavailable"}));
    for (cookie, number) in [(&h.member_cookies, 2), (&h.cookies, 99)] {
        h.server
            .get(&format!(
                "/api/neev/documents/{document_id}/content?revision={number}"
            ))
            .add_header(header::COOKIE, cookie)
            .await
            .assert_status_not_found();
    }

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
        Ok(Path(1)),
        request,
    ));
    while state.auth_limit.available_permits() != 0 {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        super::content::create_chat(
            State(state.clone()),
            Ok(Path(1)),
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

/// Eight magic bytes and some padding: a real PNG as far as the sniffer is
/// concerned, which is the only opinion the server has about a file's type.
fn png(fill: u8) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&[fill; 24]);
    bytes
}

impl Harness {
    /// Upload one file to a conversation and return its stored metadata.
    async fn upload(&self, path: &str, name: &str, bytes: Vec<u8>) -> axum_test::TestResponse {
        self.server
            .post(path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, &self.cookies)
            .add_header("x-neev-csrf", &self.csrf)
            .add_header("x-neev-filename", name)
            .bytes(Bytes::from(bytes))
            .await
    }
}

/// The whole chat-attachment round trip over HTTP: upload, send, reload, retry.
///
/// This is the path a person actually drives — drag a file in, send, come back
/// tomorrow — so it is tested end to end rather than a layer at a time. What it
/// pins down: the files survive the reload in the order they were sent, a send
/// with files and no words is recorded as a real ask instead of an empty row,
/// the bytes are downloadable by someone authorized for the chat, and a retry
/// means the same thing it meant the first time or is refused.
#[tokio::test]
async fn chat_attachments_persist_reload_and_replay() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let upload_path = format!("/api/neev/chats/{chat_id}/attachments");

    let first = h.upload(&upload_path, "notes.png", png(1)).await;
    first.assert_status(StatusCode::CREATED);
    let first: Value = first.json();
    assert_eq!(first["fileName"], "notes.png");
    assert_eq!(first["contentType"], "image/png");
    assert_eq!(first["byteLength"], 32);
    let second: Value = h.upload(&upload_path, "second.png", png(2)).await.json();
    let (key_a, key_b) = (
        first["key"].as_str().unwrap().to_owned(),
        second["key"].as_str().unwrap().to_owned(),
    );

    // Files with no words: the durable body is the server's own sentence, so
    // the room sees an ask rather than a blank message with chips under it.
    let request_id = uuid::Uuid::new_v4().to_string();
    let send = json!({
        "requestId": request_id.clone(),
        "expectedAuthRevision": "1",
        "body": "",
        "attachmentKeys": [key_a.clone(), key_b.clone()],
    });
    let sent = h
        .mutation(
            "POST",
            &format!("/api/neev/chats/{chat_id}/messages"),
            send.clone(),
        )
        .await;
    sent.assert_status(StatusCode::CREATED);
    let message: Value = sent.json();
    assert_eq!(message["body"], "Please review the attached files.");
    let carried = message["attachments"].as_array().unwrap();
    assert_eq!(carried.len(), 2);
    assert_eq!(carried[0]["key"], json!(key_a.clone()));
    assert_eq!(carried[0]["fileName"], "notes.png");
    assert_eq!(carried[1]["key"], json!(key_b.clone()));

    // A reload reads the same thing from storage, in the same order.
    let listed = h
        .server
        .get(&format!("/api/neev/chats/{chat_id}/messages"))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    listed.assert_status_ok();
    let listed: Value = listed.json();
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["attachments"], message["attachments"]);

    // The bytes themselves are served to someone authorized for the chat, and
    // never as something the browser will render in our own origin.
    let download = h
        .server
        .get(&format!("/api/neev/attachments/{key_a}"))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    download.assert_status_ok();
    assert_eq!(download.header(header::CONTENT_DISPOSITION), "attachment");
    assert_eq!(download.as_bytes().as_ref(), png(1).as_slice());

    // The same request id and the same files replays the same message.
    let replay = h
        .mutation("POST", &format!("/api/neev/chats/{chat_id}/messages"), send)
        .await;
    replay.assert_status(StatusCode::CREATED);
    assert_eq!(replay.json::<Value>(), message);

    // The same request id with DIFFERENT files is a different ask wearing one
    // request id, and is refused rather than quietly replayed.
    let swapped = h
        .mutation(
            "POST",
            &format!("/api/neev/chats/{chat_id}/messages"),
            json!({
                "requestId": request_id,
                "expectedAuthRevision": "1",
                "body": "",
                "attachmentKeys": [key_a.clone()],
            }),
        )
        .await;
    swapped.assert_status(StatusCode::CONFLICT);
}

/// A key is not a capability: it names a file in ONE conversation, and the
/// conversation it belongs to is proved when the message is written, not taken
/// from the sender's word.
#[tokio::test]
async fn a_message_cannot_carry_a_file_from_another_chat() {
    let h = Harness::new().await;
    let mine = h.create("chat", None).await;
    let other = h.create("chat", None).await;
    let mine_id = mine["id"].as_str().unwrap().to_owned();
    let other_id = other["id"].as_str().unwrap().to_owned();
    let elsewhere: Value = h
        .upload(
            &format!("/api/neev/chats/{other_id}/attachments"),
            "other.png",
            png(3),
        )
        .await
        .json();
    let key = elsewhere["key"].as_str().unwrap().to_owned();

    let refused = h
        .mutation(
            "POST",
            &format!("/api/neev/chats/{mine_id}/messages"),
            json!({
                "requestId": uuid::Uuid::new_v4().to_string(),
                "expectedAuthRevision": "1",
                "body": "look at this",
                "attachmentKeys": [key.clone()],
            }),
        )
        .await;
    assert_safe_invalid(refused);

    // And nothing was written: the refusal took the whole append with it.
    let listed: Value = h
        .server
        .get(&format!("/api/neev/chats/{mine_id}/messages"))
        .add_header(header::COOKIE, &h.cookies)
        .await
        .json();
    assert!(listed["items"].as_array().unwrap().is_empty());
}

/// A send with no words and no files is still nothing to say.
#[tokio::test]
async fn an_empty_message_without_files_is_still_refused() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap();
    assert_safe_invalid(
        h.mutation(
            "POST",
            &format!("/api/neev/chats/{chat_id}/messages"),
            json!({
                "requestId": uuid::Uuid::new_v4().to_string(),
                "expectedAuthRevision": "1",
                "body": "",
            }),
        )
        .await,
    );
}

/// The wire shape of a message, for clients that are already open.
///
/// A deployed client parses this response with a strict schema: a field it has
/// never heard of is not "extra information", it is a message it refuses. So a
/// message that names nobody — every message written before this existed, and
/// every agent-authored one — must serialize EXACTLY as it did before, and the
/// mentions key must appear only when there is something in it.
#[test]
fn a_message_naming_nobody_serializes_as_it_always_did() {
    use crate::neev::content::{
        mentions::StoredMention,
        types::{AuthorKind, Message},
    };
    let message = || Message {
        id: "11111111-1111-4111-8111-111111111111".into(),
        sequence: 1,
        author_id: 7,
        author_name: "Ana Duarte".into(),
        body: "hi @Ana Duarte there".into(),
        created_at: 1_700_000_000,
        author_kind: AuthorKind::Human,
        agent_label: None,
    };

    let plain = serde_json::to_value(
        super::content::message_dto(message(), Vec::new(), Vec::new()).unwrap(),
    )
    .unwrap();
    assert!(
        plain.get("mentions").is_none(),
        "a message with no mentions must not carry the key: {plain}"
    );
    // The neighbouring field is the control: `attachments` IS always present,
    // so this is about the new key and not about empty collections generally.
    assert_eq!(plain.get("attachments"), Some(&json!([])));

    let named = serde_json::to_value(
        super::content::message_dto(
            message(),
            Vec::new(),
            vec![StoredMention {
                user_id: 42,
                start: 3,
                length: 11,
            }],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        named.get("mentions"),
        Some(&json!([{"userId": 42, "start": 3, "length": 11}]))
    );
}

// ============================================================================
// Attachment soft-delete (archive/restore/trash listing)
// ============================================================================

/// Archiving an attachment makes it undownloadable; restoring brings it back.
#[tokio::test]
async fn archived_attachment_cannot_be_downloaded_but_can_be_restored() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let upload_path = format!("/api/neev/chats/{chat_id}/attachments");

    let uploaded: Value = h.upload(&upload_path, "deleteme.png", png(1)).await.json();
    let key = uploaded["key"].as_str().unwrap().to_owned();

    // The file is downloadable before archiving.
    let before = h
        .server
        .get(&format!("/api/neev/attachments/{key}"))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    before.assert_status_ok();

    // Archive the attachment.
    let archived = h
        .mutation(
            "PATCH",
            &format!("/api/neev/attachments/{key}"),
            json!({"archived": true}),
        )
        .await;
    archived.assert_status_ok();
    archived.assert_json(&json!({"ok": true}));

    // Now the file is not downloadable.
    let after = h
        .server
        .get(&format!("/api/neev/attachments/{key}"))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    after.assert_status_not_found();

    // Restore the attachment.
    let restored = h
        .mutation(
            "PATCH",
            &format!("/api/neev/attachments/{key}"),
            json!({"archived": false}),
        )
        .await;
    restored.assert_status_ok();
    restored.assert_json(&json!({"ok": true}));

    // The file is downloadable again.
    let final_check = h
        .server
        .get(&format!("/api/neev/attachments/{key}"))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    final_check.assert_status_ok();
    assert_eq!(final_check.as_bytes().as_ref(), png(1).as_slice());
}

/// Archiving is idempotent: archive+archive and restore+restore are fine.
#[tokio::test]
async fn archiving_and_restoring_are_idempotent() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let uploaded: Value = h
        .upload(
            &format!("/api/neev/chats/{chat_id}/attachments"),
            "test.png",
            png(1),
        )
        .await
        .json();
    let key = uploaded["key"].as_str().unwrap().to_owned();

    // Archive twice.
    for _ in 0..2 {
        let resp = h
            .mutation(
                "PATCH",
                &format!("/api/neev/attachments/{key}"),
                json!({"archived": true}),
            )
            .await;
        resp.assert_status_ok();
    }

    // Restore twice.
    for _ in 0..2 {
        let resp = h
            .mutation(
                "PATCH",
                &format!("/api/neev/attachments/{key}"),
                json!({"archived": false}),
            )
            .await;
        resp.assert_status_ok();
    }
}

/// Archiving requires WRITE authorization (same as upload).
#[tokio::test]
async fn archiving_requires_write_authorization() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let uploaded: Value = h
        .upload(
            &format!("/api/neev/chats/{chat_id}/attachments"),
            "owned.png",
            png(1),
        )
        .await
        .json();
    let key = uploaded["key"].as_str().unwrap().to_owned();

    // Another org member (not the chat owner) cannot archive on private chat.
    let member_archive = h
        .server
        .patch(&format!("/api/neev/attachments/{key}"))
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &h.member_cookies)
        .add_header("x-neev-csrf", &h.member_csrf)
        .json(&json!({"archived": true}))
        .await;
    member_archive.assert_status_not_found();

    // Foreign user cannot archive.
    let foreign_archive = h
        .server
        .patch(&format!("/api/neev/attachments/{key}"))
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &h.foreign_cookies)
        .add_header("x-neev-csrf", &h.foreign_csrf)
        .json(&json!({"archived": true}))
        .await;
    foreign_archive.assert_status_not_found();
}

/// Archiving respects CSRF and origin checks.
#[tokio::test]
async fn archiving_requires_csrf_and_origin() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let uploaded: Value = h
        .upload(
            &format!("/api/neev/chats/{chat_id}/attachments"),
            "csrf.png",
            png(1),
        )
        .await
        .json();
    let key = uploaded["key"].as_str().unwrap().to_owned();

    // Wrong CSRF.
    let wrong_csrf = h
        .server
        .patch(&format!("/api/neev/attachments/{key}"))
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &h.cookies)
        .add_header("x-neev-csrf", "wrong")
        .json(&json!({"archived": true}))
        .await;
    wrong_csrf.assert_status_unauthorized();

    // Wrong origin.
    let wrong_origin = h
        .server
        .patch(&format!("/api/neev/attachments/{key}"))
        .add_header(header::ORIGIN, "https://evil.example")
        .add_header(header::COOKIE, &h.cookies)
        .add_header("x-neev-csrf", &h.csrf)
        .json(&json!({"archived": true}))
        .await;
    wrong_origin.assert_status_forbidden();
}

/// Archiving a nonexistent attachment returns not found (uniform response).
#[tokio::test]
async fn archiving_nonexistent_attachment_returns_not_found() {
    let h = Harness::new().await;
    let fake_key = "00000000-0000-4000-8000-000000000000";
    let response = h
        .mutation(
            "PATCH",
            &format!("/api/neev/attachments/{fake_key}"),
            json!({"archived": true}),
        )
        .await;
    response.assert_status_not_found();
}

/// Archived attachments are excluded from message listings (for_messages).
#[tokio::test]
async fn archived_attachments_are_excluded_from_message_listings() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let upload_path = format!("/api/neev/chats/{chat_id}/attachments");

    let first: Value = h.upload(&upload_path, "keep.png", png(1)).await.json();
    let second: Value = h.upload(&upload_path, "delete.png", png(2)).await.json();
    let first_key = first["key"].as_str().unwrap().to_owned();
    let second_key = second["key"].as_str().unwrap().to_owned();

    // Send a message with both attachments.
    let send = h
        .mutation(
            "POST",
            &format!("/api/neev/chats/{chat_id}/messages"),
            json!({
                "requestId": uuid::Uuid::new_v4().to_string(),
                "expectedAuthRevision": "1",
                "body": "look at these",
                "attachmentKeys": [first_key.clone(), second_key.clone()],
            }),
        )
        .await;
    send.assert_status(StatusCode::CREATED);
    let message: Value = send.json();
    assert_eq!(message["attachments"].as_array().unwrap().len(), 2);

    // Archive the second attachment.
    h.mutation(
        "PATCH",
        &format!("/api/neev/attachments/{second_key}"),
        json!({"archived": true}),
    )
    .await
    .assert_status_ok();

    // Reload messages: only one attachment visible.
    let listed: Value = h
        .server
        .get(&format!("/api/neev/chats/{chat_id}/messages"))
        .add_header(header::COOKIE, &h.cookies)
        .await
        .json();
    let attachments = listed["items"][0]["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["key"], first_key.as_str());
}

/// GET /api/neev/chats/{id}/attachments?archived=true lists only archived.
#[tokio::test]
async fn list_archived_chat_attachments() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let upload_path = format!("/api/neev/chats/{chat_id}/attachments");

    let kept: Value = h.upload(&upload_path, "keep.png", png(1)).await.json();
    let deleted: Value = h.upload(&upload_path, "delete.png", png(2)).await.json();
    let deleted_key = deleted["key"].as_str().unwrap().to_owned();

    // Archive one attachment.
    h.mutation(
        "PATCH",
        &format!("/api/neev/attachments/{deleted_key}"),
        json!({"archived": true}),
    )
    .await
    .assert_status_ok();

    // List archived attachments.
    let listed = h
        .server
        .get(&format!(
            "/api/neev/chats/{chat_id}/attachments?archived=true"
        ))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    listed.assert_status_ok();
    let body: Value = listed.json();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["key"], deleted_key.as_str());
    assert_eq!(items[0]["fileName"], "delete.png");
    assert_eq!(items[0]["contentType"], "image/png");
    assert_eq!(items[0]["byteLength"], 32);
    assert_eq!(body["nextAfter"], Value::Null);
}

/// List archived attachments requires READ authorization.
#[tokio::test]
async fn list_archived_requires_read_authorization() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let deleted: Value = h
        .upload(
            &format!("/api/neev/chats/{chat_id}/attachments"),
            "del.png",
            png(1),
        )
        .await
        .json();
    let key = deleted["key"].as_str().unwrap().to_owned();

    h.mutation(
        "PATCH",
        &format!("/api/neev/attachments/{key}"),
        json!({"archived": true}),
    )
    .await
    .assert_status_ok();

    // Another org member cannot list archived on private chat.
    h.server
        .get(&format!(
            "/api/neev/chats/{chat_id}/attachments?archived=true"
        ))
        .add_header(header::COOKIE, &h.member_cookies)
        .await
        .assert_status_not_found();

    // Foreign user cannot list archived.
    h.server
        .get(&format!(
            "/api/neev/chats/{chat_id}/attachments?archived=true"
        ))
        .add_header(header::COOKIE, &h.foreign_cookies)
        .await
        .assert_status_not_found();
}

/// Pagination for archived attachments listing (cap 100, cursor support).
#[tokio::test]
async fn list_archived_attachments_with_pagination() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let upload_path = format!("/api/neev/chats/{chat_id}/attachments");

    // Upload and archive 3 attachments.
    let mut keys = Vec::new();
    for i in 0..3 {
        let uploaded: Value = h
            .upload(&upload_path, &format!("file{i}.png"), png(i as u8))
            .await
            .json();
        let key = uploaded["key"].as_str().unwrap().to_owned();
        h.mutation(
            "PATCH",
            &format!("/api/neev/attachments/{key}"),
            json!({"archived": true}),
        )
        .await
        .assert_status_ok();
        keys.push(key);
    }

    // First page (no cursor).
    let first_page: Value = h
        .server
        .get(&format!(
            "/api/neev/chats/{chat_id}/attachments?archived=true"
        ))
        .add_header(header::COOKIE, &h.cookies)
        .await
        .json();
    assert!(first_page["items"].as_array().unwrap().len() <= 100);

    // If nextAfter is present, use it for pagination.
    if let Some(cursor) = first_page["nextAfter"].as_str() {
        let next_page: Value = h
            .server
            .get(&format!(
                "/api/neev/chats/{chat_id}/attachments?archived=true&after={cursor}"
            ))
            .add_header(header::COOKIE, &h.cookies)
            .await
            .json();
        assert!(next_page["items"].is_array());
    }
}

/// Quota is not double-counted when archiving/restoring.
/// Attachments use their own storage accounting via SUM(byte_length) from the
/// attachments table, NOT neev_content_usage.
#[tokio::test]
async fn archiving_does_not_affect_quota_accounting() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let uploaded: Value = h
        .upload(
            &format!("/api/neev/chats/{chat_id}/attachments"),
            "quota.png",
            png(1),
        )
        .await
        .json();
    let key = uploaded["key"].as_str().unwrap().to_owned();
    let byte_length = uploaded["byteLength"].as_i64().unwrap();

    // Check initial quota usage (attachments use SUM(byte_length) from the table).
    let initial_usage: i64 = crate::neev::db::one(
        &h.database,
        "SELECT CAST(COALESCE(SUM(byte_length),0) AS BIGINT) AS n FROM neev_attachments WHERE org_id=$1",
        vec![h.org_id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "n")
    .unwrap();
    assert!(initial_usage >= byte_length);

    // Archive.
    h.mutation(
        "PATCH",
        &format!("/api/neev/attachments/{key}"),
        json!({"archived": true}),
    )
    .await
    .assert_status_ok();

    // Quota unchanged (soft delete, blob retained).
    let after_archive: i64 = crate::neev::db::one(
        &h.database,
        "SELECT CAST(COALESCE(SUM(byte_length),0) AS BIGINT) AS n FROM neev_attachments WHERE org_id=$1",
        vec![h.org_id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "n")
    .unwrap();
    assert_eq!(initial_usage, after_archive);

    // Restore.
    h.mutation(
        "PATCH",
        &format!("/api/neev/attachments/{key}"),
        json!({"archived": false}),
    )
    .await
    .assert_status_ok();

    // Quota still unchanged.
    let after_restore: i64 = crate::neev::db::one(
        &h.database,
        "SELECT CAST(COALESCE(SUM(byte_length),0) AS BIGINT) AS n FROM neev_attachments WHERE org_id=$1",
        vec![h.org_id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "n")
    .unwrap();
    assert_eq!(initial_usage, after_restore);
}

/// New uploads default to non-archived (active).
#[tokio::test]
async fn new_uploads_default_to_active() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    let uploaded: Value = h
        .upload(
            &format!("/api/neev/chats/{chat_id}/attachments"),
            "new.png",
            png(1),
        )
        .await
        .json();
    let key = uploaded["key"].as_str().unwrap().to_owned();

    // Should be downloadable immediately (not archived).
    let download = h
        .server
        .get(&format!("/api/neev/attachments/{key}"))
        .add_header(header::COOKIE, &h.cookies)
        .await;
    download.assert_status_ok();

    // Should NOT appear in archived listing.
    let archived: Value = h
        .server
        .get(&format!(
            "/api/neev/chats/{chat_id}/attachments?archived=true"
        ))
        .add_header(header::COOKIE, &h.cookies)
        .await
        .json();
    assert!(archived["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn organization_attachment_trash_respects_current_chat_acl_and_restore() {
    let h = Harness::new().await;
    let chat = h.create("chat", None).await;
    let id = chat["id"].as_str().unwrap();
    let uploaded: Value = h
        .upload(
            &format!("/api/neev/chats/{id}/attachments"),
            "actual-type.txt",
            png(1),
        )
        .await
        .json();
    let key = uploaded["key"].as_str().unwrap();
    h.mutation(
        "PATCH",
        &format!("/api/neev/attachments/{key}"),
        json!({"archived":true}),
    )
    .await
    .assert_status_ok();
    let route = "/api/neev/attachments?archived=true";
    let own = h
        .server
        .get(route)
        .add_header(header::COOKIE, &h.cookies)
        .await;
    own.assert_status_ok();
    let own: Value = own.json();
    assert_eq!(own["items"][0]["key"], key);
    assert_eq!(own["items"][0]["contentType"], "image/png");
    for cookies in [&h.member_cookies, &h.foreign_cookies] {
        let page = h
            .server
            .get(route)
            .add_header(header::COOKIE, cookies)
            .await;
        page.assert_status_ok();
        assert!(page.json::<Value>()["items"].as_array().unwrap().is_empty());
    }
    // Changing the parent audience changes trash visibility immediately.
    crate::neev::db::execute(
        &h.database,
        "UPDATE neev_resources SET visibility='organization' WHERE id=$1",
        vec![id.to_owned().into()],
    )
    .await
    .unwrap();
    let shared = h
        .server
        .get(route)
        .add_header(header::COOKIE, &h.member_cookies)
        .await;
    shared.assert_status_ok();
    assert_eq!(shared.json::<Value>()["items"][0]["key"], key);
    crate::neev::db::execute(
        &h.database,
        "UPDATE neev_resources SET visibility='private' WHERE id=$1",
        vec![id.to_owned().into()],
    )
    .await
    .unwrap();
    let hidden = h
        .server
        .get(route)
        .add_header(header::COOKIE, &h.member_cookies)
        .await;
    assert!(hidden.json::<Value>()["items"]
        .as_array()
        .unwrap()
        .is_empty());
    h.mutation(
        "PATCH",
        &format!("/api/neev/attachments/{key}"),
        json!({"archived":false}),
    )
    .await
    .assert_status_ok();
    assert!(h
        .server
        .get(route)
        .add_header(header::COOKIE, &h.cookies)
        .await
        .json::<Value>()["items"]
        .as_array()
        .unwrap()
        .is_empty());
    h.server
        .get("/api/neev/attachments?archived=true&after=bad")
        .add_header(header::COOKIE, &h.cookies)
        .await
        .assert_status_bad_request();
}

impl Harness {
    async fn as_member(&self, method: &str, path: &str, body: Value) -> axum_test::TestResponse {
        let request = match method {
            "POST" => self.server.post(path),
            "PATCH" => self.server.patch(path),
            _ => panic!("unsupported method"),
        };
        request
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, &self.member_cookies)
            .add_header("x-neev-csrf", &self.member_csrf)
            .json(&body)
            .await
    }

    async fn sharing(&self, cookies: &str, node: i32) -> axum_test::TestResponse {
        self.server
            .get(&format!("/api/neev/nodes/{node}/sharing"))
            .add_header(header::COOKIE, cookies)
            .await
    }
}

#[tokio::test]
async fn node_sharing_routes_publish_the_contract_and_enforce_authority() {
    let h = Harness::new().await;
    let path = format!("/api/neev/nodes/{}/sharing", h.node);
    let initial = h.sharing(&h.cookies, h.node).await;
    initial.assert_status_ok();
    assert_eq!(initial.header(header::CACHE_CONTROL), "no-store");
    let initial: Value = initial.json();
    assert_eq!(
        initial,
        json!({"nodeId":h.node,"revision":initial["revision"],"defaultVisibility":"inherit","effectiveVisibility":"private","inheritedFrom":null,"canManage":true})
    );
    let revision = initial["revision"].as_str().unwrap().to_owned();

    // A plain member reads it but cannot manage the owner's node.
    let member: Value = h.sharing(&h.member_cookies, h.node).await.json();
    assert_eq!(member["canManage"], false);
    let refused = h
        .as_member("PATCH", &path, json!({"expectedRevision":revision,"defaultVisibility":"organization"}))
        .await;
    refused.assert_status(StatusCode::FORBIDDEN);
    refused.assert_json(&json!({"code":"forbidden"}));

    // Another tenant learns nothing.
    let foreign = h
        .server
        .get(&path)
        .add_header(header::COOKIE, &h.foreign_cookies)
        .await;
    foreign.assert_status(StatusCode::NOT_FOUND);
    let foreign_patch = h
        .server
        .patch(&path)
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &h.foreign_cookies)
        .add_header("x-neev-csrf", &h.foreign_csrf)
        .json(&json!({"expectedRevision":revision,"defaultVisibility":"organization"}))
        .await;
    foreign_patch.assert_status(StatusCode::NOT_FOUND);

    // CSRF is required.
    h.server
        .patch(&path)
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &h.cookies)
        .json(&json!({"expectedRevision":revision,"defaultVisibility":"organization"}))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    // Strict bodies.
    for body in [
        json!({"expectedRevision":revision,"defaultVisibility":"public"}),
        json!({"expectedRevision":revision,"defaultVisibility":"Organization"}),
        json!({"expectedRevision":revision}),
        json!({"expectedRevision":"01","defaultVisibility":"organization"}),
        json!({"expectedRevision":revision,"defaultVisibility":"organization","extra":true}),
    ] {
        assert_safe_invalid(h.mutation("PATCH", &path, body).await);
    }
    h.sharing(&h.cookies, 0).await.assert_status_bad_request();

    // Stale revision conflicts; the current one succeeds and bumps the revision.
    let stale = h
        .mutation("PATCH", &path, json!({"expectedRevision":"999","defaultVisibility":"organization"}))
        .await;
    stale.assert_status(StatusCode::CONFLICT);
    let changed = h
        .mutation("PATCH", &path, json!({"expectedRevision":revision,"defaultVisibility":"organization"}))
        .await;
    changed.assert_status_ok();
    let changed: Value = changed.json();
    assert_eq!(changed["defaultVisibility"], "organization");
    assert_eq!(changed["effectiveVisibility"], "organization");
    assert_ne!(changed["revision"], initial["revision"]);
    assert_eq!(h.sharing(&h.member_cookies, h.node).await.json::<Value>()["effectiveVisibility"], "organization");

    // A member-created subfolder inherits, names its source, and is theirs to manage.
    let created = h
        .as_member(
            "POST",
            "/api/neev/nodes",
            json!({"parentId":h.node,"kind":"folder","name":"Member folder"}),
        )
        .await;
    created.assert_status(StatusCode::CREATED);
    let child = created.json::<Value>()["id"].as_i64().unwrap() as i32;
    let child_view: Value = h.sharing(&h.member_cookies, child).await.json();
    assert_eq!(
        child_view,
        json!({"nodeId":child,"revision":"1","defaultVisibility":"inherit","effectiveVisibility":"organization","inheritedFrom":h.node,"canManage":true})
    );
    let private_child = h
        .as_member(
            "PATCH",
            &format!("/api/neev/nodes/{child}/sharing"),
            json!({"expectedRevision":"1","defaultVisibility":"private"}),
        )
        .await;
    private_child.assert_status_ok();
    assert_eq!(private_child.json::<Value>()["inheritedFrom"], Value::Null);
}

#[tokio::test]
async fn creation_inherits_the_folder_default_unless_an_audience_is_given() {
    let h = Harness::new().await;
    let path = format!("/api/neev/nodes/{}/sharing", h.node);
    let revision = h.sharing(&h.cookies, h.node).await.json::<Value>()["revision"].clone();
    h.mutation("PATCH", &path, json!({"expectedRevision":revision,"defaultVisibility":"organization"}))
        .await
        .assert_status_ok();

    let inherited = h.create("document", None).await;
    assert_eq!(inherited["visibility"], "organization");
    let chat = h.create("chat", None).await;
    assert_eq!(chat["visibility"], "organization");

    let hr = &h;
    let create = |kind: &str, extra: Value| {
        let mut body = json!({"title":"Secret","requestId":uuid::Uuid::new_v4().to_string()});
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        let route = format!("/api/neev/nodes/{}/{kind}s", hr.node);
        async move { hr.mutation("POST", &route, body).await }
    };
    let private = create("document", json!({"visibility":"private"})).await;
    private.assert_status(StatusCode::CREATED);
    let private: Value = private.json();
    assert_eq!(private["visibility"], "private");
    // The member cannot see the private exception.
    h.server
        .get(&format!("/api/neev/resources/{}", private["id"].as_str().unwrap()))
        .add_header(header::COOKIE, &h.member_cookies)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    let null = create("chat", json!({"visibility":null})).await;
    null.assert_status(StatusCode::CREATED);
    assert_eq!(null.json::<Value>()["visibility"], "organization");

    // Attached documents take the chat's audience; an explicit one is refused.
    assert_safe_invalid(
        create("document", json!({"visibility":"private","originChatId":chat["id"]})).await,
    );
    let attached = create("document", json!({"originChatId":chat["id"]})).await;
    attached.assert_status(StatusCode::CREATED);
    assert_eq!(attached.json::<Value>()["visibility"], "organization");
    assert_safe_invalid(create("chat", json!({"visibility":"public"})).await);

    // Same request id, same body: replay. Different explicit audience: conflict.
    let request_id = uuid::Uuid::new_v4().to_string();
    let body = json!({"title":"Once","requestId":request_id,"visibility":"private"});
    let route = format!("/api/neev/nodes/{}/documents", h.node);
    let first: Value = h.mutation("POST", &route, body.clone()).await.json();
    let again = h.mutation("POST", &route, body).await;
    again.assert_status(StatusCode::CREATED);
    assert_eq!(again.json::<Value>()["id"], first["id"]);
    h.mutation("POST", &route, json!({"title":"Once","requestId":request_id}))
        .await
        .assert_status(StatusCode::CONFLICT);
}
