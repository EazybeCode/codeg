//! The tool-free supervision seam: `run_tool_free` and its text-only guard.

use super::*;
use crate::neev::agents::activity::{ActivityDraft, ActivityKind};

const HANDLE: &str = "0123456789abcdef0123456789abcdef";

fn tool_free(provider: &str) -> ToolFreeRequest {
    ToolFreeRequest {
        user_id: 7,
        org_id: 3,
        chat_id: "chat-1".into(),
        provider: provider.into(),
        model: "claude-opus-4-1".into(),
        effort: Some("high".into()),
        credential_handle: Some(HANDLE.into()),
        instructions: "Answer only with the JSON object.".into(),
        prompt: "review this".into(),
        max_output_bytes: 1024,
    }
}

fn runtime(binaries: ProviderBinaries, stream: CodexStream) -> SandboxedProviderCliRuntime {
    SandboxedProviderCliRuntime::new(
        "python3",
        "/opt/neev/scripts/operations/neev_agent_launch.py",
        binaries,
        "/data/agents",
        vec!["140.82.112.3".into()],
        None,
        stream,
    )
}

fn all_binaries() -> ProviderBinaries {
    ProviderBinaries {
        claude: Some("/usr/local/bin/claude".into()),
        codex: Some("/usr/local/bin/codex".into()),
        cursor: Some("/usr/local/bin/agent".into()),
        gemini: Some("/usr/local/bin/gemini".into()),
        opencode: Some("/usr/local/bin/opencode".into()),
    }
}

const LANE: Lane = Lane {
    slot: "a",
    uid: 61000,
};

/// The refusal, without requiring `RunHandle: Debug`.
fn refused(result: Result<RunHandle, ExecutionError>) -> ExecutionError {
    match result {
        Ok(_) => panic!("a run started that must have been refused"),
        Err(error) => error,
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn a_tool_free_launch_is_claude_with_the_policy_and_nothing_else() {
    let runtime = runtime(all_binaries(), CodexStream::AppServer);
    let request = tool_free("claude").as_run_request();
    let argv = runtime
        .launch_argv_with(
            &request,
            LANE,
            Path::new("/data/agents/run-x"),
            "/usr/local/bin/claude",
            HANDLE,
            ToolPolicy::ToolFree,
        )
        .unwrap();
    let sep = argv.iter().position(|a| a == "--").unwrap();
    let launcher = &argv[..sep];
    let at = launcher.iter().position(|a| a == "--tool-policy").unwrap();
    assert_eq!(launcher[at + 1], "tool-free");
    assert!(!launcher.contains(&"--enable-steering".to_owned()));
    // Exactly the canonical shape the launcher's `tool_free_argv` accepts
    // (`RUST_TOOL_FREE_CLAUDE` in test_neev_tool_free_supervision.py).
    assert_eq!(
        argv[sep + 1..],
        strings(&[
            "/usr/local/bin/claude",
            "-p",
            "review this",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--verbose",
            "--append-system-prompt",
            "Answer only with the JSON object.",
            "--effort",
            "high",
            "--settings",
            "{\"fastMode\":false}",
            "--model",
            "claude-opus-4-1",
        ])[..]
    );
}

#[test]
fn an_ordinary_launch_is_byte_identical_to_before() {
    let runtime = runtime(all_binaries(), CodexStream::AppServer);
    let request = tool_free("claude").as_run_request();
    let workdir = Path::new("/data/agents/run-x");
    let ordinary = runtime
        .launch_argv(&request, LANE, workdir, "/usr/local/bin/claude", HANDLE)
        .unwrap();
    assert!(!ordinary.contains(&"--tool-policy".to_owned()));
    assert!(ordinary.contains(&"--enable-steering".to_owned()));
    let standard = runtime
        .launch_argv_with(
            &request,
            LANE,
            workdir,
            "/usr/local/bin/claude",
            HANDLE,
            ToolPolicy::Standard,
        )
        .unwrap();
    assert_eq!(ordinary, standard);
}

#[test]
fn the_request_carries_no_files_no_library_and_no_speed() {
    let request = tool_free("claude").as_run_request();
    assert!(request.attachments.is_empty());
    assert!(request.context_files.is_empty());
    assert!(request.library_items.is_empty());
    assert!(!request.fast);
    assert_eq!(request.min_free_lanes, 1);
    assert_eq!(
        request.briefing.as_deref(),
        Some("Answer only with the JSON object.")
    );
}

#[tokio::test]
async fn codex_and_acp_are_declined_before_a_lane_is_taken() {
    for stream in [CodexStream::Exec, CodexStream::AppServer] {
        let runtime = runtime(all_binaries(), stream);
        for provider in ["codex", "cursor", "gemini", "opencode", "nope"] {
            let error = refused(runtime.run_tool_free(tool_free(provider)).await);
            assert_eq!(error, ExecutionError::Unsupported, "{provider}");
            assert_eq!(runtime.slots.free_count(), 2);
        }
    }
}

#[tokio::test]
async fn an_unbounded_or_oversized_output_limit_is_refused() {
    let runtime = runtime(all_binaries(), CodexStream::Exec);
    for limit in [0, TOOL_FREE_MAX_OUTPUT_BYTES + 1] {
        let mut request = tool_free("claude");
        request.max_output_bytes = limit;
        assert_eq!(
            refused(runtime.run_tool_free(request).await),
            ExecutionError::Unauthorized
        );
    }
}

#[tokio::test]
async fn a_missing_credential_is_refused_like_any_run() {
    let runtime = runtime(all_binaries(), CodexStream::Exec);
    let mut request = tool_free("claude");
    request.credential_handle = None;
    assert_eq!(
        refused(runtime.run_tool_free(request).await),
        ExecutionError::Unauthorized
    );
}

#[tokio::test]
async fn runtimes_without_an_enforced_mode_decline() {
    let disabled = AgentRuntime::Disabled(DisabledRuntime);
    assert_eq!(
        refused(disabled.run_tool_free(tool_free("claude")).await),
        ExecutionError::Disabled
    );
    // The unsandboxed dev runtime cannot prove anything: trait default.
    let provider = AgentRuntime::Provider(ProviderCliRuntime::new("claude", "/tmp"));
    assert_eq!(
        refused(provider.run_tool_free(tool_free("claude")).await),
        ExecutionError::Unsupported
    );
}

/// A hand-scripted inner run: `events` are sent, then it waits for cancel.
fn scripted(events: Vec<RunEvent>, hang: bool) -> (RunHandle, oneshot::Receiver<()>) {
    let (tx, rx) = mpsc::channel(32);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let (seen_tx, seen_rx) = oneshot::channel();
    tokio::spawn(async move {
        for event in events {
            let _ = tx.send(event).await;
        }
        if hang {
            let _ = cancel_rx.await;
            let _ = seen_tx.send(());
            let _ = tx.send(RunEvent::Failed("canceled".into())).await;
        }
    });
    let handle = RunHandle {
        run_id: "r1".into(),
        events: rx,
        cancel: Some(cancel_tx),
        workspace_root: None,
        steer_tx: None,
        supports_steering: true,
    };
    (handle, seen_rx)
}

async fn drain(mut handle: RunHandle) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Some(event) = handle.next().await {
        events.push(event);
    }
    events
}

fn tool_call() -> RunEvent {
    RunEvent::Activity(ActivityDraft {
        id: "a1".into(),
        kind: ActivityKind::ToolCall {
            name: "Bash".into(),
            input: "{}".into(),
            input_truncated: false,
        },
    })
}

#[tokio::test]
async fn text_passes_through_and_the_handle_offers_no_steering_or_spool() {
    let (inner, _) = scripted(
        vec![
            RunEvent::Started("r1".into()),
            RunEvent::Chunk("{\"ok\":".into()),
            RunEvent::Chunk("true}".into()),
            RunEvent::Completed,
        ],
        false,
    );
    let guarded = text_only(inner, 64);
    assert!(guarded.steer_tx.is_none() && !guarded.supports_steering);
    assert!(guarded.workspace_root.is_none());
    assert_eq!(
        drain(guarded).await,
        vec![
            RunEvent::Started("r1".into()),
            RunEvent::Chunk("{\"ok\":".into()),
            RunEvent::Chunk("true}".into()),
            RunEvent::Completed,
        ]
    );
}

#[tokio::test]
async fn a_tool_event_stops_the_run_and_fails_it() {
    let (inner, stopped) = scripted(
        vec![RunEvent::Started("r1".into()), tool_call(), RunEvent::Chunk("late".into())],
        true,
    );
    let events = drain(text_only(inner, 64)).await;
    assert_eq!(
        events,
        vec![
            RunEvent::Started("r1".into()),
            RunEvent::Failed("tool_use".into())
        ]
    );
    stopped.await.expect("the inner run was cancelled");
}

#[tokio::test]
async fn output_over_the_limit_stops_the_run_and_fails_it() {
    let (inner, stopped) = scripted(
        vec![
            RunEvent::Started("r1".into()),
            RunEvent::Chunk("12345".into()),
            RunEvent::Message("678".into()),
        ],
        true,
    );
    let events = drain(text_only(inner, 7)).await;
    assert_eq!(
        events,
        vec![
            RunEvent::Started("r1".into()),
            RunEvent::Chunk("12345".into()),
            RunEvent::Failed("output_limit".into())
        ]
    );
    stopped.await.expect("the inner run was cancelled");
}

#[tokio::test]
async fn non_tool_activity_is_dropped_not_forwarded() {
    let status = RunEvent::Activity(ActivityDraft {
        id: "s1".into(),
        kind: ActivityKind::Status {
            state: "thinking".into(),
            detail: None,
            category: None,
        },
    });
    let (inner, _) = scripted(vec![status, RunEvent::Chunk("x".into()), RunEvent::Completed], false);
    assert_eq!(
        drain(text_only(inner, 8)).await,
        vec![RunEvent::Chunk("x".into()), RunEvent::Completed]
    );
}

#[tokio::test]
async fn cancelling_the_guarded_handle_cancels_the_inner_run() {
    let (inner, stopped) = scripted(vec![RunEvent::Started("r1".into())], true);
    let mut guarded = text_only(inner, 8);
    assert_eq!(guarded.next().await, Some(RunEvent::Started("r1".into())));
    guarded.cancel();
    assert_eq!(guarded.next().await, Some(RunEvent::Failed("canceled".into())));
    stopped.await.expect("the inner run was cancelled");
}

#[tokio::test]
async fn a_spool_left_by_the_inner_run_is_discarded_never_read() {
    let root = tempfile::tempdir().unwrap();
    let spool = root.path().join("spool").join("run-r1");
    std::fs::create_dir_all(&spool).unwrap();
    let (mut inner, _) = scripted(vec![RunEvent::Completed], false);
    inner.workspace_root = Some(root.path().to_path_buf());
    let events = drain(text_only(inner, 8)).await;
    assert_eq!(events, vec![RunEvent::Completed]);
    assert!(!spool.exists());
}

#[tokio::test]
async fn the_fake_runtime_records_and_guards_tool_free_runs() {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let runtime = AgentRuntime::Fake(
        FakeRuntime::new(vec!["{}".into()]).recording_tool_free(log.clone()),
    );
    let handle = runtime.run_tool_free(tool_free("claude")).await.unwrap();
    assert!(!handle.supports_steering);
    let events = drain(handle).await;
    assert_eq!(events.last(), Some(&RunEvent::Completed));
    assert_eq!(log.lock().unwrap().as_slice(), &[tool_free("claude")]);
    // The fake declines exactly what production declines.
    assert_eq!(
        refused(runtime.run_tool_free(tool_free("codex")).await),
        ExecutionError::Unsupported
    );
}
