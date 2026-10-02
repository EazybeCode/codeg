//! Test knowledge chat: auth, isolation, citations, revocation, provider
//! errors, stop, caps, timeout and retention.

use super::ask::{self, AskInput, HistoryTurn};
use super::test_support::{database, staged_text, K};
use super::types::KnowledgeError;
use crate::neev::{
    agents::{
        execution::{ExecutionError, FakeRuntime, RunRequest, SlotPool},
        provider_error::Category,
    },
    content::types::Visibility,
    types::OrgContext,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};

fn input(question: &str) -> AskInput {
    AskInput {
        provider: "claude".into(),
        model: "default".into(),
        question: question.into(),
        history: Vec::new(),
    }
}

type Log = Arc<Mutex<Vec<RunRequest>>>;

/// The first `[^k-…]` marker written in a staged page file.
fn first_key(request: &RunRequest) -> String {
    let text = staged_text(request, "q-p01").unwrap_or_default();
    let start = text.find("[^k-").expect("a staged cite key");
    text[start + 2..start + 16].to_owned()
}

/// A fake agent that answers from the staged page: one real citation, one
/// invented key, one unit label, a real link and an unknown link.
fn answering(log: Log) -> FakeRuntime {
    FakeRuntime::new(Vec::new()).recording(log).with_outputs(|request| {
        let key = first_key(request);
        vec![(
            ask::ANSWER_FILE.to_owned(),
            format!("The team ships on Fridays.[^{key}] Also true.[^k-000000000000][^u01] See [[team]] and [[elsewhere]].").into_bytes(),
        )]
    })
}

/// A wiki with one readable page citing a shared chat, and one page withheld
/// because its only source went private.
async fn wiki(k: &K) -> (String, String) {
    k.enable_capture().await;
    let chat = k.shared_chat("Team").await;
    k.post(&chat.id, "we ship on fridays").await;
    let secret = k.shared_chat("Secret").await;
    k.post(&secret.id, "the secret plan").await;
    k.capture().await;
    let key = k.current_snapshots(&chat.id).await[0].cite_key.clone();
    let secret_key = k.current_snapshots(&secret.id).await[0].cite_key.clone();
    k.seed_page("team", "Team", &format!("# Team\n\nShips on Fridays.[^k-{key}] See [[secret-plan]].\n"), &[&chat.id]).await;
    k.seed_page("secret-plan", "Secret plan", &format!("# Secret plan\n\nHidden words.[^k-{secret_key}]\n"), &[&secret.id]).await;
    k.set_visibility(&secret.id, Visibility::Private).await;
    (chat.id, key)
}

async fn wait(k: &K, actor: &OrgContext, id: &str) -> Value {
    for _ in 0..500 {
        let value = ask::get(&k.state, &k.state.database, actor, id).await.unwrap().expect("ask visible");
        if value["state"] != "running" {
            return value;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("ask never finished");
}

async fn count(k: &K, table: &str) -> i64 {
    k.scalar_i64(&format!("SELECT COUNT(*) AS n FROM {table}"), vec![]).await
}

#[tokio::test]
async fn an_answer_cites_only_server_resolved_sources_from_readable_pages() {
    let log: Log = Arc::default();
    let k = K::with_runtime(database().await, answering(log.clone())).await;
    let (chat_id, key) = wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    let sessions = count(&k, "neev_agent_sessions").await;
    let turns = count(&k, "neev_agent_turns").await;
    let started = ask::start(&k.state, &k.admin, input("When does the team ship?")).await.unwrap();
    let id = started["id"].as_str().unwrap().to_owned();
    let done = wait(&k, &k.admin, &id).await;
    assert_eq!(done["state"], "answered", "{done}");
    let answer = done["answer"].as_str().unwrap();
    assert!(answer.contains(&format!("[^k-{key}]")));
    assert!(!answer.contains("k-000000000000") && !answer.contains("[^u01]"), "{answer}");
    assert!(answer.contains("[[team]]") && answer.contains(" elsewhere") && !answer.contains("[[elsewhere]]"), "{answer}");
    let citations = done["citations"].as_array().unwrap();
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0]["key"], format!("k-{key}"));
    assert_eq!(citations[0]["sourceId"], chat_id);
    assert_eq!(citations[0]["title"], "Team");
    assert_eq!(done["pages"][0]["slug"], "team");
    assert_eq!(done["pagesConsidered"], 1);

    // Isolated run: own credential, fixed briefing, staged pages only.
    let request = log.lock().unwrap()[0].clone();
    assert!(request.library_items.is_empty() && request.attachments.is_empty());
    assert_eq!(request.briefing.as_deref(), Some(ask::instructions()));
    assert!(request.credential_handle.is_some());
    assert_eq!(request.min_free_lanes, 2, "one lane stays free for ordinary chat");
    let staged: Vec<&str> = request.context_files.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(staged, vec!["q-index", "q-p01"]);
    for file in &request.context_files {
        let text = String::from_utf8_lossy(&file.bytes);
        assert!(!text.contains("secret-plan") && !text.contains("Hidden words") && !text.contains("Secret plan"), "{text}");
    }
    // Nothing persisted: no session, no turn, no live chat run.
    assert_eq!(count(&k, "neev_agent_sessions").await, sessions);
    assert_eq!(count(&k, "neev_agent_turns").await, turns);
    assert!(k.state.agent_runs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn asks_are_admin_only_and_scoped_to_org_user_and_session() {
    let k = K::with_runtime(database().await, answering(Arc::default())).await;
    wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    k.connect(&k.member, "claude").await;
    assert!(matches!(
        ask::start(&k.state, &k.member, input("q")).await,
        Err(KnowledgeError::Auth(crate::neev::error::AuthError::Forbidden))
    ));
    let id = ask::start(&k.state, &k.admin, input("When?")).await.unwrap()["id"].as_str().unwrap().to_owned();
    wait(&k, &k.admin, &id).await;
    assert!(ask::get(&k.state, &k.state.database, &k.foreign, &id).await.unwrap().is_none());
    let mut other_session = k.admin.clone();
    other_session.identity.session_id = "another-session".into();
    assert!(ask::get(&k.state, &k.state.database, &other_session, &id).await.unwrap().is_none());
    assert!(matches!(ask::stop(&k.state, &k.state.database, &other_session, &id).await, Err(KnowledgeError::Content(_))));
    assert!(matches!(ask::stop(&k.state, &k.state.database, &k.member, &id).await, Err(KnowledgeError::Auth(_))));
}

#[tokio::test]
async fn input_and_credential_are_validated_before_any_run() {
    let log: Log = Arc::default();
    let k = K::with_runtime(database().await, answering(log.clone())).await;
    wiki(&k).await;
    assert!(matches!(ask::start(&k.state, &k.admin, input("q")).await, Err(KnowledgeError::CredentialMissing)));
    k.connect(&k.admin, "claude").await;
    let mut bad = input("q");
    bad.provider = "gemini".into();
    assert!(matches!(ask::start(&k.state, &k.admin, bad).await, Err(KnowledgeError::Content(_))));
    assert!(matches!(ask::start(&k.state, &k.admin, input(&"x".repeat(2001))).await, Err(KnowledgeError::Content(_))));
    assert!(matches!(ask::start(&k.state, &k.admin, input("   ")).await, Err(KnowledgeError::Content(_))));
    let mut long = input("q");
    long.history = (0..7).map(|_| HistoryTurn { question: "a".into(), answer: "b".into() }).collect();
    assert!(matches!(ask::start(&k.state, &k.admin, long).await, Err(KnowledgeError::Content(_))));
    let mut heavy = input("q");
    heavy.history = (0..5).map(|_| HistoryTurn { question: "a".into(), answer: "b".repeat(5_000) }).collect();
    assert!(matches!(ask::start(&k.state, &k.admin, heavy).await, Err(KnowledgeError::Content(_))));
    assert!(log.lock().unwrap().is_empty());
    assert_eq!(k.state.knowledge_asks.len(), 0);
}

#[tokio::test]
async fn follow_up_history_is_staged_as_fenced_context_only() {
    let log: Log = Arc::default();
    let k = K::with_runtime(database().await, answering(log.clone())).await;
    wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    let mut follow = input("And on holidays?");
    follow.history = vec![HistoryTurn { question: "When does the team ship?".into(), answer: "On Fridays.".into() }];
    let id = ask::start(&k.state, &k.admin, follow).await.unwrap()["id"].as_str().unwrap().to_owned();
    wait(&k, &k.admin, &id).await;
    let request = log.lock().unwrap()[0].clone();
    let history = staged_text(&request, "q-history").expect("history file");
    assert!(history.contains("<<<DATA ") && history.contains("When does the team ship?") && history.contains("On Fridays."));
    assert!(request.prompt.contains("And on holidays?"));
}

#[tokio::test]
async fn the_agent_saying_insufficient_is_a_clear_insufficient_answer() {
    let k = K::with_runtime(database().await, FakeRuntime::new(vec!["INSUFFICIENT_KNOWLEDGE\nNothing about pricing.".into()])).await;
    wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    let id = ask::start(&k.state, &k.admin, input("What is our price?")).await.unwrap()["id"].as_str().unwrap().to_owned();
    let done = wait(&k, &k.admin, &id).await;
    assert_eq!(done["state"], "insufficient");
    assert_eq!(done["insufficient"]["reason"], "agent");
    assert_eq!(done["insufficient"]["note"], "Nothing about pricing.");
    assert!(done["answer"].is_null());
}

#[tokio::test]
async fn an_answer_without_a_valid_citation_is_withheld_as_ungrounded() {
    let k = K::with_runtime(database().await, FakeRuntime::new(vec!["The team ships daily.[^k-000000000000]".into()])).await;
    wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    let id = ask::start(&k.state, &k.admin, input("When?")).await.unwrap()["id"].as_str().unwrap().to_owned();
    let done = wait(&k, &k.admin, &id).await;
    assert_eq!(done["state"], "insufficient");
    assert_eq!(done["insufficient"]["reason"], "ungrounded");
    assert!(done["answer"].is_null());
}

#[tokio::test]
async fn no_readable_page_answers_insufficient_without_running_an_agent() {
    let log: Log = Arc::default();
    let k = K::with_runtime(database().await, answering(log.clone())).await;
    k.connect(&k.admin, "claude").await;
    let done = ask::start(&k.state, &k.admin, input("Anything?")).await.unwrap();
    assert_eq!(done["state"], "insufficient");
    assert_eq!(done["insufficient"]["reason"], "no_readable_pages");
    assert!(log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_source_revoked_after_the_answer_discards_it_on_the_next_read() {
    let k = K::with_runtime(database().await, answering(Arc::default())).await;
    let (chat_id, _) = wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    let id = ask::start(&k.state, &k.admin, input("When?")).await.unwrap()["id"].as_str().unwrap().to_owned();
    assert_eq!(wait(&k, &k.admin, &id).await["state"], "answered");
    k.set_visibility(&chat_id, Visibility::Private).await;
    let after = ask::get(&k.state, &k.state.database, &k.admin, &id).await.unwrap().unwrap();
    assert_eq!(after["state"], "failed");
    assert_eq!(after["error"]["code"], "source_revoked");
    assert!(after["answer"].is_null() && after["citations"].as_array().unwrap().is_empty());
    // Sharing it again does not resurrect the discarded answer.
    k.set_visibility(&chat_id, Visibility::Organization).await;
    let again = ask::get(&k.state, &k.state.database, &k.admin, &id).await.unwrap().unwrap();
    assert_eq!(again["state"], "failed");
}

#[tokio::test]
async fn a_source_revoked_while_running_fails_closed_before_recording() {
    let log: Log = Arc::default();
    let k = K::with_runtime(database().await, FakeRuntime::new(Vec::new()).hanging().recording(log.clone())).await;
    let (chat_id, key) = wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    let id = ask::start(&k.state, &k.admin, input("When?")).await.unwrap()["id"].as_str().unwrap().to_owned();
    k.set_visibility(&chat_id, Visibility::Private).await;
    let outcome = ask::conclude(&k.state, &id, &format!("Fridays.[^k-{key}]")).await;
    assert_eq!(outcome, ask::Outcome::Failed(ask::AskFailure { code: "source_revoked".into(), category: None, message: None }));
    ask::stop(&k.state, &k.state.database, &k.admin, &id).await.unwrap();
}

#[tokio::test]
async fn an_asker_who_lost_admin_while_running_gets_no_answer() {
    let k = K::with_runtime(database().await, FakeRuntime::new(Vec::new()).hanging()).await;
    let (_, key) = wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    let id = ask::start(&k.state, &k.admin, input("When?")).await.unwrap()["id"].as_str().unwrap().to_owned();
    crate::neev::db::execute(
        &k.state.database,
        "UPDATE neev_memberships SET role='member' WHERE org_id=$1 AND user_id=$2",
        vec![k.admin.org_id.into(), k.admin.identity.user_id.into()],
    )
    .await
    .unwrap();
    let outcome = ask::conclude(&k.state, &id, &format!("Fridays.[^k-{key}]")).await;
    assert!(matches!(outcome, ask::Outcome::Failed(ref f) if f.code == "not_admin"), "{outcome:?}");
}

#[tokio::test]
async fn stop_cancels_a_running_ask_and_frees_the_per_user_slot() {
    let k = K::with_runtime(database().await, FakeRuntime::new(Vec::new()).hanging()).await;
    wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    let first = ask::start(&k.state, &k.admin, input("When?")).await.unwrap();
    assert_eq!(first["state"], "running");
    let id = first["id"].as_str().unwrap().to_owned();
    assert!(matches!(ask::start(&k.state, &k.admin, input("Again?")).await, Err(KnowledgeError::AskBusy)));
    let stopped = ask::stop(&k.state, &k.state.database, &k.admin, &id).await.unwrap();
    assert_eq!(stopped["state"], "canceled");
    assert!(stopped["error"].is_null());
    // The driver sees the cancel and must not overwrite the stop.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(wait(&k, &k.admin, &id).await["state"], "canceled");
    // Stop is idempotent on a finished ask.
    assert_eq!(ask::stop(&k.state, &k.state.database, &k.admin, &id).await.unwrap()["state"], "canceled");
    let second = ask::start(&k.state, &k.admin, input("Again?")).await.unwrap();
    ask::stop(&k.state, &k.state.database, &k.admin, second["id"].as_str().unwrap()).await.unwrap();
}

#[tokio::test]
async fn a_run_past_the_timeout_is_cancelled_and_fails() {
    let k = K::with_runtime(database().await, FakeRuntime::new(Vec::new()).hanging()).await;
    k.state.knowledge_asks.set_timeout_for_test(std::time::Duration::from_millis(50));
    wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    let id = ask::start(&k.state, &k.admin, input("When?")).await.unwrap()["id"].as_str().unwrap().to_owned();
    let done = wait(&k, &k.admin, &id).await;
    assert_eq!(done["state"], "failed");
    assert_eq!(done["error"]["code"], "timeout");
}

#[tokio::test]
async fn runtime_refusals_are_reported_as_safe_codes() {
    for (error, code) in [
        (ExecutionError::Unauthorized, "unauthorized"),
        (ExecutionError::LanesBusy, "lanes_busy"),
        (ExecutionError::Disabled, "disabled"),
        (ExecutionError::Unsupported, "unsupported"),
    ] {
        let k = K::with_runtime(database().await, FakeRuntime::new(Vec::new()).failing(error)).await;
        wiki(&k).await;
        k.connect(&k.admin, "claude").await;
        let done = ask::start(&k.state, &k.admin, input("When?")).await.unwrap();
        assert_eq!(done["state"], "failed");
        assert_eq!(done["error"]["code"], code);
        // A refused ask does not hold the per-user slot.
        assert!(!matches!(ask::start(&k.state, &k.admin, input("Again?")).await, Err(KnowledgeError::AskBusy)));
    }
}

#[test]
fn a_streamed_provider_failure_keeps_its_class_and_a_redacted_message() {
    let handle = "0123456789abcdef0123456789abcdef".to_owned();
    let failure = ask::provider_failure(
        Some((Category::AuthExpired, Some(format!("OAuth token expired for {handle} in /Users/sam/.claude token=abcdefgh123")))),
        Some("provider"),
        &handle,
    );
    assert_eq!(failure.code, "unauthorized");
    assert_eq!(failure.category.as_deref(), Some("auth_expired"));
    let message = failure.message.unwrap();
    assert!(!message.contains(&handle) && !message.contains("/Users") && !message.contains("abcdefgh123"), "{message}");
    assert!(message.contains("expired"));
    let bare = ask::provider_failure(None, Some("io"), &handle);
    assert_eq!((bare.code.as_str(), bare.category, bare.message), ("unavailable", None, None));
}

#[tokio::test]
async fn finished_asks_expire_after_the_retention_window() {
    let k = K::with_runtime(database().await, answering(Arc::default())).await;
    wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    let id = ask::start(&k.state, &k.admin, input("When?")).await.unwrap()["id"].as_str().unwrap().to_owned();
    wait(&k, &k.admin, &id).await;
    k.advance(ask::ASK_RETENTION_SECS + 1);
    assert!(ask::get(&k.state, &k.state.database, &k.admin, &id).await.unwrap().is_none());
    assert_eq!(k.state.knowledge_asks.len(), 0);
}

#[tokio::test]
async fn stop_on_a_finished_answer_rechecks_and_never_returns_a_revoked_answer() {
    let k = K::with_runtime(database().await, answering(Arc::default())).await;
    let (chat_id, _) = wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    let id = ask::start(&k.state, &k.admin, input("When?")).await.unwrap()["id"].as_str().unwrap().to_owned();
    assert_eq!(wait(&k, &k.admin, &id).await["state"], "answered");
    k.set_visibility(&chat_id, Visibility::Private).await;
    let stopped = ask::stop(&k.state, &k.state.database, &k.admin, &id).await.unwrap();
    assert_eq!(stopped["state"], "failed");
    assert_eq!(stopped["error"]["code"], "source_revoked");
    assert!(stopped["answer"].is_null() && stopped["citations"].as_array().unwrap().is_empty() && stopped["pages"].as_array().unwrap().is_empty());
    let text = stopped.to_string();
    assert!(!text.contains("Fridays") && !text.contains("\"Team\""), "{text}");
    let after = ask::get(&k.state, &k.state.database, &k.admin, &id).await.unwrap().unwrap();
    assert_eq!(after["error"]["code"], "source_revoked");
}

#[tokio::test]
async fn an_archived_staged_page_no_longer_backs_an_answer() {
    let k = K::with_runtime(database().await, answering(Arc::default())).await;
    wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    let id = ask::start(&k.state, &k.admin, input("When?")).await.unwrap()["id"].as_str().unwrap().to_owned();
    assert_eq!(wait(&k, &k.admin, &id).await["state"], "answered");
    crate::neev::db::execute(
        &k.state.database,
        "UPDATE neev_knowledge_pages SET status='archived' WHERE org_id=$1 AND slug='team'",
        vec![k.admin.org_id.into()],
    )
    .await
    .unwrap();
    let after = ask::get(&k.state, &k.state.database, &k.admin, &id).await.unwrap().unwrap();
    assert_eq!(after["state"], "failed");
    assert_eq!(after["error"]["code"], "source_revoked");
    assert!(after["answer"].is_null());
}

#[tokio::test]
async fn the_background_sweep_expires_finished_asks_without_any_request() {
    let k = K::with_runtime(database().await, answering(Arc::default())).await;
    wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    let id = ask::start(&k.state, &k.admin, input("When?")).await.unwrap()["id"].as_str().unwrap().to_owned();
    wait(&k, &k.admin, &id).await;
    ask::sweep(&k.state, k.now());
    assert_eq!(k.state.knowledge_asks.len(), 1, "still inside the retention window");
    k.advance(ask::ASK_RETENTION_SECS + 1);
    ask::sweep(&k.state, k.now());
    assert_eq!(k.state.knowledge_asks.len(), 0);
}

#[tokio::test]
async fn an_ask_never_takes_the_last_lane_and_a_stale_driver_is_cancelled_by_the_sweep() {
    let slots = SlotPool::new();
    let k = K::with_runtime(database().await, FakeRuntime::new(Vec::new()).hanging().with_slots(slots.clone())).await;
    wiki(&k).await;
    k.connect(&k.admin, "claude").await;
    // An ordinary chat holds one of the two lanes: the ask must not take the other.
    let chat_lane = slots.acquire_if(1).unwrap();
    let refused = ask::start(&k.state, &k.admin, input("When?")).await.unwrap();
    assert_eq!(refused["error"]["code"], "lanes_busy");
    assert_eq!(slots.free_count(), 1);
    slots.release(chat_lane);
    // With both lanes free it runs, holding one.
    let running = ask::start(&k.state, &k.admin, input("When?")).await.unwrap();
    assert_eq!(running["state"], "running");
    let id = running["id"].as_str().unwrap().to_owned();
    assert_eq!(slots.free_count(), 1);
    // The driver never finishes (as if wedged); past timeout + grace the sweep
    // fails the ask, cancels its run and frees the user's slot.
    k.advance(ask::ASK_TIMEOUT_SECS as i64 + ask::STALE_GRACE_SECS);
    ask::sweep(&k.state, k.now());
    let swept = ask::get(&k.state, &k.state.database, &k.admin, &id).await.unwrap().unwrap();
    assert_eq!(swept["state"], "failed");
    assert_eq!(swept["error"]["code"], "timeout");
    for _ in 0..200 {
        if slots.free_count() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(slots.free_count(), 2, "the cancelled run released its lane");
    let again = ask::start(&k.state, &k.admin, input("Again?")).await.unwrap();
    assert_eq!(again["state"], "running");
    ask::stop(&k.state, &k.state.database, &k.admin, again["id"].as_str().unwrap()).await.unwrap();
}
