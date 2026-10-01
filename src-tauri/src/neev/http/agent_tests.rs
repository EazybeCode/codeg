use super::event_json;
use crate::neev::agents::execution::{
    new_job_registry, AgentExecutionRuntime, AgentRuntime, FakeRuntime, ProviderCliRuntime,
    RunEvent, RunRequest,
};
use crate::neev::auth::session;
use crate::neev::catalog::nodes;
use crate::neev::content::{
    resources,
    types::{ResourceInput, ResourceKind},
};
use crate::neev::startup::ServerState;
use crate::neev::test_support as h;
use crate::neev::types::{NodeInput, NodeKind, OrgContext};
use axum::http::header;
use std::sync::Arc;

/// Create a project node + a chat owned by `actor`, returning the chat id.
async fn make_chat(state: &Arc<ServerState>, actor: &OrgContext) -> String {
    let node = nodes::create(
        state,
        actor,
        NodeInput {
            parent_id: None,
            kind: NodeKind::Project,
            name: "Project".into(),
        },
    )
    .await
    .unwrap()
    .id;
    resources::create(
        state,
        actor,
        ResourceInput {
            node_id: node,
            kind: ResourceKind::Chat,
            title: "Chat".into(),
            origin_chat_id: None,
            request_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap()
    .id
}

/// Build an authenticated org session + a `TestServer` with `runtime` injected
/// (config guard untouched; test-only). Returns the server, Cookie header, CSRF,
/// and a chat id owned by the session user.
async fn authed_server(runtime: AgentRuntime) -> (axum_test::TestServer, String, String, String) {
    let db = h::database().await;
    let user = h::native_account(&db, "runner", "runner@example.test").await;
    let (org, _) = h::organization(&db, user, "admin").await;
    let now = 1000;
    let issued = session::issue_identity(&db, user, now).await.unwrap();
    let identity = session::identity(&db, &issued.session, now).await.unwrap();
    let scoped = session::select_organization(&db, &identity, org, now)
        .await
        .unwrap();
    let mut server_state =
        ServerState::new(db.clone(), h::preview_config(), Arc::new(h::TestClock::new(now))).unwrap();
    server_state.set_agent_runtime(runtime);
    let state = Arc::new(server_state);
    let actor = h::scoped_context(&db, user, org, now).await;
    let chat_id = make_chat(&state, &actor).await;
    let names = super::cookies::CookieNames::for_preview(true);
    let cookies = format!(
        "{}={}; {}={}",
        names.session, scoped.session, names.csrf, scoped.csrf
    );
    let server = axum_test::TestServer::new(crate::neev::http::router(state)).unwrap();
    (server, cookies, scoped.csrf, chat_id)
}

fn run_json(chat_id: &str) -> serde_json::Value {
    serde_json::json!({
        "chat_id": chat_id,
        "prompt": "Reply with exactly one word: PONG",
        "provider": "claude",
        "model": "default"
    })
}

fn run_request() -> RunRequest {
    RunRequest {
        user_id: 7,
        org_id: 3,
        chat_id: "chat-1".into(),
        provider: "claude".into(),
        model: "default".into(),
        prompt: "draft a plan".into(),
        // The fake/disabled runtimes used here do not deliver credentials; the
        // sandboxed runtime's requirement is covered in `execution`'s tests.
        credential_handle: None,
        briefing: None,
        effort: None,
        fast: false,
        attachments: Vec::new(),
        context_files: Vec::new(),
        library_items: Vec::new(),
        min_free_lanes: 1,
    }
}

#[test]
fn event_json_maps_each_variant_with_escaping() {
    assert_eq!(
        event_json(&RunEvent::Started("r1".into())),
        Some(r#"{"type":"started","run_id":"r1"}"#.to_owned())
    );
    assert_eq!(
        event_json(&RunEvent::Chunk("hi \"x\"".into())),
        Some(r#"{"type":"chunk","text":"hi \"x\""}"#.to_owned())
    );
    assert_eq!(
        event_json(&RunEvent::Completed),
        Some(r#"{"type":"completed"}"#.to_owned())
    );
    assert_eq!(
        event_json(&RunEvent::Failed("canceled".into())),
        Some(r#"{"type":"failed","reason":"canceled"}"#.to_owned())
    );
}

#[tokio::test]
async fn agent_run_rejects_unauthenticated_request() {
    let state = Arc::new(
        crate::neev::startup::ServerState::new(
            h::database().await,
            h::preview_config(),
            Arc::new(h::TestClock::new(1000)),
        )
        .unwrap(),
    );
    let server = axum_test::TestServer::new(crate::neev::http::router(state)).unwrap();
    // No Origin / session cookie: the user-scoped guard refuses before any run.
    server
        .post("/api/neev/agent/run")
        .json(&serde_json::json!({
            "chat_id": "c", "prompt": "hi", "provider": "claude", "model": "m"
        }))
        .await
        .assert_status_forbidden();
}

#[tokio::test]
async fn disabled_runtime_default_refuses_runs() {
    // The production default runtime refuses, so agents cannot run by omission.
    let state = Arc::new(
        crate::neev::startup::ServerState::new(
            h::database().await,
            h::preview_config(),
            Arc::new(h::TestClock::new(1000)),
        )
        .unwrap(),
    );
    assert!(state.agents.run(run_request()).await.is_err());
}

#[tokio::test]
async fn job_registry_cancel_interrupts_an_in_flight_run() {
    let runtime = FakeRuntime::new(vec!["a".into(), "b".into(), "c".into()]);
    let mut handle = runtime.run(run_request()).await.unwrap();
    let registry = new_job_registry();
    let run_id = handle.run_id.clone();
    // The run handler registers the canceler under the run id, owned by user 7.
    registry
        .lock()
        .unwrap()
        .insert(run_id.clone(), (7, handle.take_canceler().unwrap()));
    // A different user cannot cancel it (ownership check mirrors the handler).
    {
        let jobs = registry.lock().unwrap();
        assert!(!matches!(jobs.get(&run_id), Some((owner, _)) if *owner == 9));
    }
    // The owner's cancel request looks it up and interrupts the turn.
    let (_, canceler) = registry.lock().unwrap().remove(&run_id).unwrap();
    assert!(canceler.send(()).is_ok());
    let mut events = Vec::new();
    while let Some(event) = handle.next().await {
        events.push(event);
    }
    assert_eq!(events.last(), Some(&RunEvent::Failed("canceled".into())));
    assert!(!events.contains(&RunEvent::Completed));
    // The registry no longer holds the canceled run.
    assert!(registry.lock().unwrap().is_empty());
}

#[tokio::test]
async fn authed_run_streams_fake_events_over_sse() {
    let (server, cookies, csrf, chat_id) =
        authed_server(AgentRuntime::Fake(FakeRuntime::new(vec![
            "hi ".into(),
            "there".into(),
        ])))
        .await;
    let response = server
        .post("/api/neev/agent/run")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, cookies)
        .add_header("x-neev-csrf", csrf)
        .json(&run_json(&chat_id))
        .await;
    response.assert_status_ok();
    let body = response.text();
    assert!(body.contains("\"started\""), "sse missing started: {body}");
    assert!(
        body.contains("\"chunk\"") && body.contains("there"),
        "sse missing chunks: {body}"
    );
    assert!(body.contains("\"completed\""), "sse missing completed: {body}");
}

#[tokio::test]
async fn run_requires_an_organization_scoped_session() {
    let db = h::database().await;
    let user = h::native_account(&db, "runner", "runner@example.test").await;
    let now = 1000;
    let issued = session::issue_identity(&db, user, now).await.unwrap();
    let mut state = ServerState::new(
        db,
        h::preview_config(),
        Arc::new(h::TestClock::new(now)),
    )
    .unwrap();
    state.set_agent_runtime(AgentRuntime::Fake(FakeRuntime::new(vec!["x".into()])));
    let names = super::cookies::CookieNames::for_preview(true);
    let cookies = format!(
        "{}={}; {}={}",
        names.session, issued.session, names.csrf, issued.csrf
    );
    let server = axum_test::TestServer::new(crate::neev::http::router(Arc::new(state))).unwrap();
    let response = server
        .post("/api/neev/agent/run")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, cookies)
        .add_header("x-neev-csrf", issued.csrf)
        .json(&run_json("11111111-1111-4111-8111-111111111111"))
        .await;
    assert!(
        response.status_code().is_client_error(),
        "identity-only session must be denied, got {}",
        response.status_code()
    );
}

#[tokio::test]
async fn run_denies_a_chat_from_another_org() {
    // Session belongs to org A; the chat belongs to a different org/user.
    let db = h::database().await;
    let runner = h::native_account(&db, "runner", "runner@example.test").await;
    let (org_a, _) = h::organization(&db, runner, "admin").await;
    let outsider = h::native_account(&db, "outsider", "outsider@example.test").await;
    let (org_b, _) = h::organization(&db, outsider, "admin").await;
    let now = 1000;
    let issued = session::issue_identity(&db, runner, now).await.unwrap();
    let identity = session::identity(&db, &issued.session, now).await.unwrap();
    let scoped = session::select_organization(&db, &identity, org_a, now)
        .await
        .unwrap();
    let mut server_state =
        ServerState::new(db.clone(), h::preview_config(), Arc::new(h::TestClock::new(now))).unwrap();
    server_state.set_agent_runtime(AgentRuntime::Fake(FakeRuntime::new(vec!["x".into()])));
    let state = Arc::new(server_state);
    // A chat in org B, created by the outsider.
    let outsider_actor = h::scoped_context(&db, outsider, org_b, now).await;
    let foreign_chat = make_chat(&state, &outsider_actor).await;
    let names = super::cookies::CookieNames::for_preview(true);
    let cookies = format!(
        "{}={}; {}={}",
        names.session, scoped.session, names.csrf, scoped.csrf
    );
    let server = axum_test::TestServer::new(crate::neev::http::router(state)).unwrap();
    let response = server
        .post("/api/neev/agent/run")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, cookies)
        .add_header("x-neev-csrf", scoped.csrf)
        .json(&run_json(&foreign_chat))
        .await;
    // Cross-org chat is not found for org A — no launch.
    assert!(
        response.status_code().is_client_error(),
        "cross-org chat must be denied, got {}",
        response.status_code()
    );
}

#[tokio::test]
async fn run_denies_another_users_private_chat_in_same_org() {
    // Two members of the same org; a private chat owned by the other member.
    let db = h::database().await;
    let owner = h::native_account(&db, "owner", "owner@example.test").await;
    let (org, _) = h::organization(&db, owner, "admin").await;
    let member = h::native_account(&db, "member", "member@example.test").await;
    h::add_membership(&db, org, member, "member").await;
    let now = 1000;
    let mut server_state =
        ServerState::new(db.clone(), h::preview_config(), Arc::new(h::TestClock::new(now))).unwrap();
    server_state.set_agent_runtime(AgentRuntime::Fake(FakeRuntime::new(vec!["x".into()])));
    let state = Arc::new(server_state);
    // Owner creates a PRIVATE chat (default visibility).
    let owner_actor = h::scoped_context(&db, owner, org, now).await;
    let private_chat = make_chat(&state, &owner_actor).await;
    // The member's session cannot reach the owner's private chat.
    let issued = session::issue_identity(&db, member, now).await.unwrap();
    let identity = session::identity(&db, &issued.session, now).await.unwrap();
    let scoped = session::select_organization(&db, &identity, org, now)
        .await
        .unwrap();
    let names = super::cookies::CookieNames::for_preview(true);
    let cookies = format!(
        "{}={}; {}={}",
        names.session, scoped.session, names.csrf, scoped.csrf
    );
    let server = axum_test::TestServer::new(crate::neev::http::router(state)).unwrap();
    let response = server
        .post("/api/neev/agent/run")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, cookies)
        .add_header("x-neev-csrf", scoped.csrf)
        .json(&run_json(&private_chat))
        .await;
    assert!(
        response.status_code().is_client_error(),
        "another user's private chat must be denied, got {}",
        response.status_code()
    );
}

// Full browser -> authenticated HTTP -> real provider -> streamed reply. Ignored
// (spawns the real claude CLI + network); run manually to demonstrate the
// end-to-end journey:
//   <bin> neev::http::agent::agent_tests::authed_run_streams_real_provider_over_http --ignored
#[tokio::test]
#[ignore = "spawns the real authenticated claude CLI over HTTP; run manually"]
async fn authed_run_streams_real_provider_over_http() {
    let (server, cookies, csrf, chat_id) = authed_server(AgentRuntime::Provider(
        ProviderCliRuntime::new("claude", std::env::temp_dir()),
    ))
    .await;
    let response = server
        .post("/api/neev/agent/run")
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, cookies)
        .add_header("x-neev-csrf", csrf)
        .json(&run_json(&chat_id))
        .await;
    response.assert_status_ok();
    let body = response.text();
    assert!(body.to_uppercase().contains("PONG"), "no PONG in sse: {body}");
    assert!(body.contains("\"completed\""), "no completed in sse: {body}");
}
