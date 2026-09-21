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
    /// The requested provider has no binary bound in this deployment. Distinct
    /// from `Provider` (a real launch failure) so the UI can say "this provider
    /// is not available here" instead of implying the run was attempted — and
    /// so we never quietly run one provider for another's selection.
    Unsupported,
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
    /// The caller's own per-user, per-provider credential volume handle (the
    /// `storage_handle` of their connected `neev_agent_connections` row).
    ///
    /// This is how §5.4 is enforced at the execution boundary: the sandboxed
    /// runtime REFUSES to launch without one, so a run can never fall back to a
    /// shared host login or to whatever identity the server process happens to
    /// carry. `None` means "this user has not connected this provider".
    pub(crate) credential_handle: Option<String>,
    /// Standing instructions plus the workspace context this run is entitled
    /// to (see `agents::briefing`). Delivered as a SYSTEM prompt, never folded
    /// into the user's text, so the model can tell the two apart and the
    /// stored turn stays exactly what the user typed.
    pub(crate) briefing: Option<String>,
}

/// A credential handle is 32 hex characters (`neev_credential_store` handle
/// length), matching the `storage_handle` column's constraint. Validated before
/// it can reach an argv.
pub(crate) fn valid_credential_handle(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// A model name safe to place in an argv: bounded, and free of anything that
/// could be read as a flag or a separator.
///
/// `sessions::model` applies the SAME rule at creation time, so a session can
/// never be stored with a model every one of its runs would refuse.
pub(crate) fn safe_model(value: &str) -> bool {
    (1..=80).contains(&value.len())
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_'))
}

/// The provider binaries this deployment actually has. A provider with no
/// binary is [`ExecutionError::Unsupported`] — never silently served by another.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProviderBinaries {
    pub(crate) claude: Option<String>,
    pub(crate) codex: Option<String>,
}

impl ProviderBinaries {
    pub(crate) fn claude(binary: impl Into<String>) -> Self {
        Self {
            claude: Some(binary.into()),
            codex: None,
        }
    }

    fn resolve(&self, provider: &str) -> Option<&str> {
        match provider {
            "claude" => self.claude.as_deref(),
            "codex" => self.codex.as_deref(),
            _ => None,
        }
    }
}

/// Streamed lifecycle of a run, mapped from ACP session updates by a real adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunEvent {
    Started(String),
    Chunk(String),
    /// One recorded step of the provider's work — a tool call, its result, or a
    /// status marker. Carried beside the text rather than inside it so the
    /// reply stays the reply.
    Activity(super::activity::ActivityDraft),
    Failed(String),
    Completed,
}

/// The provider's own arguments: prompt, streaming JSON, model.
///
/// Each CLI is asked in ITS OWN dialect. Both were previously given Claude's
/// flags, which Codex does not accept at all — its non-interactive entry point
/// is a subcommand (`exec`) and its streaming switch is `--json`, verified
/// against `codex exec --help` in the deployed agent image. An unknown provider
/// gets Claude's form, which is what `resolve` already restricts the binary to.
fn provider_argv(
    provider: &str,
    prompt: &str,
    model: &str,
    briefing: Option<&str>,
) -> Vec<String> {
    let mut argv: Vec<String> = match provider {
        "codex" => vec![
            "exec".into(),
            // JSONL events on stdout; the parser reads the `item.*` envelopes.
            "--json".into(),
            prompt.to_owned(),
        ],
        _ => vec![
            "-p".into(),
            prompt.to_owned(),
            "--output-format".into(),
            "stream-json".into(),
            "--include-partial-messages".into(),
            "--verbose".into(),
        ],
    };
    // Claude takes real system instructions. Codex `exec` has no equivalent
    // switch, so its briefing is prepended to the prompt inside a delimiter —
    // clearly marked as context rather than as something the user said.
    if let Some(briefing) = briefing.filter(|text| !text.is_empty()) {
        match provider {
            "codex" => {
                let last = argv.len() - 1;
                argv[last] = format!("<workspace-context>\n{briefing}\n</workspace-context>\n\n{prompt}");
            }
            _ => {
                argv.push("--append-system-prompt".into());
                argv.push(briefing.to_owned());
            }
        }
    }
    // An explicitly chosen model is passed through, so the UI's selection is the
    // one the provider actually runs. Codex spells it `-m`.
    if model != "default" {
        argv.push(if provider == "codex" { "-m".into() } else { "--model".into() });
        argv.push(model.to_owned());
    }
    argv
}

/// Forward one mapped stream item to the run's consumer.
///
/// Empty text is dropped rather than sent: a zero-length delta would spend a
/// broadcast and a database write to say nothing.
async fn send_item(
    tx: &mpsc::Sender<RunEvent>,
    item: super::activity::StreamItem,
) -> Result<(), ()> {
    let event = match item {
        super::activity::StreamItem::Text(text) => {
            if text.is_empty() {
                return Ok(());
            }
            RunEvent::Chunk(text)
        }
        super::activity::StreamItem::Activity(draft) => RunEvent::Activity(draft),
    };
    tx.send(event).await.map_err(|_| ())
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
        // Same model contract as the sandboxed runtime, so dev and production
        // cannot disagree about what "default" means.
        if request.model != "default" && !safe_model(&request.model) {
            return Err(ExecutionError::Unauthorized);
        }
        let mut command = Command::new(&self.binary);
        // Shares `provider_argv` with the sandboxed runtime, so dev cannot drift
        // into invoking a provider differently from production.
        command.args(provider_argv(
            &request.provider,
            &request.prompt,
            &request.model,
            request.briefing.as_deref(),
        ));
        let mut child = command
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
        // Built before the request moves. The handle is what names this user's
        // credential volume, so it is scrubbed out of anything a trace records.
        let mut parser = super::activity::StreamParser::new(
            &request.provider,
            request.credential_handle.iter().cloned().collect(),
        );
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
                            for event in parser.push(&line) {
                                if send_item(&tx, event).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Ok(None) => {
                            // A tool whose block never closed still ran.
                            for event in parser.finish() {
                                if send_item(&tx, event).await.is_err() {
                                    return;
                                }
                            }
                            break;
                        }
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

/// RAII guard: removes the ephemeral per-run workdir and — **only if that
/// succeeded** — returns the lane to the pool, on every exit path including a
/// panic.
///
/// The conditional release is the isolation guarantee. The launcher chowns the
/// run directory to the dropped run uid at mode 0700, so the unprivileged web
/// server frequently CANNOT delete it. Releasing the lane anyway would hand the
/// same uid to the next tenant while the previous tenant's private files were
/// still sitting in a directory that uid owns — a cross-tenant read. So a lane
/// whose workdir could not be removed is **quarantined**: it never re-enters the
/// pool, the run count simply drops, and a privileged sweep (startup, or the
/// supervisor's own teardown) is what reclaims it.
struct LaneGuard {
    slots: SlotPool,
    lane: Lane,
    workdir: PathBuf,
}

impl Drop for LaneGuard {
    fn drop(&mut self) {
        if workdir_is_clear(&self.workdir) {
            self.slots.release(self.lane);
        }
        // Otherwise: quarantined. Losing a lane costs throughput; reusing a uid
        // over another tenant's readable files costs isolation.
    }
}

/// Whether the run directory is genuinely gone. Removal succeeding and the
/// directory never having existed are both fine; anything else (permission
/// denied because the run uid owns it, a busy mount) is not.
fn workdir_is_clear(workdir: &Path) -> bool {
    match std::fs::remove_dir_all(workdir) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => !workdir.exists(),
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
    binaries: ProviderBinaries,
    workspace_root: PathBuf,
    provider_ipv4: Vec<String>,
    /// The fixed-command privileged launch helper (`neev-agent-caplaunch`,
    /// installed with file caps). When set, the (unprivileged) server execs THIS
    /// instead of `python <launcher>`; the helper raises the ambient caps and
    /// execs the launcher itself. This is what keeps the web server unprivileged
    /// (deploy review C1). `None` = direct exec (dev/test only, needs an already
    /// privileged context).
    cap_helper: Option<String>,
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
        binaries: ProviderBinaries,
        workspace_root: impl Into<PathBuf>,
        provider_ipv4: Vec<String>,
        cap_helper: Option<String>,
    ) -> Self {
        Self {
            python: python.into(),
            launcher: launcher.into(),
            binaries,
            workspace_root: workspace_root.into(),
            provider_ipv4,
            cap_helper,
            slots: SlotPool::new(),
        }
    }

    /// Validate a request against what this deployment can actually do, before
    /// any lane, workdir or process exists. Returns the resolved provider binary.
    ///
    /// The credential handle is required here, not optional: that is the point
    /// of §5.4. Without the caller's OWN per-user volume there is nothing to
    /// deliver as the run's HOME, and launching anyway would silently reuse
    /// whatever identity the host has — exactly the shared-login shortcut the
    /// spec rejects.
    fn resolve(&self, request: &RunRequest) -> Result<(String, String), ExecutionError> {
        if request.user_id <= 0 || request.org_id <= 0 || request.prompt.trim().is_empty() {
            return Err(ExecutionError::Unauthorized);
        }
        let binary = self
            .binaries
            .resolve(&request.provider)
            .ok_or(ExecutionError::Unsupported)?;
        let handle = request
            .credential_handle
            .as_deref()
            .ok_or(ExecutionError::Unauthorized)?;
        if !valid_credential_handle(handle) {
            return Err(ExecutionError::Unauthorized);
        }
        // `default` means "the provider's own default", expressed by passing no
        // `--model` at all rather than by inventing a name here.
        if request.model != "default" && !safe_model(&request.model) {
            return Err(ExecutionError::Unauthorized);
        }
        Ok((binary.to_owned(), handle.to_owned()))
    }

    /// The launcher's own arguments (everything the launcher parses); the provider
    /// binary only ever appears AFTER the `--` sandbox separator.
    fn launcher_args(
        &self,
        request: &RunRequest,
        lane: Lane,
        workdir: &Path,
        binary: &str,
        handle: &str,
    ) -> Vec<String> {
        let mut argv = vec![
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
            // Which per-user credential volume to open and mount as the run's
            // HOME. The launcher derives the scope from `--tenant` + this
            // provider, so a handle alone cannot reach another tenant's volume.
            "--provider".into(),
            request.provider.clone(),
            "--credential-handle".into(),
            handle.to_owned(),
        ];
        for ip in &self.provider_ipv4 {
            argv.push("--provider-ipv4".into());
            argv.push(ip.clone());
        }
        argv.push("--".into());
        argv.push(binary.to_owned());
        argv.extend(provider_argv(
            &request.provider,
            &request.prompt,
            &request.model,
            request.briefing.as_deref(),
        ));
        argv
    }

    /// The full spawn argv: the privileged cap-helper (production, keeps the
    /// server unprivileged) OR a direct `python <launcher>` (dev/test) — followed
    /// by the launcher's args. Pure; unit-tested.
    fn launch_argv(
        &self,
        request: &RunRequest,
        lane: Lane,
        workdir: &Path,
        binary: &str,
        handle: &str,
    ) -> Vec<String> {
        let mut argv = match &self.cap_helper {
            Some(helper) => vec![helper.clone()],
            None => vec![
                self.python.clone(),
                self.launcher.to_string_lossy().into_owned(),
            ],
        };
        argv.extend(self.launcher_args(request, lane, workdir, binary, handle));
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
        // Everything that can be refused is refused BEFORE a lane, a workdir or
        // a process exists: an unsupported provider or a caller with no
        // credential of their own never consumes an isolation slot.
        let (binary, handle) = self.resolve(&request)?;
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
        let argv = self.launch_argv(&request, lane, &workdir, &binary, &handle);
        let mut child = match Command::new(&argv[0])
            .args(&argv[1..])
            // Defence in depth for §5.4: the server's own HOME/config must not
            // be inheritable by anything downstream. The launcher mounts the
            // caller's credential volume and sets the real HOME after dropping
            // privileges, so clearing these here can only remove an accident.
            .env_remove("HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
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
        // The credential handle names this user's volume; scrub it from traces.
        let mut parser = super::activity::StreamParser::new(
            &request.provider,
            request.credential_handle.iter().cloned().collect(),
        );
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
                            for event in parser.push(&line) {
                                if send_item(&tx, event).await.is_err() {
                                    return true; // receiver gone -> stop launcher
                                }
                            }
                        }
                        Ok(None) => {
                            // A tool whose block never closed still ran.
                            for event in parser.finish() {
                                if send_item(&tx, event).await.is_err() {
                                    return true;
                                }
                            }
                            break;
                        }
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn sandboxed_runtime(
    python: impl Into<String>,
    launcher: impl Into<PathBuf>,
    binaries: ProviderBinaries,
    workspace_root: impl Into<PathBuf>,
    provider_ipv4: Vec<String>,
    cap_helper: Option<String>,
) -> std::sync::Arc<AgentRuntime> {
    std::sync::Arc::new(AgentRuntime::Sandboxed(SandboxedProviderCliRuntime::new(
        python,
        launcher,
        binaries,
        workspace_root,
        provider_ipv4,
        cap_helper,
    )))
}

pub(crate) fn new_job_registry() -> JobRegistry {
    std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HANDLE: &str = "0123456789abcdef0123456789abcdef";

    /// A well-formed request: a supported provider, the provider's own default
    /// model, and the caller's own credential handle.
    fn request(prompt: &str) -> RunRequest {
        RunRequest {
            user_id: 7,
            org_id: 3,
            chat_id: "chat-1".into(),
            provider: "claude".into(),
            model: "default".into(),
            prompt: prompt.into(),
            credential_handle: Some(HANDLE.into()),
            briefing: None,
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
        use super::super::activity::{StreamItem, StreamParser};
        let mut parser = StreamParser::new("claude", Vec::new());
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"PONG"}}}"#;
        assert_eq!(parser.push(line), vec![StreamItem::Text("PONG".into())]);
        // An init line is now a status marker rather than nothing, which is the
        // point of the change: the model that answered is part of the trace.
        assert!(matches!(
            parser
                .push(r#"{"type":"system","subtype":"init","model":"m"}"#)
                .as_slice(),
            [StreamItem::Activity(_)]
        ));
        // Provider chatter that is not JSON is still not content.
        assert!(parser.push("not json").is_empty());
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
            ProviderBinaries::claude("/usr/local/bin/claude"),
            "/data/agents",
            vec!["140.82.112.3".into()],
            None, // direct exec (dev/test)
        )
    }

    #[test]
    fn sandboxed_uses_cap_helper_prefix_when_set() {
        let runtime = SandboxedProviderCliRuntime::new(
            "python3",
            "/opt/neev/scripts/operations/neev_agent_launch.py",
            ProviderBinaries::claude("/usr/local/bin/claude"),
            "/data/agents",
            vec!["140.82.112.3".into()],
            Some("/opt/neev/bin/neev-agent-caplaunch".into()),
        );
        let lane = Lane { slot: "a", uid: 61000 };
        let argv = runtime.launch_argv(&request("hi"), lane, Path::new("/data/agents/run-x"), "/usr/local/bin/claude", HANDLE);
        // The privileged fixed-command helper is argv[0] — NOT python and NOT the
        // provider; the server execs only the helper (keeps itself unprivileged).
        assert_eq!(argv[0], "/opt/neev/bin/neev-agent-caplaunch");
        assert!(!argv.contains(&"python3".to_string()));
        assert!(argv.contains(&"--tenant".to_string()));
        let sep = argv.iter().position(|a| a == "--").expect("has -- separator");
        assert!(!argv[..sep].contains(&"/usr/local/bin/claude".to_string()));
        assert_eq!(argv[sep + 1], "/usr/local/bin/claude");
    }

    #[test]
    fn sandboxed_launch_argv_goes_through_the_launcher_not_bare_provider() {
        let runtime = sandboxed();
        let lane = Lane { slot: "a", uid: 61000 };
        let workdir = Path::new("/var/lib/neev-native/agents/run-abc");
        let argv = runtime.launch_argv(&request("draft"), lane, workdir, "/usr/local/bin/claude", HANDLE);
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
        assert!(!argv[..sep].contains(&"/usr/local/bin/claude".to_string()));
        assert_eq!(argv[sep + 1], "/usr/local/bin/claude");
        assert_eq!(&argv[sep + 2..sep + 4], &["-p".to_string(), "draft".to_string()]);
        assert!(argv.contains(&"stream-json".to_string()));
    }

    #[test]
    fn each_provider_is_invoked_in_its_own_dialect() {
        // Claude Code is headless via `-p` with a streaming output format.
        let claude = provider_argv("claude", "draft", "default", None);
        assert_eq!(&claude[..2], &["-p".to_string(), "draft".to_string()]);
        assert!(claude.contains(&"stream-json".to_string()));
        assert!(claude.contains(&"--include-partial-messages".to_string()));

        // Codex is a SUBCOMMAND with `--json`, verified against `codex exec
        // --help` in the deployed agent image. It rejects Claude's flags
        // outright, which is why sharing them produced no output at all.
        let codex = provider_argv("codex", "draft", "default", None);
        assert_eq!(
            codex,
            vec![
                "exec".to_string(),
                "--json".to_string(),
                "draft".to_string()
            ]
        );
        assert!(!codex.contains(&"-p".to_string()));
        assert!(!codex.contains(&"--output-format".to_string()));

        // Model selection is spelled differently by each.
        assert!(provider_argv("codex", "d", "gpt-5", None)
            .windows(2)
            .any(|pair| pair == ["-m".to_string(), "gpt-5".to_string()]));
        assert!(provider_argv("claude", "d", "opus", None)
            .windows(2)
            .any(|pair| pair == ["--model".to_string(), "opus".to_string()]));
        // "default" means the provider's own default: nothing is passed.
        assert!(!provider_argv("codex", "d", "default", None).contains(&"-m".to_string()));
    }

    #[test]
    fn sandboxed_deny_all_when_no_provider_ip() {
        let runtime = SandboxedProviderCliRuntime::new(
            "python3", "/l/launch.py", ProviderBinaries::claude("/usr/local/bin/claude"), "/w", vec![], None,
        );
        let lane = Lane { slot: "b", uid: 61001 };
        let argv = runtime.launch_argv(&request("hi"), lane, Path::new("/w/run-x"), "/usr/local/bin/claude", HANDLE);
        assert!(!argv.contains(&"--provider-ipv4".to_string()));
    }

    #[tokio::test]
    async fn an_unbound_provider_is_refused_not_served_by_another_binary() {
        // Only Claude is bound in this deployment.
        let runtime = sandboxed();
        let mut codex = request("hi");
        codex.provider = "codex".into();
        assert_eq!(
            runtime.run(codex).await.err(),
            Some(ExecutionError::Unsupported),
            "a Codex selection must never be served by the Claude binary"
        );
        // Refused before any isolation lane was consumed.
        assert!(runtime.slots.acquire().is_some());
        assert!(runtime.slots.acquire().is_some());

        // With a Codex binary bound, the SAME request routes to that binary.
        let both = SandboxedProviderCliRuntime::new(
            "python3",
            "/l/launch.py",
            ProviderBinaries {
                claude: Some("/usr/local/bin/claude".into()),
                codex: Some("/usr/local/bin/codex".into()),
            },
            "/w",
            vec![],
            None,
        );
        let mut codex = request("hi");
        codex.provider = "codex".into();
        let (binary, _) = both.resolve(&codex).expect("codex resolves");
        assert_eq!(binary, "/usr/local/bin/codex");
        let argv = both.launch_argv(
            &codex,
            Lane { slot: "a", uid: 61000 },
            Path::new("/w/run-x"),
            &binary,
            HANDLE,
        );
        let sep = argv.iter().position(|a| a == "--").expect("separator");
        assert_eq!(argv[sep + 1], "/usr/local/bin/codex");
        // The launcher is told which provider scope to open credentials for.
        assert!(argv.contains(&"--provider".to_string()));
        assert!(argv.contains(&"codex".to_string()));
    }

    #[tokio::test]
    async fn a_run_without_the_caller_s_own_credential_is_refused() {
        let runtime = sandboxed();
        let mut anonymous = request("hi");
        anonymous.credential_handle = None;
        // No per-user credential means no identity to deliver. Refusing here is
        // what stops a run from silently inheriting a shared host login (§5.4).
        assert_eq!(
            runtime.run(anonymous).await.err(),
            Some(ExecutionError::Unauthorized)
        );
        // A malformed handle is refused too — it never reaches an argv.
        for bad in ["", "short", "../../etc/passwd", &"z".repeat(32)] {
            let mut request = request("hi");
            request.credential_handle = Some(bad.to_owned());
            assert_eq!(
                runtime.run(request).await.err(),
                Some(ExecutionError::Unauthorized),
                "accepted handle {bad:?}"
            );
        }
        // And no lane was consumed by any of those refusals.
        assert!(runtime.slots.acquire().is_some());
        assert!(runtime.slots.acquire().is_some());
    }

    #[test]
    fn the_credential_handle_is_passed_to_the_launcher_not_the_provider() {
        let runtime = sandboxed();
        let argv = runtime.launch_argv(
            &request("hi"),
            Lane { slot: "a", uid: 61000 },
            Path::new("/data/agents/run-x"),
            "/usr/local/bin/claude",
            HANDLE,
        );
        let sep = argv.iter().position(|a| a == "--").expect("separator");
        // The handle is a LAUNCHER argument: the privileged side opens the
        // volume and mounts it as HOME. The provider argv never sees it.
        assert!(argv[..sep].contains(&"--credential-handle".to_string()));
        assert!(argv[..sep].contains(&HANDLE.to_string()));
        assert!(!argv[sep..].contains(&HANDLE.to_string()));
        assert!(argv[..sep].contains(&"--provider".to_string()));
        assert!(argv[..sep].contains(&"claude".to_string()));
    }

    #[test]
    fn an_explicit_model_reaches_the_provider_and_default_means_no_flag() {
        let runtime = sandboxed();
        let lane = Lane { slot: "a", uid: 61000 };
        let workdir = Path::new("/w/run-x");

        // "default" is expressed by passing no --model at all, so the provider
        // picks its own; we never invent a model name on the user's behalf.
        let argv = runtime.launch_argv(
            &request("hi"),
            lane,
            workdir,
            "/usr/local/bin/claude",
            HANDLE,
        );
        assert!(!argv.contains(&"--model".to_string()));

        // A chosen model is actually passed through — the UI's selection is what
        // runs, so the session's stored model is not a decorative label.
        let mut chosen = request("hi");
        chosen.model = "claude-opus-4-1".into();
        let argv = runtime.launch_argv(&chosen, lane, workdir, "/usr/local/bin/claude", HANDLE);
        let position = argv
            .iter()
            .position(|arg| arg == "--model")
            .expect("--model present");
        assert_eq!(argv[position + 1], "claude-opus-4-1");
        // It sits in the PROVIDER argv, after the sandbox separator.
        let sep = argv.iter().position(|a| a == "--").expect("separator");
        assert!(position > sep);
    }

    #[tokio::test]
    async fn a_model_that_could_be_read_as_a_flag_is_refused() {
        let runtime = sandboxed();
        for bad in ["--dangerously-skip-permissions", "a b", "model;rm", &"m".repeat(81)] {
            let mut request = request("hi");
            request.model = bad.to_owned();
            assert_eq!(
                runtime.run(request).await.err(),
                Some(ExecutionError::Unauthorized),
                "accepted model {bad:?}"
            );
        }
    }

    #[test]
    fn a_lane_whose_workdir_survives_is_quarantined_rather_than_reused() {
        let pool = SlotPool::new();
        let root = tempfile::tempdir().unwrap();
        let clean = root.path().join("run-clean");
        let stuck = root.path().join("run-stuck");
        std::fs::create_dir_all(&clean).unwrap();
        std::fs::create_dir_all(&stuck).unwrap();

        let first = pool.acquire().expect("first lane");
        let second = pool.acquire().expect("second lane");
        assert!(pool.acquire().is_none(), "only two lanes exist");

        // A run whose directory really is gone returns its lane.
        drop(LaneGuard {
            slots: pool.clone(),
            lane: first,
            workdir: clean.clone(),
        });
        assert!(!clean.exists());
        assert_eq!(pool.acquire(), Some(first), "a cleaned lane is reusable");
        pool.release(first);

        // Simulate what production looks like when the run uid owns the
        // directory at 0700 and the unprivileged server cannot remove it: the
        // path is still there after the guard drops.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(stuck.join("credentials"), b"tenant secret").unwrap();
            // Make the PARENT unwritable so the child cannot be unlinked.
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        drop(LaneGuard {
            slots: pool.clone(),
            lane: second,
            workdir: stuck.clone(),
        });
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let blocked = stuck.exists();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            if blocked {
                // The lane must NOT come back: handing this uid to another
                // tenant would give them a directory still holding the previous
                // tenant's files.
                assert_ne!(
                    pool.acquire(),
                    Some(second),
                    "a lane with a surviving workdir was reused"
                );
            }
        }
    }

    #[test]
    fn a_missing_workdir_counts_as_clear() {
        // Nothing to remove is not a failure: the run never wrote anything, or
        // a privileged sweep already reclaimed it.
        let root = tempfile::tempdir().unwrap();
        assert!(workdir_is_clear(&root.path().join("never-existed")));
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
                // Dev/test path only: the unsandboxed runtime uses the operator's
                // own already-authenticated CLI and delivers no credentials.
                credential_handle: None,
                briefing: None,
            })
            .await
            .unwrap();
        let mut text = String::new();
        let mut completed = false;
        let mut started = false;
        while let Some(event) = handle.next().await {
            match event {
                RunEvent::Started(_) => started = true,
                RunEvent::Activity(_) => {},
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
