//! Task 9: the knowledge HTTP surface (admin-only, 503 when unconfigured,
//! cross-org 404, contract DTOs and error codes).

use axum::http::{header, StatusCode};
use serde_json::{json, Value};

use super::cookies::CookieNames;
use crate::neev::{
    auth::session,
    knowledge::test_support::{database, K},
    types::OrgContext,
};

struct Client {
    server: axum_test::TestServer,
    cookies: String,
    csrf: String,
}

async fn client(k: &K, actor: &OrgContext) -> Client {
    let db = &k.state.database;
    let issued = session::issue_identity(db, actor.identity.user_id, k.now()).await.unwrap();
    let identity = session::identity(db, &issued.session, k.now()).await.unwrap();
    let scoped = session::select_organization(db, &identity, actor.org_id, k.now()).await.unwrap();
    let names = CookieNames::for_preview(true);
    Client {
        server: axum_test::TestServer::new(super::router(k.state.clone())).unwrap(),
        cookies: format!("{}={}; {}={}", names.session, scoped.session, names.csrf, scoped.csrf),
        csrf: scoped.csrf,
    }
}

impl Client {
    async fn get(&self, path: &str) -> axum_test::TestResponse {
        self.server.get(path).add_header(header::COOKIE, &self.cookies).await
    }
    async fn send(&self, method: &str, path: &str, body: Value) -> axum_test::TestResponse {
        let request = match method {
            "POST" => self.server.post(path),
            "PUT" => self.server.put(path),
            _ => panic!("method"),
        };
        request
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, &self.cookies)
            .add_header("x-neev-csrf", &self.csrf)
            .json(&body)
            .await
    }
}

const GETS: [&str; 9] = [
    "/api/neev/knowledge/settings",
    "/api/neev/knowledge/status",
    "/api/neev/knowledge/sources",
    "/api/neev/knowledge/jobs/current",
    "/api/neev/knowledge/pages",
    "/api/neev/knowledge/pages/00000000-0000-4000-8000-000000000000",
    "/api/neev/knowledge/index",
    "/api/neev/knowledge/log",
    "/api/neev/knowledge/export",
];

#[tokio::test]
async fn every_knowledge_route_is_admin_only() {
    let k = K::new(database().await).await;
    let member = client(&k, &k.member).await;
    for path in GETS {
        member.get(path).await.assert_status(StatusCode::FORBIDDEN);
    }
    for (method, path, body) in [
        ("PUT", "/api/neev/knowledge/settings", json!({"captureEnabled": true, "expectedRevision": 0})),
        ("POST", "/api/neev/knowledge/jobs", json!({"mode": "incremental", "provider": "claude", "model": "default"})),
        ("POST", "/api/neev/knowledge/jobs/abc/pause", json!({})),
        ("POST", "/api/neev/knowledge/uncovered/reset", json!({})),
    ] {
        member.send(method, path, body).await.assert_status(StatusCode::FORBIDDEN);
    }
    // Capture stayed off: the member changed nothing.
    let admin = client(&k, &k.admin).await;
    assert_eq!(admin.get("/api/neev/knowledge/settings").await.json::<Value>(), json!({"captureEnabled": false, "revision": 0}));
}

#[tokio::test]
async fn an_unconfigured_knowledge_root_is_503_for_every_route() {
    let k = K::unconfigured(database().await).await;
    let admin = client(&k, &k.admin).await;
    for path in GETS {
        let response = admin.get(path).await;
        response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
        response.assert_json(&json!({"code": "knowledge_unavailable"}));
    }
}

#[tokio::test]
async fn settings_jobs_pages_and_errors_follow_the_contract() {
    let k = K::new(database().await).await;
    let admin = client(&k, &k.admin).await;
    let on = admin.send("PUT", "/api/neev/knowledge/settings", json!({"captureEnabled": true, "expectedRevision": 0})).await;
    on.assert_status_ok();
    on.assert_json(&json!({"captureEnabled": true, "revision": 1}));
    admin
        .send("PUT", "/api/neev/knowledge/settings", json!({"captureEnabled": false, "expectedRevision": 0}))
        .await
        .assert_status(StatusCode::CONFLICT);
    let missing = admin.send("POST", "/api/neev/knowledge/jobs", json!({"mode": "incremental", "provider": "claude", "model": "default"})).await;
    missing.assert_status(StatusCode::CONFLICT);
    missing.assert_json(&json!({"code": "credential_missing"}));
    k.connect(&k.admin, "claude").await;
    let started = admin.send("POST", "/api/neev/knowledge/jobs", json!({"mode": "incremental", "provider": "claude", "model": "default"})).await;
    started.assert_status_ok();
    let job: Value = started.json();
    let mut job_keys: Vec<&str> = job.as_object().unwrap().keys().map(String::as_str).collect();
    job_keys.sort();
    assert_eq!(job_keys, ["batchesDone", "createdAt", "id", "mode", "model", "pauseReason", "provider", "sessionId", "startedBy", "state", "unitsRemaining", "updatedAt", "waitReason"]);
    let again = admin.send("POST", "/api/neev/knowledge/jobs", json!({"mode": "rebuild", "provider": "claude", "model": "default"})).await;
    again.assert_status(StatusCode::CONFLICT);
    again.assert_json(&json!({"code": "job_active"}));
    let id = job["id"].as_str().unwrap();
    let paused: Value = admin.send("POST", &format!("/api/neev/knowledge/jobs/{id}/pause"), json!({})).await.json();
    assert_eq!((paused["state"].as_str(), paused["pauseReason"].as_str()), (Some("paused"), Some("manual")));
    let resumed: Value = admin.send("POST", &format!("/api/neev/knowledge/jobs/{id}/resume"), json!({})).await.json();
    assert_eq!(resumed["state"], "running");
    let current: Value = admin.get("/api/neev/knowledge/jobs/current").await.json();
    assert_eq!(current["id"], id);
    let stopped: Value = admin.send("POST", &format!("/api/neev/knowledge/jobs/{id}/stop"), json!({})).await.json();
    assert_eq!(stopped["state"], "stopped");
    admin.send("POST", "/api/neev/knowledge/uncovered/reset", json!({})).await.assert_status_ok();
    // A readable page by its opaque id; another org's admin gets 404.
    let chat = k.shared_chat("Team").await;
    k.post(&chat.id, "fact").await;
    k.capture().await;
    let key = k.current_snapshots(&chat.id).await[0].cite_key.clone();
    let page = k.seed_page("team", "Team", &format!("# Team\n\nFact.[^k-{key}]\n"), &[&chat.id]).await;
    let view: Value = admin.get(&format!("/api/neev/knowledge/pages/{page}")).await.json();
    assert_eq!((view["status"].as_str(), view["slug"].as_str()), (Some("readable"), Some("team")));
    assert_eq!(view["citations"][0]["key"], format!("k-{key}"));
    let foreign = client(&k, &k.foreign).await;
    foreign.get(&format!("/api/neev/knowledge/pages/{page}")).await.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(foreign.get("/api/neev/knowledge/pages").await.json::<Value>(), json!([]));
    admin.get("/api/neev/knowledge/pages/..%2Fsecret").await.assert_status(StatusCode::NOT_FOUND);
    let index = admin.get("/api/neev/knowledge/index").await;
    index.assert_status_ok();
    assert!(index.text().contains("- [[team]] Team — Fact."));
    let export = admin.get("/api/neev/knowledge/export").await;
    export.assert_status_ok();
    assert_eq!(export.header(header::CONTENT_TYPE), "application/zip");
    assert!(export.header(header::CONTENT_DISPOSITION).to_str().unwrap().starts_with(&format!("attachment; filename=\"knowledge-{}-", k.admin.org_id)));
    assert_eq!(export.header(header::CACHE_CONTROL), "private, no-store");
    let log: Value = admin.get("/api/neev/knowledge/log").await.json();
    let seqs: Vec<i64> = log.as_array().unwrap().iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] > w[1]), "newest first: {seqs:?}");
    let earlier: Value = admin.get(&format!("/api/neev/knowledge/log?before={}", seqs[0])).await.json();
    assert!(earlier.as_array().unwrap().iter().all(|e| e["seq"].as_i64().unwrap() < seqs[0]));
}

/// A fake agent that cites the first key of the first staged page.
fn citing_runtime() -> crate::neev::agents::execution::FakeRuntime {
    crate::neev::agents::execution::FakeRuntime::new(Vec::new()).with_outputs(|request| {
        let text = crate::neev::knowledge::test_support::staged_text(request, "q-p01").unwrap_or_default();
        let start = text.find("[^k-").unwrap();
        vec![("knowledge-answer.md".to_owned(), format!("Fact.{}", &text[start..start + 17]).into_bytes())]
    })
}

#[tokio::test]
async fn ask_routes_are_admin_only_and_503_when_unconfigured() {
    let k = K::new(database().await).await;
    let member = client(&k, &k.member).await;
    member
        .send("POST", "/api/neev/knowledge/ask", json!({"provider": "claude", "model": "default", "question": "q"}))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    member.get("/api/neev/knowledge/ask/abc").await.assert_status(StatusCode::FORBIDDEN);
    member.send("POST", "/api/neev/knowledge/ask/abc/stop", json!({})).await.assert_status(StatusCode::FORBIDDEN);
    let off = K::unconfigured(database().await).await;
    let admin = client(&off, &off.admin).await;
    let response = admin.get("/api/neev/knowledge/ask/abc").await;
    response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    response.assert_json(&json!({"code": "knowledge_unavailable"}));
}

#[tokio::test]
async fn ask_start_poll_stop_follow_the_contract() {
    let k = K::with_runtime(database().await, citing_runtime()).await;
    k.enable_capture().await;
    let chat = k.shared_chat("Team").await;
    k.post(&chat.id, "fact").await;
    k.capture().await;
    let key = k.current_snapshots(&chat.id).await[0].cite_key.clone();
    k.seed_page("team", "Team", &format!("# Team\n\nFact.[^k-{key}]\n"), &[&chat.id]).await;
    let admin = client(&k, &k.admin).await;
    let body = json!({"provider": "claude", "model": "default", "question": "What is the fact?"});
    let missing = admin.send("POST", "/api/neev/knowledge/ask", body.clone()).await;
    missing.assert_status(StatusCode::CONFLICT);
    missing.assert_json(&json!({"code": "credential_missing"}));
    admin
        .send("POST", "/api/neev/knowledge/ask", json!({"provider": "claude", "model": "default", "question": "q", "extra": 1}))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    k.connect(&k.admin, "claude").await;
    let started = admin.send("POST", "/api/neev/knowledge/ask", body).await;
    started.assert_status_ok();
    assert_eq!(started.header(header::CACHE_CONTROL), "no-store");
    let id = started.json::<Value>()["id"].as_str().unwrap().to_owned();
    let mut done = Value::Null;
    for _ in 0..500 {
        done = admin.get(&format!("/api/neev/knowledge/ask/{id}")).await.json();
        if done["state"] != "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(done["state"], "answered", "{done}");
    let mut keys: Vec<&str> = done.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["answer", "citations", "createdAt", "error", "finishedAt", "id", "insufficient", "model", "pages", "pagesConsidered", "provider", "question", "state"]);
    assert_eq!(done["citations"][0]["sourceId"], chat.id);
    let foreign = client(&k, &k.foreign).await;
    foreign.get(&format!("/api/neev/knowledge/ask/{id}")).await.assert_status(StatusCode::NOT_FOUND);
    // Another browser session of the same admin cannot read it either.
    let other = client(&k, &k.admin).await;
    other.get(&format!("/api/neev/knowledge/ask/{id}")).await.assert_status(StatusCode::NOT_FOUND);
    let stopped: Value = admin.send("POST", &format!("/api/neev/knowledge/ask/{id}/stop"), json!({})).await.json();
    assert_eq!(stopped["state"], "answered");
    // Stop re-checks like GET: once the source is revoked it returns no answer.
    k.set_visibility(&chat.id, crate::neev::content::types::Visibility::Private).await;
    let revoked: Value = admin.send("POST", &format!("/api/neev/knowledge/ask/{id}/stop"), json!({})).await.json();
    assert_eq!((revoked["state"].as_str(), revoked["error"]["code"].as_str()), (Some("failed"), Some("source_revoked")));
    assert!(revoked["answer"].is_null());
    admin.get("/api/neev/knowledge/ask/..%2Fx").await.assert_status(StatusCode::NOT_FOUND);
}
