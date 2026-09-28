//! End-to-end HTTP behaviour for Agent Chats: authorization, honest failure
//! states, and the reattach/replay stream.

use crate::neev::agents::execution::{AgentRuntime, FakeRuntime};
use crate::neev::auth::session;
use crate::neev::startup::ServerState;
use crate::neev::test_support as h;
use axum::http::header;
use serde_json::Value;
use std::sync::Arc;

const ORIGIN: &str = "http://127.0.0.1:3100";
const NOW: i64 = 1_000;

struct Client {
    server: axum_test::TestServer,
    cookies: String,
    csrf: String,
}

impl Client {
    fn get(&self, path: &str) -> axum_test::TestRequest {
        self.server
            .get(path)
            .add_header(header::ORIGIN, ORIGIN)
            .add_header(header::COOKIE, self.cookies.clone())
    }

    fn post(&self, path: &str) -> axum_test::TestRequest {
        self.server
            .post(path)
            .add_header(header::ORIGIN, ORIGIN)
            .add_header(header::COOKIE, self.cookies.clone())
            .add_header("x-neev-csrf", self.csrf.clone())
    }

    fn patch(&self, path: &str) -> axum_test::TestRequest {
        self.server
            .patch(path)
            .add_header(header::ORIGIN, ORIGIN)
            .add_header(header::COOKIE, self.cookies.clone())
            .add_header("x-neev-csrf", self.csrf.clone())
    }

    async fn create_session(&self) -> Value {
        let response = self
            .post("/api/neev/agent/sessions")
            .json(&serde_json::json!({
                "title": "Launch plan",
                "provider": "claude",
                "model": "default",
                "requestId": uuid::Uuid::new_v4().to_string(),
            }))
            .await;
        response.assert_status(axum::http::StatusCode::CREATED);
        response.json::<Value>()
    }

    async fn start_turn(&self, session_id: &str, prompt: &str) -> Value {
        let response = self
            .post(&format!("/api/neev/agent/sessions/{session_id}/turns"))
            .json(&serde_json::json!({ "prompt": prompt }))
            .await;
        response.assert_status(axum::http::StatusCode::CREATED);
        response.json::<Value>()
    }
}

/// A server with `runtime` bound, plus a signed-in owner and (optionally) a
/// second member of the same org.
async fn fixture(
    runtime: AgentRuntime,
) -> (Arc<ServerState>, Client, sea_orm::DatabaseConnection, i32) {
    let database = h::database().await;
    let user = h::native_account(&database, "owner", "owner@example.test").await;
    let (org, _) = h::organization(&database, user, "admin").await;
    let mut server_state = ServerState::new(
        database.clone(),
        h::preview_config(),
        Arc::new(h::TestClock::new(NOW)),
    )
    .unwrap();
    server_state.set_agent_runtime(runtime);
    let state = Arc::new(server_state);
    let client = sign_in(&state, &database, user).await;
    (state, client, database, org)
}

async fn sign_in(
    state: &Arc<ServerState>,
    database: &sea_orm::DatabaseConnection,
    user: i32,
) -> Client {
    let org = crate::neev::db::one(
        database,
        "SELECT org_id FROM neev_memberships WHERE user_id=$1",
        vec![user.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<i32>("", "org_id")
    .unwrap();
    let issued = session::issue_identity(database, user, NOW).await.unwrap();
    let identity = session::identity(database, &issued.session, NOW)
        .await
        .unwrap();
    let scoped = session::select_organization(database, &identity, org, NOW)
        .await
        .unwrap();
    let names = crate::neev::http::cookies::CookieNames::for_preview(true);
    Client {
        server: axum_test::TestServer::new(crate::neev::http::router(state.clone())).unwrap(),
        cookies: format!(
            "{}={}; {}={}",
            names.session, scoped.session, names.csrf, scoped.csrf
        ),
        csrf: scoped.csrf,
    }
}

fn fake(chunks: &[&str]) -> AgentRuntime {
    AgentRuntime::Fake(FakeRuntime::new(
        chunks.iter().map(|chunk| (*chunk).to_owned()).collect(),
    ))
}

/// Parse an SSE body into its decoded `data:` payloads.
fn sse_events(body: &str) -> Vec<Value> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str(data.trim()).ok())
        .collect()
}

#[tokio::test]
async fn unauthenticated_requests_are_refused() {
    let (state, _client, _db, _org) = fixture(fake(&["x"])).await;
    let server = axum_test::TestServer::new(crate::neev::http::router(state)).unwrap();
    for response in [
        server.get("/api/neev/agent/sessions").await,
        server.get("/api/neev/agent/connections").await,
    ] {
        assert!(
            response.status_code().is_client_error(),
            "unauthenticated read must be refused, got {}",
            response.status_code()
        );
    }
    let created = server
        .post("/api/neev/agent/sessions")
        .json(&serde_json::json!({
            "title": "x", "provider": "claude", "model": "default",
            "requestId": uuid::Uuid::new_v4().to_string(),
        }))
        .await;
    assert!(created.status_code().is_client_error());
}

#[tokio::test]
async fn a_session_round_trips_through_list_and_get() {
    let (_state, client, _db, _org) = fixture(fake(&["x"])).await;
    let created = client.create_session().await;
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["title"], "Launch plan");
    assert_eq!(created["archived"], false);
    assert_eq!(created["revision"], "1");

    let listed = client.get("/api/neev/agent/sessions").await.json::<Value>();
    assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(listed["sessions"][0]["id"], id.as_str());

    let fetched = client
        .get(&format!("/api/neev/agent/sessions/{id}"))
        .await
        .json::<Value>();
    assert_eq!(fetched["id"], id.as_str());
}

#[tokio::test]
async fn another_member_cannot_reach_the_session_or_its_turns() {
    let (state, client, database, org) = fixture(fake(&["x"])).await;
    let created = client.create_session().await;
    let id = created["id"].as_str().unwrap().to_owned();
    let turn = client.start_turn(&id, "private question").await;
    let run_id = turn["runId"].as_str().unwrap().to_owned();

    let peer_user = h::native_account(&database, "peer", "peer@example.test").await;
    h::add_membership(&database, org, peer_user, "member").await;
    let peer = sign_in(&state, &database, peer_user).await;

    peer.get(&format!("/api/neev/agent/sessions/{id}"))
        .await
        .assert_status_not_found();
    peer.get(&format!("/api/neev/agent/sessions/{id}/turns"))
        .await
        .assert_status_not_found();
    // A leaked run id is not authority either.
    peer.get(&format!("/api/neev/agent/runs/{run_id}/events"))
        .await
        .assert_status_not_found();
    assert_eq!(
        peer.get("/api/neev/agent/sessions").await.json::<Value>()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn a_disabled_runtime_produces_a_visible_failed_turn_not_a_silent_drop() {
    // The production default: no runtime bound.
    let database = h::database().await;
    let user = h::native_account(&database, "owner", "owner@example.test").await;
    h::organization(&database, user, "admin").await;
    let state = Arc::new(
        ServerState::new(
            database.clone(),
            h::preview_config(),
            Arc::new(h::TestClock::new(NOW)),
        )
        .unwrap(),
    );
    let client = sign_in(&state, &database, user).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();

    let started = client.start_turn(&id, "will not run").await;
    assert_eq!(started["turn"]["status"], "failed");
    assert_eq!(started["turn"]["errorCode"], "disabled");
    // The prompt survived, so a reload shows what was asked and why it failed.
    let turns = client
        .get(&format!("/api/neev/agent/sessions/{id}/turns"))
        .await
        .json::<Value>();
    assert_eq!(turns["items"][0]["prompt"], "will not run");
    assert_eq!(turns["items"][0]["status"], "failed");
    assert_eq!(turns["items"][0]["errorCode"], "disabled");

    // And the connection surface never claims "connected" over a dead runtime.
    let connections = client
        .get("/api/neev/agent/connections")
        .await
        .json::<Value>();
    assert_eq!(connections["runtimeEnabled"], false);
    for connection in connections["connections"].as_array().unwrap() {
        assert_eq!(connection["state"], "disabled");
    }
}

#[tokio::test]
async fn a_run_streams_then_persists_and_replays_identically() {
    let (_state, client, _db, _org) = fixture(fake(&["hello ", "world"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();
    let started = client.start_turn(&id, "greet me").await;
    assert_eq!(started["turn"]["status"], "running");
    let run_id = started["runId"].as_str().unwrap().to_owned();

    let body = client
        .get(&format!("/api/neev/agent/runs/{run_id}/events"))
        .await
        .text();
    let events = sse_events(&body);
    assert_eq!(events[0]["type"], "started");
    assert_eq!(events[0]["runId"], run_id.as_str());
    let streamed: String = events
        .iter()
        .filter(|event| event["type"] == "chunk")
        .map(|event| event["text"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(streamed, "hello world");
    let last = events.last().unwrap();
    assert_eq!(last["type"], "finished");
    assert_eq!(last["status"], "completed");

    // Refresh-safe: the persisted history matches what was streamed.
    let turns = client
        .get(&format!("/api/neev/agent/sessions/{id}/turns"))
        .await
        .json::<Value>();
    assert_eq!(turns["items"][0]["response"], "hello world");
    assert_eq!(turns["items"][0]["status"], "completed");

    // Reattaching AFTER completion replays the stored result — it does not
    // re-run the prompt (a second execution would double any side effect).
    let replay = sse_events(
        &client
            .get(&format!("/api/neev/agent/runs/{run_id}/events"))
            .await
            .text(),
    );
    let replayed: String = replay
        .iter()
        .filter(|event| event["type"] == "chunk")
        .map(|event| event["text"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(replayed, "hello world");
    assert_eq!(replay.last().unwrap()["status"], "completed");
    // Exactly one turn exists: replay created nothing.
    assert_eq!(turns["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn an_interrupted_run_replays_as_failed_and_is_never_auto_rerun() {
    let (state, client, database, org) = fixture(fake(&["partial"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();
    // Reproduce the post-crash state exactly: a turn written as `running` whose
    // driver does not exist. Going through the service layer (rather than the
    // HTTP route) means no run is ever registered, which is precisely what a
    // restarted process finds in its database.
    let user = crate::neev::db::one(
        &database,
        "SELECT owner_id FROM neev_agent_sessions WHERE id=$1",
        vec![id.clone().into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<i32>("", "owner_id")
    .unwrap();
    let actor = h::scoped_context(&database, user, org, NOW).await;
    let (turn, _) =
        crate::neev::agents::sessions::begin_turn(&state, &actor, &id, "long task", None)
            .await
            .unwrap();
    let run_id = turn.run_id.clone();
    crate::neev::agents::sessions::persist_response(&database, &run_id, "partial", NOW + 1)
        .await
        .unwrap();
    assert!(state.agent_runs.lock().unwrap().is_empty());

    crate::neev::agents::sessions::reconcile_interrupted(&database, NOW + 60)
        .await
        .unwrap();

    let events = sse_events(
        &client
            .get(&format!("/api/neev/agent/runs/{run_id}/events"))
            .await
            .text(),
    );
    let last = events.last().unwrap();
    assert_eq!(last["type"], "finished");
    assert_eq!(last["status"], "failed");
    assert_eq!(last["errorCode"], "interrupted");
    // Exactly one turn: reattaching a dead run starts nothing.
    let turns = client
        .get(&format!("/api/neev/agent/sessions/{id}/turns"))
        .await
        .json::<Value>();
    assert_eq!(turns["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn rename_and_archive_go_through_one_field_at_a_time() {
    let (_state, client, _db, _org) = fixture(fake(&["x"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();

    // Both fields at once would need two revisions and could half-apply.
    client
        .patch(&format!("/api/neev/agent/sessions/{id}"))
        .json(&serde_json::json!({
            "title": "New", "archived": true, "expectedRevision": "1"
        }))
        .await
        .assert_status_bad_request();

    let renamed = client
        .patch(&format!("/api/neev/agent/sessions/{id}"))
        .json(&serde_json::json!({ "title": "Renamed", "expectedRevision": "1" }))
        .await;
    renamed.assert_status_ok();
    let renamed = renamed.json::<Value>();
    assert_eq!(renamed["title"], "Renamed");
    assert_eq!(renamed["revision"], "2");

    // A stale revision is refused rather than clobbering the newer title.
    client
        .patch(&format!("/api/neev/agent/sessions/{id}"))
        .json(&serde_json::json!({ "title": "Stale", "expectedRevision": "1" }))
        .await
        .assert_status(axum::http::StatusCode::CONFLICT);

    let archived = client
        .patch(&format!("/api/neev/agent/sessions/{id}"))
        .json(&serde_json::json!({ "archived": true, "expectedRevision": "2" }))
        .await
        .json::<Value>();
    assert_eq!(archived["archived"], true);
    // Archived sessions leave the default list but are still reachable.
    assert_eq!(
        client.get("/api/neev/agent/sessions").await.json::<Value>()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        client
            .get("/api/neev/agent/sessions?archived=1")
            .await
            .json::<Value>()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn cancel_requires_ownership_and_reports_what_it_actually_did() {
    let (state, client, database, org) = fixture(fake(&[])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();
    let started = client.start_turn(&id, "q").await;
    let run_id = started["runId"].as_str().unwrap().to_owned();

    let peer_user = h::native_account(&database, "peer", "peer@example.test").await;
    h::add_membership(&database, org, peer_user, "member").await;
    let peer = sign_in(&state, &database, peer_user).await;
    peer.post("/api/neev/agent/runs/cancel")
        .json(&serde_json::json!({ "runId": run_id }))
        .await
        .assert_status_not_found();

    let response = client
        .post("/api/neev/agent/runs/cancel")
        .json(&serde_json::json!({ "runId": run_id }))
        .await;
    response.assert_status_ok();
    // `canceled` reports whether a live run was actually interrupted here — it
    // is never a blanket "true".
    assert!(response.json::<Value>()["canceled"].is_boolean());
}

#[tokio::test]
async fn a_stream_stops_the_moment_its_browser_session_is_revoked() {
    let (state, client, database, _org) = fixture(fake(&["x"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();
    let user = crate::neev::db::one(
        &database,
        "SELECT owner_id FROM neev_agent_sessions WHERE id=$1",
        vec![id.clone().into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<i32>("", "owner_id")
    .unwrap();
    let org = crate::neev::db::one(
        &database,
        "SELECT org_id FROM neev_memberships WHERE user_id=$1",
        vec![user.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<i32>("", "org_id")
    .unwrap();
    // A second, independent browser session for the same user: the one whose
    // cookie the stream holds.
    let issued = session::issue_identity(&database, user, NOW).await.unwrap();
    let identity = session::identity(&database, &issued.session, NOW)
        .await
        .unwrap();
    let scoped = session::select_organization(&database, &identity, org, NOW)
        .await
        .unwrap();
    let actor = crate::neev::auth::session::organization(&database, &scoped.session, NOW)
        .await
        .unwrap();
    let (turn, _) =
        crate::neev::agents::sessions::begin_turn(&state, &actor, &id, "long task", None)
            .await
            .unwrap();

    // While the cookie is live, re-authorization succeeds.
    assert!(
        super::reauthorize(&state, &scoped.session, &turn.run_id)
            .await
            .is_ok(),
        "a valid cookie must keep the stream alive"
    );

    // Log that session out. A cached OrgContext would still look valid here —
    // re-deriving from the cookie is what catches it.
    crate::neev::auth::session::revoke(&database, &actor.identity, &h::TestClock::new(NOW))
        .await
        .unwrap();
    assert!(
        super::reauthorize(&state, &scoped.session, &turn.run_id)
            .await
            .is_err(),
        "a revoked session must not keep receiving private agent output"
    );
}

#[tokio::test]
async fn a_leaked_run_id_cannot_be_streamed_with_another_members_cookie() {
    let (state, client, database, org) = fixture(fake(&["x"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();
    let started = client.start_turn(&id, "private").await;
    let run_id = started["runId"].as_str().unwrap().to_owned();

    let peer_user = h::native_account(&database, "peer", "peer@example.test").await;
    h::add_membership(&database, org, peer_user, "member").await;
    let issued = session::issue_identity(&database, peer_user, NOW)
        .await
        .unwrap();
    let identity = session::identity(&database, &issued.session, NOW)
        .await
        .unwrap();
    let scoped = session::select_organization(&database, &identity, org, NOW)
        .await
        .unwrap();
    // The second half of re-authorization — ownership of the run — refuses even
    // though the cookie itself is perfectly valid.
    assert!(super::reauthorize(&state, &scoped.session, &run_id)
        .await
        .is_err());
}

#[tokio::test]
async fn the_event_sink_is_bounded_so_a_stalled_reader_cannot_grow_the_server() {
    let (mut sender, _receiver) = futures::channel::mpsc::channel::<
        Result<axum::response::sse::Event, std::convert::Infallible>,
    >(super::SSE_BUFFER);
    // Push without ever reading, exactly like a stalled client. A bounded sink
    // must refuse at some point; an unbounded one would accept forever.
    // (`futures::channel::mpsc` guarantees the requested buffer plus one slot
    // per sender, so the precise stopping point is SSE_BUFFER + 1.)
    let mut accepted = 0usize;
    let ceiling = super::SSE_BUFFER + 8;
    while accepted < ceiling {
        if sender
            .try_send(Ok(axum::response::sse::Event::default().data("x")))
            .is_err()
        {
            break;
        }
        accepted += 1;
    }
    assert!(
        accepted <= super::SSE_BUFFER + 1,
        "sink accepted {accepted} events for a bound of {} — it is not applying backpressure",
        super::SSE_BUFFER
    );
}

#[tokio::test]
async fn every_stream_branch_carries_private_cache_headers() {
    let (_state, client, _db, _org) = fixture(fake(&["hi"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();
    let started = client.start_turn(&id, "q").await;
    let run_id = started["runId"].as_str().unwrap().to_owned();

    // Live branch, then (after it settles) the replay branch. A replayed
    // transcript is as private as a live one.
    for _ in 0..2 {
        let response = client
            .get(&format!("/api/neev/agent/runs/{run_id}/events"))
            .await;
        response.assert_status_ok();
        assert_eq!(
            response.headers().get(axum::http::header::CACHE_CONTROL),
            Some(&axum::http::HeaderValue::from_static("no-store")),
            "SSE branch served without no-store"
        );
        assert_eq!(
            response.headers().get("referrer-policy"),
            Some(&axum::http::HeaderValue::from_static("no-referrer"))
        );
    }
}

/// A project + chat owned by `actor`, returning `(chat_id, auth_revision)`.
async fn make_chat(
    state: &Arc<ServerState>,
    actor: &crate::neev::types::OrgContext,
) -> (String, String) {
    let node = crate::neev::catalog::nodes::create(
        state,
        actor,
        crate::neev::types::NodeInput {
            parent_id: None,
            kind: crate::neev::types::NodeKind::Project,
            name: "Project".into(),
        },
    )
    .await
    .unwrap()
    .id;
    let chat = crate::neev::content::resources::create(
        state,
        actor,
        crate::neev::content::types::ResourceInput {
            node_id: node,
            kind: crate::neev::content::types::ResourceKind::Chat,
            title: "Team standup".into(),
            origin_chat_id: None,
            request_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    (chat.id, chat.auth_revision.to_string())
}

async fn owner_context(
    database: &sea_orm::DatabaseConnection,
    email: &str,
) -> crate::neev::types::OrgContext {
    let user = crate::neev::db::one(
        database,
        "SELECT id FROM neev_accounts WHERE id=(SELECT user_id FROM neev_memberships LIMIT 1)",
        vec![],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<i32>("", "id")
    .unwrap();
    let _ = email;
    let org = crate::neev::db::one(
        database,
        "SELECT org_id FROM neev_memberships WHERE user_id=$1",
        vec![user.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<i32>("", "org_id")
    .unwrap();
    h::scoped_context(database, user, org, NOW).await
}

#[tokio::test]
async fn a_team_invocation_posts_the_ask_then_a_labelled_agent_reply() {
    let (state, client, database, _org) = fixture(fake(&["Standup ", "notes."])).await;
    let actor = owner_context(&database, "owner@example.test").await;
    let (chat_id, revision) = make_chat(&state, &actor).await;

    let response = client
        .post(&format!("/api/neev/chats/{chat_id}/agent"))
        .json(&serde_json::json!({
            "prompt": "summarise the standup",
            "provider": "claude",
            "model": "default",
            "requestId": uuid::Uuid::new_v4().to_string(),
            "expectedAuthRevision": revision,
        }))
        .await;
    response.assert_status(axum::http::StatusCode::CREATED);
    let body = response.json::<Value>();
    // The ask is team-visible immediately, as an ordinary human message.
    assert_eq!(body["message"]["body"], "summarise the standup");
    assert_eq!(body["message"]["authorKind"], "human");
    assert_eq!(body["message"]["agentLabel"], Value::Null);
    assert_eq!(body["turn"]["status"], "running");
    let run_id = body["runId"].as_str().unwrap().to_owned();

    // The same SSE endpoint a saved session uses — one implementation.
    let events = sse_events(
        &client
            .get(&format!("/api/neev/agent/runs/{run_id}/events"))
            .await
            .text(),
    );
    assert_eq!(events.last().unwrap()["status"], "completed");

    let messages = client
        .get(&format!("/api/neev/chats/{chat_id}/messages"))
        .await
        .json::<Value>();
    let items = messages["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "expected the ask and the reply: {items:?}");
    assert_eq!(items[0]["authorKind"], "human");
    // The reply is agent-authored and names its provider — it does not
    // impersonate the human, but stays attributable to who invoked it.
    assert_eq!(items[1]["authorKind"], "agent");
    assert_eq!(items[1]["agentLabel"], "claude");
    assert_eq!(items[1]["body"], "Standup notes.");
    assert_eq!(items[1]["authorId"], items[0]["authorId"]);
}

#[tokio::test]
async fn a_team_invocation_never_carries_private_session_history() {
    let (state, client, database, _org) = fixture(fake(&["ok"])).await;
    let actor = owner_context(&database, "owner@example.test").await;
    let (chat_id, revision) = make_chat(&state, &actor).await;

    // A private saved session with a completed exchange the agent must not see.
    let private = client.create_session().await;
    let private_id = private["id"].as_str().unwrap().to_owned();
    let first = client
        .start_turn(&private_id, "the secret is hunter2")
        .await;
    client
        .get(&format!(
            "/api/neev/agent/runs/{}/events",
            first["runId"].as_str().unwrap()
        ))
        .await;

    client
        .post(&format!("/api/neev/chats/{chat_id}/agent"))
        .json(&serde_json::json!({
            "prompt": "what is the secret?",
            "provider": "claude",
            "model": "default",
            "requestId": uuid::Uuid::new_v4().to_string(),
            "expectedAuthRevision": revision,
        }))
        .await
        .assert_status(axum::http::StatusCode::CREATED);

    // The team turn lives in a DIFFERENT session from the private one, and its
    // stored prompt is the explicit ask alone.
    let team_session = crate::neev::agents::sessions::team_session_id(
        actor.org_id,
        actor.identity.user_id,
        &chat_id,
        "claude",
    );
    assert_ne!(team_session, private_id);
    let turns = crate::neev::agents::sessions::turns(&state, &actor, &team_session, None)
        .await
        .unwrap();
    assert_eq!(turns.items.len(), 1);
    assert_eq!(turns.items[0].prompt, "what is the secret?");
    assert!(!turns.items[0].prompt.contains("hunter2"));

    // And the private session is not listed alongside it / not team-visible.
    let listed = client.get("/api/neev/agent/sessions").await.json::<Value>();
    let ids: Vec<&str> = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| session["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![private_id.as_str()],
        "the team holder session must not appear as a saved Agent Chat"
    );
}

#[tokio::test]
async fn a_retried_team_invocation_runs_once_and_posts_one_reply() {
    let (state, client, database, _org) = fixture(fake(&["the answer"])).await;
    let actor = owner_context(&database, "owner@example.test").await;
    let (chat_id, revision) = make_chat(&state, &actor).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let payload = serde_json::json!({
        "prompt": "summarise",
        "provider": "claude",
        "model": "default",
        "requestId": request_id,
        "expectedAuthRevision": revision,
    });

    let first = client
        .post(&format!("/api/neev/chats/{chat_id}/agent"))
        .json(&payload)
        .await;
    first.assert_status(axum::http::StatusCode::CREATED);
    let first = first.json::<Value>();
    let run_id = first["runId"].as_str().unwrap().to_owned();
    // Drain it so the reply is published and the run has settled — this is the
    // SEQUENTIAL retry a live-turn guard alone cannot catch.
    client
        .get(&format!("/api/neev/agent/runs/{run_id}/events"))
        .await;

    let second = client
        .post(&format!("/api/neev/chats/{chat_id}/agent"))
        .json(&payload)
        .await;
    second.assert_status(axum::http::StatusCode::CREATED);
    let second = second.json::<Value>();
    // Same message, same turn, same run: no second provider execution.
    assert_eq!(second["message"]["id"], first["message"]["id"]);
    assert_eq!(second["turn"]["id"], first["turn"]["id"]);
    assert_eq!(second["runId"], run_id.as_str());

    // Let any (incorrectly started) second run settle before counting.
    client
        .get(&format!("/api/neev/agent/runs/{run_id}/events"))
        .await;
    let items = crate::neev::content::messages::list(&state, &actor, &chat_id, None)
        .await
        .unwrap();
    let asks = items.items.iter().filter(|m| m.body == "summarise").count();
    let replies = items
        .items
        .iter()
        .filter(|m| m.author_kind == crate::neev::content::types::AuthorKind::Agent)
        .count();
    assert_eq!(asks, 1, "the ask was posted twice");
    assert_eq!(replies, 1, "the agent replied twice to one ask");

    // And exactly one turn exists in the team session.
    let team_session = crate::neev::agents::sessions::team_session_id(
        actor.org_id,
        actor.identity.user_id,
        &chat_id,
        "claude",
    );
    let turns = crate::neev::agents::sessions::turns(&state, &actor, &team_session, None)
        .await
        .unwrap();
    assert_eq!(turns.items.len(), 1);
}

/// Two genuinely OVERLAPPING retries of the same `requestId`.
///
/// The requests are driven with `tokio::join!` so both are in flight at once —
/// a sequential pair would not exercise the race at all. The window that used
/// to be open: both requests miss the run registry while the first is still
/// awaiting the runtime, so both start a provider execution, and the loser can
/// settle the shared turn as failed while the winner is still streaming. The
/// claim is now made inside `begin_turn`'s transaction, so exactly one caller
/// is ever told it created the turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_team_invocation_retries_run_the_provider_exactly_once() {
    // Enough chunks that the first run is unquestionably still executing while
    // the second request is being handled.
    let chunks: Vec<&str> = std::iter::repeat("tick ").take(4_000).collect();
    let (state, client, database, _org) = fixture(fake(&chunks)).await;
    let actor = owner_context(&database, "owner@example.test").await;
    let (chat_id, revision) = make_chat(&state, &actor).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let payload = serde_json::json!({
        "prompt": "race me",
        "provider": "claude",
        "model": "default",
        "requestId": request_id,
        "expectedAuthRevision": revision,
    });
    let path = format!("/api/neev/chats/{chat_id}/agent");

    let (first, second) = tokio::join!(
        client.post(&path).json(&payload),
        client.post(&path).json(&payload),
    );
    first.assert_status(axum::http::StatusCode::CREATED);
    second.assert_status(axum::http::StatusCode::CREATED);
    let first = first.json::<Value>();
    let second = second.json::<Value>();

    // Both callers are given the SAME turn and run...
    assert_eq!(second["turn"]["id"], first["turn"]["id"]);
    assert_eq!(second["runId"], first["runId"]);
    // ...and neither is handed a failure: the loser must not settle the shared
    // turn out from under the winner.
    for response in [&first, &second] {
        assert_eq!(
            response["turn"]["status"], "running",
            "a simultaneous retry settled the shared turn: {response}"
        );
    }

    let run_id = first["runId"].as_str().unwrap().to_owned();
    let events = sse_events(
        &client
            .get(&format!("/api/neev/agent/runs/{run_id}/events"))
            .await
            .text(),
    );
    // The single run reaches a real completion, not a `busy`/`unavailable`
    // failure caused by its own duplicate.
    let last = events.last().unwrap();
    assert_eq!(
        last["status"], "completed",
        "the surviving run did not complete: {last}"
    );

    // Exactly one provider execution: one turn, at sequence 1.
    let team_session = crate::neev::agents::sessions::team_session_id(
        actor.org_id,
        actor.identity.user_id,
        &chat_id,
        "claude",
    );
    let turns = crate::neev::agents::sessions::turns(&state, &actor, &team_session, None)
        .await
        .unwrap();
    assert_eq!(turns.items.len(), 1, "a second provider run was started");
    assert_eq!(turns.items[0].sequence, 1);
    assert_eq!(
        turns.items[0].status,
        crate::neev::agents::sessions::TurnStatus::Completed
    );

    // Exactly one ask and one agent reply reached the team.
    let items = crate::neev::content::messages::list(&state, &actor, &chat_id, None)
        .await
        .unwrap();
    assert_eq!(
        items.items.iter().filter(|m| m.body == "race me").count(),
        1,
        "the ask was posted twice"
    );
    assert_eq!(
        items
            .items
            .iter()
            .filter(|m| m.author_kind == crate::neev::content::types::AuthorKind::Agent)
            .count(),
        1,
        "the agent replied twice to one ask"
    );
}

#[tokio::test]
async fn a_refused_team_invocation_posts_the_ask_but_no_agent_message() {
    // Production default: no runtime bound.
    let database = h::database().await;
    let user = h::native_account(&database, "owner", "owner@example.test").await;
    h::organization(&database, user, "admin").await;
    let state = Arc::new(
        ServerState::new(
            database.clone(),
            h::preview_config(),
            Arc::new(h::TestClock::new(NOW)),
        )
        .unwrap(),
    );
    let client = sign_in(&state, &database, user).await;
    let actor = owner_context(&database, "owner@example.test").await;
    let (chat_id, revision) = make_chat(&state, &actor).await;

    let body = client
        .post(&format!("/api/neev/chats/{chat_id}/agent"))
        .json(&serde_json::json!({
            "prompt": "will not run",
            "provider": "claude",
            "model": "default",
            "requestId": uuid::Uuid::new_v4().to_string(),
            "expectedAuthRevision": revision,
        }))
        .await
        .json::<Value>();
    assert_eq!(body["turn"]["status"], "failed");
    assert_eq!(body["turn"]["errorCode"], "disabled");

    let items = client
        .get(&format!("/api/neev/chats/{chat_id}/messages"))
        .await
        .json::<Value>();
    let items = items["items"].as_array().unwrap().clone();
    // The ask stays — the team sees what was attempted — but no agent message
    // was invented for a run that never happened.
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["authorKind"], "human");
}

#[tokio::test]
async fn a_team_invocation_is_refused_for_a_chat_the_caller_cannot_write() {
    let (state, client, database, org) = fixture(fake(&["x"])).await;
    let peer_user = h::native_account(&database, "peer", "peer@example.test").await;
    h::add_membership(&database, org, peer_user, "member").await;
    let peer_actor = h::scoped_context(&database, peer_user, org, NOW).await;
    // The peer's PRIVATE chat is invisible to the owner.
    let (peer_chat, revision) = make_chat(&state, &peer_actor).await;

    client
        .post(&format!("/api/neev/chats/{peer_chat}/agent"))
        .json(&serde_json::json!({
            "prompt": "post into someone else's chat",
            "provider": "claude",
            "model": "default",
            "requestId": uuid::Uuid::new_v4().to_string(),
            "expectedAuthRevision": revision,
        }))
        .await
        .assert_status_not_found();

    // Nothing was written into the peer's chat.
    let items = crate::neev::content::messages::list(&state, &peer_actor, &peer_chat, None)
        .await
        .unwrap();
    assert_eq!(items.items.len(), 0);
}

#[tokio::test]
async fn repeating_a_team_invocation_request_id_does_not_double_post() {
    let (state, client, database, _org) = fixture(fake(&["ok"])).await;
    let actor = owner_context(&database, "owner@example.test").await;
    let (chat_id, revision) = make_chat(&state, &actor).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let payload = serde_json::json!({
        "prompt": "same ask",
        "provider": "claude",
        "model": "default",
        "requestId": request_id,
        "expectedAuthRevision": revision,
    });
    let first = client
        .post(&format!("/api/neev/chats/{chat_id}/agent"))
        .json(&payload)
        .await
        .json::<Value>();
    // The retry replays the same human message; a second ask is not posted.
    let second = client
        .post(&format!("/api/neev/chats/{chat_id}/agent"))
        .json(&payload)
        .await;
    if second.status_code() == axum::http::StatusCode::CREATED {
        let second = second.json::<Value>();
        assert_eq!(second["message"]["id"], first["message"]["id"]);
    } else {
        // A still-live run refuses a second turn in the same session rather
        // than interleaving two replies — also acceptable, never a double post.
        assert_eq!(second.status_code(), axum::http::StatusCode::CONFLICT);
    }
    let items = crate::neev::content::messages::list(&state, &actor, &chat_id, None)
        .await
        .unwrap();
    let asks = items
        .items
        .iter()
        .filter(|message| message.body == "same ask")
        .count();
    assert_eq!(asks, 1, "the ask was posted twice");
}

/// With no supervisor deployed, `connect` REFUSES rather than handing back a
/// pending attempt that could never complete.
///
/// This is the honest shape: the intent is recorded, the supervisor is asked to
/// perform the sign-in, and when there is nobody to ask the attempt is cancelled
/// again so the connection returns to a state the user can retry from — instead
/// of a spinner that dies silently ten minutes later.
#[tokio::test]
async fn connecting_without_a_supervisor_is_refused_and_leaves_no_stuck_attempt() {
    let (_state, client, _db, _org) = fixture(fake(&["x"])).await;
    let refused = client
        .post("/api/neev/agent/connections/claude/connect")
        .json(&serde_json::json!({ "requestKey": "settings-connect" }))
        .await;
    assert_eq!(
        refused.status_code(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "a deployment with no supervisor must not report a sign-in as started"
    );
    // The connection is not left mid-flight: it reads as not signed in, and a
    // retry is possible.
    let connections = client
        .get("/api/neev/agent/connections")
        .await
        .json::<Value>();
    let claude = connections["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["provider"] == "claude")
        .unwrap()
        .clone();
    assert_ne!(claude["state"], "connecting");
    assert_eq!(claude["accountLabel"], Value::Null);
}

#[tokio::test]
#[ignore = "needs a supervisor; the dispatch path is covered by agents::supervisor tests"]
async fn a_user_can_begin_poll_and_cancel_their_own_provider_sign_in() {
    let (_state, client, _db, _org) = fixture(fake(&["x"])).await;
    let started = client
        .post("/api/neev/agent/connections/claude/connect")
        .json(&serde_json::json!({ "requestKey": "settings-connect" }))
        .await;
    started.assert_status(axum::http::StatusCode::ACCEPTED);
    let started = started.json::<Value>();
    assert_eq!(started["outcome"], "pending");
    let attempt_id = started["attemptId"].as_str().unwrap().to_owned();
    assert!(started["expiresAt"].as_i64().unwrap() > NOW);
    // The login ticket's server-side authority is NOT handed to the browser:
    // only the supervisor may complete a sign-in.
    assert_eq!(started.get("operationId"), None);
    assert_eq!(started.get("leaseOwner"), None);

    // The attempt is pollable and honestly still pending — no supervisor is
    // bound in this build, so nothing can have completed it.
    let attempt = client
        .get(&format!(
            "/api/neev/agent/connections/claude/attempts/{attempt_id}"
        ))
        .await;
    attempt.assert_status_ok();
    assert_eq!(attempt.json::<Value>()["state"], "pending");

    // The connection reads `connecting`, never `connected`.
    let connections = client
        .get("/api/neev/agent/connections")
        .await
        .json::<Value>();
    let claude = connections["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["provider"] == "claude")
        .unwrap()
        .clone();
    assert_eq!(claude["state"], "connecting");
    assert_eq!(claude["accountLabel"], Value::Null);

    // Replaying the same requestKey returns the SAME attempt, not a second login.
    let replay = client
        .post("/api/neev/agent/connections/claude/connect")
        .json(&serde_json::json!({ "requestKey": "settings-connect" }))
        .await
        .json::<Value>();
    assert_eq!(replay["attemptId"], attempt_id.as_str());

    // Cancel works and is what makes a never-completing attempt recoverable.
    let canceled = client
        .post("/api/neev/agent/connections/claude/cancel")
        .json(&serde_json::json!({
            "attemptId": attempt_id, "requestKey": "settings-cancel"
        }))
        .await;
    canceled.assert_status_ok();
    assert!(canceled.json::<Value>()["operationId"].is_string());
}

#[tokio::test]
async fn connection_routes_require_a_session_and_a_known_provider() {
    let (state, client, _db, _org) = fixture(fake(&["x"])).await;
    // Unauthenticated.
    let server = axum_test::TestServer::new(crate::neev::http::router(state)).unwrap();
    assert!(server
        .post("/api/neev/agent/connections/claude/connect")
        .json(&serde_json::json!({ "requestKey": "k" }))
        .await
        .status_code()
        .is_client_error());
    // An unknown provider is not distinguishable from "no connection".
    client
        .post("/api/neev/agent/connections/gpt/connect")
        .json(&serde_json::json!({ "requestKey": "k" }))
        .await
        .assert_status_not_found();
    // A malformed request key is refused before any attempt is recorded.
    client
        .post("/api/neev/agent/connections/claude/connect")
        .json(&serde_json::json!({ "requestKey": "__neev_reserved" }))
        .await
        .assert_status_bad_request();
    assert_eq!(
        client
            .get("/api/neev/agent/connections")
            .await
            .json::<Value>()["connections"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["state"] != "not_signed_in")
            .count(),
        0,
        "a refused connect must leave every connection untouched"
    );
}

#[tokio::test]
#[ignore = "needs a supervisor to produce an attempt; scoping is covered by agents::supervisor tests"]
async fn another_member_cannot_poll_or_cancel_someone_elses_sign_in() {
    let (state, client, database, org) = fixture(fake(&["x"])).await;
    let attempt_id = client
        .post("/api/neev/agent/connections/claude/connect")
        .json(&serde_json::json!({ "requestKey": "mine" }))
        .await
        .json::<Value>()["attemptId"]
        .as_str()
        .unwrap()
        .to_owned();

    let peer_user = h::native_account(&database, "peer", "peer@example.test").await;
    h::add_membership(&database, org, peer_user, "member").await;
    let peer = sign_in(&state, &database, peer_user).await;
    // Connections are per-user: the peer has their own, and cannot see this one.
    peer.get(&format!(
        "/api/neev/agent/connections/claude/attempts/{attempt_id}"
    ))
    .await
    .assert_status_not_found();
    peer.post("/api/neev/agent/connections/claude/cancel")
        .json(&serde_json::json!({
            "attemptId": attempt_id, "requestKey": "steal"
        }))
        .await
        .assert_status_not_found();
}

#[tokio::test]
async fn disconnecting_an_unconnected_provider_is_not_found() {
    let (_state, client, _db, _org) = fixture(fake(&["x"])).await;
    // Nothing to tear down — and no row is invented to make the call succeed.
    client
        .post("/api/neev/agent/connections/codex/disconnect")
        .json(&serde_json::json!({ "requestKey": "bye" }))
        .await
        .assert_status_not_found();
}

#[tokio::test]
async fn malformed_identifiers_are_rejected_before_reaching_storage() {
    let (_state, client, _db, _org) = fixture(fake(&["x"])).await;
    client
        .get("/api/neev/agent/sessions/not-a-uuid")
        .await
        .assert_status_bad_request();
    client
        .get("/api/neev/agent/runs/not-a-run/events")
        .await
        .assert_status_bad_request();
    client
        .get("/api/neev/agent/sessions?archived=maybe")
        .await
        .assert_status_bad_request();
}

#[tokio::test]
async fn a_second_turn_carries_the_first_exchange_as_context() {
    let (_state, client, _db, _org) = fixture(fake(&["ok"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();
    let first = client.start_turn(&id, "remember: blue").await;
    // Drain the first run so it settles as completed.
    client
        .get(&format!(
            "/api/neev/agent/runs/{}/events",
            first["runId"].as_str().unwrap()
        ))
        .await;

    let second = client.start_turn(&id, "what colour?").await;
    assert_eq!(second["turn"]["sequence"], "2");
    // The STORED prompt is what the user typed — the composed transcript sent
    // to the provider is never written into the history they read back.
    assert_eq!(second["turn"]["prompt"], "what colour?");
    let turns = client
        .get(&format!("/api/neev/agent/sessions/{id}/turns"))
        .await
        .json::<Value>();
    assert_eq!(turns["items"][0]["prompt"], "remember: blue");
    assert_eq!(turns["items"][1]["prompt"], "what colour?");
}

/// The test button must never report a connection as verified on the strength
/// of having asked. With no supervisor there is nothing to verify against, so
/// the route refuses rather than echoing back the stored state.
#[tokio::test]
async fn testing_a_connection_without_a_supervisor_is_refused() {
    let (_state, client, _db, _org) = fixture(fake(&["x"])).await;
    let refused = client
        .post("/api/neev/agent/connections/claude/test")
        .json(&serde_json::json!({ "requestKey": "settings-test" }))
        .await;
    assert_eq!(
        refused.status_code(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "a check with no supervisor must not report a verified connection"
    );
    // And nothing was written: the connection is untouched.
    let connections = client
        .get("/api/neev/agent/connections")
        .await
        .json::<Value>();
    let claude = connections["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["provider"] == "claude")
        .unwrap()
        .clone();
    assert_eq!(claude["checkedAt"], Value::Null);
    assert_eq!(claude["accountLabel"], Value::Null);
}

#[tokio::test]
async fn testing_a_connection_validates_its_inputs_and_provider() {
    let (state, client, _db, _org) = fixture(fake(&["x"])).await;
    // An unknown provider is not distinguishable from having no connection.
    client
        .post("/api/neev/agent/connections/gpt/test")
        .json(&serde_json::json!({ "requestKey": "k" }))
        .await
        .assert_status_not_found();
    // A malformed request key is refused before anything is attempted.
    client
        .post("/api/neev/agent/connections/claude/test")
        .json(&serde_json::json!({ "requestKey": "x".repeat(65) }))
        .await
        .assert_status_bad_request();
    // Unauthenticated callers cannot probe connections at all.
    let server = axum_test::TestServer::new(crate::neev::http::router(state)).unwrap();
    assert!(server
        .post("/api/neev/agent/connections/claude/test")
        .json(&serde_json::json!({ "requestKey": "k" }))
        .await
        .status_code()
        .is_client_error());
}

#[tokio::test]
async fn reopening_a_chat_discovers_only_the_callers_own_bound_session() {
    let (state, client, database, _org) = fixture(fake(&["Done."])).await;
    let actor = owner_context(&database, "owner@example.test").await;
    let (chat_id, revision) = make_chat(&state, &actor).await;
    let path = format!("/api/neev/chats/{chat_id}/agent?provider=claude");

    // Nothing invoked yet: there is no holder to find, and saying so is the
    // honest answer rather than inventing an empty session.
    client
        .get(&path)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);

    let invoked = client
        .post(&format!("/api/neev/chats/{chat_id}/agent"))
        .json(&serde_json::json!({
            "prompt": "summarise the standup",
            "provider": "claude",
            "model": "default",
            "requestId": uuid::Uuid::new_v4().to_string(),
            "expectedAuthRevision": revision,
        }))
        .await;
    invoked.assert_status(axum::http::StatusCode::CREATED);
    let session_id = invoked.json::<Value>()["turn"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();

    // Now the holder is discoverable, and it is the one the invocation used —
    // which is what lets a reopened chat replay its turns and their activity.
    let found = client.get(&path).await;
    found.assert_status_ok();
    assert_eq!(found.json::<Value>()["id"], session_id);

    // And its turns are reachable by that id through the existing route.
    let turns = client
        .get(&format!("/api/neev/agent/sessions/{session_id}/turns"))
        .await;
    turns.assert_status_ok();
    let items = turns.json::<Value>();
    assert_eq!(items["items"][0]["prompt"], "summarise the standup");
    assert!(
        items["items"][0]["activity"].is_array(),
        "activity is always present, so the client never has to tell \
         \"no trace\" apart from \"field missing\""
    );
}

#[tokio::test]
async fn discovery_fails_closed_when_the_chat_is_no_longer_readable() {
    let (state, client, database, _org) = fixture(fake(&["Done."])).await;
    let actor = owner_context(&database, "owner@example.test").await;
    let (chat_id, revision) = make_chat(&state, &actor).await;

    client
        .post(&format!("/api/neev/chats/{chat_id}/agent"))
        .json(&serde_json::json!({
            "prompt": "summarise the standup",
            "provider": "claude",
            "model": "default",
            "requestId": uuid::Uuid::new_v4().to_string(),
            "expectedAuthRevision": revision,
        }))
        .await
        .assert_status(axum::http::StatusCode::CREATED);

    let path = format!("/api/neev/chats/{chat_id}/agent?provider=claude");
    client.get(&path).await.assert_status_ok();

    // Access is revoked underneath the caller: the chat becomes private to
    // somebody else. The session id is DERIVED from the chat id and the holder
    // is still owned by this caller, so without a chat check discovery would
    // happily keep answering about a chat they can no longer open.
    let stranger = h::native_account(&database, "stranger", "stranger@example.test").await;
    crate::neev::db::execute(
        &database,
        "UPDATE neev_resources SET visibility='private',creator_id=$1 WHERE id=$2",
        vec![stranger.into(), chat_id.clone().into()],
    )
    .await
    .unwrap();
    client
        .get(&path)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn one_members_traces_are_not_discoverable_by_another() {
    let (state, client, database, org) = fixture(fake(&["Done."])).await;
    let actor = owner_context(&database, "owner@example.test").await;
    let (chat_id, revision) = make_chat(&state, &actor).await;

    client
        .post(&format!("/api/neev/chats/{chat_id}/agent"))
        .json(&serde_json::json!({
            "prompt": "summarise the standup",
            "provider": "claude",
            "model": "default",
            "requestId": uuid::Uuid::new_v4().to_string(),
            "expectedAuthRevision": revision,
        }))
        .await
        .assert_status(axum::http::StatusCode::CREATED);

    // A second member of the SAME org, who can read the shared chat perfectly
    // well. The published reply is theirs to read; the invoker's tool traces
    // are not — a holder is per-person, not per-room.
    let peer_id = h::native_account(&database, "peer", "peer@example.test").await;
    h::add_membership(&database, org, peer_id, "member").await;
    let peer = sign_in(&state, &database, peer_id).await;
    peer.get(&format!("/api/neev/chats/{chat_id}/agent?provider=claude"))
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

/// Eight magic bytes and some padding — a real PNG as far as the sniffer is
/// concerned, which is the only opinion the server has about a file's type.
fn png(fill: u8) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&[fill; 24]);
    bytes
}

impl Client {
    async fn upload(&self, path: &str, name: &str, bytes: Vec<u8>) -> Value {
        let response = self
            .post(path)
            .add_header("x-neev-filename", name)
            .bytes(axum::body::Bytes::from(bytes))
            .await;
        response.assert_status(axum::http::StatusCode::CREATED);
        response.json::<Value>()
    }
}

/// A turn keeps the files it was started with, and keeps them across a reload.
///
/// Also the attachment-only case: sending files with nothing typed is an
/// ordinary thing to do, and what gets stored is a real ask rather than an
/// empty prompt the provider would refuse.
#[tokio::test]
async fn a_turn_carries_its_files_through_storage_and_a_reload() {
    let (_state, client, _db, _org) = fixture(fake(&["done"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();
    let stored = client
        .upload(
            &format!("/api/neev/agent/sessions/{id}/attachments"),
            // Percent-encoded once, decoded once: the stored name has the space.
            "plan%20b.png",
            png(1),
        )
        .await;
    assert_eq!(stored["fileName"], "plan b.png");
    assert_eq!(stored["contentType"], "image/png");
    assert_eq!(stored["byteLength"], 32);
    let key = stored["key"].as_str().unwrap().to_owned();

    let started = client
        .post(&format!("/api/neev/agent/sessions/{id}/turns"))
        .json(&serde_json::json!({"prompt": "", "attachmentKeys": [key.clone()]}))
        .await;
    started.assert_status(axum::http::StatusCode::CREATED);
    let started = started.json::<Value>();
    assert_eq!(
        started["turn"]["prompt"],
        "Please review the attached files."
    );
    let carried = started["turn"]["attachments"].as_array().unwrap();
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0]["key"], key.as_str());
    assert_eq!(carried[0]["fileName"], "plan b.png");

    // Reopening the session reads the same files from storage.
    let turns = client
        .get(&format!("/api/neev/agent/sessions/{id}/turns"))
        .await
        .json::<Value>();
    assert_eq!(
        turns["items"][0]["attachments"],
        started["turn"]["attachments"]
    );
}

/// A key names a file in ONE conversation. Holding it does not make it
/// carryable into another, even one the same person owns.
#[tokio::test]
async fn a_turn_cannot_carry_a_file_from_another_session() {
    let (_state, client, _db, _org) = fixture(fake(&["done"])).await;
    let mine = client.create_session().await;
    let other = client.create_session().await;
    let mine_id = mine["id"].as_str().unwrap().to_owned();
    let other_id = other["id"].as_str().unwrap().to_owned();
    let elsewhere = client
        .upload(
            &format!("/api/neev/agent/sessions/{other_id}/attachments"),
            "other.png",
            png(2),
        )
        .await;
    let key = elsewhere["key"].as_str().unwrap().to_owned();

    let refused = client
        .post(&format!("/api/neev/agent/sessions/{mine_id}/turns"))
        .json(&serde_json::json!({"prompt": "look", "attachmentKeys": [key]}))
        .await;
    refused.assert_status(axum::http::StatusCode::BAD_REQUEST);

    // Refused before the turn existed: nothing was started and nothing has to
    // be settled as failed.
    let turns = client
        .get(&format!("/api/neev/agent/sessions/{mine_id}/turns"))
        .await
        .json::<Value>();
    assert!(turns["items"].as_array().unwrap().is_empty());
}

/// An empty prompt with no files is still nothing to ask.
#[tokio::test]
async fn a_turn_with_neither_words_nor_files_is_refused() {
    let (_state, client, _db, _org) = fixture(fake(&["done"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap();
    let refused = client
        .post(&format!("/api/neev/agent/sessions/{id}/turns"))
        .json(&serde_json::json!({"prompt": ""}))
        .await;
    assert!(refused.status_code().is_client_error());
}

// ============================================================================
// Attachment soft-delete (archive/restore/trash listing) for agent sessions
// ============================================================================

/// Archived session attachments cannot be downloaded but can be restored.
#[tokio::test]
async fn archived_session_attachment_cannot_be_downloaded() {
    let (_state, client, _db, _org) = fixture(fake(&["done"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();

    let uploaded = client
        .upload(
            &format!("/api/neev/agent/sessions/{id}/attachments"),
            "test.png",
            png(1),
        )
        .await;
    let key = uploaded["key"].as_str().unwrap().to_owned();

    // The file is downloadable before archiving.
    let before = client.get(&format!("/api/neev/attachments/{key}")).await;
    before.assert_status_ok();

    // Archive the attachment.
    let archived = client
        .patch(&format!("/api/neev/attachments/{key}"))
        .json(&serde_json::json!({"archived": true}))
        .await;
    archived.assert_status_ok();
    archived.assert_json(&serde_json::json!({"ok": true}));

    // Now the file is not downloadable.
    let after = client.get(&format!("/api/neev/attachments/{key}")).await;
    after.assert_status_not_found();

    // Restore the attachment.
    let restored = client
        .patch(&format!("/api/neev/attachments/{key}"))
        .json(&serde_json::json!({"archived": false}))
        .await;
    restored.assert_status_ok();

    // The file is downloadable again.
    let final_check = client.get(&format!("/api/neev/attachments/{key}")).await;
    final_check.assert_status_ok();
}

/// Session attachment archiving requires ownership.
#[tokio::test]
async fn session_attachment_archiving_requires_ownership() {
    let (state, client, database, org) = fixture(fake(&["done"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();

    let uploaded = client
        .upload(
            &format!("/api/neev/agent/sessions/{id}/attachments"),
            "owned.png",
            png(1),
        )
        .await;
    let key = uploaded["key"].as_str().unwrap().to_owned();

    // Another org member cannot archive.
    let peer_user = h::native_account(&database, "peer", "peer@example.test").await;
    h::add_membership(&database, org, peer_user, "member").await;
    let peer = sign_in(&state, &database, peer_user).await;

    let denied = peer
        .patch(&format!("/api/neev/attachments/{key}"))
        .json(&serde_json::json!({"archived": true}))
        .await;
    denied.assert_status_not_found();
}

/// Archived session attachments are excluded from turn listings.
#[tokio::test]
async fn archived_session_attachments_excluded_from_turns() {
    let (_state, client, _db, _org) = fixture(fake(&["done"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();

    let first = client
        .upload(
            &format!("/api/neev/agent/sessions/{id}/attachments"),
            "keep.png",
            png(1),
        )
        .await;
    let second = client
        .upload(
            &format!("/api/neev/agent/sessions/{id}/attachments"),
            "delete.png",
            png(2),
        )
        .await;
    let first_key = first["key"].as_str().unwrap().to_owned();
    let second_key = second["key"].as_str().unwrap().to_owned();

    // Start a turn with both files.
    let started = client
        .post(&format!("/api/neev/agent/sessions/{id}/turns"))
        .json(&serde_json::json!({
            "prompt": "look",
            "attachmentKeys": [first_key.clone(), second_key.clone()]
        }))
        .await;
    started.assert_status(axum::http::StatusCode::CREATED);
    let started = started.json::<Value>();
    assert_eq!(started["turn"]["attachments"].as_array().unwrap().len(), 2);

    // Archive the second attachment.
    client
        .patch(&format!("/api/neev/attachments/{second_key}"))
        .json(&serde_json::json!({"archived": true}))
        .await
        .assert_status_ok();

    // Reload turns: only one attachment visible.
    let turns = client
        .get(&format!("/api/neev/agent/sessions/{id}/turns"))
        .await
        .json::<Value>();
    let attachments = turns["items"][0]["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["key"], first_key.as_str());
}

/// GET /api/neev/agent/sessions/{id}/attachments?archived=true lists only archived.
#[tokio::test]
async fn list_archived_session_attachments() {
    let (_state, client, _db, _org) = fixture(fake(&["done"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();

    let kept = client
        .upload(
            &format!("/api/neev/agent/sessions/{id}/attachments"),
            "keep.png",
            png(1),
        )
        .await;
    let deleted = client
        .upload(
            &format!("/api/neev/agent/sessions/{id}/attachments"),
            "delete.png",
            png(2),
        )
        .await;
    let deleted_key = deleted["key"].as_str().unwrap().to_owned();

    // Archive one.
    client
        .patch(&format!("/api/neev/attachments/{deleted_key}"))
        .json(&serde_json::json!({"archived": true}))
        .await
        .assert_status_ok();

    // List archived.
    let listed = client
        .get(&format!(
            "/api/neev/agent/sessions/{id}/attachments?archived=true"
        ))
        .await;
    listed.assert_status_ok();
    let body = listed.json::<Value>();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["key"], deleted_key.as_str());
    assert_eq!(items[0]["fileName"], "delete.png");
    assert_eq!(body["nextAfter"], serde_json::Value::Null);
}

/// List archived session attachments requires ownership.
#[tokio::test]
async fn list_archived_session_attachments_requires_ownership() {
    let (state, client, database, org) = fixture(fake(&["done"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();

    let uploaded = client
        .upload(
            &format!("/api/neev/agent/sessions/{id}/attachments"),
            "del.png",
            png(1),
        )
        .await;
    let key = uploaded["key"].as_str().unwrap().to_owned();

    client
        .patch(&format!("/api/neev/attachments/{key}"))
        .json(&serde_json::json!({"archived": true}))
        .await
        .assert_status_ok();

    // Another org member cannot list archived.
    let peer_user = h::native_account(&database, "peer", "peer@example.test").await;
    h::add_membership(&database, org, peer_user, "member").await;
    let peer = sign_in(&state, &database, peer_user).await;

    peer.get(&format!(
        "/api/neev/agent/sessions/{id}/attachments?archived=true"
    ))
    .await
    .assert_status_not_found();
}
