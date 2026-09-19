//! Native agent-execution seam (see `.context/NEEV-AGENT-EXECUTION-CONTRACT.md`).
//!
//! A port the Neev chat calls to run a personal agent, stream output, and cancel
//! it. The real adapter binds the existing codeg ACP runtime
//! (`acp::connection::AgentConnection` — connect/prompt/stream/cancel) wrapped in
//! a per-tenant OS isolation profile; that adapter and its Linux isolation gate
//! are NOT built here. The default [`DisabledRuntime`] refuses to run, so
//! multi-tenant agent execution cannot occur by omission — production stays off
//! until the sandbox + end-to-end gates pass. [`FakeRuntime`] drives tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

/// Fixed, sanitized execution failure categories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExecutionError {
    /// The runtime is disabled (no isolation profile / production default).
    Disabled,
    Unauthorized,
    Isolation,
    Provider,
    Unavailable,
}

/// An author-scoped request to run one prompt in one chat with a connected model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunRequest {
    pub(crate) user_id: i32,
    pub(crate) org_id: i32,
    pub(crate) chat_id: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) prompt: String,
}

/// Streamed lifecycle of a run, mapped from ACP session updates by a real adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunEvent {
    Started(String),
    Chunk(String),
    Failed(String),
    Completed,
}

/// A live run: a bounded stream of [`RunEvent`] plus an idempotent cancel that
/// interrupts the turn (never a login/DB effect).
pub(crate) struct RunHandle {
    pub(crate) run_id: String,
    events: mpsc::Receiver<RunEvent>,
    cancel: Option<oneshot::Sender<()>>,
}

impl RunHandle {
    pub(crate) async fn next(&mut self) -> Option<RunEvent> {
        self.events.recv().await
    }

    pub(crate) fn cancel(&mut self) {
        if let Some(sender) = self.cancel.take() {
            let _ = sender.send(());
        }
    }
}

/// The execution port. The real implementation authorizes (user, org, chat),
/// binds an isolated per-tenant workspace, spawns via ACP, and maps its stream.
pub(crate) trait AgentExecutionRuntime: Send + Sync {
    async fn run(&self, request: RunRequest) -> Result<RunHandle, ExecutionError>;
}

/// Production default: refuses every run. No unsandboxed multi-tenant execution
/// can happen because the runtime is off until a real isolated adapter is bound.
#[derive(Debug, Default)]
pub(crate) struct DisabledRuntime;

impl AgentExecutionRuntime for DisabledRuntime {
    async fn run(&self, _request: RunRequest) -> Result<RunHandle, ExecutionError> {
        Err(ExecutionError::Disabled)
    }
}

/// Test double: streams deterministic chunks for a prompt and honors cancel.
/// Never spawns a process; carries no isolation guarantee and is not a provider.
#[derive(Debug, Clone)]
pub(crate) struct FakeRuntime {
    chunks: Vec<String>,
}

impl FakeRuntime {
    pub(crate) fn new(chunks: Vec<String>) -> Self {
        Self { chunks }
    }
}

impl AgentExecutionRuntime for FakeRuntime {
    async fn run(&self, request: RunRequest) -> Result<RunHandle, ExecutionError> {
        if request.prompt.trim().is_empty() {
            return Err(ExecutionError::Unauthorized);
        }
        let run_id = format!("run-{}-{}", request.user_id, request.chat_id);
        let (tx, rx) = mpsc::channel::<RunEvent>(1);
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        let chunks = self.chunks.clone();
        let started = run_id.clone();
        tokio::spawn(async move {
            if tx.send(RunEvent::Started(started)).await.is_err() {
                return;
            }
            let stream = async {
                for chunk in chunks {
                    if tx.send(RunEvent::Chunk(chunk)).await.is_err() {
                        return;
                    }
                }
                let _ = tx.send(RunEvent::Completed).await;
            };
            tokio::select! {
                // Cancel (explicit signal or a dropped handle) interrupts the turn.
                _ = &mut cancel_rx => {
                    let _ = tx.send(RunEvent::Failed("canceled".into())).await;
                }
                _ = stream => {}
            }
        });
        Ok(RunHandle {
            run_id,
            events: rx,
            cancel: Some(cancel_tx),
        })
    }
}

/// Real adapter: runs a personal agent by driving the authenticated provider CLI
/// headless (`claude -p … --output-format stream-json`), mapping its streamed
/// events to [`RunEvent`]s and cancelling by killing the child. This is the
/// non-production local path: it runs as the current process with `workspace` as
/// cwd and carries NO per-tenant OS isolation, so it must stay behind the
/// runtime-enabled + Linux-sandbox gates before any multi-tenant use.
#[cfg(any(test, feature = "dev-agent"))]
#[derive(Debug, Clone)]
pub(crate) struct ProviderCliRuntime {
    binary: String,
    workspace: PathBuf,
}

#[cfg(any(test, feature = "dev-agent"))]
impl ProviderCliRuntime {
    pub(crate) fn new(binary: impl Into<String>, workspace: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            workspace: workspace.into(),
        }
    }

    /// Map one line of `--output-format stream-json` to an optional chunk.
    fn chunk_from_line(line: &str) -> Option<String> {
        chunk_from_line(line)
    }
}

#[cfg(any(test, feature = "dev-agent"))]
impl AgentExecutionRuntime for ProviderCliRuntime {
    async fn run(&self, request: RunRequest) -> Result<RunHandle, ExecutionError> {
        if request.user_id <= 0 || request.org_id <= 0 {
            return Err(ExecutionError::Unauthorized);
        }
        if request.prompt.trim().is_empty() {
            return Err(ExecutionError::Unauthorized);
        }
        let mut child = Command::new(&self.binary)
            .arg("-p")
            .arg(&request.prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--include-partial-messages")
            .arg("--verbose")
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| ExecutionError::Provider)?;
        let stdout = child.stdout.take().ok_or(ExecutionError::Unavailable)?;
        let run_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = mpsc::channel::<RunEvent>(16);
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        let started = run_id.clone();
        tokio::spawn(async move {
            if tx.send(RunEvent::Started(started)).await.is_err() {
                let _ = child.kill().await;
                return;
            }
            let mut lines = BufReader::new(stdout).lines();
            let stream = async {
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            if let Some(text) = Self::chunk_from_line(&line) {
                                if !text.is_empty() && tx.send(RunEvent::Chunk(text)).await.is_err()
                                {
                                    return;
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(_) => {
                            let _ = tx.send(RunEvent::Failed("io".into())).await;
                            return;
                        }
                    }
                }
                match child.wait().await {
                    Ok(status) if status.success() => {
                        let _ = tx.send(RunEvent::Completed).await;
                    }
                    _ => {
                        let _ = tx.send(RunEvent::Failed("provider".into())).await;
                    }
                }
            };
            tokio::select! {
                _ = &mut cancel_rx => {
                    // Cancel/restart interrupts the turn by killing the child.
                    let _ = tx.send(RunEvent::Failed("canceled".into())).await;
                }
                _ = stream => {}
            }
        });
        Ok(RunHandle {
            run_id,
            events: rx,
            cancel: Some(cancel_tx),
        })
    }
}

/// One acquirable isolation lane: an egress slot paired with a dedicated agent
/// uid. Pairing the uid with the slot guarantees two concurrent runs get BOTH
/// distinct slots AND distinct uids, so a concurrent tenant can never share a uid
/// (and thus can never read a peer's 0700 workdir). Cross-run credential leakage
/// is prevented separately by using an ephemeral, per-run workdir that is wiped
/// on teardown (see `SandboxedProviderCliRuntime::run` + `LaneGuard`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Lane {
    pub(crate) slot: &'static str,
    pub(crate) uid: u32,
}

/// Bounds concurrent sandboxed agents to the two provider-egress lanes the login
/// isolation model provisions (`neev_login_network_profile` slots a/b). Acquire
/// returns `None` when both are in use, so a third concurrent run is refused
/// (`Unavailable`) rather than colliding on a slot's netns/bridge.
#[derive(Clone)]
pub(crate) struct SlotPool {
    free: std::sync::Arc<std::sync::Mutex<Vec<Lane>>>,
}

impl SlotPool {
    pub(crate) fn new() -> Self {
        Self {
            free: std::sync::Arc::new(std::sync::Mutex::new(vec![
                Lane { slot: "a", uid: 61000 },
                Lane { slot: "b", uid: 61001 },
            ])),
        }
    }

    fn acquire(&self) -> Option<Lane> {
        self.free.lock().ok()?.pop()
    }

    fn release(&self, lane: Lane) {
        if let Ok(mut free) = self.free.lock() {
            if !free.contains(&lane) {
                free.push(lane);
            }
        }
    }
}

/// RAII guard: releases the lane back to the pool AND removes the ephemeral
/// per-run workdir, on every exit path including a panic (fixes the slot-leak and
/// residual-workdir issues). Removing the workdir means a later run that reuses
/// the same uid finds nothing to read.
struct LaneGuard {
    slots: SlotPool,
    lane: Lane,
    workdir: PathBuf,
}

impl Drop for LaneGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workdir);
        self.slots.release(self.lane);
    }
}

/// Map one line of `--output-format stream-json` to an optional text chunk.
/// Shared by the production sandboxed runtime and the dev/test provider runtime.
fn chunk_from_line(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "stream_event" {
        return None;
    }
    let event = value.get("event")?;
    if event.get("type")?.as_str()? != "content_block_delta" {
        return None;
    }
    let delta = event.get("delta")?;
    if delta.get("type")?.as_str()? == "text_delta" {
        return delta.get("text")?.as_str().map(str::to_string);
    }
    None
}

/// Production adapter: runs a personal agent by spawning the per-tenant sandbox +
/// provider-egress launcher (`neev_agent_launch.py`) — NEVER the provider binary
/// directly — so every agent process is isolated (mount/pid/net/uts/ipc
/// namespaces, private /proc, a dropped per-user uid owning a 0700 workdir,
/// no-new-privs + empty capability bounding set, rlimits) and its egress is
/// default-deny except to its provider proxy. The launcher forwards the
/// provider's `--output-format stream-json` on stdout; cancel sends SIGTERM so
/// the launcher tears the sandbox + egress down (SIGKILL would leak the netns).
///
/// Requires the process to run as root on Linux (unshare/setpriv/ip/nft) with
/// the login-isolation slot bridges/proxies provisioned. Concurrency is bounded
/// to the two egress slots (see [`SlotPool`]); per-user credential isolation
/// comes from the per-user uid + 0700 workdir, independent of the slot.
#[derive(Clone)]
pub(crate) struct SandboxedProviderCliRuntime {
    python: String,
    launcher: PathBuf,
    binary: String,
    workspace_root: PathBuf,
    provider_ipv4: Vec<String>,
    slots: SlotPool,
}

/// Bounded grace for the launcher to tear down its sandbox + egress on SIGTERM.
/// Must comfortably exceed the launcher's worst-case teardown (process-group
/// reap + several `ip`/`nft` teardown commands) so we never SIGKILL it mid-teardown
/// and leak the netns.
const LAUNCHER_TEARDOWN_GRACE_SECS: u64 = 45;

impl SandboxedProviderCliRuntime {
    pub(crate) fn new(
        python: impl Into<String>,
        launcher: impl Into<PathBuf>,
        binary: impl Into<String>,
        workspace_root: impl Into<PathBuf>,
        provider_ipv4: Vec<String>,
    ) -> Self {
        Self {
            python: python.into(),
            launcher: launcher.into(),
            binary: binary.into(),
            workspace_root: workspace_root.into(),
            provider_ipv4,
            slots: SlotPool::new(),
        }
    }

    /// Pure: the exact launcher argv for a request (unit-tested; no spawn). The
    /// provider binary only ever appears AFTER the `--` sandbox separator.
    fn launch_argv(&self, request: &RunRequest, lane: Lane, workdir: &Path) -> Vec<String> {
        let mut argv = vec![
            self.python.clone(),
            self.launcher.to_string_lossy().into_owned(),
            "--tenant".into(),
            format!("org:{}/user:{}", request.org_id, request.user_id),
            "--slot".into(),
            lane.slot.to_string(),
            "--workdir".into(),
            workdir.to_string_lossy().into_owned(),
            "--uid".into(),
            lane.uid.to_string(),
            "--gid".into(),
            lane.uid.to_string(),
        ];
        for ip in &self.provider_ipv4 {
            argv.push("--provider-ipv4".into());
            argv.push(ip.clone());
        }
        argv.push("--".into());
        argv.push(self.binary.clone());
        argv.push("-p".into());
        argv.push(request.prompt.clone());
        argv.push("--output-format".into());
        argv.push("stream-json".into());
        argv.push("--include-partial-messages".into());
        argv.push("--verbose".into());
        argv
    }
}

/// Gracefully stop the launcher so it tears down its sandbox + egress: SIGTERM,
/// wait up to the grace, then SIGKILL only as a last resort. Used on EVERY
/// non-clean-EOF exit path (cancel, io error, dropped receiver) so no path relies
/// on `kill_on_drop` (a bare SIGKILL) which would strand the netns.
async fn graceful_stop(child: &mut tokio::process::Child, pid: Option<u32>) {
    if let Some(pid) = pid {
        // SAFETY: `child` has not been reaped on any caller path (the only
        // `child.wait()` lives in the stream future, which is dropped un-awaited
        // before we get here), so the pid is still the launcher's — not reused.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
    if tokio::time::timeout(
        std::time::Duration::from_secs(LAUNCHER_TEARDOWN_GRACE_SECS),
        child.wait(),
    )
    .await
    .is_err()
    {
        // Grace expired: force-kill so we do not hang, accepting a possible netns
        // remnant (the launcher had the full grace to tear it down first).
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

impl AgentExecutionRuntime for SandboxedProviderCliRuntime {
    async fn run(&self, request: RunRequest) -> Result<RunHandle, ExecutionError> {
        if request.user_id <= 0 || request.org_id <= 0 || request.prompt.trim().is_empty() {
            return Err(ExecutionError::Unauthorized);
        }
        let lane = self.slots.acquire().ok_or(ExecutionError::Unavailable)?;
        let run_id = uuid::Uuid::new_v4().to_string();
        // Ephemeral, unique-per-run workdir; the LaneGuard removes it on teardown
        // so a later run reusing this uid finds nothing to read (cross-run leak
        // prevention), and concurrent runs get distinct uids+dirs from the lane.
        let workdir = self.workspace_root.join(format!("run-{run_id}"));
        // The guard owns lane release + workdir removal from here on (RAII: fires
        // on any early return, task end, or panic).
        let guard = LaneGuard {
            slots: self.slots.clone(),
            lane,
            workdir: workdir.clone(),
        };
        if std::fs::create_dir_all(&workdir).is_err() {
            return Err(ExecutionError::Isolation); // guard drops -> releases lane
        }
        let argv = self.launch_argv(&request, lane, &workdir);
        let mut child = match Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true) // last-resort safety only; all paths stop gracefully first
            .spawn()
        {
            Ok(child) => child,
            Err(_) => return Err(ExecutionError::Provider), // guard drops
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => return Err(ExecutionError::Unavailable), // guard drops
        };
        let pid = child.id();
        let (tx, rx) = mpsc::channel::<RunEvent>(16);
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        let started = run_id.clone();
        tokio::spawn(async move {
            // Move the guard into the task so lane release + workdir removal happen
            // when the task ends, on every path including panic.
            let _guard = guard;
            if tx.send(RunEvent::Started(started)).await.is_err() {
                graceful_stop(&mut child, pid).await;
                return;
            }
            let mut lines = BufReader::new(stdout).lines();
            // `needs_stop` is true whenever WE end the run early (so we must tell
            // the launcher to tear down); a clean provider EOF leaves it false
            // because the launcher exits on its own and runs its finally teardown.
            let stream = async {
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            if let Some(text) = chunk_from_line(&line) {
                                if !text.is_empty()
                                    && tx.send(RunEvent::Chunk(text)).await.is_err()
                                {
                                    return true; // receiver gone -> stop launcher
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(_) => {
                            let _ = tx.send(RunEvent::Failed("io".into())).await;
                            return true; // io error -> stop launcher
                        }
                    }
                }
                match child.wait().await {
                    Ok(status) if status.success() => {
                        let _ = tx.send(RunEvent::Completed).await;
                    }
                    _ => {
                        let _ = tx.send(RunEvent::Failed("provider".into())).await;
                    }
                }
                false // clean EOF: launcher already exiting + tearing down itself
            };
            tokio::select! {
                _ = &mut cancel_rx => {
                    graceful_stop(&mut child, pid).await;
                    let _ = tx.send(RunEvent::Failed("canceled".into())).await;
                }
                needs_stop = stream => {
                    if needs_stop {
                        graceful_stop(&mut child, pid).await;
                    }
                }
            }
        });
        Ok(RunHandle {
            run_id,
            events: rx,
            cancel: Some(cancel_tx),
        })
    }
}

/// Concrete dispatch over the runtime implementations (avoids a boxed async
/// trait object). Production is `Disabled` unless `NEEV_RUNTIME_ENABLED=true`,
/// in which case startup binds [`SandboxedProviderCliRuntime`] — never the bare
/// [`ProviderCliRuntime`], which carries no OS isolation and is dev/test only.
pub(crate) enum AgentRuntime {
    Disabled(DisabledRuntime),
    /// Unsandboxed direct provider spawn — DEV/TEST ONLY. Gated out of production
    /// builds so a release binary cannot even represent an unsandboxed runtime.
    #[cfg(any(test, feature = "dev-agent"))]
    Provider(ProviderCliRuntime),
    Sandboxed(SandboxedProviderCliRuntime),
    #[cfg(test)]
    Fake(FakeRuntime),
}

impl AgentRuntime {
    /// Whether an actual runtime is bound. `Disabled` (production default) is
    /// false, so capabilities never advertise agents unless a real/fake runtime
    /// is present; the native chat UI reads this to gate its "Ask agent" control.
    pub(crate) fn is_available(&self) -> bool {
        !matches!(self, Self::Disabled(_))
    }

    pub(crate) async fn run(&self, request: RunRequest) -> Result<RunHandle, ExecutionError> {
        match self {
            Self::Disabled(runtime) => runtime.run(request).await,
            #[cfg(any(test, feature = "dev-agent"))]
            Self::Provider(runtime) => runtime.run(request).await,
            Self::Sandboxed(runtime) => runtime.run(request).await,
            #[cfg(test)]
            Self::Fake(runtime) => runtime.run(request).await,
        }
    }
}

impl RunHandle {
    /// Take the cancel sender so a job registry can cancel this run by id from a
    /// separate request. Leaves `cancel()` on this handle a no-op afterwards.
    pub(crate) fn take_canceler(&mut self) -> Option<oneshot::Sender<()>> {
        self.cancel.take()
    }
}

/// Live-run cancel handles keyed by run id, each tagged with the owning user id
/// so a cancel request can only interrupt a run the caller started. Shared across
/// requests so a separate cancel/restart call can reach an in-flight run.
pub(crate) type JobRegistry = std::sync::Arc<
    std::sync::Mutex<std::collections::HashMap<String, (i32, oneshot::Sender<()>)>>,
>;

/// The production-default runtime (refuses; keeps agents off until an isolated
/// adapter replaces it).
pub(crate) fn disabled_runtime() -> std::sync::Arc<AgentRuntime> {
    std::sync::Arc::new(AgentRuntime::Disabled(DisabledRuntime))
}

/// The production sandboxed runtime, bound by startup only when
/// `NEEV_RUNTIME_ENABLED=true`. Spawns every agent through the isolation +
/// provider-egress launcher; never a bare provider process.
pub(crate) fn sandboxed_runtime(
    python: impl Into<String>,
    launcher: impl Into<PathBuf>,
    binary: impl Into<String>,
    workspace_root: impl Into<PathBuf>,
    provider_ipv4: Vec<String>,
) -> std::sync::Arc<AgentRuntime> {
    std::sync::Arc::new(AgentRuntime::Sandboxed(SandboxedProviderCliRuntime::new(
        python,
        launcher,
        binary,
        workspace_root,
        provider_ipv4,
    )))
}

pub(crate) fn new_job_registry() -> JobRegistry {
    std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(prompt: &str) -> RunRequest {
        RunRequest {
            user_id: 7,
            org_id: 3,
            chat_id: "chat-1".into(),
            provider: "codex".into(),
            model: "gpt".into(),
            prompt: prompt.into(),
        }
    }

    #[tokio::test]
    async fn disabled_runtime_refuses_every_run() {
        let error = DisabledRuntime.run(request("hi")).await.err();
        assert_eq!(error, Some(ExecutionError::Disabled));
    }

    #[tokio::test]
    async fn fake_runtime_streams_started_chunks_and_completed() {
        let runtime = FakeRuntime::new(vec!["a".into(), "b".into()]);
        let mut handle = runtime.run(request("draft a plan")).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = handle.next().await {
            events.push(event);
        }
        assert_eq!(
            events,
            vec![
                RunEvent::Started("run-7-chat-1".into()),
                RunEvent::Chunk("a".into()),
                RunEvent::Chunk("b".into()),
                RunEvent::Completed,
            ]
        );
    }

    #[tokio::test]
    async fn fake_runtime_cancel_interrupts_the_stream() {
        let runtime = FakeRuntime::new(vec!["a".into(), "b".into(), "c".into()]);
        let mut handle = runtime.run(request("long task")).await.unwrap();
        handle.cancel();
        let mut events = Vec::new();
        while let Some(event) = handle.next().await {
            events.push(event);
        }
        assert_eq!(events.first(), Some(&RunEvent::Started("run-7-chat-1".into())));
        assert_eq!(events.last(), Some(&RunEvent::Failed("canceled".into())));
        // The full chunk stream never completed.
        assert!(!events.contains(&RunEvent::Completed));
    }

    #[tokio::test]
    async fn fake_runtime_rejects_empty_prompt() {
        let runtime = FakeRuntime::new(vec!["a".into()]);
        assert_eq!(
            runtime.run(request("   ")).await.err(),
            Some(ExecutionError::Unauthorized)
        );
    }

    #[test]
    fn provider_cli_maps_stream_json_text_delta_to_chunk() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"PONG"}}}"#;
        assert_eq!(ProviderCliRuntime::chunk_from_line(line), Some("PONG".into()));
        // Non-text events yield no chunk.
        assert_eq!(
            ProviderCliRuntime::chunk_from_line(r#"{"type":"system","subtype":"init"}"#),
            None
        );
        assert_eq!(ProviderCliRuntime::chunk_from_line("not json"), None);
    }

    #[tokio::test]
    async fn provider_cli_rejects_unauthorized() {
        let runtime = ProviderCliRuntime::new("claude", std::env::temp_dir());
        let mut bad = request("hi");
        bad.user_id = 0;
        assert_eq!(runtime.run(bad).await.err(), Some(ExecutionError::Unauthorized));
    }

    fn sandboxed() -> SandboxedProviderCliRuntime {
        SandboxedProviderCliRuntime::new(
            "python3",
            "/opt/neev/scripts/operations/neev_agent_launch.py",
            "claude",
            "/var/lib/neev-native/agents",
            vec!["140.82.112.3".into()],
        )
    }

    #[test]
    fn sandboxed_launch_argv_goes_through_the_launcher_not_bare_provider() {
        let runtime = sandboxed();
        let lane = Lane { slot: "a", uid: 61000 };
        let workdir = Path::new("/var/lib/neev-native/agents/run-abc");
        let argv = runtime.launch_argv(&request("draft"), lane, workdir);
        // First tokens are the launcher, never the provider binary.
        assert_eq!(argv[0], "python3");
        assert!(argv[1].ends_with("neev_agent_launch.py"));
        assert!(argv.iter().any(|a| a == "--tenant"));
        assert!(argv.contains(&"org:3/user:7".to_string()));
        assert!(argv.contains(&"--slot".to_string()) && argv.contains(&"a".to_string()));
        assert!(argv.contains(&"--uid".to_string()) && argv.contains(&"61000".to_string()));
        assert!(argv.contains(&"--workdir".to_string()) && argv.iter().any(|a| a.ends_with("run-abc")));
        // Provider allowlist is passed through.
        assert!(argv.contains(&"--provider-ipv4".to_string()));
        assert!(argv.contains(&"140.82.112.3".to_string()));
        // The provider argv is AFTER the `--` separator (sandboxed), and the
        // provider binary never appears before it.
        let sep = argv.iter().position(|a| a == "--").expect("has -- separator");
        assert!(!argv[..sep].contains(&"claude".to_string()));
        assert_eq!(argv[sep + 1], "claude");
        assert_eq!(&argv[sep + 2..sep + 4], &["-p".to_string(), "draft".to_string()]);
        assert!(argv.contains(&"stream-json".to_string()));
    }

    #[test]
    fn sandboxed_deny_all_when_no_provider_ip() {
        let runtime = SandboxedProviderCliRuntime::new(
            "python3", "/l/launch.py", "claude", "/w", vec![],
        );
        let lane = Lane { slot: "b", uid: 61001 };
        let argv = runtime.launch_argv(&request("hi"), lane, Path::new("/w/run-x"));
        assert!(!argv.contains(&"--provider-ipv4".to_string()));
    }

    #[test]
    fn slot_pool_lanes_have_distinct_slots_and_uids_and_release() {
        let pool = SlotPool::new();
        let a = pool.acquire().expect("first lane");
        let b = pool.acquire().expect("second lane");
        // Two concurrent lanes never share a slot OR a uid (concurrent isolation).
        assert_ne!(a.slot, b.slot);
        assert_ne!(a.uid, b.uid);
        assert!(pool.acquire().is_none(), "only two lanes exist");
        pool.release(a);
        assert!(pool.acquire().is_some(), "released lane is reusable");
        pool.release(b);
    }

    #[tokio::test]
    async fn sandboxed_rejects_unauthorized_before_acquiring_a_slot() {
        let runtime = sandboxed();
        let mut bad = request("hi");
        bad.org_id = 0;
        assert_eq!(runtime.run(bad).await.err(), Some(ExecutionError::Unauthorized));
        // Both slots remain available (none consumed by the rejected run).
        assert!(runtime.slots.acquire().is_some());
        assert!(runtime.slots.acquire().is_some());
    }

    // Real end-to-end run against the authenticated provider CLI. Ignored so the
    // offline gate stays deterministic; run manually to demonstrate execution:
    //   <bin> neev::agents::execution::tests::provider_cli_streams_real_output --ignored
    #[tokio::test]
    #[ignore = "spawns the real authenticated claude CLI; run manually"]
    async fn provider_cli_streams_real_output() {
        let runtime = ProviderCliRuntime::new("claude", std::env::temp_dir());
        let mut handle = runtime
            .run(RunRequest {
                user_id: 7,
                org_id: 3,
                chat_id: "smoke".into(),
                provider: "claude".into(),
                model: "default".into(),
                prompt: "Reply with exactly one word: PONG".into(),
            })
            .await
            .unwrap();
        let mut text = String::new();
        let mut completed = false;
        let mut started = false;
        while let Some(event) = handle.next().await {
            match event {
                RunEvent::Started(_) => started = true,
                RunEvent::Chunk(chunk) => text.push_str(&chunk),
                RunEvent::Completed => completed = true,
                RunEvent::Failed(reason) => panic!("real run failed: {reason}"),
            }
        }
        assert!(started, "no Started event");
        assert!(completed, "run did not complete");
        assert!(
            text.to_uppercase().contains("PONG"),
            "unexpected agent output: {text}"
        );
    }
}
