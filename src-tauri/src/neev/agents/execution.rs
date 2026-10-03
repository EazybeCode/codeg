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
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
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
    /// Not enough isolation lanes are free for this run (`min_free_lanes`).
    /// Distinct from `Unavailable` (a runtime fault) so a caller that can wait
    /// for capacity — an org-knowledge batch — never mistakes a broken runtime
    /// for a busy one. Stored on chat turns exactly as `unavailable` was.
    LanesBusy,
    /// The requested provider has no binary bound in this deployment. Distinct
    /// from `Provider` (a real launch failure) so the UI can say "this provider
    /// is not available here" instead of implying the run was attempted — and
    /// so we never quietly run one provider for another's selection.
    Unsupported,
}

/// An author-scoped request to run one prompt in one chat with a connected model.
#[derive(Debug, Clone)]
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
    /// Reasoning effort, already validated against what THIS provider's CLI
    /// accepts. `None` passes no flag at all, leaving the provider on its own
    /// default rather than on one we chose for the reader.
    pub(crate) effort: Option<String>,
    /// Run in the provider's fast mode. Already validated against the chosen
    /// MODEL, because Claude's Fast is Opus-only and switching the model to
    /// satisfy a toggle would answer a question the reader did not ask.
    pub(crate) fast: bool,
    /// Files the user attached to this turn, as bytes.
    ///
    /// The runtime stages them into its own working directory before launching,
    /// because only it knows where that directory is. A run that cannot stage
    /// what it was given fails rather than proceeding without the files.
    pub(crate) attachments: Vec<RunAttachment>,
    /// Files the SERVER composed for this run: the complete transcript of the
    /// conversation, and the files sent earlier in it (§ `agents::transcript`).
    ///
    /// Staged in their OWN directory, never mixed with `attachments`, and never
    /// offered as native provider inputs. Keeping the two sets apart is what
    /// lets the prompt say "the reader just sent you these" about one of them
    /// and mean it — and what keeps a name shared between a new upload and an
    /// old one from resolving to the wrong bytes.
    pub(crate) context_files: Vec<RunAttachment>,
    /// Enabled library items (skills and MCP configs) for the caller's org.
    ///
    /// Loaded BEFORE the pending turn is created so a failure can be reported
    /// honestly. Skills are staged as files the agent can read and reference;
    /// MCP servers are injected into the provider's configuration.
    pub(crate) library_items: Vec<super::library_runtime::LibraryRunItem>,
    /// Lanes that must be free for this run to start, checked atomically with
    /// taking one. Ordinary runs use 1; an org-knowledge batch uses 2, so it
    /// never takes the last lane from interactive chats.
    pub(crate) min_free_lanes: usize,
}

/// The most text a tool-free run may return, whatever the caller asks for.
pub(crate) const TOOL_FREE_MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// A supervisor (planner/reviewer) turn: text in, bounded text out, under the
/// caller's OWN subscription credential, with no tools at all.
///
/// A separate type rather than a flag on [`RunRequest`] so that it cannot carry
/// what a tool-free run must not have: there is no field for attachments,
/// context files, library items (skills, MCP), speed or steering. Enforcement
/// is at the provider boundary (`neev_agent_launch.tool_free_argv` and the
/// Claude adapter's `TOOL_FREE_ARGS` + `system/init` check); [`text_only`] is a
/// second line, not the protection. Only providers whose CLI can be told to
/// declare no tools are supported ([`tool_free_supported`]); every other one is
/// [`ExecutionError::Unsupported`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolFreeRequest {
    pub(crate) user_id: i32,
    pub(crate) org_id: i32,
    pub(crate) chat_id: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) effort: Option<String>,
    pub(crate) credential_handle: Option<String>,
    /// Our fixed supervisor instructions, delivered as the system prompt.
    pub(crate) instructions: String,
    /// The bundle to plan or review, inline. Never a path.
    pub(crate) prompt: String,
    /// 1..=[`TOOL_FREE_MAX_OUTPUT_BYTES`]; anything else is refused.
    pub(crate) max_output_bytes: usize,
}

impl ToolFreeRequest {
    /// The ordinary request shape the launch path builds argv from, with every
    /// side channel empty by construction.
    fn as_run_request(&self) -> RunRequest {
        RunRequest {
            user_id: self.user_id,
            org_id: self.org_id,
            chat_id: self.chat_id.clone(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            prompt: self.prompt.clone(),
            credential_handle: self.credential_handle.clone(),
            briefing: Some(self.instructions.clone()).filter(|text| !text.is_empty()),
            effort: self.effort.clone(),
            fast: false,
            attachments: Vec::new(),
            context_files: Vec::new(),
            library_items: Vec::new(),
            min_free_lanes: 1,
        }
    }
}

/// Whether this provider has an enforced tool-free mode. Claude only: Codex's
/// pinned app-server has no way to start a thread without tools (its models are
/// `code_mode_only`, and a read-only sandbox would only make tools fail inside
/// ours), and ACP agents expose no such switch.
pub(crate) fn tool_free_supported(provider: &str) -> bool {
    provider == "claude"
}

fn validate_tool_free(request: &ToolFreeRequest) -> Result<(), ExecutionError> {
    if !tool_free_supported(&request.provider) {
        return Err(ExecutionError::Unsupported);
    }
    if !(1..=TOOL_FREE_MAX_OUTPUT_BYTES).contains(&request.max_output_bytes) {
        return Err(ExecutionError::Unauthorized);
    }
    Ok(())
}

/// How the launcher must constrain a run's tools. `Standard` adds nothing to
/// the argv, so every existing launch is unchanged byte for byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolPolicy {
    Standard,
    ToolFree,
}

/// One file the user attached: its bytes, and enough to name and classify it.
///
/// Bytes rather than a path, because only the runtime knows where this run's
/// working directory is — the HTTP layer must not guess it, and a path chosen
/// before the workdir exists would be a path to nowhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunAttachment {
    /// The attachment's storage key, used to make its staged leaf unique.
    pub(crate) key: String,
    pub(crate) file_name: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) is_image: bool,
}

/// One staged file: where it ended up, and what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StagedAttachment {
    pub(crate) path: String,
    pub(crate) file_name: String,
    pub(crate) is_image: bool,
}

/// The directory attachments are staged into, under the run's own workdir.
///
/// Its own leaf, never the workdir root: a file called `AGENTS.md` sitting at
/// the root of a working directory is configuration that the agent will obey,
/// and an upload must never become that. In here it is just a file the reader
/// gave us, which the agent may open if the prompt points at it — and the leaf
/// names are key-prefixed, so it is not even called `AGENTS.md` on disk.
const ATTACHMENT_DIR: &str = "neev-attachments";

/// Where server-composed context is staged: the conversation transcript and the
/// files earlier turns of the same conversation carried.
///
/// A DIFFERENT leaf from `ATTACHMENT_DIR` on purpose. These are not uploads the
/// reader just made, and the two sets must be distinguishable by path alone —
/// both for the prompt, which describes them differently, and for a reviewer
/// reading a workdir. It is equally not the workdir root, for the same reason
/// attachments are not: a file at the root of a working directory is
/// configuration an agent will obey.
const CONTEXT_DIR: &str = "neev-context";

/// Write this turn's attachments into `workdir` and return their paths.
///
/// The directory is **0755** and the files are 0644, both owned by the server
/// uid that writes them. That looks permissive and is not: the run root above
/// it is 0700 and owned by the dropped run uid, so the only uids that can reach
/// this path at all are that run's uid and root.
///
/// It cannot be 0700. This directory is created by the SERVER uid, while the
/// process that must read it runs as the dropped run uid — a 0700 directory
/// owned by the server would block traversal entirely, and the 0644 files
/// inside would be unreachable no matter how open their own mode was. The
/// alternative is chowning the child in the privileged launcher; making the
/// parent the boundary is the smaller mechanism.
///
/// Every write is `O_CREAT|O_EXCL|O_NOFOLLOW` on a path built from the workdir
/// plus ONE already-validated name component. A name that is not a plain single
/// segment was refused at upload; this refuses again rather than trusting that.
///
/// The leaf is `<key>--<name>`, truncated to fit. Two files called `notes.txt`
/// in one turn would otherwise collide, and `O_EXCL` would fail the whole run;
/// the prefix also means a file called `AGENTS.md` cannot land under a name
/// anything reads as configuration. The original name is kept in the metadata
/// for display, so truncation never reaches the reader.
#[cfg(unix)]
fn stage_files(
    workdir: &Path,
    root_name: &str,
    attachments: &[RunAttachment],
) -> Result<Vec<StagedAttachment>, ExecutionError> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if attachments.is_empty() {
        return Ok(Vec::new());
    }
    // The stable root. The sandboxed runtime gets a fresh workdir per run, so
    // this is normally created once — but the dev runtime reuses one workspace
    // for every run, and a plain `create_dir` failed on the second. Tolerating
    // `AlreadyExists` alone would be worse: the existing entry could be a
    // symlink pointing anywhere. So an existing entry is ACCEPTED only after
    // `symlink_metadata` proves it is a real directory, which never follows it.
    let root = workdir.join(root_name);
    match std::fs::create_dir(&root) {
        Ok(()) => {
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))
                .map_err(|_| ExecutionError::Unavailable)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing =
                std::fs::symlink_metadata(&root).map_err(|_| ExecutionError::Unavailable)?;
            if !existing.is_dir() || existing.file_type().is_symlink() {
                return Err(ExecutionError::Unauthorized);
            }
        }
        Err(_) => return Err(ExecutionError::Unavailable),
    }
    // One child per call, so two runs sharing a workspace cannot collide and
    // neither can see the other's files by guessing a name. `create_dir` on a
    // fresh random name fails rather than reusing anything that is already
    // there, symlink or not.
    let directory = root.join(uuid::Uuid::new_v4().hyphenated().to_string());
    std::fs::create_dir(&directory).map_err(|_| ExecutionError::Unavailable)?;
    // Explicit, for the same reason as the files: `create_dir` is umask-masked.
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755))
        .map_err(|_| ExecutionError::Unavailable)?;
    let mut staged = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        // Re-prove the name is one ordinary path segment. Anything else — a
        // separator, `..`, an absolute path — is refused rather than repaired.
        let name = std::path::Path::new(&attachment.file_name);
        let mut components = name.components();
        let single = matches!(
            (components.next(), components.next()),
            (Some(std::path::Component::Normal(_)), None)
        );
        if !single || attachment.file_name.starts_with('.') {
            return Err(ExecutionError::Unauthorized);
        }
        let path = directory.join(staged_leaf(&attachment.key, &attachment.file_name));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            // A REQUEST, not a guarantee: the creating mode is masked by the
            // process umask, so a server running at 077 would land this at 0600
            // and the dropped run uid could not read its own attachment. The
            // explicit set below is what actually decides the mode.
            .mode(0o644)
            .open(&path)
            .map_err(|_| ExecutionError::Unavailable)?;
        use std::io::Write;
        file.write_all(&attachment.bytes)
            .map_err(|_| ExecutionError::Unavailable)?;
        file.sync_all().map_err(|_| ExecutionError::Unavailable)?;
        // Set, not requested, and after creation so no umask applies. Safe to
        // widen here: the file already exists at a path only this run's uid and
        // root can reach, because the run root above is 0700 and theirs.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .map_err(|_| ExecutionError::Unavailable)?;
        staged.push(StagedAttachment {
            path: path.to_string_lossy().into_owned(),
            file_name: attachment.file_name.clone(),
            is_image: attachment.is_image,
        });
    }
    Ok(staged)
}

#[cfg(not(unix))]
fn stage_files(
    _workdir: &Path,
    _root_name: &str,
    attachments: &[RunAttachment],
) -> Result<Vec<StagedAttachment>, ExecutionError> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }
    Err(ExecutionError::Unavailable)
}

/// This turn's own uploads.
fn stage_attachments(
    workdir: &Path,
    attachments: &[RunAttachment],
) -> Result<Vec<StagedAttachment>, ExecutionError> {
    stage_files(workdir, ATTACHMENT_DIR, attachments)
}

/// The transcript and the conversation's earlier files.
///
/// Failures PROPAGATE, exactly as they do for this turn's uploads. Swallowing
/// them was the tempting shape — context is a nicety, a reply is not — and it is
/// wrong: the briefing has already told the model that a transcript is staged
/// and pointed at it by name. A run that then quietly has no such file is a run
/// whose instructions are false, and the model would report the reader's own
/// conversation as missing. Better to refuse the turn honestly, which the caller
/// records as a visible failure the reader can retry.
fn stage_context(
    workdir: &Path,
    files: &[RunAttachment],
) -> Result<Vec<StagedAttachment>, ExecutionError> {
    stage_files(workdir, CONTEXT_DIR, files)
}

/// The longest leaf a filesystem will take, in BYTES.
///
/// `NAME_MAX` is 255 bytes on Linux, and an attachment name is bounded at 200
/// CHARACTERS — which is up to 800 bytes of UTF-8, before a 36-byte key and a
/// separator are added. Without this the staging of a long non-ASCII name fails
/// with `ENAMETOOLONG` and takes the whole run with it.
const MAX_LEAF_BYTES: usize = 255;

/// `<key>--<name>`, with the name truncated on a character boundary so the leaf
/// fits. The key is never truncated: uniqueness is what the prefix is for.
fn staged_leaf(key: &str, file_name: &str) -> String {
    let prefix = format!("{key}--");
    let budget = MAX_LEAF_BYTES.saturating_sub(prefix.len());
    let mut name = file_name;
    if name.len() > budget {
        // Back off to a character boundary: a leaf cut mid-codepoint would not
        // be valid UTF-8, and the name is only for a human reading a path.
        let mut end = budget;
        while end > 0 && !name.is_char_boundary(end) {
            end -= 1;
        }
        name = &name[..end];
    }
    format!("{prefix}{name}")
}

/// Name the staged files in the prompt, so the agent knows they are there.
///
/// Both providers get this. Codex additionally takes images as native inputs,
/// but neither takes a PDF or a text file that way — those are real files the
/// agent opens with its own read tool, and it cannot open what it was never
/// told about. The paths are appended as a clearly delimited block rather than
/// mixed into the reader's own words.
/// What a turn says when the reader attached files and typed nothing.
///
/// Substituted HERE rather than in the UI, so both providers and both surfaces
/// send the same sentence and an empty prompt can never reach a provider that
/// would refuse it. Sending files with no words is an ordinary thing to do.
pub(crate) const ATTACHMENT_ONLY_PROMPT: &str = "Please review the attached files.";

fn prompt_with_attachments(prompt: &str, staged: &[StagedAttachment]) -> String {
    if staged.is_empty() {
        return prompt.to_owned();
    }
    let prompt = if prompt.trim().is_empty() {
        ATTACHMENT_ONLY_PROMPT
    } else {
        prompt
    };
    let mut block = String::from("<attached-files>\n");
    for item in staged {
        block.push_str(&item.path);
        block.push('\n');
    }
    block.push_str("</attached-files>\n\n");
    format!("{block}{prompt}")
}

/// Forward one mapped stream item to the run's consumer.

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

/// Which Codex transport the sandbox runs.
///
/// `codex exec --json` has no token-level output, so a reply arrives whole when
/// the turn is already over. `AppServer` runs the same binary as an app-server
/// behind an adapter INSIDE the sandbox, which streams the message as it is
/// written. The choice is a deployment flag defaulting to [`CodexStream::Exec`],
/// so production behaviour is unchanged until an operator sets it and reverting
/// is a restart rather than a rebuild.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CodexStream {
    #[default]
    Exec,
    AppServer,
}

impl CodexStream {
    /// Strict: an unrecognised value is `None` so a typo aborts startup rather
    /// than silently selecting a transport. Mirrors `NEEV_RUNTIME_ENABLED`.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "exec" => Some(Self::Exec),
            "app-server" => Some(Self::AppServer),
            _ => None,
        }
    }

    fn as_arg(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::AppServer => "app-server",
        }
    }
}

/// The provider binaries this deployment actually has. A provider with no
/// binary is [`ExecutionError::Unsupported`] — never silently served by another.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProviderBinaries {
    pub(crate) claude: Option<String>,
    pub(crate) codex: Option<String>,
    /// ACP providers. Each must be the image-pinned path the launcher
    /// allowlists (`neev_agent_launch.ACP_AGENT_PINS`); any other path is
    /// refused by the launcher before a process exists.
    pub(crate) cursor: Option<String>,
    pub(crate) gemini: Option<String>,
    pub(crate) opencode: Option<String>,
}

impl ProviderBinaries {
    pub(crate) fn claude(binary: impl Into<String>) -> Self {
        Self {
            claude: Some(binary.into()),
            ..Self::default()
        }
    }

    fn resolve(&self, provider: &str) -> Option<&str> {
        match provider {
            "claude" => self.claude.as_deref(),
            "codex" => self.codex.as_deref(),
            "cursor" => self.cursor.as_deref(),
            "gemini" => self.gemini.as_deref(),
            "opencode" => self.opencode.as_deref(),
            _ => None,
        }
    }
}

/// Refuse what the ACP path cannot honour, so a choice is never silently
/// dropped: ACP runs take the provider's default model and no effort or speed
/// control exists for them (`acp_providers`).
fn acp_request_supported(request: &RunRequest) -> bool {
    request.model == "default"
        && request.effort.as_deref().is_none_or(str::is_empty)
        && !request.fast
}

/// Streamed lifecycle of a run, mapped from ACP session updates by a real adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunEvent {
    Started(String),
    Chunk(String),
    /// A whole message, which starts where the last one ended rather than
    /// running on from it. See [`super::activity::StreamItem::Message`].
    Message(String),
    /// One recorded step of the provider's work — a tool call, its result, or a
    /// status marker. Carried beside the text rather than inside it so the
    /// reply stays the reply.
    Activity(super::activity::ActivityDraft),
    /// Usage evidence for Auto routing (see `routing_usage`). Consumed by the
    /// run driver only: never stored, broadcast or shown.
    Usage(super::activity::UsageSignal),
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
/// `images` are absolute paths ALREADY staged inside the run's sandbox working
/// directory. They are passed as separate argv entries, never interpolated into
/// the prompt or into a shell string, so a file name can carry any character the
/// upload allowed without changing how the command parses.
fn provider_argv(
    provider: &str,
    prompt: &str,
    model: &str,
    briefing: Option<&str>,
    effort: Option<&str>,
    fast: bool,
    images: &[String],
) -> Vec<String> {
    if super::acp_providers::is_acp(provider) {
        // ACP has no system-prompt parameter: the briefing goes in front of the
        // prompt inside a delimiter, as for Codex. The launcher rewrites this
        // canonical shape into the adapter's argv (`acp_stream_argv`); the
        // marker is never passed to a provider. No model, effort, speed or
        // image flag exists on this path (refused earlier, in `resolve`).
        let prompt = match briefing.filter(|text| !text.is_empty()) {
            Some(briefing) => {
                format!("<workspace-context>\n{briefing}\n</workspace-context>\n\n{prompt}")
            }
            None => prompt.to_owned(),
        };
        // Staged images travel as `--image <path>` so the adapter can send them
        // as native ACP image blocks when the agent advertises support (all
        // three pinned agents do); the prompt names them as files regardless.
        let mut argv = vec![super::acp_providers::LAUNCH_MARKER.to_owned()];
        for path in images {
            argv.push("--image".to_owned());
            argv.push(path.clone());
        }
        argv.push("--".to_owned());
        argv.push(prompt);
        return argv;
    }
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
                argv[last] =
                    format!("<workspace-context>\n{briefing}\n</workspace-context>\n\n{prompt}");
            }
            _ => {
                argv.push("--append-system-prompt".into());
                argv.push(briefing.to_owned());
            }
        }
    }
    // Effort, in each CLI's own spelling. Claude gained a native `--effort` in
    // 2.1.267; Codex takes it as a config override. Validation happened before
    // this point on purpose: `claude` does NOT reject an unknown level, it warns
    // and runs at its default, so an unchecked value would look like it worked
    // while quietly ignoring the reader's choice.
    if let Some(effort) = effort.filter(|value| !value.is_empty()) {
        match provider {
            "codex" => {
                argv.push("-c".into());
                argv.push(format!("model_reasoning_effort=\"{effort}\""));
            }
            _ => {
                argv.push("--effort".into());
                argv.push(effort.to_owned());
            }
        }
    }
    // Speed. Neither CLI takes a flag for this, which is why searching `--help`
    // for one finds nothing: it is settings on Claude and config overrides on
    // Codex. Sent EXPLICITLY in both states so a run can never inherit a fast
    // mode from ambient config the reader cannot see.
    match provider {
        "codex" => {
            // Chats run in isolated, per-turn directories, not Git checkouts.
            // This skips only the repository precondition, not the sandbox.
            argv.push("--skip-git-repo-check".into());
            argv.push("-c".into());
            argv.push(format!(
                "service_tier=\"{}\"",
                if fast { "fast" } else { "default" }
            ));
            argv.push("-c".into());
            argv.push(format!("features.fast_mode={fast}"));
        }
        _ => {
            argv.push("--settings".into());
            argv.push(format!("{{\"fastMode\":{fast}}}"));
        }
    }
    // An explicitly chosen model is passed through, so the UI's selection is the
    // one the provider actually runs. Codex spells it `-m`.
    if model != "default" {
        argv.push(if provider == "codex" {
            "-m".into()
        } else {
            "--model".into()
        });
        argv.push(model.to_owned());
    }
    // Images, where the provider takes them natively. Codex `exec` has
    // `-i, --image <FILE>...`; the flag is repeated per file rather than
    // comma-joined so a name containing a comma cannot split into two paths.
    //
    // Claude has no equivalent switch. Its images are staged files like every
    // other attachment and it opens them with its own read tool, which is why
    // the staged paths are named in the prompt for BOTH providers.
    if provider == "codex" {
        for path in images {
            argv.push("--image".into());
            argv.push(path.clone());
        }
    }
    argv
}

/// How many context paths the briefing spells out.
///
/// Every argument in an argv is bounded (`MAX_ARG_STRLEN`, 128 KiB on Linux) and
/// the briefing is ONE argument. A conversation may hold thousands of files, all
/// of them genuinely staged; naming each one would build an argument no kernel
/// will accept, and the exec would fail for the whole run. So the transcript and
/// the first few files are named, the directory is named, and the count is
/// stated — the model can list the directory for the rest, which is true rather
/// than a polite way of hiding them. Matches `transcript::MANIFEST_NAMES`, which
/// bounds the same list on the briefing side.
const CONTEXT_PATHS: usize = super::transcript::MANIFEST_NAMES + 1;

/// Append the staged context paths to the briefing.
///
/// Done HERE and nowhere earlier: the briefing is composed while the run's
/// working directory does not yet exist, so it can name a FILE but never a path.
/// This is the one point where both are known.
fn briefing_with_context(briefing: Option<&str>, staged: &[StagedAttachment]) -> Option<String> {
    if staged.is_empty() {
        return briefing.map(str::to_owned);
    }
    let mut text = briefing.unwrap_or_default().to_owned();
    text.push_str("\n<context-files>\n");
    if let Some(directory) = std::path::Path::new(&staged[0].path)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
    {
        text.push_str(&format!("directory: {directory}\n"));
    }
    for item in staged.iter().take(CONTEXT_PATHS) {
        text.push_str(&item.path);
        text.push('\n');
    }
    if staged.len() > CONTEXT_PATHS {
        text.push_str(&format!(
            "({} more file(s) in that directory, not listed here to keep this short — \
list the directory to see them all.)\n",
            staged.len() - CONTEXT_PATHS
        ));
    }
    text.push_str("</context-files>\n");
    Some(text)
}

/// The staged paths a provider can take as native image inputs.
fn image_paths(staged: &[StagedAttachment]) -> Vec<String> {
    staged
        .iter()
        .filter(|item| item.is_image)
        .map(|item| item.path.clone())
        .collect()
}

/// Combine all briefing elements: context files and library skills.
///
/// The library skills briefing is appended AFTER the context files so the agent
/// sees its workspace first, then learns about shared skills it can invoke.
fn briefing_with_library(
    briefing: Option<String>,
    skills: &[super::library_runtime::StagedSkill],
) -> Option<String> {
    let skills_section = super::library_runtime::skills_briefing(skills);
    match (briefing, skills_section) {
        (Some(base), Some(skills)) => Some(format!("{base}\n{skills}")),
        (Some(base), None) => Some(base),
        (None, Some(skills)) => Some(skills),
        (None, None) => None,
    }
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
        super::activity::StreamItem::Message(text) => {
            if text.is_empty() {
                return Ok(());
            }
            RunEvent::Message(text)
        }
        super::activity::StreamItem::Activity(draft) => RunEvent::Activity(draft),
        super::activity::StreamItem::Usage(signal) => RunEvent::Usage(signal),
    };
    tx.send(event).await.map_err(|_| ())
}

/// A live run: a bounded stream of [`RunEvent`] plus an idempotent cancel that
/// interrupts the turn (never a login/DB effect).
pub(crate) struct RunHandle {
    pub(crate) run_id: String,
    events: mpsc::Receiver<RunEvent>,
    cancel: Option<oneshot::Sender<()>>,
    /// The workspace root where spool directories live. Used by the server to
    /// find generated output files after the run completes. Only set for sandboxed
    /// runtimes that actually produce a spool.
    pub(crate) workspace_root: Option<PathBuf>,
    /// Channel to relay follow-up user input to the adapter for mid-run steering.
    /// `None` for transports that don't support steering (e.g., `codex exec`).
    pub(crate) steer_tx: Option<mpsc::Sender<super::runs::SteerInput>>,
    /// Whether this run's transport supports mid-run steering.
    pub(crate) supports_steering: bool,
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

/// A metadata read, scoped exactly like a run: the caller's own credential
/// volume, inside the same isolation profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveryRequest {
    pub(crate) user_id: i32,
    pub(crate) org_id: i32,
    pub(crate) provider: String,
    pub(crate) credential_handle: Option<String>,
}

/// Bounds on a discovery probe. It talks to a network service on somebody's
/// behalf, so it gets a hard ceiling on time and output and is torn down on
/// every path — including the timeout, which is the one people forget.
const DISCOVERY_TIMEOUT_SECS: u64 = 10;
const DISCOVERY_MAX_BYTES: usize = 256 * 1024;

/// Drive one `model/list` exchange against `codex app-server`.
///
/// The sequence is the one the pinned binary actually requires, confirmed by
/// probing it: `initialize`, WAIT for the reply, then the `initialized`
/// notification, then `model/list`. Queueing all three and closing stdin does
/// not work — the server answers the handshake before accepting a method, and an
/// early EOF ends the session before the catalogue exists. stdin therefore stays
/// open until the answer arrives.
async fn app_server_models(mut command: Command) -> Result<String, ExecutionError> {
    use tokio::io::AsyncWriteExt;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ExecutionError::Provider)?;
    let pid = child.id();
    let mut stdin = child.stdin.take().ok_or(ExecutionError::Unavailable)?;
    let stdout = child.stdout.take().ok_or(ExecutionError::Unavailable)?;

    let exchange = async {
        stdin
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"neev\",\"version\":\"1\"}}}\n",
            )
            .await
            .map_err(|_| ExecutionError::Provider)?;
        stdin.flush().await.map_err(|_| ExecutionError::Provider)?;

        let mut reader = BufReader::new(stdout);
        let mut read_total = 0usize;
        let mut handshaken = false;
        loop {
            // Bounded BEFORE the allocation. `read_line` grows its buffer to
            // whatever the far end sent and only then could a cap look at it,
            // which is not a bound.
            let remaining = DISCOVERY_MAX_BYTES.saturating_sub(read_total);
            if remaining == 0 {
                return Err(ExecutionError::Unavailable);
            }
            let mut line = Vec::new();
            let read = (&mut reader)
                .take(remaining as u64 + 1)
                .read_until(b'\n', &mut line)
                .await
                .map_err(|_| ExecutionError::Provider)?;
            if read == 0 {
                break;
            }
            if line.len() > remaining {
                // One oversized message is enough to distrust the whole answer.
                return Err(ExecutionError::Unavailable);
            }
            read_total += line.len();
            let text = String::from_utf8_lossy(&line).into_owned();
            let Ok(message) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
                continue; // notifications and chatter are not the answer
            };
            let id = message.get("id").and_then(|id| id.as_i64());
            if id == Some(1) && !handshaken {
                handshaken = true;
                stdin
                    .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"initialized\",\"params\":{}}\n")
                    .await
                    .map_err(|_| ExecutionError::Provider)?;
                stdin
                    .write_all(
                        b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"model/list\",\"params\":{}}\n",
                    )
                    .await
                    .map_err(|_| ExecutionError::Provider)?;
                stdin.flush().await.map_err(|_| ExecutionError::Provider)?;
                continue;
            }
            if id == Some(2) {
                // The catalogue, or a refusal of it. Either ends the exchange.
                return Ok(text);
            }
        }
        Err(ExecutionError::Provider)
    };

    let result = match tokio::time::timeout(
        std::time::Duration::from_secs(DISCOVERY_TIMEOUT_SECS),
        exchange,
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(ExecutionError::Unavailable),
    };
    // Teardown. `graceful_stop` is the launcher's own shutdown — SIGTERM, then a
    // bounded wait — which is what lets it unwind its namespace and egress;
    // SIGKILL would reap the parent and leak the netns.
    drop(stdin);
    graceful_stop(&mut child, pid).await;
    result
}

/// The execution port. The real implementation authorizes (user, org, chat),
/// binds an isolated per-tenant workspace, spawns via ACP, and maps its stream.
pub(crate) trait AgentExecutionRuntime: Send + Sync {
    async fn run(&self, request: RunRequest) -> Result<RunHandle, ExecutionError>;

    /// Read the provider's model catalogue under a caller's own credential.
    ///
    /// Metadata only: no turn is started and nothing is spent. Defaults to
    /// `Unsupported` so a runtime that cannot do this says so rather than being
    /// assumed able — the caller then shows the provider default and why.
    async fn discover_models(&self, _request: DiscoveryRequest) -> Result<String, ExecutionError> {
        Err(ExecutionError::Unsupported)
    }

    /// Run one tool-free supervisor turn ([`ToolFreeRequest`]). Defaults to
    /// `Unsupported`: a runtime that cannot enforce it at the provider boundary
    /// declines rather than running an ordinary turn and hoping.
    async fn run_tool_free(&self, _request: ToolFreeRequest) -> Result<RunHandle, ExecutionError> {
        Err(ExecutionError::Unsupported)
    }
}

/// Wrap a tool-free run so its consumer sees text only, bounded.
///
/// Forwards `Started`, `Chunk`/`Message` up to `limit` bytes in total, and the
/// terminal event. A tool call or result ends the run as `Failed("tool_use")`
/// and output past the limit as `Failed("output_limit")`; both cancel the inner
/// run and wait for its teardown. Other activity is dropped. The returned handle
/// has no steering and no spool: whatever the inner run left in its spool is
/// removed with `generated::cleanup_spool`, never read.
///
/// This observes events AFTER the provider emitted them, so it cannot stop a
/// tool that already ran; it exists because the provider-side switches are the
/// enforcement and a second, independent check costs nothing.
fn text_only(mut inner: RunHandle, limit: usize) -> RunHandle {
    let run_id = inner.run_id.clone();
    let spool_root = inner.workspace_root.take();
    let mut inner_cancel = inner.take_canceler();
    drop(inner.take_steer_tx());
    let (tx, rx) = mpsc::channel::<RunEvent>(16);
    let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
    let spool_id = run_id.clone();
    tokio::spawn(async move {
        let mut used = 0usize;
        let stop_reason = loop {
            let event = tokio::select! {
                _ = &mut cancel_rx => break Some("canceled"),
                event = inner.next() => event,
            };
            let Some(event) = event else {
                break Some("provider");
            };
            match event {
                RunEvent::Started(_) => {}
                RunEvent::Chunk(ref text) | RunEvent::Message(ref text) => {
                    used = used.saturating_add(text.len());
                    if used > limit {
                        break Some("output_limit");
                    }
                }
                RunEvent::Activity(ref draft) => match draft.kind {
                    super::activity::ActivityKind::ToolCall { .. }
                    | super::activity::ActivityKind::ToolResult { .. } => break Some("tool_use"),
                    _ => continue,
                },
                // A supervisor's usage is not routing evidence here.
                RunEvent::Usage(_) => continue,
                RunEvent::Failed(_) | RunEvent::Completed => {
                    let _ = tx.send(event).await;
                    break None;
                }
            }
            if tx.send(event).await.is_err() {
                break Some("canceled");
            }
        };
        if let Some(reason) = stop_reason {
            if let Some(cancel) = inner_cancel.take() {
                let _ = cancel.send(());
            }
            // Wait for the inner run's teardown before reporting and cleaning.
            while let Some(event) = inner.next().await {
                if matches!(event, RunEvent::Failed(_) | RunEvent::Completed) {
                    break;
                }
            }
            let _ = tx.send(RunEvent::Failed(reason.into())).await;
        }
        if let Some(root) = spool_root {
            super::generated::cleanup_spool(&root, &spool_id);
        }
    });
    RunHandle {
        run_id,
        events: rx,
        cancel: Some(cancel_tx),
        workspace_root: None,
        steer_tx: None,
        supports_steering: false,
    }
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
    /// Test-only scripting: files the fake "agent" writes to its spool,
    /// computed from the request it received.
    responder: Option<FakeResponder>,
    /// Every request this runtime was asked to run, in order.
    requests: Option<std::sync::Arc<std::sync::Mutex<Vec<RunRequest>>>>,
    /// Honor `min_free_lanes` against a real pool (a lane is held for the
    /// life of the run).
    slots: Option<SlotPool>,
    /// Never complete on its own: run until cancelled.
    hang: bool,
    /// Refuse every run with this error (a runtime fault).
    fail: Option<ExecutionError>,
    /// Every tool-free request this runtime was asked to run, in order.
    tool_free_requests: Option<std::sync::Arc<std::sync::Mutex<Vec<ToolFreeRequest>>>>,
}

/// `(file name, bytes)` outputs for a request.
pub(crate) type FakeOutputs = Vec<(String, Vec<u8>)>;

#[derive(Clone)]
pub(crate) struct FakeResponder {
    root: std::sync::Arc<tempfile::TempDir>,
    make: std::sync::Arc<dyn Fn(&RunRequest) -> FakeOutputs + Send + Sync>,
}

impl std::fmt::Debug for FakeResponder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FakeResponder")
    }
}

impl FakeRuntime {
    pub(crate) fn new(chunks: Vec<String>) -> Self {
        Self {
            chunks,
            responder: None,
            requests: None,
            slots: None,
            hang: false,
            fail: None,
            tool_free_requests: None,
        }
    }

    pub(crate) fn recording_tool_free(
        mut self,
        log: std::sync::Arc<std::sync::Mutex<Vec<ToolFreeRequest>>>,
    ) -> Self {
        self.tool_free_requests = Some(log);
        self
    }

    pub(crate) fn failing(mut self, error: ExecutionError) -> Self {
        self.fail = Some(error);
        self
    }

    /// Write `make(request)` as the run's spool outputs before completing.
    pub(crate) fn with_outputs(
        mut self,
        make: impl Fn(&RunRequest) -> FakeOutputs + Send + Sync + 'static,
    ) -> Self {
        self.responder = Some(FakeResponder {
            root: std::sync::Arc::new(tempfile::tempdir().expect("fake spool root")),
            make: std::sync::Arc::new(make),
        });
        self
    }

    pub(crate) fn recording(mut self, log: std::sync::Arc<std::sync::Mutex<Vec<RunRequest>>>) -> Self {
        self.requests = Some(log);
        self
    }

    pub(crate) fn with_slots(mut self, slots: SlotPool) -> Self {
        self.slots = Some(slots);
        self
    }

    pub(crate) fn hanging(mut self) -> Self {
        self.hang = true;
        self
    }
}

/// Write a spool exactly as the launcher does: `spool/run-<id>/manifest.json`
/// plus `files/<key>--<name>`.
fn write_fake_spool(root: &Path, run_id: &str, outputs: &FakeOutputs) -> std::io::Result<()> {
    use sha2::{Digest, Sha256};
    let spool = root.join("spool").join(format!("run-{run_id}"));
    std::fs::create_dir_all(spool.join("files"))?;
    let mut entries = Vec::new();
    for (name, bytes) in outputs {
        let key = uuid::Uuid::new_v4().to_string();
        std::fs::write(spool.join("files").join(format!("{key}--{name}")), bytes)?;
        entries.push(serde_json::json!({
            "key": key,
            "name": name,
            "byte_length": bytes.len(),
            "sha256": format!("{:x}", Sha256::digest(bytes)),
        }));
    }
    std::fs::write(
        spool.join("manifest.json"),
        serde_json::json!({"run_id": format!("run-{run_id}"), "errors": [], "files": entries}).to_string(),
    )
}

impl AgentExecutionRuntime for FakeRuntime {
    async fn run(&self, request: RunRequest) -> Result<RunHandle, ExecutionError> {
        if request.prompt.trim().is_empty() {
            return Err(ExecutionError::Unauthorized);
        }
        if let Some(log) = &self.requests {
            if let Ok(mut log) = log.lock() {
                log.push(request.clone());
            }
        }
        if let Some(error) = &self.fail {
            return Err(error.clone());
        }
        let lane = match &self.slots {
            Some(slots) => Some((
                slots.clone(),
                slots
                    .acquire_if(request.min_free_lanes)
                    .ok_or(ExecutionError::LanesBusy)?,
            )),
            None => None,
        };
        let run_id = if self.responder.is_some() || self.slots.is_some() || self.hang {
            uuid::Uuid::new_v4().to_string()
        } else {
            format!("run-{}-{}", request.user_id, request.chat_id)
        };
        let workspace_root = match &self.responder {
            Some(responder) => {
                let outputs = (responder.make)(&request);
                write_fake_spool(responder.root.path(), &run_id, &outputs)
                    .map_err(|_| ExecutionError::Unavailable)?;
                Some(responder.root.path().to_path_buf())
            }
            None => None,
        };
        let (tx, rx) = mpsc::channel::<RunEvent>(1);
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        let chunks = self.chunks.clone();
        let started = run_id.clone();
        let hang = self.hang;
        tokio::spawn(async move {
            // Held for the life of the run; released when this task ends.
            let _lane = lane.map(|(slots, lane)| ReleaseOnDrop { slots, lane });
            if tx.send(RunEvent::Started(started)).await.is_err() {
                return;
            }
            let stream = async {
                for chunk in chunks {
                    if tx.send(RunEvent::Chunk(chunk)).await.is_err() {
                        return;
                    }
                }
                if hang {
                    std::future::pending::<()>().await;
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
            workspace_root, // only a scripted FakeRuntime has a spool
            steer_tx: None, // FakeRuntime does not support steering
            supports_steering: false,
        })
    }

    /// Declines exactly what production declines, then streams the scripted
    /// chunks through the same [`text_only`] guard.
    async fn run_tool_free(&self, request: ToolFreeRequest) -> Result<RunHandle, ExecutionError> {
        validate_tool_free(&request)?;
        if let Some(log) = &self.tool_free_requests {
            if let Ok(mut log) = log.lock() {
                log.push(request.clone());
            }
        }
        let limit = request.max_output_bytes;
        Ok(text_only(self.run(request.as_run_request()).await?, limit))
    }
}

/// Returns a fake run's lane when the run ends.
struct ReleaseOnDrop {
    slots: SlotPool,
    lane: Lane,
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.slots.release(self.lane);
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
        let staged = stage_attachments(&self.workspace, &request.attachments)?;
        let context = stage_context(&self.workspace, &request.context_files)?;
        // Stage library items (skills and MCP configs)
        let library =
            super::library_runtime::stage_library(&self.workspace, &request.library_items)?;
        // Combine briefing: context files + library skills
        let briefing = briefing_with_context(request.briefing.as_deref(), &context);
        let briefing = briefing_with_library(briefing, &library.skill_dirs);
        let prompt = prompt_with_attachments(&request.prompt, &staged);
        // Build provider arguments
        let mut argv = provider_argv(
            &request.provider,
            &prompt,
            &request.model,
            briefing.as_deref(),
            request.effort.as_deref(),
            request.fast,
            &image_paths(&staged),
        );
        // Add MCP config for Claude, or overrides for Codex
        if !library.mcp_servers.is_empty() {
            match request.provider.as_str() {
                "claude" => {
                    if let Some(config_path) = super::library_runtime::write_claude_mcp_config(
                        &self.workspace,
                        &library.mcp_servers,
                    )? {
                        argv.push("--mcp-config".into());
                        argv.push(config_path.to_string_lossy().into_owned());
                    }
                }
                "codex" => {
                    for override_str in
                        super::library_runtime::codex_mcp_config(&library.mcp_servers)
                    {
                        argv.push("-c".into());
                        argv.push(override_str);
                    }
                }
                _ => {}
            }
        }
        let mut command = Command::new(&self.binary);
        // Shares `provider_argv` with the sandboxed runtime, so dev cannot drift
        // into invoking a provider differently from production.
        command.args(argv);
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
            workspace_root: Some(self.workspace.clone()), // ProviderCliRuntime workspace
            steer_tx: None, // ProviderCliRuntime is dev-only, no steering yet
            supports_steering: false,
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
#[derive(Clone, Debug)]
pub(crate) struct SlotPool {
    free: std::sync::Arc<std::sync::Mutex<Vec<Lane>>>,
}

impl SlotPool {
    pub(crate) fn new() -> Self {
        Self {
            free: std::sync::Arc::new(std::sync::Mutex::new(vec![
                Lane {
                    slot: "a",
                    uid: 61000,
                },
                Lane {
                    slot: "b",
                    uid: 61001,
                },
            ])),
        }
    }

    fn acquire(&self) -> Option<Lane> {
        self.acquire_if(1)
    }

    /// Take a lane only if at least `min_free` are free, under ONE lock, so two
    /// callers can never both see "two free" and leave none.
    pub(crate) fn acquire_if(&self, min_free: usize) -> Option<Lane> {
        let mut free = self.free.lock().ok()?;
        if free.len() >= min_free.max(1) {
            free.pop()
        } else {
            None
        }
    }

    pub(crate) fn free_count(&self) -> usize {
        self.free.lock().map(|free| free.len()).unwrap_or(0)
    }

    pub(crate) fn release(&self, lane: Lane) {
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
    /// Which Codex transport the launcher should run. Passed on every launch so
    /// the argv says what it is doing rather than depending on the launcher's
    /// own default.
    codex_stream: CodexStream,
    /// Concurrency budget for metadata probes, separate from the run budget.
    ///
    /// Discovery needs an egress lane like a turn does, and there are only two.
    /// Without a cap of its own, several people opening the model menu could
    /// hold both lanes and paid turns would queue behind a dropdown. One probe
    /// at a time means at most one lane is ever spent on metadata.
    probes: Arc<tokio::sync::Semaphore>,
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
        codex_stream: CodexStream,
    ) -> Self {
        Self {
            python: python.into(),
            launcher: launcher.into(),
            binaries,
            workspace_root: workspace_root.into(),
            provider_ipv4,
            cap_helper,
            codex_stream,
            probes: Arc::new(tokio::sync::Semaphore::new(1)),
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
        if super::acp_providers::is_acp(&request.provider) && !acp_request_supported(request) {
            return Err(ExecutionError::Unsupported);
        }
        Ok((binary.to_owned(), handle.to_owned()))
    }

    /// The launcher's own arguments (everything the launcher parses); the provider
    /// binary only ever appears AFTER the `--` sandbox separator.
    ///
    /// The two staged sets arrive as separate parameters rather than one
    /// combined list, because they are not interchangeable: uploads are named in
    /// the PROMPT as files that just arrived, context is named in the BRIEFING as
    /// files from earlier. Merging them to save an argument would erase exactly
    /// the distinction the caller needs to keep.
    #[allow(clippy::too_many_arguments)]
    fn launcher_args(
        &self,
        request: &RunRequest,
        lane: Lane,
        workdir: &Path,
        binary: &str,
        handle: &str,
        staged: &[StagedAttachment],
        context: &[StagedAttachment],
        library: &super::library_runtime::StagedLibrary,
        policy: ToolPolicy,
    ) -> Result<Vec<String>, ExecutionError> {
        let prompt = prompt_with_attachments(&request.prompt, staged);
        // Combine briefing: context files + library skills
        let briefing = briefing_with_context(request.briefing.as_deref(), context);
        let briefing = briefing_with_library(briefing, &library.skill_dirs);
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
            // Which Codex transport to run. Said explicitly on every launch,
            // including Claude's (where the launcher ignores it), so the argv a
            // reviewer reads is the whole instruction.
            "--codex-stream".into(),
            self.codex_stream.as_arg().to_owned(),
        ];
        // Enable steering for providers that support it:
        // - Codex app-server mode: relay JSON {requestId, text} to launcher stdin
        // - Claude: launcher routes through stream-json adapter with interrupt steering
        // The Python launcher handles the provider-specific adapter routing.
        if policy == ToolPolicy::ToolFree {
            // A tool-free turn has nothing to read and nothing to steer; the
            // launcher refuses any shape it cannot prove tool-free for.
            if !staged.is_empty()
                || !context.is_empty()
                || !library.mcp_servers.is_empty()
                || !library.skill_dirs.is_empty()
                || !tool_free_supported(&request.provider)
            {
                return Err(ExecutionError::Unsupported);
            }
            argv.push("--tool-policy".into());
            argv.push("tool-free".into());
        } else if (request.provider == "codex"
            && matches!(self.codex_stream, CodexStream::AppServer))
            || request.provider == "claude"
        {
            argv.push("--enable-steering".into());
        }
        for ip in &self.provider_ipv4 {
            argv.push("--provider-ipv4".into());
            argv.push(ip.clone());
        }
        argv.push("--".into());
        argv.push(binary.to_owned());
        // Build provider arguments
        let mut provider_args = provider_argv(
            &request.provider,
            &prompt,
            &request.model,
            briefing.as_deref(),
            request.effort.as_deref(),
            request.fast,
            &image_paths(staged),
        );
        // Add MCP config for Claude, or overrides for Codex
        if !library.mcp_servers.is_empty() {
            match request.provider.as_str() {
                "claude" => {
                    if let Some(config_path) = super::library_runtime::write_claude_mcp_config(
                        workdir,
                        &library.mcp_servers,
                    )? {
                        provider_args.push("--mcp-config".into());
                        provider_args.push(config_path.to_string_lossy().into_owned());
                    }
                }
                "codex" => {
                    for override_str in
                        super::library_runtime::codex_mcp_config(&library.mcp_servers)
                    {
                        provider_args.push("-c".into());
                        provider_args.push(override_str);
                    }
                }
                provider if super::acp_providers::is_acp(provider) => {
                    // ACP `session/new` takes MCP servers natively: inserted
                    // before the prompt separator as `--mcp <json>`.
                    let separator = provider_args.len() - 2;
                    // Tools over the argv budget travel by name only, so the
                    // trace can say they were left out.
                    let acp = super::library_runtime::acp_mcp_servers(&library.mcp_servers);
                    let mut servers = Vec::new();
                    for server in acp.forwarded {
                        servers.push("--mcp".to_owned());
                        servers.push(server);
                    }
                    for name in acp.skipped {
                        servers.push("--mcp-skipped".to_owned());
                        servers.push(name);
                    }
                    provider_args.splice(separator..separator, servers);
                }
                _ => {}
            }
        }
        argv.extend(provider_args);
        Ok(argv)
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
    ) -> Result<Vec<String>, ExecutionError> {
        self.launch_argv_with(request, lane, workdir, binary, handle, ToolPolicy::Standard)
    }

    fn launch_argv_with(
        &self,
        request: &RunRequest,
        lane: Lane,
        workdir: &Path,
        binary: &str,
        handle: &str,
        policy: ToolPolicy,
    ) -> Result<Vec<String>, ExecutionError> {
        // Staged HERE: the workdir exists by now and belongs to this run, which
        // is the only place a path for these files can honestly be chosen.
        let staged = stage_attachments(workdir, &request.attachments)?;
        let context = stage_context(workdir, &request.context_files)?;
        // Stage library items (skills and MCP configs)
        let library = super::library_runtime::stage_library(workdir, &request.library_items)?;
        let mut argv = match &self.cap_helper {
            Some(helper) => vec![helper.clone()],
            None => vec![
                self.python.clone(),
                self.launcher.to_string_lossy().into_owned(),
            ],
        };
        argv.extend(self.launcher_args(
            request, lane, workdir, binary, handle, &staged, &context, &library, policy,
        )?);
        Ok(argv)
    }

    /// The launcher argv up to and including `-- <binary>`, without the
    /// provider's own arguments.
    ///
    /// Discovery appends a metadata subcommand instead of a prompt, and so gets
    /// the identical sandbox, egress lane, uid and credential mount by
    /// construction rather than from a second implementation that could drift.
    fn launch_prefix(
        &self,
        request: &RunRequest,
        lane: Lane,
        workdir: &Path,
        binary: &str,
        handle: &str,
    ) -> Result<Vec<String>, ExecutionError> {
        let mut argv = match &self.cap_helper {
            Some(helper) => vec![helper.clone()],
            None => vec![
                self.python.clone(),
                self.launcher.to_string_lossy().into_owned(),
            ],
        };
        // Discovery carries no attachments, no context, and no library: it asks
        // the provider for metadata, not for work on a file or on a conversation.
        let empty_library = super::library_runtime::StagedLibrary::default();
        let full = self.launcher_args(
            request,
            lane,
            workdir,
            binary,
            handle,
            &[],
            &[],
            &empty_library,
            ToolPolicy::Standard,
        )?;
        let cut = full
            .iter()
            .position(|arg| arg == "--")
            .map(|index| index + 2)
            .unwrap_or(full.len());
        argv.extend(full.into_iter().take(cut));
        Ok(argv)
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
    /// Read the provider's catalogue inside the SAME isolation profile a turn
    /// uses: an egress lane, a fresh workdir owned by a dropped uid, and the
    /// caller's own credential volume mounted as HOME.
    ///
    /// Reusing that machinery rather than adding a lighter path is the point. A
    /// "just metadata" shortcut would be a second way to reach a provider with
    /// somebody's credentials, and the weaker of two doors is the one worth
    /// attacking.
    async fn discover_models(&self, request: DiscoveryRequest) -> Result<String, ExecutionError> {
        // The same refusals as a run, in the same order, before any resource is
        // taken: an unknown provider or a caller with no credential of their own
        // never reaches a process. ACP providers have no catalogue probe: their
        // model is the provider default (`acp_providers`).
        if super::acp_providers::is_acp(&request.provider) {
            return Err(ExecutionError::Unsupported);
        }
        let binary = self
            .binaries
            .resolve(&request.provider)
            .ok_or(ExecutionError::Unsupported)?
            .to_owned();
        let handle = request
            .credential_handle
            .as_deref()
            .filter(|handle| valid_credential_handle(handle))
            .ok_or(ExecutionError::Unauthorized)?
            .to_owned();
        if request.user_id <= 0 || request.org_id <= 0 {
            return Err(ExecutionError::Unauthorized);
        }
        // Taken before a lane and released with this scope. `try_acquire`, not
        // `acquire`: a reader opening a menu while a probe is already running is
        // told the catalogue is momentarily unavailable rather than made to wait
        // on one, and never queues ahead of a paid turn.
        let permit = self
            .probes
            .clone()
            .try_acquire_owned()
            .map_err(|_| ExecutionError::Unavailable)?;
        // Non-blocking as well, so a probe never waits for a lane a turn wants.
        let lane = self.slots.acquire().ok_or(ExecutionError::Unavailable)?;
        let probe_id = uuid::Uuid::new_v4().simple().to_string();
        // `run-` prefix required: the launcher only accepts a workdir whose
        // basename matches `run-…` (`_validate_workdir_under_trusted_root`), so
        // a `probe-…` dir was refused with `invalid spec` before any sandbox,
        // credential or process existed — every catalogue read has always
        // failed this way, and the reason went to the discarded stderr.
        let workdir = self.workspace_root.join(format!("run-probe-{probe_id}"));
        let guard = LaneGuard {
            slots: self.slots.clone(),
            lane,
            workdir: workdir.clone(),
        };
        if std::fs::create_dir_all(&workdir).is_err() {
            return Err(ExecutionError::Isolation);
        }
        let probe = RunRequest {
            user_id: request.user_id,
            org_id: request.org_id,
            chat_id: probe_id.clone(),
            provider: request.provider.clone(),
            model: "default".into(),
            prompt: String::new(),
            credential_handle: Some(handle.clone()),
            briefing: None,
            effort: None,
            fast: false,
            attachments: Vec::new(),
            // Discovery asks the provider for its catalogue. There is no
            // conversation to carry and nothing to be continuous with.
            context_files: Vec::new(),
            // Discovery is not a run, so no library items apply.
            library_items: Vec::new(),
            min_free_lanes: 1,
        };
        let mut argv = self.launch_prefix(&probe, lane, &workdir, &binary, &handle)?;
        argv.push("app-server".into());

        // Run the probe on a TASK that owns the child, the lane guard and the
        // probe permit.
        //
        // If this function's future is cancelled the join handle is dropped, but
        // the task keeps running: `app_server_models` still performs the
        // launcher's SIGTERM teardown, and only when that has finished are the
        // lane and the workdir released. Cancelling therefore cannot orphan a
        // sandbox, and — the part a PID-based guard got wrong — nothing ever
        // signals a numeric pid after the child has been reaped, so a recycled
        // pid cannot be hit.
        let task = tokio::spawn(async move {
            let mut command = Command::new(&argv[0]);
            command
                .args(&argv[1..])
                .env_remove("HOME")
                .env_remove("XDG_CONFIG_HOME")
                .env_remove("XDG_DATA_HOME")
                .env_remove("CLAUDE_CONFIG_DIR");
            let result = app_server_models(command).await;
            // Held until here on purpose: releasing the lane before teardown
            // would let the next run take a slot whose sandbox is still
            // unwinding.
            drop(guard);
            drop(permit);
            result
        });
        match task.await {
            Ok(result) => result,
            // The task panicked; its guards still dropped, so the lane is back.
            Err(_) => Err(ExecutionError::Unavailable),
        }
    }

    async fn run(&self, request: RunRequest) -> Result<RunHandle, ExecutionError> {
        self.run_with(request, ToolPolicy::Standard).await
    }

    async fn run_tool_free(&self, request: ToolFreeRequest) -> Result<RunHandle, ExecutionError> {
        validate_tool_free(&request)?;
        let limit = request.max_output_bytes;
        let handle = self
            .run_with(request.as_run_request(), ToolPolicy::ToolFree)
            .await?;
        Ok(text_only(handle, limit))
    }
}

impl SandboxedProviderCliRuntime {
    async fn run_with(
        &self,
        request: RunRequest,
        policy: ToolPolicy,
    ) -> Result<RunHandle, ExecutionError> {
        // Everything that can be refused is refused BEFORE a lane, a workdir or
        // a process exists: an unsupported provider or a caller with no
        // credential of their own never consumes an isolation slot.
        let (binary, handle) = self.resolve(&request)?;
        let lane = self
            .slots
            .acquire_if(request.min_free_lanes)
            .ok_or(ExecutionError::LanesBusy)?;
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
        // Steering: Codex app-server mode and Claude support mid-run steering.
        // The Python launcher routes to the appropriate adapter for each provider.
        let supports_steering = policy == ToolPolicy::Standard
            && ((request.provider == "codex"
                && matches!(self.codex_stream, CodexStream::AppServer))
                || request.provider == "claude");
        let argv = self.launch_argv_with(&request, lane, &workdir, &binary, &handle, policy)?;
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
            // Open stdin for steering in app-server mode; null otherwise.
            .stdin(if supports_steering { Stdio::piped() } else { Stdio::null() })
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
        // Take stdin for steering relay in app-server mode
        let child_stdin = if supports_steering { child.stdin.take() } else { None };
        let pid = child.id();
        let (tx, rx) = mpsc::channel::<RunEvent>(16);
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        // Steering channel: bounded to prevent runaway input
        let (steer_tx, mut steer_rx) = mpsc::channel::<super::runs::SteerInput>(8);
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

            // Steering relay: read from steer_rx and write JSON to child stdin.
            // Track pending requests so we can resolve reply channels on ack/error.
            // Runs concurrently with the output stream reader.
            let pending_steers: std::sync::Arc<
                std::sync::Mutex<
                    std::collections::HashMap<String, oneshot::Sender<Result<(), super::runs::SteerError>>>,
                >,
            > = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
            let pending_steers_clone = pending_steers.clone();
            if let Some(stdin) = child_stdin {
                use tokio::io::AsyncWriteExt;
                tokio::spawn(async move {
                    let mut stdin = stdin;
                    while let Some(input) = steer_rx.recv().await {
                        // Store the reply channel keyed by request_id
                        {
                            let mut pending = pending_steers_clone.lock().unwrap_or_else(|e| e.into_inner());
                            pending.insert(input.request_id.clone(), input.reply);
                        }
                        // Format: {"requestId": "...", "text": "..."}
                        let json = serde_json::json!({
                            "requestId": input.request_id,
                            "text": input.text,
                        });
                        let line = format!("{}\n", json);
                        if stdin.write_all(line.as_bytes()).await.is_err() {
                            // Write failed - resolve pending reply with error
                            let mut pending = pending_steers_clone.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(reply) = pending.remove(&input.request_id) {
                                let _ = reply.send(Err(super::runs::SteerError::Unavailable));
                            }
                            break;
                        }
                        if stdin.flush().await.is_err() {
                            let mut pending = pending_steers_clone.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(reply) = pending.remove(&input.request_id) {
                                let _ = reply.send(Err(super::runs::SteerError::Unavailable));
                            }
                            break;
                        }
                    }
                    // Close stdin when steer_rx is closed or on error
                    let _ = stdin.shutdown().await;
                });
            }

            let mut lines = BufReader::new(stdout).lines();
            // `needs_stop` is true whenever WE end the run early (so we must tell
            // the launcher to tear down); a clean provider EOF leaves it false
            // because the launcher exits on its own and runs its finally teardown.
            let stream = async {
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            // Check for steering acknowledgment before parser
                            // Format: {"type":"neev.steer","requestId":"...","accepted":true/false}
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                                if json.get("type").and_then(|v| v.as_str()) == Some("neev.steer") {
                                    if let Some(request_id) = json.get("requestId").and_then(|v| v.as_str()) {
                                        let accepted = json.get("accepted").and_then(|v| v.as_bool()).unwrap_or(false);
                                        let mut pending = pending_steers.lock().unwrap_or_else(|e| e.into_inner());
                                        if let Some(reply) = pending.remove(request_id) {
                                            if accepted {
                                                let _ = reply.send(Ok(()));
                                            } else {
                                                let reason = json.get("reason").and_then(|v| v.as_str()).unwrap_or("rejected");
                                                // Map reason to SteerError
                                                let error = if reason.contains("not active") || reason.contains("finished") {
                                                    super::runs::SteerError::Finished
                                                } else {
                                                    super::runs::SteerError::Rejected
                                                };
                                                let _ = reply.send(Err(error));
                                            }
                                        }
                                    }
                                    continue; // Don't pass steering acks to parser
                                }
                            }
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
                // Resolve any pending steering requests as finished (run completed)
                {
                    let mut pending = pending_steers.lock().unwrap_or_else(|e| e.into_inner());
                    for (_, reply) in pending.drain() {
                        let _ = reply.send(Err(super::runs::SteerError::Finished));
                    }
                }
                false // clean EOF: launcher already exiting + tearing down itself
            };
            tokio::select! {
                _ = &mut cancel_rx => {
                    graceful_stop(&mut child, pid).await;
                    // Resolve pending steers as finished (canceled)
                    {
                        let mut pending = pending_steers.lock().unwrap_or_else(|e| e.into_inner());
                        for (_, reply) in pending.drain() {
                            let _ = reply.send(Err(super::runs::SteerError::Finished));
                        }
                    }
                    let _ = tx.send(RunEvent::Failed("canceled".into())).await;
                }
                needs_stop = stream => {
                    if needs_stop {
                        graceful_stop(&mut child, pid).await;
                        // Resolve pending steers as unavailable (early stop)
                        {
                            let mut pending = pending_steers.lock().unwrap_or_else(|e| e.into_inner());
                            for (_, reply) in pending.drain() {
                                let _ = reply.send(Err(super::runs::SteerError::Unavailable));
                            }
                        }
                    }
                }
            }
        });
        Ok(RunHandle {
            run_id,
            events: rx,
            cancel: Some(cancel_tx),
            workspace_root: Some(self.workspace_root.clone()), // SandboxedProviderCliRuntime workspace
            steer_tx: if supports_steering { Some(steer_tx) } else { None },
            supports_steering,
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

    /// Run one tool-free supervisor turn (see [`ToolFreeRequest`]). Every
    /// runtime that cannot enforce it declines; none runs an ordinary turn
    /// instead.
    pub(crate) async fn run_tool_free(
        &self,
        request: ToolFreeRequest,
    ) -> Result<RunHandle, ExecutionError> {
        match self {
            Self::Disabled(_) => Err(ExecutionError::Disabled),
            #[cfg(any(test, feature = "dev-agent"))]
            Self::Provider(runtime) => runtime.run_tool_free(request).await,
            Self::Sandboxed(runtime) => runtime.run_tool_free(request).await,
            #[cfg(test)]
            Self::Fake(runtime) => runtime.run_tool_free(request).await,
        }
    }

    /// Read a provider's catalogue for one caller.
    ///
    /// The credential is resolved by the CALLER's own lookup before this is
    /// invoked, so a discovery request can never name somebody else's volume.
    pub(crate) async fn discover_models(
        &self,
        provider: &str,
        actor: &crate::neev::types::OrgContext,
        credential_handle: Option<String>,
    ) -> Result<Vec<super::discovery::DiscoveredModel>, ExecutionError> {
        let request = DiscoveryRequest {
            user_id: actor.identity.user_id,
            org_id: actor.org_id,
            provider: provider.to_owned(),
            credential_handle,
        };
        let payload = match self {
            Self::Disabled(runtime) => runtime.discover_models(request).await,
            #[cfg(any(test, feature = "dev-agent"))]
            Self::Provider(runtime) => runtime.discover_models(request).await,
            Self::Sandboxed(runtime) => runtime.discover_models(request).await,
            #[cfg(test)]
            Self::Fake(runtime) => runtime.discover_models(request).await,
        }?;
        Ok(super::discovery::parse_models(&payload))
    }
}

impl RunHandle {
    /// Take the cancel sender so a job registry can cancel this run by id from a
    /// separate request. Leaves `cancel()` on this handle a no-op afterwards.
    pub(crate) fn take_canceler(&mut self) -> Option<oneshot::Sender<()>> {
        self.cancel.take()
    }

    /// Take the steering channel so the live run registry can relay input.
    pub(crate) fn take_steer_tx(&mut self) -> Option<mpsc::Sender<super::runs::SteerInput>> {
        self.steer_tx.take()
    }
}

/// Live-run cancel handles keyed by run id, each tagged with the owning user id
/// so a cancel request can only interrupt a run the caller started. Shared across
/// requests so a separate cancel/restart call can reach an in-flight run.
pub(crate) type JobRegistry =
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, (i32, oneshot::Sender<()>)>>>;

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
    codex_stream: CodexStream,
) -> std::sync::Arc<AgentRuntime> {
    std::sync::Arc::new(AgentRuntime::Sandboxed(SandboxedProviderCliRuntime::new(
        python,
        launcher,
        binaries,
        workspace_root,
        provider_ipv4,
        cap_helper,
        codex_stream,
    )))
}

pub(crate) fn new_job_registry() -> JobRegistry {
    std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
#[path = "execution_tool_free_tests.rs"]
mod tool_free_tests;

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
            effort: None,
            fast: false,
            attachments: Vec::new(),
            context_files: Vec::new(),
            library_items: Vec::new(),
            min_free_lanes: 1,
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
        assert_eq!(
            events.first(),
            Some(&RunEvent::Started("run-7-chat-1".into()))
        );
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
        assert_eq!(
            runtime.run(bad).await.err(),
            Some(ExecutionError::Unauthorized)
        );
    }

    fn sandboxed() -> SandboxedProviderCliRuntime {
        SandboxedProviderCliRuntime::new(
            "python3",
            "/opt/neev/scripts/operations/neev_agent_launch.py",
            ProviderBinaries::claude("/usr/local/bin/claude"),
            "/data/agents",
            vec!["140.82.112.3".into()],
            None, // direct exec (dev/test)
            CodexStream::Exec,
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
            CodexStream::Exec,
        );
        let lane = Lane {
            slot: "a",
            uid: 61000,
        };
        let argv = runtime
            .launch_argv(
                &request("hi"),
                lane,
                Path::new("/data/agents/run-x"),
                "/usr/local/bin/claude",
                HANDLE,
            )
            .unwrap();
        // The privileged fixed-command helper is argv[0] — NOT python and NOT the
        // provider; the server execs only the helper (keeps itself unprivileged).
        assert_eq!(argv[0], "/opt/neev/bin/neev-agent-caplaunch");
        assert!(!argv.contains(&"python3".to_string()));
        assert!(argv.contains(&"--tenant".to_string()));
        let sep = argv
            .iter()
            .position(|a| a == "--")
            .expect("has -- separator");
        assert!(!argv[..sep].contains(&"/usr/local/bin/claude".to_string()));
        assert_eq!(argv[sep + 1], "/usr/local/bin/claude");
    }

    #[test]
    fn sandboxed_launch_argv_goes_through_the_launcher_not_bare_provider() {
        let runtime = sandboxed();
        let lane = Lane {
            slot: "a",
            uid: 61000,
        };
        let workdir = Path::new("/var/lib/neev-native/agents/run-abc");
        let argv = runtime
            .launch_argv(
                &request("draft"),
                lane,
                workdir,
                "/usr/local/bin/claude",
                HANDLE,
            )
            .unwrap();
        // First tokens are the launcher, never the provider binary.
        assert_eq!(argv[0], "python3");
        assert!(argv[1].ends_with("neev_agent_launch.py"));
        assert!(argv.iter().any(|a| a == "--tenant"));
        assert!(argv.contains(&"org:3/user:7".to_string()));
        assert!(argv.contains(&"--slot".to_string()) && argv.contains(&"a".to_string()));
        assert!(argv.contains(&"--uid".to_string()) && argv.contains(&"61000".to_string()));
        assert!(
            argv.contains(&"--workdir".to_string()) && argv.iter().any(|a| a.ends_with("run-abc"))
        );
        // Provider allowlist is passed through.
        assert!(argv.contains(&"--provider-ipv4".to_string()));
        assert!(argv.contains(&"140.82.112.3".to_string()));
        // The provider argv is AFTER the `--` separator (sandboxed), and the
        // provider binary never appears before it.
        let sep = argv
            .iter()
            .position(|a| a == "--")
            .expect("has -- separator");
        assert!(!argv[..sep].contains(&"/usr/local/bin/claude".to_string()));
        assert_eq!(argv[sep + 1], "/usr/local/bin/claude");
        assert_eq!(
            &argv[sep + 2..sep + 4],
            &["-p".to_string(), "draft".to_string()]
        );
        assert!(argv.contains(&"stream-json".to_string()));
    }

    fn acp_runtime() -> SandboxedProviderCliRuntime {
        SandboxedProviderCliRuntime::new(
            "python3",
            "/opt/neev/scripts/operations/neev_agent_launch.py",
            ProviderBinaries {
                claude: Some("/usr/local/bin/claude".into()),
                cursor: Some("/usr/local/bin/agent".into()),
                gemini: Some("/usr/local/bin/gemini".into()),
                opencode: Some("/usr/local/bin/opencode".into()),
                ..ProviderBinaries::default()
            },
            "/data/agents",
            vec![],
            None,
            // Even with the app-server transport (which turns steering on for
            // Codex), an ACP run is never offered steering.
            CodexStream::AppServer,
        )
    }

    #[test]
    fn an_acp_turn_is_the_canonical_launcher_shape_and_nothing_more() {
        let runtime = acp_runtime();
        let lane = Lane {
            slot: "a",
            uid: 61000,
        };
        for (provider, binary) in [
            ("cursor", "/usr/local/bin/agent"),
            ("gemini", "/usr/local/bin/gemini"),
            ("opencode", "/usr/local/bin/opencode"),
        ] {
            let mut run = request("--help me");
            run.provider = provider.into();
            run.briefing = Some("Workspace notes".into());
            let (resolved, _) = runtime.resolve(&run).expect("resolves");
            assert_eq!(resolved, binary);
            let argv = runtime
                .launch_argv(&run, lane, Path::new("/data/agents/run-x"), binary, HANDLE)
                .unwrap();
            let sep = argv.iter().position(|a| a == "--").unwrap();
            assert_eq!(argv[sep + 1], binary);
            assert_eq!(argv[sep + 2], "--neev-acp");
            assert_eq!(argv[sep + 3], "--");
            assert!(argv[sep + 4].starts_with("<workspace-context>\nWorkspace notes"));
            assert!(argv[sep + 4].ends_with("--help me"));
            assert_eq!(argv.len(), sep + 5, "{provider}: no provider flags at all");
            assert!(!argv.contains(&"--enable-steering".to_string()));
            assert_eq!(
                argv[argv.iter().position(|a| a == "--provider").unwrap() + 1],
                provider
            );
        }
    }

    #[test]
    fn an_acp_turn_carries_staged_images_and_library_tools_before_the_prompt() {
        use super::super::library_runtime::{StagedLibrary, StagedMcpServer};
        let runtime = acp_runtime();
        let lane = Lane {
            slot: "a",
            uid: 61000,
        };
        let workdir = Path::new("/data/agents/run-x");
        let staged = vec![
            StagedAttachment {
                path: "/data/agents/run-x/inputs/k--shot.png".into(),
                file_name: "shot.png".into(),
                is_image: true,
            },
            StagedAttachment {
                path: "/data/agents/run-x/inputs/k--notes.txt".into(),
                file_name: "notes.txt".into(),
                is_image: false,
            },
        ];
        let library = StagedLibrary {
            skill_dirs: Vec::new(),
            mcp_servers: vec![
                StagedMcpServer {
                    id: "abc123".into(),
                    name: "Docs".into(),
                    config: crate::neev::library::McpServerConfig::Http {
                        url: "https://mcp.example.com/sse".into(),
                    },
                },
                StagedMcpServer {
                    id: "def456".into(),
                    name: "fs".into(),
                    config: crate::neev::library::McpServerConfig::Stdio {
                        command: "npx".into(),
                        args: vec!["-y".into(), "server".into()],
                    },
                },
            ],
        };
        let mut run = request("look");
        run.provider = "gemini".into();
        let argv = runtime
            .launcher_args(&run, lane, workdir, "/usr/local/bin/gemini", HANDLE, &staged, &[], &library, ToolPolicy::Standard)
            .unwrap();
        let sep = argv.iter().position(|a| a == "--").unwrap();
        let tail = &argv[sep + 1..];
        assert_eq!(tail[0], "/usr/local/bin/gemini");
        assert_eq!(tail[1], "--neev-acp");
        // Only the IMAGE is passed as an image; the text file is named in the
        // prompt like every attachment.
        assert_eq!(
            &tail[2..4],
            &["--image".to_owned(), "/data/agents/run-x/inputs/k--shot.png".to_owned()]
        );
        let mcp: Vec<&String> = tail
            .iter()
            .enumerate()
            .filter(|(i, _)| *i > 0 && tail[i - 1] == "--mcp")
            .map(|(_, value)| value)
            .collect();
        assert_eq!(mcp.len(), 2);
        let http: serde_json::Value = serde_json::from_str(mcp[0]).unwrap();
        assert_eq!(http["type"], "http");
        assert_eq!(http["url"], "https://mcp.example.com/sse");
        let stdio: serde_json::Value = serde_json::from_str(mcp[1]).unwrap();
        assert_eq!(stdio["command"], "npx");
        assert!(stdio.get("env").is_none(), "no env ever travels");
        // The prompt stays last, behind its own separator.
        assert_eq!(tail[tail.len() - 2], "--");
        assert!(tail[tail.len() - 1].contains("look"));
        assert!(tail[tail.len() - 1].contains("notes.txt"));
    }

    #[test]
    fn choices_the_acp_path_cannot_honour_are_refused_not_dropped() {
        let runtime = acp_runtime();
        let mut model = request("hi");
        model.provider = "gemini".into();
        model.model = "gemini-2.5-pro".into();
        let mut effort = request("hi");
        effort.provider = "cursor".into();
        effort.effort = Some("high".into());
        let mut fast = request("hi");
        fast.provider = "opencode".into();
        fast.fast = true;
        for run in [model, effort, fast] {
            assert_eq!(
                runtime.resolve(&run).err(),
                Some(ExecutionError::Unsupported)
            );
        }
        // An unbound ACP provider is refused like any other.
        let mut unbound = request("hi");
        unbound.provider = "gemini".into();
        assert_eq!(
            sandboxed().resolve(&unbound).err(),
            Some(ExecutionError::Unsupported)
        );
    }

    #[tokio::test]
    async fn acp_providers_have_no_catalogue_probe() {
        let runtime = acp_runtime();
        for provider in ["cursor", "gemini", "opencode"] {
            let result = runtime
                .discover_models(DiscoveryRequest {
                    user_id: 7,
                    org_id: 3,
                    provider: provider.into(),
                    credential_handle: Some(HANDLE.into()),
                })
                .await;
            assert_eq!(result.err(), Some(ExecutionError::Unsupported));
        }
        // No lane was spent.
        assert!(runtime.slots.acquire().is_some());
    }

    #[test]
    fn each_provider_is_invoked_in_its_own_dialect() {
        // Claude Code is headless via `-p` with a streaming output format.
        let claude = provider_argv("claude", "draft", "default", None, None, false, &[]);
        assert_eq!(&claude[..2], &["-p".to_string(), "draft".to_string()]);
        assert!(claude.contains(&"stream-json".to_string()));
        assert!(claude.contains(&"--include-partial-messages".to_string()));

        // Codex is a SUBCOMMAND with `--json`, verified against `codex exec
        // --help` in the deployed agent image. It rejects Claude's flags
        // outright, which is why sharing them produced no output at all.
        let codex = provider_argv("codex", "draft", "default", None, None, false, &[]);
        // Speed is always stated, so the codex argv carries its two config
        // overrides even when nothing was switched on.
        assert_eq!(
            &codex[..3],
            &[
                "exec".to_string(),
                "--json".to_string(),
                "draft".to_string()
            ]
        );
        assert!(codex.contains(&"--skip-git-repo-check".to_string()));
        assert!(!claude.contains(&"--skip-git-repo-check".to_string()));
        assert!(!codex.contains(&"-p".to_string()));
        assert!(!codex.contains(&"--output-format".to_string()));

        // Model selection is spelled differently by each.
        assert!(provider_argv("codex", "d", "gpt-5", None, None, false, &[])
            .windows(2)
            .any(|pair| pair == ["-m".to_string(), "gpt-5".to_string()]));
        assert!(provider_argv("claude", "d", "opus", None, None, false, &[])
            .windows(2)
            .any(|pair| pair == ["--model".to_string(), "opus".to_string()]));
        // "default" means the provider's own default: nothing is passed.
        assert!(
            !provider_argv("codex", "d", "default", None, None, false, &[])
                .contains(&"-m".to_string())
        );
    }

    /// Pinned Codex (`rust-v0.155.1`) exposes `-i, --image <FILE>...` on `exec`,
    /// verified against the live binary's own `--help`. This pins our side of
    /// that contract: one flag per file, paths as separate argv entries, and
    /// never folded into the prompt or a shell string — a name containing a
    /// comma or a space must not split into two paths.
    #[test]
    fn codex_takes_staged_images_as_separate_arguments() {
        let images = vec![
            "/w/attachments/a b.png".to_string(),
            "/w/attachments/c,d.png".to_string(),
        ];
        let argv = provider_argv("codex", "draft", "default", None, None, false, &images);
        let positions: Vec<usize> = argv
            .iter()
            .enumerate()
            .filter(|(_, value)| *value == "--image")
            .map(|(index, _)| index)
            .collect();
        assert_eq!(positions.len(), 2);
        assert_eq!(argv[positions[0] + 1], "/w/attachments/a b.png");
        assert_eq!(argv[positions[1] + 1], "/w/attachments/c,d.png");
        // The prompt is untouched: the files are arguments, not text.
        assert!(argv.contains(&"draft".to_string()));
        assert!(!argv
            .iter()
            .any(|value| value.contains("attachments/a b.png") && value.contains("draft")));
    }

    /// The dev runtime reuses ONE workspace for every run, so staging has to be
    /// repeatable. It was not: the second run's `create_dir` hit the first
    /// run's directory and failed, which would have surfaced as a run that
    /// simply refused to start with attachments.
    #[cfg(unix)]
    #[test]
    fn staging_twice_into_one_workspace_keeps_the_runs_apart() {
        let root = std::env::temp_dir().join(format!(
            "neev-stage-twice-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let one = vec![RunAttachment {
            key: "11111111-1111-4111-8111-111111111111".into(),
            file_name: "notes.txt".into(),
            bytes: b"first run".to_vec(),
            is_image: false,
        }];
        let two = vec![RunAttachment {
            key: "11111111-1111-4111-8111-111111111111".into(),
            file_name: "notes.txt".into(),
            bytes: b"second run".to_vec(),
            is_image: false,
        }];
        let first = stage_attachments(&root, &one).expect("first run stages");
        let second = stage_attachments(&root, &two).expect("second run must also stage");
        // Same key, same name, same workspace — and still two distinct files
        // with their own bytes, so neither run can read the other's.
        assert_ne!(first[0].path, second[0].path);
        assert_eq!(std::fs::read(&first[0].path).unwrap(), b"first run");
        assert_eq!(std::fs::read(&second[0].path).unwrap(), b"second run");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Context and this turn's uploads land in DIFFERENT directories, and a name
    /// shared between the two resolves to the right bytes on both sides.
    ///
    /// This is what lets the prompt say "the reader just attached these" about
    /// one set and "these are from earlier in the conversation" about the other
    /// and have both be true. Distinguishing them by name would be guesswork the
    /// moment a reader sends `report.html` twice, which is the ordinary case.
    #[cfg(unix)]
    #[test]
    fn context_stages_apart_from_this_turns_uploads() {
        let root = std::env::temp_dir().join(format!(
            "neev-stage-context-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let fresh = vec![RunAttachment {
            key: "11111111-1111-4111-8111-111111111111".into(),
            file_name: "report.html".into(),
            bytes: b"<p>just sent</p>".to_vec(),
            is_image: false,
        }];
        let context = vec![
            RunAttachment {
                key: "22222222-2222-4222-8222-222222222222".into(),
                file_name: super::super::transcript::TRANSCRIPT_NAME.into(),
                bytes: b"# Conversation transcript\n".to_vec(),
                is_image: false,
            },
            RunAttachment {
                key: "33333333-3333-4333-8333-333333333333".into(),
                // The same name as the fresh upload, deliberately.
                file_name: "report.html".into(),
                bytes: b"<p>from an earlier turn</p>".to_vec(),
                is_image: false,
            },
        ];
        let staged = stage_attachments(&root, &fresh).expect("uploads stage");
        let carried = stage_context(&root, &context).expect("context stages");
        assert!(
            staged[0].path.contains(ATTACHMENT_DIR),
            "this turn's uploads stay where the prompt says they are"
        );
        assert!(
            carried.iter().all(|item| item.path.contains(CONTEXT_DIR)),
            "and context is somewhere else entirely"
        );
        assert_eq!(std::fs::read(&staged[0].path).unwrap(), b"<p>just sent</p>");
        assert_eq!(
            std::fs::read(&carried[1].path).unwrap(),
            b"<p>from an earlier turn</p>",
            "the identically-named earlier file keeps its own bytes"
        );
        // The transcript is staged first, so a truncated path list still has it.
        assert!(carried[0]
            .path
            .ends_with(super::super::transcript::TRANSCRIPT_NAME));
        assert!(
            image_paths(&carried).is_empty(),
            "context is never offered as a native image input"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The briefing names a FILE; only the runtime can name a path. This is
    /// where the two meet, and it must stay inside one argv argument.
    #[test]
    fn the_briefing_gains_the_staged_context_paths_and_stays_bounded() {
        let staged = (0..CONTEXT_PATHS + 5)
            .map(|index| StagedAttachment {
                path: format!("/run/neev-context/abc/key--file{index}.txt"),
                file_name: format!("file{index}.txt"),
                is_image: false,
            })
            .collect::<Vec<_>>();
        let text = briefing_with_context(Some("standing instructions"), &staged)
            .expect("a briefing with context");
        assert!(text.starts_with("standing instructions"));
        assert!(
            text.contains("directory: /run/neev-context/abc"),
            "the directory is named, which is what makes the unlisted files reachable"
        );
        assert!(text.contains("/run/neev-context/abc/key--file0.txt"));
        assert!(
            text.contains("5 more file(s) in that directory"),
            "and the ones not listed are counted rather than dropped in silence"
        );
        assert_eq!(
            text.matches("/run/neev-context/abc/key--").count(),
            CONTEXT_PATHS,
            "the path list is bounded: one argv argument cannot hold thousands"
        );
        assert_eq!(
            briefing_with_context(Some("standing instructions"), &[]).as_deref(),
            Some("standing instructions"),
            "and a run with no context gets no block at all"
        );
    }

    /// An existing staging root is reused only when it is a real directory.
    /// Tolerating `AlreadyExists` without looking would let a symlink planted
    /// at that path redirect every staged file out of the workspace.
    #[cfg(unix)]
    #[test]
    fn staging_refuses_a_symlink_where_its_directory_belongs() {
        let root = std::env::temp_dir().join(format!(
            "neev-stage-link-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let elsewhere = root.with_extension("target");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join(ATTACHMENT_DIR)).unwrap();
        let attachments = vec![RunAttachment {
            key: "11111111-1111-4111-8111-111111111111".into(),
            file_name: "notes.txt".into(),
            bytes: b"x".to_vec(),
            is_image: false,
        }];
        assert!(
            matches!(
                stage_attachments(&root, &attachments),
                Err(ExecutionError::Unauthorized)
            ),
            "a symlink must not be followed out of the workspace"
        );
        // And nothing was written through it.
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }

    /// Claude has no image switch. Its attachments are staged files it opens
    /// with its own read tool, so inventing a flag here would fail the run.
    /// Two files with the SAME name in one turn must both arrive, with their
    /// own bytes. Keying the leaf is what makes that true; without it `O_EXCL`
    /// would fail the run, and without `O_EXCL` one would silently win.
    #[cfg(unix)]
    #[test]
    fn duplicate_names_are_staged_as_distinct_files() {
        let root = std::env::temp_dir().join(format!(
            "neev-stage-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let attachments = vec![
            RunAttachment {
                key: "11111111-1111-4111-8111-111111111111".into(),
                file_name: "notes.txt".into(),
                bytes: b"first".to_vec(),
                is_image: false,
            },
            RunAttachment {
                key: "22222222-2222-4222-8222-222222222222".into(),
                file_name: "notes.txt".into(),
                bytes: b"second".to_vec(),
                is_image: false,
            },
        ];
        // Under a hostile umask: 0o077 would mask the creating mode down to
        // 0600 and silently lock the dropped run uid out of its own files.
        let previous = unsafe { libc::umask(0o077) };
        let staged = stage_attachments(&root, &attachments).unwrap();
        unsafe { libc::umask(previous) };
        assert_eq!(staged.len(), 2);
        assert_ne!(staged[0].path, staged[1].path);
        assert_eq!(std::fs::read(&staged[0].path).unwrap(), b"first");
        assert_eq!(std::fs::read(&staged[1].path).unwrap(), b"second");
        // Displayed by their own names, staged under keyed leaves.
        assert_eq!(staged[0].file_name, "notes.txt");
        assert!(staged[0].path.contains("11111111"));
        // Nothing lands at the workdir root, where a name like AGENTS.md would
        // be read as configuration.
        assert!(!root.join("notes.txt").exists());
        assert!(!root.join("AGENTS.md").exists());
        assert!(!root.join(ATTACHMENT_DIR).join("AGENTS.md").exists());
        // The staging directory must be traversable by the dropped run uid,
        // which is NOT the uid that created it. Its protection is the 0700 run
        // root above, not this mode.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(root.join(ATTACHMENT_DIR))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755, "a 0700 child would block the dropped run uid");
        let file_mode = std::fs::metadata(&staged[0].path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o644);
        // And the paths are named in the prompt, or the agent cannot open them.
        let prompt = prompt_with_attachments("look", &staged);
        assert!(prompt.contains(&staged[0].path));
        assert!(prompt.contains(&staged[1].path));
        assert!(prompt.ends_with("look"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn claude_is_given_no_image_flag() {
        let images = vec!["/w/attachments/a.png".to_string()];
        let argv = provider_argv("claude", "draft", "default", None, None, false, &images);
        assert!(!argv.contains(&"--image".to_string()));
        assert!(!argv.contains(&"-i".to_string()));
    }

    #[test]
    fn effort_and_speed_reach_each_cli_in_its_own_spelling() {
        // Claude: a native flag for effort, settings for speed.
        let claude = provider_argv("claude", "draft", "opus", None, Some("xhigh"), true, &[]);
        assert!(claude
            .windows(2)
            .any(|pair| pair == ["--effort".to_string(), "xhigh".to_string()]));
        assert!(claude
            .windows(2)
            .any(|pair| pair == ["--settings".to_string(), "{\"fastMode\":true}".to_string()]));

        // Codex: config overrides for both. `service_tier` and
        // `features.fast_mode` are the two the docs pair together.
        let codex = provider_argv(
            "codex",
            "draft",
            "default",
            None,
            Some("minimal"),
            true,
            &[],
        );
        assert!(codex.windows(2).any(|pair| pair
            == [
                "-c".to_string(),
                "model_reasoning_effort=\"minimal\"".to_string()
            ]));
        assert!(codex
            .windows(2)
            .any(|pair| pair == ["-c".to_string(), "service_tier=\"fast\"".to_string()]));
        assert!(codex
            .windows(2)
            .any(|pair| pair == ["-c".to_string(), "features.fast_mode=true".to_string()]));
    }

    #[test]
    fn speed_off_is_sent_explicitly_rather_than_left_to_ambient_config() {
        // A run that inherited fast mode from a config file the reader cannot
        // see would be spending their money on a setting they never chose, so
        // the OFF state is stated rather than omitted.
        let claude = provider_argv("claude", "draft", "opus", None, None, false, &[]);
        assert!(claude
            .windows(2)
            .any(|pair| pair == ["--settings".to_string(), "{\"fastMode\":false}".to_string()]));
        let codex = provider_argv("codex", "draft", "default", None, None, false, &[]);
        assert!(codex
            .windows(2)
            .any(|pair| pair == ["-c".to_string(), "service_tier=\"default\"".to_string()]));
        assert!(codex
            .windows(2)
            .any(|pair| pair == ["-c".to_string(), "features.fast_mode=false".to_string()]));
    }

    #[test]
    fn an_unset_effort_passes_no_effort_argument_at_all() {
        // Unset means the provider's own default, which is not the same as any
        // level we could name for them.
        let claude = provider_argv("claude", "draft", "default", None, None, false, &[]);
        assert!(!claude.contains(&"--effort".to_string()));
        let codex = provider_argv("codex", "draft", "default", None, None, false, &[]);
        assert!(!codex
            .iter()
            .any(|arg| arg.starts_with("model_reasoning_effort")));
    }

    #[test]
    fn the_codex_transport_is_a_launcher_argument_and_defaults_to_exec() {
        let lane = Lane {
            slot: "a",
            uid: 61000,
        };
        let workdir = Path::new("/w/run-x");
        let mut codex = request("hi");
        codex.provider = "codex".into();

        // Default: the argv says `exec` rather than leaving the launcher to
        // assume it, and the provider's own arguments are the exec ones.
        let argv = sandboxed()
            .launch_argv(&codex, lane, workdir, "/usr/local/bin/codex", HANDLE)
            .unwrap();
        let sep = argv.iter().position(|a| a == "--").expect("separator");
        let position = argv[..sep]
            .iter()
            .position(|a| a == "--codex-stream")
            .expect("transport is a launcher argument");
        assert_eq!(argv[position + 1], "exec");
        assert_eq!(argv[sep + 2], "exec");

        // Enabled: ONLY the launcher argument changes. Which binary runs, which
        // credential volume it opens and what the provider is asked are all the
        // same — the rewrite into an app-server launch happens inside the
        // sandbox, where the isolation already is.
        let streaming = SandboxedProviderCliRuntime::new(
            "python3",
            "/opt/neev/scripts/operations/neev_agent_launch.py",
            ProviderBinaries::claude("/usr/local/bin/claude"),
            "/data/agents",
            vec!["140.82.112.3".into()],
            None,
            CodexStream::AppServer,
        );
        let enabled = streaming
            .launch_argv(&codex, lane, workdir, "/usr/local/bin/codex", HANDLE)
            .unwrap();
        let position = enabled
            .iter()
            .position(|a| a == "--codex-stream")
            .expect("transport is a launcher argument");
        assert_eq!(enabled[position + 1], "app-server");
        // Launcher arguments may differ (app-server also enables steering), so
        // compare what follows each argv's OWN separator: the provider's part.
        let enabled_sep = enabled.iter().position(|a| a == "--").expect("separator");
        assert_eq!(enabled[enabled_sep + 1..], argv[sep + 1..]);
    }

    #[test]
    fn an_unknown_codex_transport_is_refused_rather_than_read_as_a_default() {
        assert_eq!(CodexStream::parse("exec"), Some(CodexStream::Exec));
        assert_eq!(
            CodexStream::parse("app-server"),
            Some(CodexStream::AppServer)
        );
        assert_eq!(CodexStream::default(), CodexStream::Exec);
        // A typo must not be read as either choice: startup aborts on `None`.
        for value in ["appserver", "app_server", "APP-SERVER", "true", ""] {
            assert_eq!(CodexStream::parse(value), None, "{value} must be refused");
        }
    }

    #[test]
    fn sandboxed_deny_all_when_no_provider_ip() {
        let runtime = SandboxedProviderCliRuntime::new(
            "python3",
            "/l/launch.py",
            ProviderBinaries::claude("/usr/local/bin/claude"),
            "/w",
            vec![],
            None,
            CodexStream::Exec,
        );
        let lane = Lane {
            slot: "b",
            uid: 61001,
        };
        let argv = runtime
            .launch_argv(
                &request("hi"),
                lane,
                Path::new("/w/run-x"),
                "/usr/local/bin/claude",
                HANDLE,
            )
            .unwrap();
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
                ..ProviderBinaries::default()
            },
            "/w",
            vec![],
            None,
            CodexStream::Exec,
        );
        let mut codex = request("hi");
        codex.provider = "codex".into();
        let (binary, _) = both.resolve(&codex).expect("codex resolves");
        assert_eq!(binary, "/usr/local/bin/codex");
        let argv = both
            .launch_argv(
                &codex,
                Lane {
                    slot: "a",
                    uid: 61000,
                },
                Path::new("/w/run-x"),
                &binary,
                HANDLE,
            )
            .unwrap();
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
        let argv = runtime
            .launch_argv(
                &request("hi"),
                Lane {
                    slot: "a",
                    uid: 61000,
                },
                Path::new("/data/agents/run-x"),
                "/usr/local/bin/claude",
                HANDLE,
            )
            .unwrap();
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
        let lane = Lane {
            slot: "a",
            uid: 61000,
        };
        let workdir = Path::new("/w/run-x");

        // "default" is expressed by passing no --model at all, so the provider
        // picks its own; we never invent a model name on the user's behalf.
        let argv = runtime
            .launch_argv(
                &request("hi"),
                lane,
                workdir,
                "/usr/local/bin/claude",
                HANDLE,
            )
            .unwrap();
        assert!(!argv.contains(&"--model".to_string()));

        // A chosen model is actually passed through — the UI's selection is what
        // runs, so the session's stored model is not a decorative label.
        let mut chosen = request("hi");
        chosen.model = "claude-opus-4-1".into();
        let argv = runtime
            .launch_argv(&chosen, lane, workdir, "/usr/local/bin/claude", HANDLE)
            .unwrap();
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
        for bad in [
            "--dangerously-skip-permissions",
            "a b",
            "model;rm",
            &"m".repeat(81),
        ] {
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
        assert_eq!(
            runtime.run(bad).await.err(),
            Some(ExecutionError::Unauthorized)
        );
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
                effort: None,
                fast: false,
                attachments: Vec::new(),
                context_files: Vec::new(),
                library_items: Vec::new(),
                min_free_lanes: 1,
            })
            .await
            .unwrap();
        let mut text = String::new();
        let mut completed = false;
        let mut started = false;
        while let Some(event) = handle.next().await {
            match event {
                RunEvent::Started(_) => started = true,
                RunEvent::Activity(_) | RunEvent::Usage(_) => {}
                // Raw stream: this harness accumulates what arrived, without
                // the seam handling that `LiveRun::append_message` adds.
                RunEvent::Chunk(chunk) | RunEvent::Message(chunk) => text.push_str(&chunk),
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

    /// The catalogue probe must ask the launcher for a workdir it will accept.
    ///
    /// The launcher only takes a `run-…` basename
    /// (`neev_agent_launch._validate_workdir_under_trusted_root`). The probe
    /// asked for `probe-…`, so every catalogue read was refused with
    /// `invalid spec` before a sandbox, a credential or a process existed — and
    /// the reason went to the stderr this caller discards. A pure argv test
    /// could not have caught it: the name is built here, at the call site.
    ///
    /// The fake launcher stands in for the cap helper, records the argv it was
    /// given, and answers the app-server handshake, so this exercises the real
    /// `discover_models` path end to end.
    #[tokio::test]
    async fn the_catalogue_probe_asks_for_a_workdir_the_launcher_accepts() {
        let dir = tempfile::tempdir().unwrap();
        let recorded = dir.path().join("argv.txt");
        let helper = dir.path().join("fake-launcher.sh");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {record}\n\
                 read -r _line\n\
                 printf '{{\"id\":1,\"result\":{{\"userAgent\":\"fake\"}}}}\\n'\n\
                 read -r _initialized\n\
                 read -r _list\n\
                 printf '{{\"id\":2,\"result\":{{\"data\":[{{\"id\":\"gpt-test\"}}]}}}}\\n'\n",
                record = recorded.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let workspace = dir.path().join("agents");
        std::fs::create_dir_all(&workspace).unwrap();

        let runtime = SandboxedProviderCliRuntime::new(
            "python3",
            dir.path().join("unused-launcher.py"),
            ProviderBinaries {
                claude: None,
                codex: Some("/usr/local/bin/codex".into()),
                ..ProviderBinaries::default()
            },
            &workspace,
            Vec::new(),
            Some(helper.to_string_lossy().into_owned()),
            CodexStream::Exec,
        );

        let answer = runtime
            .discover_models(DiscoveryRequest {
                provider: "codex".into(),
                user_id: 7,
                org_id: 3,
                credential_handle: Some("0123456789abcdef0123456789abcdef".into()),
            })
            .await
            .expect("the probe should reach the provider and return its catalogue");
        assert!(
            answer.contains("gpt-test"),
            "the catalogue should come back: {answer}"
        );

        let argv: Vec<String> = std::fs::read_to_string(&recorded)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let workdir = argv
            .iter()
            .position(|arg| arg == "--workdir")
            .map(|at| argv[at + 1].clone())
            .expect("the launcher is always told a workdir");
        let name = std::path::Path::new(&workdir)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("run-"),
            "the launcher only accepts a run- workdir, got {name}"
        );
        assert!(
            name.starts_with("run-probe-"),
            "and it is still identifiable as a probe: {name}"
        );
        // It drives `app-server`, never a turn the reader would be charged for.
        // Read from the PROVIDER's own arguments — after the sandbox separator
        // — because `exec` is also the name of a transport the launcher is told
        // about, and a launcher argument saying so is not a subcommand.
        let separator = argv.iter().position(|arg| arg == "--").expect("separator");
        let provider_argv = &argv[separator + 1..];
        assert_eq!(provider_argv.last().map(String::as_str), Some("app-server"));
        assert!(!provider_argv.iter().any(|arg| arg == "exec"));
    }
    fn shared_library_request(provider: &str) -> RunRequest {
        use super::super::library_runtime::{LibraryItemKind, LibraryRunFile, LibraryRunItem};
        let mut req = request("Use the workspace tools");
        req.provider = provider.into();
        req.library_items = vec![LibraryRunItem {
            id: "workspace-tools".into(),
            name: "Team tools".into(),
            kind: LibraryItemKind::Mcp,
            files: vec![LibraryRunFile {
                path: "mcp.json".into(),
                content: serde_json::to_vec(&serde_json::json!({
                    "name":"echo", "type":"stdio", "command":"python3", "args":[]
                }))
                .unwrap(),
            }],
            setup_note: None,
        }];
        req
    }

    #[test]
    fn library_configuration_reaches_both_provider_launches() {
        for provider in ["claude", "codex"] {
            let dir = tempfile::tempdir().unwrap();
            let req = shared_library_request(provider);
            let argv = sandboxed()
                .launch_argv(
                    &req,
                    Lane {
                        slot: "a",
                        uid: 61000,
                    },
                    dir.path(),
                    provider,
                    HANDLE,
                )
                .unwrap();
            if provider == "claude" {
                let idx = argv
                    .iter()
                    .position(|arg| arg == "--mcp-config")
                    .expect("Claude MCP config argument");
                let value: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&argv[idx + 1]).unwrap()).unwrap();
                assert_eq!(value["mcpServers"].as_object().unwrap().len(), 1);
            } else {
                let overrides: Vec<_> = argv
                    .windows(2)
                    .filter(|p| p[0] == "-c" && p[1].starts_with("mcp_servers."))
                    .map(|p| &p[1])
                    .collect();
                assert!(!overrides.is_empty());
                assert!(overrides.iter().any(|arg| arg.ends_with(".args=[]")));
                for arg in overrides {
                    toml::from_str::<toml::Value>(arg).unwrap();
                }
            }
        }
    }

    #[test]
    fn library_configuration_write_failure_refuses_launch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("neev-library/mcp.json")).unwrap();
        assert!(sandboxed()
            .launch_argv(
                &shared_library_request("claude"),
                Lane {
                    slot: "a",
                    uid: 61000
                },
                dir.path(),
                "claude",
                HANDLE
            )
            .is_err());
    }
}

#[cfg(test)]
mod lane_tests {
    use super::*;

    #[test]
    fn acquire_if_takes_a_lane_only_when_enough_are_free() {
        let pool = SlotPool::new();
        assert_eq!(pool.free_count(), 2);
        // A knowledge batch needs two free lanes.
        let first = pool.acquire_if(2).expect("two free");
        assert_eq!(pool.free_count(), 1);
        assert!(pool.acquire_if(2).is_none(), "one free is not enough for a knowledge batch");
        assert_eq!(pool.free_count(), 1, "a refusal takes nothing");
        // A chat run still gets the last lane.
        let second = pool.acquire_if(1).expect("one free");
        assert!(pool.acquire_if(1).is_none());
        pool.release(first);
        pool.release(second);
        assert_eq!(pool.free_count(), 2);
    }
}
