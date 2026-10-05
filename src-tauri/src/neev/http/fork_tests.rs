//! The fork endpoints' wire contract: shape, status codes, coded refusals.

use axum::http::{header, StatusCode};
use serde_json::{json, Value};

use crate::neev::{
    auth::session,
    catalog::nodes,
    content::{messages, resources, types::ResourceKind},
    test_support as h,
    types::{NodeInput, NodeKind},
};

use super::cookies::CookieNames;

struct Client {
    server: axum_test::TestServer,
    cookies: String,
    csrf: String,
}

impl Client {
    async fn post(&self, path: &str, body: Value) -> axum_test::TestResponse {
        self.server
            .post(path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, &self.cookies)
            .add_header("x-neev-csrf", &self.csrf)
            .json(&body)
            .await
    }
}

async fn setup() -> (h::OrgHarness, Client, String, String, String) {
    let org = h::OrgHarness::new().await;
    let owner = org.owner_context().clone();
    let node = nodes::create(
        &org.state,
        &owner,
        NodeInput {
            parent_id: None,
            kind: NodeKind::Project,
            name: "Forks".into(),
        },
    )
    .await
    .unwrap()
    .id;
    let chat = resources::create(
        &org.state,
        &owner,
        crate::neev::content::types::ResourceInput {
            node_id: node,
            kind: ResourceKind::Chat,
            title: "Plan".into(),
            origin_chat_id: None,
            request_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    let ask = messages::append(&org.state, &owner, &chat.id, &uuid::Uuid::new_v4().to_string(), 1, "Ask")
        .await
        .unwrap();
    let reply = messages::publish_agent_reply(
        &org.state,
        &owner,
        &chat.id,
        "Answer",
        "claude",
        &uuid::Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    let identity = session::issue_identity(&org.database, owner.identity.user_id, 1_700_000_000)
        .await
        .unwrap();
    let identity = session::identity(&org.database, &identity.session, 1_700_000_000)
        .await
        .unwrap();
    let scoped = session::select_organization(&org.database, &identity, org.org_id(), 1_700_000_000)
        .await
        .unwrap();
    let names = CookieNames::for_preview(true);
    let client = Client {
        server: axum_test::TestServer::new(super::router(org.state.clone())).unwrap(),
        cookies: format!(
            "{}={}; {}={}",
            names.session, scoped.session, names.csrf, scoped.csrf
        ),
        csrf: scoped.csrf,
    };
    (org, client, chat.id, ask.id, reply.id)
}

#[tokio::test]
async fn forking_a_chat_answers_the_new_chat_and_what_was_copied() {
    let (_org, client, chat, ask, reply) = setup().await;
    let request = uuid::Uuid::new_v4().to_string();
    let response = client
        .post(
            &format!("/api/neev/chats/{chat}/fork"),
            json!({"throughMessageId": reply, "requestId": request}),
        )
        .await;
    response.assert_status(StatusCode::CREATED);
    assert_eq!(response.header(header::CACHE_CONTROL), "no-store");
    let body: Value = response.json();
    assert_eq!(body["sourceChatId"], chat);
    assert_eq!(body["throughMessageId"], reply);
    assert_eq!(body["copiedMessages"], 2);
    assert_eq!(body["copiedAttachments"], 0);
    assert_eq!(body["archivedAttachmentsNotCopied"], 0);
    assert_eq!(body["replayed"], false);
    let fork = body["chatId"].as_str().unwrap().to_owned();
    assert_ne!(fork, chat);

    // A retry is the same fork.
    let again: Value = client
        .post(
            &format!("/api/neev/chats/{chat}/fork"),
            json!({"throughMessageId": reply, "requestId": request}),
        )
        .await
        .json();
    assert_eq!((again["chatId"].as_str(), again["replayed"].as_bool()), (Some(fork.as_str()), Some(true)));

    // A person's message is refused with a code the client can name.
    let refused = client
        .post(
            &format!("/api/neev/chats/{chat}/fork"),
            json!({"throughMessageId": ask, "requestId": uuid::Uuid::new_v4().to_string()}),
        )
        .await;
    refused.assert_status(StatusCode::CONFLICT);
    refused.assert_json(&json!({"code": "fork_not_complete"}));

    // Strict input.
    client
        .post(
            &format!("/api/neev/chats/{chat}/fork"),
            json!({"throughMessageId": reply, "requestId": uuid::Uuid::new_v4().to_string(), "visibility": "organization"}),
        )
        .await
        .assert_status_bad_request();
}

#[tokio::test]
async fn forking_someone_elses_or_a_missing_agent_chat_is_not_found() {
    let (_org, client, _chat, _ask, _reply) = setup().await;
    let response = client
        .post(
            &format!("/api/neev/agent/sessions/{}/fork", uuid::Uuid::new_v4()),
            json!({"throughTurnId": uuid::Uuid::new_v4().to_string(), "requestId": uuid::Uuid::new_v4().to_string()}),
        )
        .await;
    response.assert_status(StatusCode::NOT_FOUND);
}
