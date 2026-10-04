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
async fn a_replay_after_started_carries_each_accepted_follow_up_once() {
    let (_state, client, database, org) = fixture(fake(&["hé", "llo 😀"])).await;
    let session = client.create_session().await;
    let id = session["id"].as_str().unwrap().to_owned();
    let started = client.start_turn(&id, "greet me").await;
    let run_id = started["runId"].as_str().unwrap().to_owned();
    // Drain the live stream so the turn has settled.
    client
        .get(&format!("/api/neev/agent/runs/{run_id}/events"))
        .await
        .text();
    let turn = crate::neev::agents::sessions::turn_by_run_id(&database, &run_id)
        .await
        .unwrap()
        .unwrap();
    let accepted = uuid::Uuid::new_v4().to_string();
    let pending = uuid::Uuid::new_v4().to_string();
    for request_id in [&accepted, &pending] {
        crate::neev::db::record_steer_input(
            &database, org, &id, turn.sequence, request_id, "also wave", NOW,
        )
        .await
        .unwrap();
    }
    // Offset counts Unicode scalars: "hé" is two, though three UTF-8 bytes.
    crate::neev::db::accept_steer_input(&database, org, &id, turn.sequence, &accepted, 2)
        .await
        .unwrap();

    // The client resets live inputs on `started`, so the replay must carry
    // the accepted follow-up again — once, before `finished`, and never the
    // unacknowledged one.
    let replay = sse_events(
        &client
            .get(&format!("/api/neev/agent/runs/{run_id}/events"))
            .await
            .text(),
    );
    assert_eq!(replay[0]["type"], "started");
    let inputs: Vec<&Value> = replay.iter().filter(|e| e["type"] == "input").collect();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0]["runId"], run_id.as_str());
    assert_eq!(inputs[0]["requestId"], accepted.as_str());
    assert_eq!(inputs[0]["text"], "also wave");
    assert_eq!(inputs[0]["responseOffset"], 2);
    assert_eq!(replay.last().unwrap()["type"], "finished");
}

#[tokio::test]
async fn steer_text_is_bounded_in_bytes_before_any_reservation() {
    let (_state, client, _db, _org) = fixture(fake(&["x"])).await;
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let steer = |text: String| {
        client
            .post(&format!("/api/neev/agent/runs/{run_id}/steer"))
            .json(&serde_json::json!({
                "text": text,
                "requestId": uuid::Uuid::new_v4().to_string(),
            }))
    };
    // 2049 four-byte scalars: well under 32768 characters, over 8192 bytes.
    steer("😀".repeat(2049)).await.assert_status_bad_request();
    // Exactly at the cap passes validation and reaches the ownership lookup.
    steer("😀".repeat(2048)).await.assert_status_not_found();
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

    let global = client.get("/api/neev/attachments?archived=true").await;
    global.assert_status_ok();
    assert_eq!(global.json::<Value>()["items"][0]["key"], key);

    // Another org member cannot list archived.
    let peer_user = h::native_account(&database, "peer", "peer@example.test").await;
    h::add_membership(&database, org, peer_user, "member").await;
    let peer = sign_in(&state, &database, peer_user).await;

    let hidden = peer.get("/api/neev/attachments?archived=true").await;
    hidden.assert_status_ok();
    assert!(hidden.json::<Value>()["items"]
        .as_array()
        .unwrap()
        .is_empty());

    peer.get(&format!(
        "/api/neev/agent/sessions/{id}/attachments?archived=true"
    ))
    .await
    .assert_status_not_found();
}

// ── Retrying a failed run ────────────────────────────────────────────────────

/// Mark a settled run as a provider failure, as a real failed provider would
/// have left it (the fake runtime has no way to fail on its own).
async fn fail_run(database: &sea_orm::DatabaseConnection, run_id: &str) {
    let done = crate::neev::db::execute(
        database,
        "UPDATE neev_agent_turns SET status='failed',error_code='provider',completed_at=updated_at WHERE run_id=$1",
        vec![run_id.into()],
    )
    .await
    .unwrap();
    assert_eq!(done.rows_affected(), 1);
}

async fn settle(client: &Client, run_id: &str) -> Vec<Value> {
    sse_events(
        &client
            .get(&format!("/api/neev/agent/runs/{run_id}/events"))
            .await
            .text(),
    )
}

#[tokio::test]
async fn retrying_a_failed_team_run_reruns_the_stored_ask_without_reposting_it() {
    let (state, client, database, _org) = fixture(fake(&["Standup ", "notes."])).await;
    let actor = owner_context(&database, "owner@example.test").await;
    let (chat_id, revision) = make_chat(&state, &actor).await;
    let first = client
        .post(&format!("/api/neev/chats/{chat_id}/agent"))
        .json(&serde_json::json!({
            "prompt": "summarise the standup",
            "provider": "claude",
            "model": "default",
            "requestId": uuid::Uuid::new_v4().to_string(),
            "expectedAuthRevision": revision,
        }))
        .await
        .json::<Value>();
    let failed_run = first["runId"].as_str().unwrap().to_owned();
    settle(&client, &failed_run).await;
    fail_run(&database, &failed_run).await;

    let retry_id = uuid::Uuid::new_v4().to_string();
    let response = client
        .post(&format!("/api/neev/agent/runs/{failed_run}/retry"))
        .json(&serde_json::json!({ "requestId": retry_id }))
        .await;
    response.assert_status(axum::http::StatusCode::CREATED);
    let retried = response.json::<Value>();
    let new_run = retried["runId"].as_str().unwrap().to_owned();
    assert_ne!(new_run, failed_run, "a retry is a new run");
    // The stored ask, not anything the request could have said.
    assert_eq!(retried["turn"]["prompt"], "summarise the standup");
    assert_eq!(settle(&client, &new_run).await.last().unwrap()["status"], "completed");

    // The same retry again (a double click, a dropped response) is the same run.
    let again = client
        .post(&format!("/api/neev/agent/runs/{failed_run}/retry"))
        .json(&serde_json::json!({ "requestId": retry_id }))
        .await
        .json::<Value>();
    assert_eq!(again["runId"], new_run);

    let items = client
        .get(&format!("/api/neev/chats/{chat_id}/messages"))
        .await
        .json::<Value>()["items"]
        .as_array()
        .unwrap()
        .clone();
    let asks = items.iter().filter(|m| m["authorKind"] == "human").count();
    let replies = items.iter().filter(|m| m["authorKind"] == "agent").count();
    assert_eq!(asks, 1, "the ask is never posted twice: {items:?}");
    // The original run's reply (published before it was marked failed) and the
    // retry's — one each, not one per request.
    assert_eq!(replies, 2, "{items:?}");
}

#[tokio::test]
async fn retrying_a_failed_private_turn_reuses_its_prompt_and_only_failed_turns_retry() {
    let (_state, client, database, _org) = fixture(fake(&["Plan ", "ready."])).await;
    let session = client.create_session().await;
    let session_id = session["id"].as_str().unwrap().to_owned();
    let started = client.start_turn(&session_id, "draft the launch plan").await;
    let run_id = started["runId"].as_str().unwrap().to_owned();
    settle(&client, &run_id).await;

    // A completed turn has nothing to retry.
    client
        .post(&format!("/api/neev/agent/runs/{run_id}/retry"))
        .json(&serde_json::json!({ "requestId": uuid::Uuid::new_v4().to_string() }))
        .await
        .assert_status(axum::http::StatusCode::CONFLICT);

    fail_run(&database, &run_id).await;
    let retried = client
        .post(&format!("/api/neev/agent/runs/{run_id}/retry"))
        .json(&serde_json::json!({ "requestId": uuid::Uuid::new_v4().to_string() }))
        .await
        .json::<Value>();
    assert_eq!(retried["turn"]["prompt"], "draft the launch plan");
    let new_run = retried["runId"].as_str().unwrap().to_owned();
    assert_eq!(settle(&client, &new_run).await.last().unwrap()["status"], "completed");
    let turns = client
        .get(&format!("/api/neev/agent/sessions/{session_id}/turns"))
        .await
        .json::<Value>()["items"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0]["status"], "failed");
    assert_eq!(turns[1]["status"], "completed");
    assert_eq!(turns[1]["response"], "Plan ready.");
}

#[tokio::test]
async fn a_retry_needs_a_well_formed_run_and_request_id() {
    let (_state, client, _database, _org) = fixture(fake(&["x"])).await;
    client
        .post("/api/neev/agent/runs/not-a-run/retry")
        .json(&serde_json::json!({ "requestId": uuid::Uuid::new_v4().to_string() }))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
    client
        .post("/api/neev/agent/runs/0123456789abcdef0123456789abcdef/retry")
        .json(&serde_json::json!({ "requestId": "nope" }))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
    // A well-formed run this caller does not own (or that does not exist).
    client
        .post("/api/neev/agent/runs/0123456789abcdef0123456789abcdef/retry")
        .json(&serde_json::json!({ "requestId": uuid::Uuid::new_v4().to_string() }))
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_failed_team_retry_can_be_retried_but_a_stale_retry_of_the_original_cannot() {
    let (state, client, database, _org) = fixture(fake(&["Standup ", "notes."])).await;
    let actor = owner_context(&database, "owner@example.test").await;
    let (chat_id, revision) = make_chat(&state, &actor).await;
    let first = client
        .post(&format!("/api/neev/chats/{chat_id}/agent"))
        .json(&serde_json::json!({
            "prompt": "summarise the standup",
            "provider": "claude",
            "model": "default",
            "requestId": uuid::Uuid::new_v4().to_string(),
            "expectedAuthRevision": revision,
        }))
        .await
        .json::<Value>();
    let original = first["runId"].as_str().unwrap().to_owned();
    settle(&client, &original).await;
    fail_run(&database, &original).await;

    // Retry 1 — and it fails too (rate limited again, say).
    let one = client
        .post(&format!("/api/neev/agent/runs/{original}/retry"))
        .json(&serde_json::json!({ "requestId": uuid::Uuid::new_v4().to_string() }))
        .await;
    one.assert_status(axum::http::StatusCode::CREATED);
    let one_run = one.json::<Value>()["runId"].as_str().unwrap().to_owned();
    settle(&client, &one_run).await;
    fail_run(&database, &one_run).await;

    // Retrying the RETRY works: it still knows which message it answers.
    let two = client
        .post(&format!("/api/neev/agent/runs/{one_run}/retry"))
        .json(&serde_json::json!({ "requestId": uuid::Uuid::new_v4().to_string() }))
        .await;
    two.assert_status(axum::http::StatusCode::CREATED);
    let two = two.json::<Value>();
    assert_eq!(two["turn"]["prompt"], "summarise the standup");
    let two_run = two["runId"].as_str().unwrap().to_owned();
    assert_eq!(settle(&client, &two_run).await.last().unwrap()["status"], "completed");

    // A stale tab retrying the ORIGINAL with a fresh request id: that run was
    // already retried, so nothing runs and nothing is republished.
    client
        .post(&format!("/api/neev/agent/runs/{original}/retry"))
        .json(&serde_json::json!({ "requestId": uuid::Uuid::new_v4().to_string() }))
        .await
        .assert_status(axum::http::StatusCode::CONFLICT);

    let items = client
        .get(&format!("/api/neev/chats/{chat_id}/messages"))
        .await
        .json::<Value>()["items"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(items.iter().filter(|m| m["authorKind"] == "human").count(), 1);
    // Original (published before it was marked failed), retry 1 (likewise) and
    // retry 2 — exactly one per run that ran, none from the refused stale retry.
    assert_eq!(items.iter().filter(|m| m["authorKind"] == "agent").count(), 3);
}

#[tokio::test]
async fn a_replayed_retry_returns_its_turn_even_after_the_source_changed() {
    let (_state, client, database, _org) = fixture(fake(&["Plan ", "ready."])).await;
    let session = client.create_session().await;
    let session_id = session["id"].as_str().unwrap().to_owned();
    let started = client.start_turn(&session_id, "draft the launch plan").await;
    let run_id = started["runId"].as_str().unwrap().to_owned();
    settle(&client, &run_id).await;
    fail_run(&database, &run_id).await;
    let request = uuid::Uuid::new_v4().to_string();
    let first = client
        .post(&format!("/api/neev/agent/runs/{run_id}/retry"))
        .json(&serde_json::json!({ "requestId": request }))
        .await
        .json::<Value>();
    let new_run = first["runId"].as_str().unwrap().to_owned();
    settle(&client, &new_run).await;
    // The source is no longer the latest failure (its retry completed) and
    // could not be retried afresh — but the SAME request still replays.
    let again = client
        .post(&format!("/api/neev/agent/runs/{run_id}/retry"))
        .json(&serde_json::json!({ "requestId": request }))
        .await;
    again.assert_status(axum::http::StatusCode::CREATED);
    let again = again.json::<Value>();
    assert_eq!(again["runId"], new_run);
    assert_eq!(again["turn"]["status"], "completed");
}

#[tokio::test]
async fn a_private_retry_carries_the_follow_ups_the_failed_attempt_accepted() {
    let (_state, client, database, org) = fixture(fake(&["ok"])).await;
    let session = client.create_session().await;
    let session_id = session["id"].as_str().unwrap().to_owned();
    let started = client.start_turn(&session_id, "draft the launch plan").await;
    let run_id = started["runId"].as_str().unwrap().to_owned();
    let sequence: i64 = started["turn"]["sequence"].as_str().unwrap().parse().unwrap();
    settle(&client, &run_id).await;
    // One follow-up accepted into that run, one never accepted.
    for (id, text, accepted) in [
        ("11111111-1111-4111-8111-111111111111", "also cover pricing", true),
        ("22222222-2222-4222-8222-222222222222", "never delivered", false),
    ] {
        crate::neev::db::record_steer_input(&database, org, &session_id, sequence, id, text, NOW)
            .await
            .unwrap();
        if accepted {
            crate::neev::db::accept_steer_input(&database, org, &session_id, sequence, id, 0)
                .await
                .unwrap();
        }
    }
    fail_run(&database, &run_id).await;
    let retried = client
        .post(&format!("/api/neev/agent/runs/{run_id}/retry"))
        .json(&serde_json::json!({ "requestId": uuid::Uuid::new_v4().to_string() }))
        .await
        .json::<Value>();
    let prompt = retried["turn"]["prompt"].as_str().unwrap();
    assert!(prompt.starts_with("draft the launch plan"), "{prompt}");
    assert!(prompt.contains("also cover pricing"), "{prompt}");
    assert!(!prompt.contains("never delivered"), "{prompt}");
}

#[tokio::test]
async fn a_retry_that_cannot_be_linked_is_settled_not_left_running() {
    let (_state, client, database, _org) = fixture(fake(&["Plan ", "ready."])).await;
    let session = client.create_session().await;
    let session_id = session["id"].as_str().unwrap().to_owned();
    let started = client.start_turn(&session_id, "draft the launch plan").await;
    let run_id = started["runId"].as_str().unwrap().to_owned();
    settle(&client, &run_id).await;
    fail_run(&database, &run_id).await;
    // A storage failure that is NOT a conflict, at exactly the step after the
    // retry turn was created and before it launches.
    crate::neev::db::execute(
        &database,
        "CREATE TRIGGER fail_retry_link BEFORE INSERT ON neev_requests WHEN NEW.action='agent.run.retry' BEGIN SELECT RAISE(ABORT,'injected'); END",
        vec![],
    )
    .await
    .unwrap();
    let response = client
        .post(&format!("/api/neev/agent/runs/{run_id}/retry"))
        .json(&serde_json::json!({ "requestId": uuid::Uuid::new_v4().to_string() }))
        .await;
    assert!(!response.status_code().is_success(), "{}", response.status_code());
    let turns = client
        .get(&format!("/api/neev/agent/sessions/{session_id}/turns"))
        .await
        .json::<Value>()["items"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        turns.iter().all(|turn| turn["status"] != "running"),
        "no turn may be left running and never launched: {turns:?}"
    );
    // And the session is not wedged: the next ask is accepted.
    crate::neev::db::execute(&database, "DROP TRIGGER fail_retry_link", vec![])
        .await
        .unwrap();
    client.start_turn(&session_id, "try again later").await;
}

#[tokio::test]
async fn a_retry_refuses_rather_than_silently_dropping_follow_ups_it_cannot_read() {
    let (_state, client, database, _org) = fixture(fake(&["ok"])).await;
    let session = client.create_session().await;
    let session_id = session["id"].as_str().unwrap().to_owned();
    let started = client.start_turn(&session_id, "draft the launch plan").await;
    let run_id = started["runId"].as_str().unwrap().to_owned();
    settle(&client, &run_id).await;
    fail_run(&database, &run_id).await;
    // The follow-ups cannot be read at all.
    crate::neev::db::execute(&database, "ALTER TABLE neev_steer_inputs RENAME TO neev_steer_inputs_gone", vec![])
        .await
        .unwrap();
    let response = client
        .post(&format!("/api/neev/agent/runs/{run_id}/retry"))
        .json(&serde_json::json!({ "requestId": uuid::Uuid::new_v4().to_string() }))
        .await;
    assert!(response.status_code().is_server_error(), "{}", response.status_code());
    // Refused BEFORE any turn exists: nothing ran without them.
    let turns = client
        .get(&format!("/api/neev/agent/sessions/{session_id}/turns"))
        .await
        .json::<Value>()["items"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(turns.len(), 1, "{turns:?}");
}

// ---------------------------------------------------------------------------
// Direct sign-in `/callback` (direct-signin-review.md B2/I4): ownership, CSRF,
// idempotent replay, conflict and release, against the real routes with a
// scripted supervisor that records every exchange.
// ---------------------------------------------------------------------------

mod direct_signin {
    use super::*;
    use crate::neev::agents::control_dto as dto;
    use crate::neev::agents::supervisor::{ScriptedSupervisor, Supervisor, SupervisorError};

    const STATE: &str = "Qx7bW2mZ0kP9sR4tV1yU8nA3cF6hJ5dL2gE0iO7uK4w";

    fn response(op: dto::Op, body: Option<dto::ResultBody>, error: Option<dto::ErrorCode>) -> dto::Response {
        dto::Response {
            coordinator: dto::Coordinator::new(
                dto::Counter::new("1".into()).unwrap(),
                dto::Id::new("1".repeat(32)).unwrap(),
            ),
            supervisor_boot_id: dto::Id::new("3".repeat(32)).unwrap(),
            sequence: dto::Counter::new("1".into()).unwrap(),
            request_id: dto::Id::new("5".repeat(32)).unwrap(),
            operation: op,
            outcome: if error.is_some() { dto::Outcome::Error } else { dto::Outcome::Ok },
            body,
            error: error.map(|code| dto::AppError { code, retry_after_ms: None }),
        }
    }

    fn started() -> Result<dto::Response, SupervisorError> {
        Ok(response(
            dto::Op::StartLogin,
            Some(dto::ResultBody::StartLogin(dto::StartLoginResult {
                disposition: dto::Disposition::Accepted,
                poll_after_ms: dto::RetryMs::new(500).unwrap(),
            })),
            None,
        ))
    }

    fn delivered() -> Result<dto::Response, SupervisorError> {
        Ok(response(
            dto::Op::DeliverCallback,
            Some(dto::ResultBody::DeliverCallback(dto::LoginSnapshot::Verifying {
                expires_at: dto::UnixSec::new(NOW + 600).unwrap(),
                poll_after_ms: dto::RetryMs::new(500).unwrap(),
            })),
            None,
        ))
    }

    fn rejected() -> Result<dto::Response, SupervisorError> {
        Ok(response(dto::Op::DeliverCallback, None, Some(dto::ErrorCode::Rejected)))
    }

    async fn scripted(
        answers: Vec<Result<dto::Response, SupervisorError>>,
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
        server_state.set_agent_runtime(fake(&["x"]));
        server_state.supervisor = Arc::new(Supervisor::Scripted(ScriptedSupervisor::new(answers)));
        let state = Arc::new(server_state);
        let client = sign_in(&state, &database, user).await;
        (state, client, database, org)
    }

    /// The ops the supervisor was asked for, by wire name (the wire enums
    /// deliberately have no `Debug`).
    fn exchanges(state: &ServerState) -> Vec<&'static str> {
        let Supervisor::Scripted(scripted) = state.supervisor.as_ref() else {
            panic!("scripted supervisor expected");
        };
        scripted.requests().iter().map(|request| request.op().as_str()).collect()
    }

    async fn connect(client: &Client) -> String {
        let started = client
            .post("/api/neev/agent/connections/codex/connect")
            .json(&serde_json::json!({ "requestKey": "direct-connect", "method": "browser" }))
            .await;
        started.assert_status(axum::http::StatusCode::ACCEPTED);
        started.json::<Value>()["attemptId"].as_str().unwrap().to_owned()
    }

    fn body(key: &str) -> Value {
        serde_json::json!({ "requestKey": key, "query": format!("code=ac_1&state={STATE}") })
    }

    #[tokio::test]
    async fn the_owner_delivers_once_and_a_replay_sends_nothing() {
        let (state, client, _db, _org) = scripted(vec![started(), delivered()]).await;
        let attempt = connect(&client).await;
        let url = format!("/api/neev/agent/connections/codex/attempts/{attempt}/callback");
        client.post(&url).json(&body("key-aaaa-1")).await.assert_status(axum::http::StatusCode::ACCEPTED);
        // Same key (network retry, or the page reloaded): same answer, and the
        // supervisor is NOT asked again.
        client.post(&url).json(&body("key-aaaa-1")).await.assert_status(axum::http::StatusCode::ACCEPTED);
        // Another key (extension + paste race): 409, client re-reads.
        client.post(&url).json(&body("key-bbbb-2")).await.assert_status(axum::http::StatusCode::CONFLICT);
        assert_eq!(exchanges(&state), vec!["start_login", "deliver_callback"]);
        // The start asked for direct sign-in explicitly.
        let Supervisor::Scripted(scripted) = state.supervisor.as_ref() else { unreachable!() };
        match &scripted.requests()[0].body {
            dto::RequestBody::StartLogin(start) => {
                assert!(start.method == Some(dto::LoginMethod::Browser))
            }
            _ => panic!("first exchange is not a start"),
        }
    }

    #[tokio::test]
    async fn a_refused_callback_is_400_and_releases_the_attempt_for_a_retry() {
        let (state, client, _db, _org) = scripted(vec![started(), rejected(), delivered()]).await;
        let attempt = connect(&client).await;
        let url = format!("/api/neev/agent/connections/codex/attempts/{attempt}/callback");
        client.post(&url).json(&body("key-aaaa-1")).await.assert_status_bad_request();
        // Not burned: the same key (or a paste) can try again.
        client.post(&url).json(&body("key-aaaa-1")).await.assert_status(axum::http::StatusCode::ACCEPTED);
        assert_eq!(
            exchanges(&state),
            vec!["start_login", "deliver_callback", "deliver_callback"]
        );
    }

    #[tokio::test]
    async fn only_the_owner_on_the_right_provider_with_csrf_reaches_the_supervisor() {
        let (state, client, database, org) = scripted(vec![started()]).await;
        let attempt = connect(&client).await;
        let url = format!("/api/neev/agent/connections/codex/attempts/{attempt}/callback");
        // Another member of the same org.
        let peer_user = h::native_account(&database, "peer", "peer@example.test").await;
        h::add_membership(&database, org, peer_user, "member").await;
        let peer = sign_in(&state, &database, peer_user).await;
        peer.post(&url).json(&body("key-peer-01")).await.assert_status_not_found();
        // The owner, but through the other provider's path.
        client
            .post(&format!("/api/neev/agent/connections/claude/attempts/{attempt}/callback"))
            .json(&body("key-aaaa-1"))
            .await
            .assert_status_not_found();
        // No CSRF header.
        let no_csrf = client
            .server
            .post(&url)
            .add_header(header::ORIGIN, ORIGIN)
            .add_header(header::COOKIE, client.cookies.clone())
            .json(&body("key-aaaa-1"))
            .await;
        assert!(no_csrf.status_code().is_client_error());
        // A foreign origin.
        let foreign = client
            .server
            .post(&url)
            .add_header(header::ORIGIN, "https://evil.example")
            .add_header(header::COOKIE, client.cookies.clone())
            .add_header("x-neev-csrf", client.csrf.clone())
            .json(&body("key-aaaa-1"))
            .await;
        assert!(foreign.status_code().is_client_error());
        // Malformed key, oversized body, non-ASCII query.
        client.post(&url).json(&body("short")).await.assert_status_bad_request();
        let huge = serde_json::json!({ "requestKey": "key-aaaa-1", "query": "x".repeat(9000) });
        assert!(client.post(&url).json(&huge).await.status_code().is_client_error());
        client
            .post(&url)
            .json(&serde_json::json!({ "requestKey": "key-aaaa-1", "query": "code=\u{e9}" }))
            .await
            .assert_status_bad_request();
        // None of it reached the supervisor.
        assert_eq!(exchanges(&state), vec!["start_login"]);
    }
}

// ---- Auto model routing ----

mod auto_routing {
    use super::*;
    use crate::neev::agents::routing::{
        Decision, DecisionError, DecisionInput, DecisionProvider, Router,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Picks the option the catalogue labels `wanted`, at a high complexity.
    struct Pick {
        wanted: &'static str,
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl DecisionProvider for Pick {
        fn name(&self) -> &'static str {
            "jev"
        }
        async fn decide(&self, input: &DecisionInput) -> Result<Decision, DecisionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let option = input
                .options
                .iter()
                .position(|option| option.model == self.wanted)
                .ok_or(DecisionError::Malformed)?;
            Ok(Decision {
                option,
                confidence: 0.9,
                complexity: 2.0,
                complexity_confidence: 0.9,
            })
        }
    }

    async fn routed_fixture(
        router: Router,
        log: Arc<std::sync::Mutex<Vec<crate::neev::agents::execution::RunRequest>>>,
    ) -> Client {
        let database = h::database().await;
        let user = h::native_account(&database, "owner", "owner@example.test").await;
        h::organization(&database, user, "admin").await;
        let mut server_state = ServerState::new(
            database.clone(),
            h::preview_config(),
            Arc::new(h::TestClock::new(NOW)),
        )
        .unwrap();
        server_state.set_agent_runtime(AgentRuntime::Fake(
            FakeRuntime::new(vec!["done".to_owned()]).recording(log),
        ));
        server_state.set_agent_router(router);
        let state = Arc::new(server_state);
        sign_in(&state, &database, user).await
    }

    fn pick(wanted: &'static str) -> (Router, Arc<Pick>) {
        let provider = Arc::new(Pick {
            wanted,
            calls: AtomicUsize::new(0),
        });
        (
            Router::Ready {
                provider: provider.clone(),
                min_confidence: 0.3,
            },
            provider,
        )
    }

    async fn auto_turn(client: &Client, session: &str, prompt: &str) -> Value {
        let response = client
            .post(&format!("/api/neev/agent/sessions/{session}/turns"))
            .json(&serde_json::json!({ "prompt": prompt, "auto": true }))
            .await;
        response.assert_status(axum::http::StatusCode::CREATED);
        response.json::<Value>()
    }

    #[tokio::test]
    async fn status_is_off_by_default_and_honest_about_a_missing_key() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let client = routed_fixture(Router::Off, log.clone()).await;
        let status = client.get("/api/neev/agent/routing").await.json::<Value>();
        assert_eq!(status["state"], "off");
        assert_eq!(status["router"], Value::Null);

        let client =
            routed_fixture(Router::MissingCredential { router: "jev" }, log).await;
        let status = client.get("/api/neev/agent/routing").await.json::<Value>();
        assert_eq!(status["state"], "missing_credential");
        assert_eq!(status["router"], "jev");
        assert!(status["maxTaskChars"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn an_auto_turn_runs_and_records_the_routed_model() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (router, picker) = pick("Opus");
        let client = routed_fixture(router, log.clone()).await;
        let session = client.create_session().await;
        let id = session["id"].as_str().unwrap();
        let started =
            auto_turn(&client, id, "Redesign the sync engine across three services").await;
        assert_eq!(started["route"]["outcome"], "routed");
        assert_eq!(started["route"]["model"], "opus");
        assert_eq!(started["route"]["effort"], "high");
        assert_eq!(started["route"]["fast"], false);
        assert_eq!(started["route"]["router"], "jev");
        assert_eq!(started["turn"]["model"], "opus");
        assert_eq!(picker.calls.load(Ordering::SeqCst), 1);
        // The session now says what its latest turn ran on, so a retry and the
        // next brief follow-up use the same choice.
        let stored = client
            .get(&format!("/api/neev/agent/sessions/{id}"))
            .await
            .json::<Value>();
        assert_eq!(stored["model"], "opus");
        assert_eq!(stored["effort"], "high");
        let runs = log.lock().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].model, "opus");
        assert_eq!(runs[0].effort.as_deref(), Some("high"));
        assert!(!runs[0].fast);
    }

    #[tokio::test]
    async fn without_a_router_auto_uses_the_session_settings_and_says_why() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let client = routed_fixture(Router::MissingCredential { router: "jev" }, log.clone()).await;
        let session = client.create_session().await;
        let id = session["id"].as_str().unwrap();
        let started = auto_turn(&client, id, "Summarise the open pull requests").await;
        assert_eq!(started["route"]["outcome"], "fallback");
        assert_eq!(started["route"]["reason"], "missing_credential");
        assert_eq!(started["route"]["router"], Value::Null);
        assert_eq!(started["turn"]["model"], "default");
        let stored = client
            .get(&format!("/api/neev/agent/sessions/{id}"))
            .await
            .json::<Value>();
        assert_eq!(stored["model"], "default");
        assert_eq!(log.lock().unwrap()[0].model, "default");
    }

    #[tokio::test]
    async fn a_manual_turn_is_untouched_and_carries_no_route() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (router, picker) = pick("Opus");
        let client = routed_fixture(router, log.clone()).await;
        let session = client.create_session().await;
        let started = client
            .start_turn(session["id"].as_str().unwrap(), "Explain this diff")
            .await;
        assert!(started.get("route").is_none());
        assert_eq!(started["turn"]["model"], "default");
        assert_eq!(picker.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn routing_a_new_chat_with_nothing_connected_is_a_conflict() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (router, picker) = pick("Opus");
        let client = routed_fixture(router, log).await;
        let response = client
            .post("/api/neev/agent/route")
            .json(&serde_json::json!({
                "prompt": "Build the importer",
                "provider": "claude",
                "model": "default",
            }))
            .await;
        response.assert_status(axum::http::StatusCode::CONFLICT);
        assert_eq!(picker.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn routing_a_new_chat_refuses_an_empty_or_unknown_body() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let client = routed_fixture(Router::Off, log).await;
        for body in [
            serde_json::json!({"prompt": "  ", "provider": "claude", "model": "default"}),
            serde_json::json!({"prompt": "x", "provider": "claude", "model": "default", "apiKey": "k"}),
        ] {
            let response = client.post("/api/neev/agent/route").json(&body).await;
            assert!(response.status_code().is_client_error(), "{body}");
        }
    }

    // ---- Supervision, tickets and usage ----

    use std::collections::HashMap;

    type Runs = Arc<std::sync::Mutex<Vec<crate::neev::agents::execution::RunRequest>>>;
    type ToolFree =
        Arc<std::sync::Mutex<Vec<crate::neev::agents::execution::ToolFreeRequest>>>;

    struct Full {
        client: Client,
        state: Arc<ServerState>,
        org: i32,
        user: i32,
        runs: Runs,
        tool_free: ToolFree,
        /// The storage handle of each connected provider, by name.
        handles: HashMap<String, String>,
    }

    /// Picks `wanted` at a given complexity level.
    struct PickAt {
        wanted: &'static str,
        complexity: f64,
        calls: AtomicUsize,
        seen: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl DecisionProvider for PickAt {
        fn name(&self) -> &'static str {
            "jev"
        }
        async fn decide(&self, input: &DecisionInput) -> Result<Decision, DecisionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.seen.lock().unwrap() = input
                .options
                .iter()
                .map(|option| format!("{}/{}", option.agent, option.model))
                .collect();
            let option = input
                .options
                .iter()
                .position(|option| option.model == self.wanted)
                .ok_or(DecisionError::Malformed)?;
            Ok(Decision {
                option,
                confidence: 0.9,
                complexity: self.complexity,
                complexity_confidence: 0.9,
            })
        }
    }

    fn pick_at(wanted: &'static str, complexity: f64) -> (Router, Arc<PickAt>) {
        let provider = Arc::new(PickAt {
            wanted,
            complexity,
            calls: AtomicUsize::new(0),
            seen: std::sync::Mutex::new(Vec::new()),
        });
        (
            Router::Ready {
                provider: provider.clone(),
                min_confidence: 0.3,
            },
            provider,
        )
    }

    async fn full(
        router: Router,
        supervision: crate::neev::config::SupervisionMode,
        connected: &[&str],
    ) -> Full {
        full_with(router, supervision, connected, |runtime| runtime).await
    }

    /// `full` with the fake runtime adjusted (hanging runs or supervisors,
    /// failing executor launches).
    async fn full_with(
        router: Router,
        supervision: crate::neev::config::SupervisionMode,
        connected: &[&str],
        tweak: fn(FakeRuntime) -> FakeRuntime,
    ) -> Full {
        full_on(h::database().await, router, supervision, connected, tweak).await
    }

    /// `full_with` on a given database (an isolated PostgreSQL one included).
    async fn full_on(
        database: sea_orm::DatabaseConnection,
        router: Router,
        supervision: crate::neev::config::SupervisionMode,
        connected: &[&str],
        tweak: fn(FakeRuntime) -> FakeRuntime,
    ) -> Full {
        let user = h::native_account(&database, "owner", "owner@example.test").await;
        let (org, _) = h::organization(&database, user, "admin").await;
        let mut handles = HashMap::new();
        for provider in connected {
            let handle = uuid::Uuid::new_v4().simple().to_string();
            handles.insert((*provider).to_owned(), handle.clone());
            crate::neev::db::execute(
                &database,
                "INSERT INTO neev_agent_connections(id,org_id,user_id,provider,generation,state,account_label,checked_at,error_code,storage_handle,created_at,updated_at) VALUES($1,$2,$3,$4,1,'connected','acct',1700000000,NULL,$5,1700000000,1700000000)",
                vec![
                    uuid::Uuid::new_v4().simple().to_string().into(),
                    org.into(),
                    user.into(),
                    (*provider).into(),
                    handle.into(),
                ],
            )
            .await
            .unwrap();
        }
        let mut config = h::preview_config();
        config.supervision.mode = supervision;
        let mut server_state =
            ServerState::new(database.clone(), config, Arc::new(h::TestClock::new(NOW))).unwrap();
        let runs: Runs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tool_free: ToolFree = Arc::new(std::sync::Mutex::new(Vec::new()));
        let runtime = FakeRuntime::new(vec!["done".to_owned()])
            .recording(runs.clone())
            .recording_tool_free(tool_free.clone());
        let runtime = tweak(runtime);
        server_state.set_agent_runtime(AgentRuntime::Fake(runtime));
        server_state.set_agent_router(router);
        let state = Arc::new(server_state);
        let client = sign_in(&state, &database, user).await;
        Full {
            client,
            state,
            org,
            user,
            runs,
            tool_free,
            handles,
        }
    }

    async fn settled(client: &Client, session: &str) -> Value {
        for _ in 0..200 {
            let page = client
                .get(&format!("/api/neev/agent/sessions/{session}/turns"))
                .await
                .json::<Value>();
            let last = page["items"].as_array().unwrap().last().cloned().unwrap();
            if last["status"] != "running" {
                return last;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("turn never settled");
    }

    #[tokio::test]
    async fn complex_auto_work_is_planned_and_reviewed_on_the_callers_own_claude() {
        use crate::neev::config::SupervisionMode;
        let (router, _) = pick_at("Opus", 2.0);
        let f = full(router, SupervisionMode::PlanReview, &["claude"]).await;
        let session = f.client.create_session().await;
        let id = session["id"].as_str().unwrap();
        let started = auto_turn(&f.client, id, "Redesign the sync engine across services").await;
        assert_eq!(started["route"]["supervision"]["state"], "active");
        assert_eq!(started["route"]["supervision"]["phases"], "plan_review");
        assert_eq!(started["route"]["supervision"]["model"], "opus");
        let turn = settled(&f.client, id).await;
        // The executor's answer is the turn's answer, whatever the review said.
        assert_eq!(turn["status"], "completed");
        let supervised = f.tool_free.lock().unwrap().clone();
        assert_eq!(supervised.len(), 2, "one plan and one review");
        for request in &supervised {
            assert_eq!(request.provider, "claude");
            assert_eq!(request.model, "opus");
            assert_eq!(request.credential_handle.as_ref(), f.handles.get("claude"));
            assert_eq!(request.user_id, f.user);
            assert_eq!(request.org_id, f.org);
        }
        assert_eq!(f.runs.lock().unwrap()[0].model, "opus");
        let trace = serde_json::to_string(&turn["activity"]).unwrap();
        assert!(trace.contains("supervision:plan"), "{trace}");
        assert!(trace.contains("supervision:review"), "{trace}");
    }

    #[tokio::test]
    async fn without_a_claude_connection_supervision_is_reported_unavailable_not_faked() {
        use crate::neev::config::SupervisionMode;
        let (router, _) = pick_at("Opus", 3.0);
        let f = full(router, SupervisionMode::Review, &[]).await;
        let session = f.client.create_session().await;
        let id = session["id"].as_str().unwrap();
        let started = auto_turn(&f.client, id, "Migrate the billing schema safely").await;
        assert_eq!(started["route"]["supervision"]["state"], "unavailable");
        assert_eq!(started["route"]["supervision"]["reason"], "no_claude_connection");
        assert_eq!(settled(&f.client, id).await["status"], "completed");
        assert!(f.tool_free.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn simple_work_is_never_supervised() {
        use crate::neev::config::SupervisionMode;
        let (router, _) = pick_at("Haiku", 0.0);
        let f = full(router, SupervisionMode::PlanReview, &["claude"]).await;
        let session = f.client.create_session().await;
        let id = session["id"].as_str().unwrap();
        let started = auto_turn(&f.client, id, "What does this regex match exactly?").await;
        assert!(started["route"].get("supervision").is_none(), "{started}");
        settled(&f.client, id).await;
        assert!(f.tool_free.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_new_chat_is_routed_once_and_its_first_turn_presents_the_decision_once() {
        use crate::neev::config::SupervisionMode;
        let (router, picker) = pick_at("Sonnet", 1.0);
        let f = full(router, SupervisionMode::Off, &["claude"]).await;
        let decided = f
            .client
            .post("/api/neev/agent/route")
            .json(&serde_json::json!({
                "prompt": "Write the importer", "provider": "claude", "model": "default"
            }))
            .await
            .json::<Value>();
        assert_eq!(decided["outcome"], "routed");
        let route_id = decided["routeId"].as_str().unwrap().to_owned();
        let created = f
            .client
            .post("/api/neev/agent/sessions")
            .json(&serde_json::json!({
                "title": "Importer", "provider": "claude", "model": decided["model"],
                "effort": decided["effort"], "requestId": uuid::Uuid::new_v4().to_string(),
            }))
            .await
            .json::<Value>();
        let id = created["id"].as_str().unwrap();
        let first = f
            .client
            .post(&format!("/api/neev/agent/sessions/{id}/turns"))
            .json(&serde_json::json!({ "prompt": "Write the importer", "routeId": route_id }))
            .await
            .json::<Value>();
        assert_eq!(first["route"]["outcome"], "routed");
        assert_eq!(first["turn"]["model"], "sonnet");
        assert_eq!(picker.calls.load(Ordering::SeqCst), 1, "not routed again");
        settled(&f.client, id).await;
        // A ticket is spent on first use.
        let again = f
            .client
            .post(&format!("/api/neev/agent/sessions/{id}/turns"))
            .json(&serde_json::json!({ "prompt": "And tests", "routeId": route_id }))
            .await
            .json::<Value>();
        assert!(again.get("route").is_none(), "{again}");
    }

    #[tokio::test]
    async fn a_spent_provider_is_withheld_from_auto_and_reported() {
        use crate::neev::agents::routing_usage::{AccountKey, FailureSignal};
        use crate::neev::config::SupervisionMode;
        let (router, picker) = pick_at("Opus", 1.0);
        let f = full(router, SupervisionMode::Off, &["claude", "codex"]).await;
        let key = |user: i32, provider: &str| AccountKey {
            org_id: f.org,
            user_id: user,
            provider: provider.into(),
            generation: 0,
        };
        f.state
            .agent_usage
            .record_failure(&key(f.user, "codex"), FailureSignal::QuotaExhausted, NOW);
        let decided = f
            .client
            .post("/api/neev/agent/route")
            .json(&serde_json::json!({
                "prompt": "Write the importer", "provider": "codex", "model": "default"
            }))
            .await
            .json::<Value>();
        assert_eq!(decided["withheld"][0]["provider"], "codex");
        assert_eq!(decided["withheld"][0]["availability"], "exhausted_unknown_reset");
        assert!(picker
            .seen
            .lock()
            .unwrap()
            .iter()
            .all(|option| !option.starts_with("Codex")));
        assert_eq!(decided["provider"], "claude");
    }

    #[tokio::test]
    async fn a_chat_on_a_spent_provider_keeps_its_selection_and_says_why() {
        use crate::neev::agents::routing_usage::{AccountKey, FailureSignal};
        use crate::neev::config::SupervisionMode;
        let (router, picker) = pick_at("Opus", 1.0);
        let f = full(router, SupervisionMode::Off, &["claude"]).await;
        f.state.agent_usage.record_failure(
            &AccountKey {
                org_id: f.org,
                user_id: f.user,
                provider: "claude".into(),
                generation: 0,
            },
            FailureSignal::RateLimited,
            NOW,
        );
        let session = f.client.create_session().await;
        let id = session["id"].as_str().unwrap();
        let started = auto_turn(&f.client, id, "Refactor the sync engine and its tests").await;
        assert_eq!(started["route"]["reason"], "usage_limited");
        assert_eq!(started["turn"]["model"], "default");
        assert_eq!(picker.calls.load(Ordering::SeqCst), 0);
        // Manual sends are never filtered.
        settled(&f.client, id).await;
        let manual = f.client.start_turn(id, "Do it anyway").await;
        assert_eq!(manual["turn"]["status"], "running");
    }

    #[tokio::test]
    async fn the_status_shows_supervision_and_only_the_callers_own_usage() {
        use crate::neev::agents::routing_usage::{AccountKey, FailureSignal};
        use crate::neev::config::SupervisionMode;
        let f = full(Router::Off, SupervisionMode::Review, &["codex"]).await;
        // Someone else's spent account must not appear.
        f.state.agent_usage.record_failure(
            &AccountKey {
                org_id: f.org,
                user_id: f.user + 1000,
                provider: "codex".into(),
                generation: 0,
            },
            FailureSignal::QuotaExhausted,
            NOW,
        );
        let status = f.client.get("/api/neev/agent/routing").await.json::<Value>();
        assert_eq!(status["supervision"], "review");
        assert_eq!(status["usage"].as_array().unwrap().len(), 1);
        assert_eq!(status["usage"][0]["provider"], "codex");
        assert_eq!(status["usage"][0]["availability"], "available");
        assert_eq!(status["usage"][0]["remaining"], "unknown");
    }

    #[tokio::test]
    async fn decisions_are_throttled_per_person_but_manual_sends_never_are() {
        use crate::neev::agents::routing::DECISIONS_PER_USER_PER_MINUTE;
        use crate::neev::config::SupervisionMode;
        let (router, picker) = pick_at("Sonnet", 1.0);
        let f = full(router, SupervisionMode::Off, &["claude"]).await;
        let ask = || {
            f.client.post("/api/neev/agent/route").json(&serde_json::json!({
                "prompt": "Write the importer", "provider": "claude", "model": "default"
            }))
        };
        for _ in 0..DECISIONS_PER_USER_PER_MINUTE {
            assert_eq!(ask().await.json::<Value>()["outcome"], "routed");
        }
        let throttled = ask().await.json::<Value>();
        assert_eq!(throttled["outcome"], "fallback");
        assert_eq!(throttled["reason"], "throttled");
        assert!(throttled.get("routeId").is_none());
        // An Auto turn is a decision too, and is throttled the same way.
        let session = f.client.create_session().await;
        let id = session["id"].as_str().unwrap();
        let started = auto_turn(&f.client, id, "Refactor the importer and add tests").await;
        assert_eq!(started["route"]["reason"], "throttled");
        assert_eq!(
            picker.calls.load(Ordering::SeqCst),
            DECISIONS_PER_USER_PER_MINUTE,
            "the router was never called past the limit"
        );
        // The send itself still runs on the person's own selection.
        assert_eq!(started["turn"]["status"], "running");
        settled(&f.client, id).await;
        let manual = f.client.start_turn(id, "Then document it").await;
        assert_eq!(manual["turn"]["status"], "running");
    }

    async fn fail_and_retry(f: &Full, run_id: &str) -> Value {
        crate::neev::db::execute(
            &f.state.database,
            "UPDATE neev_agent_turns SET status='failed',error_code='provider',completed_at=updated_at WHERE run_id=$1",
            vec![run_id.into()],
        )
        .await
        .unwrap();
        let response = f
            .client
            .post(&format!("/api/neev/agent/runs/{run_id}/retry"))
            .json(&serde_json::json!({ "requestId": uuid::Uuid::new_v4().to_string() }))
            .await;
        response.assert_status(axum::http::StatusCode::CREATED);
        response.json::<Value>()
    }

    #[tokio::test]
    async fn a_supervised_turn_is_retried_supervised_on_the_callers_own_claude() {
        use crate::neev::config::SupervisionMode;
        let (router, _) = pick_at("Opus", 2.0);
        let f = full(router, SupervisionMode::Review, &["claude"]).await;
        let session = f.client.create_session().await;
        let id = session["id"].as_str().unwrap();
        let started = auto_turn(&f.client, id, "Redesign the sync engine across services").await;
        let run_id = started["runId"].as_str().unwrap().to_owned();
        settled(&f.client, id).await;
        assert_eq!(f.tool_free.lock().unwrap().len(), 1, "one review");
        fail_and_retry(&f, &run_id).await;
        let retried = settled(&f.client, id).await;
        assert_eq!(retried["status"], "completed");
        let supervised = f.tool_free.lock().unwrap().clone();
        assert_eq!(supervised.len(), 2, "the retry was reviewed too");
        assert_eq!(supervised[1].credential_handle.as_ref(), f.handles.get("claude"));
        assert!(serde_json::to_string(&retried["activity"])
            .unwrap()
            .contains("supervision:review"));
    }

    #[tokio::test]
    async fn a_retry_that_cannot_be_supervised_claims_no_supervision() {
        use crate::neev::config::SupervisionMode;
        let (router, _) = pick_at("Opus", 2.0);
        let f = full(router, SupervisionMode::Review, &["claude"]).await;
        let session = f.client.create_session().await;
        let id = session["id"].as_str().unwrap();
        let started = auto_turn(&f.client, id, "Redesign the sync engine across services").await;
        let run_id = started["runId"].as_str().unwrap().to_owned();
        settled(&f.client, id).await;
        crate::neev::db::execute(
            &f.state.database,
            "DELETE FROM neev_agent_connections WHERE provider='claude'",
            vec![],
        )
        .await
        .unwrap();
        fail_and_retry(&f, &run_id).await;
        let retried = settled(&f.client, id).await;
        assert_eq!(f.tool_free.lock().unwrap().len(), 1, "no supervisor ran");
        assert!(!serde_json::to_string(&retried["activity"])
            .unwrap()
            .contains("supervision:"));
    }

    // ---- Auto in a resource-bound chat ("Ask agent" in NativeChat) ----

    async fn team_chat(f: &Full) -> (String, String) {
        let actor = h::scoped_context(&f.state.database, f.user, f.org, NOW).await;
        make_chat(&f.state, &actor).await
    }

    async fn team_ask(
        f: &Full,
        chat: &(String, String),
        prompt: &str,
        provider: &str,
        auto: Option<bool>,
        request_id: &str,
    ) -> axum_test::TestResponse {
        let mut body = serde_json::json!({
            "prompt": prompt,
            "provider": provider,
            "model": "default",
            "requestId": request_id,
            "expectedAuthRevision": chat.1,
        });
        if let Some(auto) = auto {
            body["auto"] = Value::Bool(auto);
        }
        f.client
            .post(&format!("/api/neev/chats/{}/agent", chat.0))
            .json(&body)
            .await
    }

    async fn drain(f: &Full, run_id: &str) {
        f.client
            .get(&format!("/api/neev/agent/runs/{run_id}/events"))
            .await
            .text();
    }

    async fn chat_messages(f: &Full, chat: &(String, String)) -> Vec<Value> {
        f.client
            .get(&format!("/api/neev/chats/{}/messages", chat.0))
            .await
            .json::<Value>()["items"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn holder(f: &Full, chat: &(String, String), provider: &str) -> String {
        crate::neev::agents::sessions::team_session_id(f.org, f.user, &chat.0, provider)
    }

    async fn holder_turns(f: &Full, chat: &(String, String), provider: &str) -> usize {
        let count = crate::neev::db::one(
            &f.state.database,
            "SELECT COUNT(*) AS n FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2",
            vec![f.org.into(), holder(f, chat, provider).into()],
        )
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
        count as usize
    }

    #[tokio::test]
    async fn a_team_ask_without_auto_is_unchanged_and_carries_no_route() {
        use crate::neev::config::SupervisionMode;
        let (router, picker) = pick_at("Opus", 2.0);
        let f = full(router, SupervisionMode::Off, &["claude", "codex"]).await;
        let chat = team_chat(&f).await;
        let id = uuid::Uuid::new_v4().to_string();
        let response = team_ask(&f, &chat, "Summarise the thread", "codex", None, &id).await;
        response.assert_status(axum::http::StatusCode::CREATED);
        let body = response.json::<Value>();
        assert!(body.get("route").is_none(), "{body}");
        assert_eq!(body["turn"]["provider"], "codex");
        assert_eq!(picker.calls.load(Ordering::SeqCst), 0, "no decision without auto");
        // `auto: false` is accepted and means exactly the same.
        let explicit = team_ask(&f, &chat, "Again", "codex", Some(false), &uuid::Uuid::new_v4().to_string()).await;
        explicit.assert_status(axum::http::StatusCode::CREATED);
        assert!(explicit.json::<Value>().get("route").is_none());
    }

    #[tokio::test]
    async fn a_first_auto_ask_routes_across_providers_and_publishes_once() {
        use crate::neev::config::SupervisionMode;
        let (router, picker) = pick_at("Opus", 1.0);
        let f = full(router, SupervisionMode::Off, &["claude", "codex"]).await;
        let chat = team_chat(&f).await;
        let id = uuid::Uuid::new_v4().to_string();
        let response =
            team_ask(&f, &chat, "Redesign the sync engine across services", "codex", Some(true), &id).await;
        response.assert_status(axum::http::StatusCode::CREATED);
        let body = response.json::<Value>();
        assert_eq!(body["route"]["outcome"], "routed", "{body}");
        assert_eq!(body["route"]["provider"], "claude");
        assert_eq!(body["route"]["model"], "opus");
        assert_eq!(body["turn"]["provider"], "claude");
        assert_eq!(body["turn"]["model"], "opus");
        assert_eq!(picker.calls.load(Ordering::SeqCst), 1);
        // The routed provider's holder carries the routed settings; the
        // composer's provider got no holder and no turn.
        let actor = h::scoped_context(&f.state.database, f.user, f.org, NOW).await;
        let claude = crate::neev::agents::sessions::get(&f.state, &actor, &holder(&f, &chat, "claude"))
            .await
            .unwrap();
        assert_eq!(claude.model, "opus");
        assert_eq!(holder_turns(&f, &chat, "codex").await, 0);
        {
            let runs = f.runs.lock().unwrap();
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0].provider, "claude");
            assert_eq!(runs[0].model, "opus");
            assert_eq!(runs[0].credential_handle.as_ref(), f.handles.get("claude"));
        }
        drain(&f, body["runId"].as_str().unwrap()).await;
        let items = chat_messages(&f, &chat).await;
        assert_eq!(items.len(), 2, "the ask and one reply: {items:?}");
        assert_eq!(items[1]["authorKind"], "agent");
        assert_eq!(items[1]["agentLabel"], "claude");
    }

    #[tokio::test]
    async fn a_retried_auto_ask_replays_without_deciding_or_running_again() {
        use crate::neev::config::SupervisionMode;
        let (router, picker) = pick_at("Opus", 1.0);
        let f = full(router, SupervisionMode::Off, &["claude", "codex"]).await;
        let chat = team_chat(&f).await;
        let id = uuid::Uuid::new_v4().to_string();
        let first = team_ask(&f, &chat, "Plan the migration", "codex", Some(true), &id)
            .await
            .json::<Value>();
        let again = team_ask(&f, &chat, "Plan the migration", "codex", Some(true), &id).await;
        again.assert_status(axum::http::StatusCode::CREATED);
        let again = again.json::<Value>();
        assert_eq!(again["runId"], first["runId"]);
        assert_eq!(again["turn"]["id"], first["turn"]["id"]);
        assert!(again.get("route").is_none(), "a replay does not re-decide: {again}");
        assert_eq!(picker.calls.load(Ordering::SeqCst), 1);
        assert_eq!(f.runs.lock().unwrap().len(), 1);
        drain(&f, first["runId"].as_str().unwrap()).await;
        assert_eq!(chat_messages(&f, &chat).await.len(), 2);
    }

    #[tokio::test]
    async fn the_same_request_on_another_holder_is_a_replay_inside_the_turn_transaction() {
        // The in-transaction guard behind the replay: even when a second attempt
        // arrives at a DIFFERENT provider's holder (a concurrent first attempt
        // that routed elsewhere), it gets the existing turn and executes nothing.
        use crate::neev::config::SupervisionMode;
        let f = full(Router::Off, SupervisionMode::Off, &["claude", "codex"]).await;
        let chat = team_chat(&f).await;
        let actor = h::scoped_context(&f.state.database, f.user, f.org, NOW).await;
        let request = uuid::Uuid::new_v4().to_string();
        let mut sessions = Vec::new();
        for provider in ["claude", "codex"] {
            sessions.push(
                crate::neev::agents::sessions::ensure_team_session(
                    &f.state, &actor, &chat.0, provider, "default", "Team invocation", None, false,
                )
                .await
                .unwrap(),
            );
        }
        let (first, created) = crate::neev::agents::sessions::begin_team_turn(
            &f.state, &actor, &chat.0, &sessions[0].id, "ask", &request,
        )
        .await
        .unwrap();
        assert!(created);
        let (second, created) = crate::neev::agents::sessions::begin_team_turn(
            &f.state, &actor, &chat.0, &sessions[1].id, "ask", &request,
        )
        .await
        .unwrap();
        assert!(!created, "the second attempt must not execute");
        assert_eq!(second.id, first.id);
        assert_eq!(second.session_id, sessions[0].id);
        assert_eq!(holder_turns(&f, &chat, "codex").await, 0);
        // The lookup a handler does before routing finds it too.
        let found = crate::neev::agents::sessions::team_turn_by_request(&f.state, &actor, &chat.0, &request)
            .await
            .unwrap()
            .expect("found across holders");
        assert_eq!(found.id, first.id);
    }

    #[tokio::test]
    async fn a_follow_up_auto_ask_is_decided_within_the_last_holders_provider() {
        use crate::neev::config::SupervisionMode;
        let (router, picker) = pick_at("Opus", 1.0);
        let f = full(router, SupervisionMode::Off, &["claude", "codex"]).await;
        let chat = team_chat(&f).await;
        let first = team_ask(&f, &chat, "Redesign the importer", "codex", Some(true), &uuid::Uuid::new_v4().to_string())
            .await
            .json::<Value>();
        drain(&f, first["runId"].as_str().unwrap()).await;
        let second = team_ask(&f, &chat, "Now redesign the exporter in the same way, end to end", "codex", Some(true), &uuid::Uuid::new_v4().to_string())
            .await
            .json::<Value>();
        assert_eq!(second["turn"]["provider"], "claude", "{second}");
        assert_eq!(picker.calls.load(Ordering::SeqCst), 2);
        let seen = picker.seen.lock().unwrap().clone();
        // Every option offered is the holder's provider (labelled by its agent
        // name): the decision never left Claude for the composer's Codex.
        assert!(!seen.is_empty() && seen.iter().all(|option| option.starts_with("Claude Code/")), "{seen:?}");
        assert_eq!(holder_turns(&f, &chat, "codex").await, 0);
    }

    #[tokio::test]
    async fn an_undecidable_auto_ask_runs_the_composer_selection_honestly() {
        use crate::neev::config::SupervisionMode;
        let f = full(Router::Off, SupervisionMode::Off, &["claude", "codex"]).await;
        let chat = team_chat(&f).await;
        let body = team_ask(&f, &chat, "Summarise", "codex", Some(true), &uuid::Uuid::new_v4().to_string())
            .await
            .json::<Value>();
        assert_eq!(body["route"]["outcome"], "fallback", "{body}");
        assert_eq!(body["route"]["provider"], "codex");
        assert_eq!(body["turn"]["provider"], "codex");
        assert_eq!(f.runs.lock().unwrap()[0].provider, "codex");
    }

    #[tokio::test]
    async fn auto_never_routes_for_a_caller_who_cannot_write_the_chat() {
        use crate::neev::config::SupervisionMode;
        let (router, picker) = pick_at("Opus", 2.0);
        let f = full(router, SupervisionMode::PlanReview, &["claude", "codex"]).await;
        let peer = h::native_account(&f.state.database, "peer", "peer@example.test").await;
        h::add_membership(&f.state.database, f.org, peer, "member").await;
        let peer_actor = h::scoped_context(&f.state.database, peer, f.org, NOW).await;
        let peer_chat = make_chat(&f.state, &peer_actor).await;
        team_ask(&f, &peer_chat, "post here", "codex", Some(true), &uuid::Uuid::new_v4().to_string())
            .await
            .assert_status_not_found();
        assert_eq!(picker.calls.load(Ordering::SeqCst), 0);
        assert!(f.runs.lock().unwrap().is_empty());
        assert!(f.tool_free.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_supervised_team_ask_publishes_only_the_executors_final_reply() {
        use crate::neev::config::SupervisionMode;
        let (router, _) = pick_at("Opus", 2.0);
        let f = full(router, SupervisionMode::PlanReview, &["claude"]).await;
        let chat = team_chat(&f).await;
        let body = team_ask(&f, &chat, "Redesign the sync engine across services", "claude", Some(true), &uuid::Uuid::new_v4().to_string())
            .await
            .json::<Value>();
        assert_eq!(body["route"]["supervision"]["state"], "active", "{body}");
        drain(&f, body["runId"].as_str().unwrap()).await;
        let supervised = f.tool_free.lock().unwrap().clone();
        assert_eq!(supervised.len(), 2, "one plan and one review");
        for request in &supervised {
            assert_eq!(request.credential_handle.as_ref(), f.handles.get("claude"));
        }
        let items = chat_messages(&f, &chat).await;
        assert_eq!(items.len(), 2, "the ask and exactly one agent reply: {items:?}");
        assert_eq!(items[1]["authorKind"], "agent");
        assert_eq!(items[1]["body"], "done");
        // The supervisor's own steps stay in the invoker's private trace.
        let actor = h::scoped_context(&f.state.database, f.user, f.org, NOW).await;
        let turns = crate::neev::agents::sessions::turns(&f.state, &actor, &holder(&f, &chat, "claude"), None)
            .await
            .unwrap();
        let trace = serde_json::to_string(&turns.items[0].activity).unwrap();
        assert!(trace.contains("supervision:plan") && trace.contains("supervision:review"), "{trace}");
        assert_eq!(turns.items[0].status, crate::neev::agents::sessions::TurnStatus::Completed);
    }

    #[tokio::test]
    async fn stopping_a_supervised_team_ask_while_planning_publishes_nothing() {
        use crate::neev::config::SupervisionMode;
        let (router, _) = pick_at("Opus", 2.0);
        let f = full_with(router, SupervisionMode::PlanReview, &["claude"], |r| r.hanging()).await;
        let chat = team_chat(&f).await;
        let body = team_ask(&f, &chat, "Redesign the sync engine across services", "claude", Some(true), &uuid::Uuid::new_v4().to_string())
            .await
            .json::<Value>();
        let run_id = body["runId"].as_str().unwrap().to_owned();
        f.client
            .post("/api/neev/agent/runs/cancel")
            .json(&serde_json::json!({ "runId": run_id }))
            .await
            .assert_status_ok();
        drain(&f, &run_id).await;
        let actor = h::scoped_context(&f.state.database, f.user, f.org, NOW).await;
        let turns = crate::neev::agents::sessions::turns(&f.state, &actor, &holder(&f, &chat, "claude"), None)
            .await
            .unwrap();
        assert_eq!(turns.items[0].status, crate::neev::agents::sessions::TurnStatus::Canceled);
        // The fake records the supervisor's tool-free run in the same log; the
        // EXECUTOR (the ask itself) must never have been launched.
        assert!(
            f.runs
                .lock()
                .unwrap()
                .iter()
                .all(|run| run.prompt != "Redesign the sync engine across services"),
            "the executor never launched"
        );
        assert_eq!(f.tool_free.lock().unwrap().len(), 1, "only the plan was attempted");
        let items = chat_messages(&f, &chat).await;
        assert_eq!(items.len(), 1, "only the ask: {items:?}");
    }

    #[tokio::test]
    async fn an_undecided_first_ask_never_falls_back_to_a_disconnected_composer() {
        // B1: Auto is on, the composer names a provider that is NOT connected,
        // and the router cannot decide. The ask must run on a connected
        // provider (the cross-provider fallback), not fail after it is visible.
        use crate::neev::config::SupervisionMode;
        let f = full(Router::Off, SupervisionMode::Off, &["claude"]).await;
        let chat = team_chat(&f).await;
        let response = team_ask(&f, &chat, "Summarise the thread", "codex", Some(true), &uuid::Uuid::new_v4().to_string()).await;
        response.assert_status(axum::http::StatusCode::CREATED);
        let body = response.json::<Value>();
        assert_eq!(body["route"]["outcome"], "fallback", "{body}");
        assert_eq!(body["route"]["provider"], "claude");
        assert_eq!(body["turn"]["provider"], "claude");
        assert_eq!(f.runs.lock().unwrap()[0].provider, "claude");
        drain(&f, body["runId"].as_str().unwrap()).await;
        let items = chat_messages(&f, &chat).await;
        assert_eq!(items.len(), 2, "the ask is answered: {items:?}");
        assert_eq!(items[1]["agentLabel"], "claude");
    }

    #[tokio::test]
    async fn an_undecided_follow_up_keeps_the_holder_when_the_composer_is_disconnected() {
        use crate::neev::config::SupervisionMode;
        let f = full(Router::Off, SupervisionMode::Off, &["claude"]).await;
        let chat = team_chat(&f).await;
        let first = team_ask(&f, &chat, "Start", "claude", Some(true), &uuid::Uuid::new_v4().to_string())
            .await
            .json::<Value>();
        drain(&f, first["runId"].as_str().unwrap()).await;
        let second = team_ask(&f, &chat, "Continue with the next part", "codex", Some(true), &uuid::Uuid::new_v4().to_string())
            .await
            .json::<Value>();
        assert_eq!(second["route"]["outcome"], "fallback", "{second}");
        assert_eq!(second["turn"]["provider"], "claude");
        assert_eq!(holder_turns(&f, &chat, "codex").await, 0);
    }

    #[tokio::test]
    async fn auto_with_nothing_connected_is_refused_before_the_ask_is_visible() {
        use crate::neev::config::SupervisionMode;
        let (router, picker) = pick_at("Opus", 1.0);
        let f = full(router, SupervisionMode::Off, &[]).await;
        let chat = team_chat(&f).await;
        team_ask(&f, &chat, "Anyone?", "claude", Some(true), &uuid::Uuid::new_v4().to_string())
            .await
            .assert_status(axum::http::StatusCode::CONFLICT);
        assert!(chat_messages(&f, &chat).await.is_empty(), "no orphaned ask");
        assert_eq!(picker.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn stopping_a_supervised_team_ask_while_reviewing_publishes_nothing() {
        use crate::neev::config::SupervisionMode;
        let (router, _) = pick_at("Opus", 2.0);
        let f = full_with(router, SupervisionMode::Review, &["claude"], |r| r.hanging_tool_free()).await;
        let chat = team_chat(&f).await;
        let body = team_ask(&f, &chat, "Redesign the sync engine across services", "claude", Some(true), &uuid::Uuid::new_v4().to_string())
            .await
            .json::<Value>();
        let run_id = body["runId"].as_str().unwrap().to_owned();
        let actor = h::scoped_context(&f.state.database, f.user, f.org, NOW).await;
        // Wait until the executor has finished and the review is under way.
        let mut reviewing = false;
        for _ in 0..200 {
            if !f.tool_free.lock().unwrap().is_empty() {
                reviewing = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(reviewing, "the review started");
        f.client
            .post("/api/neev/agent/runs/cancel")
            .json(&serde_json::json!({ "runId": run_id }))
            .await
            .assert_status_ok();
        drain(&f, &run_id).await;
        let turns = crate::neev::agents::sessions::turns(&f.state, &actor, &holder(&f, &chat, "claude"), None)
            .await
            .unwrap();
        assert_eq!(turns.items[0].status, crate::neev::agents::sessions::TurnStatus::Canceled);
        assert!(
            f.runs.lock().unwrap().iter().any(|run| run.prompt == "Redesign the sync engine across services"),
            "the executor did run"
        );
        let items = chat_messages(&f, &chat).await;
        assert_eq!(items.len(), 1, "a stopped review publishes nothing: {items:?}");
    }

    #[tokio::test]
    async fn a_supervised_team_retry_is_supervised_again_and_publishes_once() {
        use crate::neev::config::SupervisionMode;
        let (router, _) = pick_at("Opus", 2.0);
        let f = full_with(router, SupervisionMode::PlanReview, &["claude"], |r| {
            r.failing_first_executor_runs(1, crate::neev::agents::execution::ExecutionError::Unavailable)
        })
        .await;
        let chat = team_chat(&f).await;
        let body = team_ask(&f, &chat, "Redesign the sync engine across services", "claude", Some(true), &uuid::Uuid::new_v4().to_string())
            .await
            .json::<Value>();
        let run_id = body["runId"].as_str().unwrap().to_owned();
        drain(&f, &run_id).await;
        let actor = h::scoped_context(&f.state.database, f.user, f.org, NOW).await;
        let failed = crate::neev::agents::sessions::turn_by_run(&f.state, &actor, &run_id).await.unwrap();
        assert_eq!(failed.status, crate::neev::agents::sessions::TurnStatus::Failed);
        assert_eq!(chat_messages(&f, &chat).await.len(), 1, "a failed ask publishes nothing");

        let retried = f
            .client
            .post(&format!("/api/neev/agent/runs/{run_id}/retry"))
            .json(&serde_json::json!({ "requestId": uuid::Uuid::new_v4().to_string() }))
            .await;
        retried.assert_status(axum::http::StatusCode::CREATED);
        let retried = retried.json::<Value>();
        let retry_run = retried["runId"].as_str().unwrap().to_owned();
        drain(&f, &retry_run).await;
        let turn = crate::neev::agents::sessions::turn_by_run(&f.state, &actor, &retry_run).await.unwrap();
        assert_eq!(turn.status, crate::neev::agents::sessions::TurnStatus::Completed);
        let trace = serde_json::to_string(&turn.activity).unwrap();
        assert!(trace.contains("supervision:plan") && trace.contains("supervision:review"), "{trace}");
        let items = chat_messages(&f, &chat).await;
        assert_eq!(items.len(), 2, "the ask and exactly one reply: {items:?}");
        assert_eq!(items[1]["authorKind"], "agent");
        assert_eq!(items[1]["body"], "done");
    }

    #[tokio::test]
    #[ignore = "requires isolated PostgreSQL"]
    async fn postgres_two_first_attempts_on_different_holders_make_one_turn() {
        use crate::neev::config::SupervisionMode;
        let (database, admin, schema) = h::postgres_database().await;
        eprintln!("team auto race owned schema: {schema}");
        let f = full_on(database.clone(), Router::Off, SupervisionMode::Off, &["claude", "codex"], |r| r).await;
        let chat = team_chat(&f).await;
        let actor = h::scoped_context(&f.state.database, f.user, f.org, NOW).await;
        let mut holders = Vec::new();
        for provider in ["claude", "codex"] {
            holders.push(
                crate::neev::agents::sessions::ensure_team_session(
                    &f.state, &actor, &chat.0, provider, "default", "Team invocation", None, false,
                )
                .await
                .unwrap()
                .id,
            );
        }
        let request = uuid::Uuid::new_v4().to_string();
        let blocker = crate::neev::db::begin(&database).await.unwrap();
        crate::neev::db::lock_admission_guard(&blocker).await.unwrap();
        let mut tasks = Vec::new();
        for holder_id in holders.clone() {
            let state = f.state.clone();
            let actor = actor.clone();
            let chat_id = chat.0.clone();
            let request = request.clone();
            tasks.push(tokio::spawn(async move {
                crate::neev::agents::sessions::begin_team_turn(
                    &state, &actor, &chat_id, &holder_id, "ask", &request,
                )
                .await
            }));
        }
        h::wait_for_postgres_locks(&admin, "%neev_schema_version%FOR UPDATE%", 2).await;
        blocker.commit().await.unwrap();
        let mut created = 0;
        let mut ids = std::collections::BTreeSet::new();
        for task in tasks {
            let (turn, made) = tokio::time::timeout(std::time::Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            created += usize::from(made);
            ids.insert(turn.id);
        }
        assert_eq!(created, 1, "exactly one attempt creates the turn");
        assert_eq!(ids.len(), 1, "both attempts resolve to the same turn");
        let total = holder_turns(&f, &chat, "claude").await + holder_turns(&f, &chat, "codex").await;
        assert_eq!(total, 1);
        drop(f);
        h::cleanup_postgres(database, admin, schema).await;
    }
}
