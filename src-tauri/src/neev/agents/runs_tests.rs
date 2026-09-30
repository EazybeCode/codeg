//! The live-run driver: what reaches storage, what reaches a reader, and what
//! happens when a reader arrives late or leaves early.

use super::*;
use crate::neev::agents::execution::{AgentExecutionRuntime, FakeRuntime, RunRequest};
use crate::neev::agents::sessions::{self, AgentSession, SessionInput};
use crate::neev::content::types::ContentError;
use crate::neev::test_support as h;
use crate::neev::types::OrgContext;
use std::sync::Arc;

const NOW: i64 = 1_000;

struct Harness {
    state: Arc<ServerState>,
    actor: OrgContext,
    session: AgentSession,
}

impl Harness {
    async fn new() -> Self {
        let database = h::database().await;
        let user = h::native_account(&database, "runner", "runner@example.test").await;
        let (org, _) = h::organization(&database, user, "admin").await;
        let state = Arc::new(
            ServerState::new(
                database.clone(),
                h::preview_config(),
                Arc::new(h::TestClock::new(NOW)),
            )
            .unwrap(),
        );
        let actor = h::scoped_context(&database, user, org, NOW).await;
        let session = sessions::create(
            &state,
            &actor,
            SessionInput {
                title: "Session".into(),
                provider: "claude".into(),
                model: "default".into(),
                bound_chat_id: None,
                node_id: None,
                effort: None,
                fast: false,
                request_id: uuid::Uuid::new_v4().to_string(),
            },
        )
        .await
        .unwrap();
        Self {
            state,
            actor,
            session,
        }
    }

    /// Begin a turn and drive it with a fake runtime producing `chunks`.
    async fn run(&self, prompt: &str, chunks: Vec<String>) -> (String, Arc<LiveRun>) {
        let (turn, _) =
            sessions::begin_turn(&self.state, &self.actor, &self.session.id, prompt, None)
                .await
                .unwrap();
        let handle = FakeRuntime::new(chunks)
            .run(RunRequest {
                user_id: self.actor.identity.user_id,
                org_id: self.actor.org_id,
                chat_id: self.session.id.clone(),
                provider: "claude".into(),
                model: "default".into(),
                prompt: prompt.into(),
                credential_handle: None,
                briefing: None,
                effort: None,
                fast: false,
                attachments: Vec::new(),
                context_files: Vec::new(),
                library_items: Vec::new(),
            })
            .await
            .unwrap();
        let live = start(
            self.state.clone(),
            turn.run_id.clone(),
            self.actor.clone(),
            self.session.id.clone(),
            turn.id.clone(),
            handle,
            None,
        );
        (turn.run_id, live)
    }

    async fn turn(&self, run_id: &str) -> sessions::AgentTurn {
        sessions::turn_by_run(&self.state, &self.actor, run_id)
            .await
            .unwrap()
    }

    /// Poll until the run leaves the registry (the driver deregisters only
    /// after the durable row is settled).
    async fn settled(&self, run_id: &str) -> sessions::AgentTurn {
        for _ in 0..400 {
            if find(&self.state.agent_runs, run_id).is_none() {
                let turn = self.turn(run_id).await;
                if turn.status != sessions::TurnStatus::Running {
                    return turn;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("run {run_id} never settled");
    }
}

#[tokio::test]
async fn a_completed_run_persists_its_whole_reply() {
    let h = Harness::new().await;
    let (run_id, _live) = h.run("draft", vec!["hello ".into(), "world".into()]).await;
    let turn = h.settled(&run_id).await;
    assert_eq!(turn.status, sessions::TurnStatus::Completed);
    assert_eq!(turn.response, "hello world");
    assert_eq!(turn.error_code, None);
    assert!(turn.completed_at.is_some());
}

#[tokio::test]
async fn a_reader_attaching_after_the_first_chunks_still_sees_all_of_them() {
    let h = Harness::new().await;
    // Many chunks, so the reader is very likely to attach mid-stream; the
    // snapshot+offset protocol must make the result identical either way.
    let chunks: Vec<String> = (0..200).map(|index| format!("{index},")).collect();
    let expected: String = chunks.concat();
    let (run_id, _live) = h.run("count", chunks).await;

    let mut seen = String::new();
    if let Some(mut attachment) = attach(&h.state.agent_runs, &run_id) {
        seen.push_str(&attachment.snapshot);
        let mut emitted = attachment.snapshot_chars;
        while let Ok(signal) = attachment.events.recv().await {
            match signal {
                RunSignal::Delta(delta) => {
                    let end = delta.offset + delta.text.chars().count();
                    if end <= emitted {
                        continue;
                    }
                    let text: String = if delta.offset >= emitted {
                        delta.text
                    } else {
                        delta.text.chars().skip(emitted - delta.offset).collect()
                    };
                    emitted = end;
                    seen.push_str(&text);
                }
                RunSignal::Activity(_) => {}
                RunSignal::Finished { .. } => break,
                RunSignal::Input { .. } => panic!("unexpected follow-up in a run without steering input"),
            }
        }
    }
    let turn = h.settled(&run_id).await;
    assert_eq!(turn.response, expected);
    // No gap and no duplicate: what the reader saw is exactly what was stored.
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn cancel_settles_the_turn_as_canceled_and_keeps_partial_output() {
    let h = Harness::new().await;
    let chunks: Vec<String> = (0..5_000).map(|_| "tick ".to_owned()).collect();
    let (run_id, live) = h.run("long", chunks).await;
    live.cancel();
    let turn = h.settled(&run_id).await;
    assert_eq!(turn.status, sessions::TurnStatus::Canceled);
    assert_eq!(turn.error_code.as_deref(), Some("canceled"));
    // Whatever arrived before the interrupt is kept; the run never completed.
    assert!(turn.response.chars().count() < 5_000 * 5);
}

#[tokio::test]
async fn cancel_is_idempotent() {
    let h = Harness::new().await;
    let chunks: Vec<String> = (0..5_000).map(|_| "tick ".to_owned()).collect();
    let (run_id, live) = h.run("long", chunks).await;
    live.cancel();
    live.cancel();
    let turn = h.settled(&run_id).await;
    assert_eq!(turn.status, sessions::TurnStatus::Canceled);
}

#[tokio::test]
async fn a_finished_run_leaves_the_registry_only_after_it_is_durable() {
    let h = Harness::new().await;
    let (run_id, _live) = h.run("q", vec!["a".into()]).await;
    for _ in 0..400 {
        if find(&h.state.agent_runs, &run_id).is_none() {
            // The moment it is no longer live, the row must already be final —
            // otherwise a reattaching reader sees neither a live run nor a
            // result.
            let turn = h.turn(&run_id).await;
            assert_ne!(
                turn.status,
                sessions::TurnStatus::Running,
                "run left the registry while its row was still running"
            );
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("run never deregistered");
}

#[tokio::test]
async fn an_oversized_reply_fails_the_turn_instead_of_silently_truncating() {
    let h = Harness::new().await;
    // Each chunk is 10k characters; 30 of them blow past the 262144 cap.
    let chunks: Vec<String> = (0..30).map(|_| "x".repeat(10_000)).collect();
    let (run_id, _live) = h.run("huge", chunks).await;
    let turn = h.settled(&run_id).await;
    assert_eq!(turn.status, sessions::TurnStatus::Failed);
    assert_eq!(turn.error_code.as_deref(), Some("too_large"));
    assert_eq!(turn.response.chars().count(), sessions::MAX_RESPONSE_CHARS);
}

#[tokio::test]
async fn a_refusal_before_any_stream_is_recorded_on_the_durable_turn() {
    let h = Harness::new().await;
    let (turn, _) = sessions::begin_turn(&h.state, &h.actor, &h.session.id, "q", None)
        .await
        .unwrap();
    // The production default runtime refuses; the prompt must not vanish.
    settle_refusal(&h.state, &turn.run_id, "disabled")
        .await
        .unwrap();
    let settled = h.turn(&turn.run_id).await;
    assert_eq!(settled.status, sessions::TurnStatus::Failed);
    assert_eq!(settled.error_code.as_deref(), Some("disabled"));
    assert_eq!(settled.prompt, "q");
    // And the session is free to take a new turn immediately.
    assert!(
        sessions::begin_turn(&h.state, &h.actor, &h.session.id, "retry", None)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_run_whose_result_cannot_be_stored_never_reports_success() {
    let h = Harness::new().await;
    let (run_id, live) = h.run("q", vec!["an answer".into()]).await;
    let mut events = live.subscribe_for_test();
    // Make the terminal write fail for the whole retry window by taking the
    // table out from under the driver — the storage-failure injection.
    crate::neev::db::execute(
        &h.state.database,
        "ALTER TABLE neev_agent_turns RENAME TO neev_agent_turns_hidden",
        vec![],
    )
    .await
    .unwrap();
    drop(live);

    let mut reported = None;
    while let Ok(signal) = events.recv().await {
        if let RunSignal::Finished { status, error_code } = signal {
            reported = Some((status, error_code));
            break;
        }
    }
    // The reader is told the truth: nothing durable was written, so no
    // "completed" is claimed even though the provider produced a full reply.
    assert_eq!(
        reported,
        Some((sessions::TurnStatus::Failed, Some("unavailable".to_owned()))),
        "a run that could not be stored must not report completion"
    );

    // Startup reconciliation remains the backstop for a row that is still
    // `running` when a process dies — it settles it as `interrupted`.
    // (In-process recovery of THIS run is covered by
    // `a_transient_storage_outage_does_not_wedge_the_session`.)
    crate::neev::db::execute(
        &h.state.database,
        "ALTER TABLE neev_agent_turns_hidden RENAME TO neev_agent_turns",
        vec![],
    )
    .await
    .unwrap();
    sessions::reconcile_interrupted(&h.state.database, NOW + 60)
        .await
        .unwrap();
    let recovered = h.turn(&run_id).await;
    assert_eq!(recovered.status, sessions::TurnStatus::Failed);
    // Either recovery path is honest; neither ever claims the run completed.
    assert!(matches!(
        recovered.error_code.as_deref(),
        Some("interrupted") | Some("unavailable")
    ));
}

#[tokio::test]
async fn over_limit_text_is_never_broadcast_only_never_stored() {
    let h = Harness::new().await;
    let (run_id, live) = h
        .run("huge", (0..30).map(|_| "x".repeat(10_000)).collect())
        .await;
    let mut events = live.subscribe_for_test();
    let mut broadcast_chars = 0usize;
    while let Ok(signal) = events.recv().await {
        match signal {
            RunSignal::Delta(delta) => broadcast_chars += delta.text.chars().count(),
            RunSignal::Activity(_) => {}
            RunSignal::Finished { .. } => break,
                RunSignal::Input { .. } => panic!("unexpected follow-up in a run without steering input"),
        }
    }
    let turn = h.settled(&run_id).await;
    // What a live viewer received and what replay will show are the SAME length:
    // the cap is applied before broadcast, not after.
    assert_eq!(broadcast_chars, turn.response.chars().count());
    assert_eq!(broadcast_chars, sessions::MAX_RESPONSE_CHARS);
}

#[tokio::test]
async fn a_transient_storage_outage_does_not_wedge_the_session() {
    let h = Harness::new().await;
    let (run_id, live) = h.run("q", vec!["an answer".into()]).await;
    let mut events = live.subscribe_for_test();
    // The terminal write cannot be made for the whole fast-retry window.
    crate::neev::db::execute(
        &h.state.database,
        "ALTER TABLE neev_agent_turns RENAME TO neev_agent_turns_hidden",
        vec![],
    )
    .await
    .unwrap();
    drop(live);
    while let Ok(signal) = events.recv().await {
        if matches!(signal, RunSignal::Finished { .. }) {
            break;
        }
    }
    // The outage ends a moment later — no restart, no operator.
    crate::neev::db::execute(
        &h.state.database,
        "ALTER TABLE neev_agent_turns_hidden RENAME TO neev_agent_turns",
        vec![],
    )
    .await
    .unwrap();

    // Background reconciliation settles the stranded row on its own.
    let mut recovered = None;
    for _ in 0..600 {
        let turn = h.turn(&run_id).await;
        if turn.status != sessions::TurnStatus::Running {
            recovered = Some(turn);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let recovered = recovered.expect("the stranded turn was never reconciled in-process");
    assert_eq!(recovered.status, sessions::TurnStatus::Failed);
    assert_eq!(recovered.error_code.as_deref(), Some("unavailable"));

    // And the session is usable again — this is the whole point: a one-second
    // database blip must not cost a tenant their session until a restart.
    assert!(
        sessions::begin_turn(
            &h.state,
            &h.actor,
            &h.session.id,
            "retry after outage",
            None
        )
        .await
        .is_ok(),
        "the session is still wedged by the stranded run"
    );
}

#[tokio::test]
async fn a_replayed_request_id_returns_the_same_turn_and_never_runs_twice() {
    let h = Harness::new().await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let (first, created) = sessions::begin_turn(
        &h.state,
        &h.actor,
        &h.session.id,
        "one ask",
        Some(&request_id),
    )
    .await
    .unwrap();
    assert!(created, "the first caller must own the turn it created");
    // Replay while the first turn is still live.
    let (concurrent, created) = sessions::begin_turn(
        &h.state,
        &h.actor,
        &h.session.id,
        "one ask",
        Some(&request_id),
    )
    .await
    .unwrap();
    // The replay is told it created NOTHING, so it never executes.
    assert!(!created, "a replay must not be told it created the turn");
    assert_eq!(concurrent.id, first.id);
    assert_eq!(concurrent.run_id, first.run_id);

    sessions::finish(
        &h.state.database,
        &first.run_id,
        "the answer",
        None,
        sessions::TurnStatus::Completed,
        None,
        NOW + 1,
    )
    .await
    .unwrap();

    // Replay AFTER it completed — the case a live-turn guard cannot catch.
    let (sequential, created) = sessions::begin_turn(
        &h.state,
        &h.actor,
        &h.session.id,
        "one ask",
        Some(&request_id),
    )
    .await
    .unwrap();
    assert!(
        !created,
        "a late replay must not be told it created the turn"
    );
    assert_eq!(sequential.id, first.id);
    assert_eq!(sequential.run_id, first.run_id);
    assert_eq!(sequential.status, sessions::TurnStatus::Completed);
    assert_eq!(sequential.response, "the answer");

    // Exactly one turn exists: no second provider execution was ever started.
    let turns = sessions::turns(&h.state, &h.actor, &h.session.id, None)
        .await
        .unwrap();
    assert_eq!(turns.items.len(), 1);

    // A DIFFERENT request id is a genuinely new ask and is allowed.
    let (fresh, created) = sessions::begin_turn(
        &h.state,
        &h.actor,
        &h.session.id,
        "another ask",
        Some(&uuid::Uuid::new_v4().to_string()),
    )
    .await
    .unwrap();
    assert!(created, "a genuinely new ask is created, not replayed");
    assert_ne!(fresh.id, first.id);
}

#[tokio::test]
async fn a_replay_is_answered_even_after_the_session_is_archived() {
    let h = Harness::new().await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let (first, _) =
        sessions::begin_turn(&h.state, &h.actor, &h.session.id, "ask", Some(&request_id))
            .await
            .unwrap();
    sessions::finish(
        &h.state.database,
        &first.run_id,
        "done",
        None,
        sessions::TurnStatus::Completed,
        None,
        NOW + 1,
    )
    .await
    .unwrap();
    let session = sessions::get(&h.state, &h.actor, &h.session.id)
        .await
        .unwrap();
    sessions::set_archived(&h.state, &h.actor, &h.session.id, true, session.revision)
        .await
        .unwrap();
    // Retrying a request that already succeeded returns its result rather than
    // a late objection about the session's current state.
    let (replay, created) =
        sessions::begin_turn(&h.state, &h.actor, &h.session.id, "ask", Some(&request_id))
            .await
            .unwrap();
    assert!(!created);
    assert_eq!(replay.id, first.id);
}

#[tokio::test]
async fn attaching_to_an_unknown_run_id_finds_nothing() {
    let h = Harness::new().await;
    assert!(attach(&h.state.agent_runs, "ffffffffffffffffffffffffffffffff").is_none());
    assert!(find(&h.state.agent_runs, "ffffffffffffffffffffffffffffffff").is_none());
}

#[tokio::test]
async fn output_is_flushed_to_storage_while_the_run_is_still_live() {
    let h = Harness::new().await;
    // Enough characters to cross the flush threshold several times, and enough
    // chunks that the run is still live when we look.
    let chunks: Vec<String> = (0..4_000).map(|_| "0123456789".to_owned()).collect();
    let (run_id, live) = h.run("stream", chunks).await;
    let mut flushed = String::new();
    for _ in 0..600 {
        let turn = h.turn(&run_id).await;
        if turn.status == sessions::TurnStatus::Running && !turn.response.is_empty() {
            flushed = turn.response;
            break;
        }
        if turn.status != sessions::TurnStatus::Running {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    live.cancel();
    h.settled(&run_id).await;
    assert!(
        !flushed.is_empty(),
        "a reload mid-run must find partial output already persisted"
    );
}

#[tokio::test]
async fn settle_refusal_refuses_a_run_id_that_is_not_the_caller_s() {
    let h = Harness::new().await;
    // No such run: the update matches nothing and nothing is invented.
    assert_eq!(
        settle_refusal(&h.state, "ffffffffffffffffffffffffffffffff", "disabled").await,
        Ok::<(), ContentError>(())
    );
    assert!(
        sessions::turn_by_run_id(&h.state.database, "ffffffffffffffffffffffffffffffff")
            .await
            .unwrap()
            .is_none()
    );
}

/// A `LiveRun` with nothing behind it, for exercising the buffer alone.
fn buffer_only() -> LiveRun {
    let (events, _) = tokio::sync::broadcast::channel(4);
    LiveRun {
        org_id: 1,
        owner_id: 1,
        session_id: "session".to_owned(),
        turn_id: "turn".to_owned(),
        buffer: std::sync::Mutex::new((String::new(), 0)),
        activity: std::sync::Mutex::new(crate::neev::agents::activity::ActivityLog::new()),
        events,
        cancel: std::sync::Mutex::new(None),
        steer_tx: None,
        supports_steering: false,
    }
}

#[test]
fn whole_messages_are_separated_by_a_blank_line_and_fragments_are_not() {
    let live = buffer_only();
    // A turn: preamble, then the answer. These arrived joined, which is how
    // "…improvements." and "Yes…" became "…improvements.Yes…".
    live.append_message("Let me look for obvious improvements.");
    live.append_message("Yes — here is the review.");
    let (text, _) = live.snapshot();
    assert_eq!(
        text,
        "Let me look for obvious improvements.\n\nYes — here is the review."
    );

    // Fragments of ONE message keep joining exactly as they arrive; a break
    // inserted between these would land mid-word.
    let live = buffer_only();
    live.append_capped("improve");
    live.append_capped("ments");
    assert_eq!(live.snapshot().0, "improvements");
}

#[test]
fn a_break_is_topped_up_never_stacked() {
    // Idempotent at the seam: newlines already present on either side count, so
    // a provider that formats its own paragraphs does not get three blank lines.
    for (first, second, expected) in [
        ("One.", "Two.", "One.\n\nTwo."),
        ("One.\n", "Two.", "One.\n\nTwo."),
        ("One.\n\n", "Two.", "One.\n\nTwo."),
        ("One.", "\nTwo.", "One.\n\nTwo."),
        ("One.", "\n\nTwo.", "One.\n\nTwo."),
        ("One.\n", "\nTwo.", "One.\n\nTwo."),
        // Already more than a blank line: left alone rather than trimmed. The
        // reply is the provider's, and this only ever ADDS a missing break.
        ("One.\n\n\n", "Two.", "One.\n\n\nTwo."),
    ] {
        let live = buffer_only();
        live.append_message(first);
        live.append_message(second);
        assert_eq!(
            live.snapshot().0,
            expected,
            "joining {first:?} to {second:?}"
        );
    }
}

#[test]
fn what_readers_are_sent_is_exactly_what_is_stored() {
    // The invariant the driver depends on: it broadcasts `accepted` and stores
    // the buffer. A separator written to one but not returned in the other
    // would make the live reply and the reloaded reply differ by the break —
    // silently, and only for readers who watched it arrive.
    let live = buffer_only();
    let mut broadcast = String::new();
    // Astral characters on both sides of a seam: `offset` counts scalars, so a
    // break inserted near one must not drift the count.
    for message in ["First 🙂.", "Second 👍.", "Third."] {
        let appended = live.append_message(message);
        assert_eq!(
            appended.offset,
            broadcast.chars().count(),
            "a delta must start where the last one ended"
        );
        broadcast.push_str(&appended.accepted);
    }
    let (stored, count) = live.snapshot();
    assert_eq!(broadcast, stored, "live stream and stored reply diverged");
    assert_eq!(count, stored.chars().count(), "offset counts scalars");
    assert_eq!(stored, "First 🙂.\n\nSecond 👍.\n\nThird.");
}

#[test]
fn a_break_cannot_push_a_reply_past_the_cap_unnoticed() {
    let live = buffer_only();
    let first = "a".repeat(MAX_RESPONSE_CHARS - 2);
    live.append_message(&first);
    // Only the two break characters fit; the message itself does not. The
    // append that overflows is the one that reports it, so the run still stops
    // rather than silently dropping the tail.
    let appended = live.append_message("b");
    assert!(
        appended.overflowed,
        "an overrun at the seam must be reported"
    );
    let (stored, count) = live.snapshot();
    assert_eq!(count, MAX_RESPONSE_CHARS, "never stores past the cap");
    assert_eq!(count, stored.chars().count());
}

async fn drive_spooled_output(corrupt: bool) {
    use sha2::{Digest, Sha256};
    let h = Harness::new().await;
    let (turn, _) = sessions::begin_turn(&h.state, &h.actor, &h.session.id, "Make a file", None)
        .await
        .unwrap();
    let mut handle = FakeRuntime::new(vec!["[Report](outputs/report.html)".into()])
        .run(RunRequest {
            user_id: h.actor.identity.user_id,
            org_id: h.actor.org_id,
            chat_id: h.session.id.clone(),
            provider: "claude".into(),
            model: "default".into(),
            prompt: "Make a file".into(),
            credential_handle: None,
            briefing: None,
            effort: None,
            fast: false,
            attachments: vec![],
            context_files: vec![],
            library_items: vec![],
        })
        .await
        .unwrap();
    assert_ne!(turn.run_id, handle.run_id);
    let root = tempfile::TempDir::new().unwrap();
    let spool = root
        .path()
        .join("spool")
        .join(format!("run-{}", handle.run_id));
    std::fs::create_dir_all(spool.join("files")).unwrap();
    let key = uuid::Uuid::new_v4().to_string();
    let bytes = b"<!doctype html><title>Durable report</title>";
    let hash = if corrupt {
        "bad".into()
    } else {
        format!("{:x}", Sha256::digest(bytes))
    };
    std::fs::write(
        spool.join("files").join(format!("{key}--report.html")),
        bytes,
    )
    .unwrap();
    std::fs::write(
        spool.join("manifest.json"),
        serde_json::json!({
            "run_id": format!("run-{}", handle.run_id), "errors": [],
            "files": [{"key":key,"name":"report.html","byte_length":bytes.len(),"sha256":hash}]
        })
        .to_string(),
    )
    .unwrap();
    handle.workspace_root = Some(root.path().to_owned());
    start(
        h.state.clone(),
        turn.run_id.clone(),
        h.actor.clone(),
        h.session.id.clone(),
        turn.id,
        handle,
        None,
    );
    let settled = h.settled(&turn.run_id).await;
    if corrupt {
        assert_eq!(settled.status, TurnStatus::Failed);
        assert!(spool.exists(), "failed persistence must retain the spool");
    } else {
        assert_eq!(settled.status, TurnStatus::Completed);
        assert!(settled.response.contains("/api/neev/attachments/"));
        assert!(!settled.response.contains("outputs/report.html"));
        assert!(!spool.exists());
    }
}

#[tokio::test]
async fn runtime_spool_id_is_distinct_from_durable_run_id_and_files_survive() {
    drive_spooled_output(false).await;
}

#[tokio::test]
async fn broken_export_never_reports_completed() {
    drive_spooled_output(true).await;
}
