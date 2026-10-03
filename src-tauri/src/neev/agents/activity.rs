//! Bounded, durable **tool activity** for an agent turn.
//!
//! The provider CLIs already stream what tools they ran and what came back; the
//! runner used to keep only `text_delta` and drop the rest, so a reply arrived
//! with no trace of the work behind it. This module is the part that survives:
//! a small, capped, serialisable record of tool calls, their results and a few
//! status markers, stored beside the turn's text so a refresh reproduces it.
//!
//! Three rules shape everything here:
//! - **No hidden reasoning.** Thinking blocks are parsed exactly far enough to
//!   be ignored. They are never accumulated, stored or sent.
//! - **Bounded by construction.** Item count, per-field length and total stored
//!   size are all capped, and hitting a cap is *recorded* (`truncated`) rather
//!   than leaving a partial trace that looks complete.
//! - **Never invent.** A provider stream we cannot map produces one
//!   `unsupported` marker, not a plausible-looking empty trace.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Items recorded per turn. Past this the trace is truncated and says so.
pub(crate) const MAX_ITEMS: usize = 200;
/// Characters kept of a tool's input arguments.
pub(crate) const MAX_INPUT_CHARS: usize = 2_048;
/// Characters kept of a tool's output.
pub(crate) const MAX_OUTPUT_CHARS: usize = 4_096;
/// Total serialised activity stored for one turn. Mirrors the schema CHECK.
pub(crate) const MAX_ACTIVITY_CHARS: usize = 65_536;
/// Bytes held back so the `truncated` marker is always affordable at the moment
/// the budget runs out. Comfortably larger than that marker serialises to,
/// because being wrong in this direction costs a few bytes of capacity and
/// being wrong in the other costs the whole trace.
const MARKER_RESERVE: usize = 128;
/// Bytes held back for ONE terminal provider-error step, so a long run that
/// spent its trace budget and THEN failed still stores why. The step's message
/// is at most `provider_error::MAX_MESSAGE_CHARS` characters (4 bytes each at
/// worst) plus its fixed fields, which this covers with room to spare.
pub(crate) const ERROR_RESERVE: usize = 1_536;

fn is_error_step(kind: &ActivityKind) -> bool {
    matches!(kind, ActivityKind::Status { state, .. } if state == super::provider_error::STATE_ERROR)
}

/// Bytes a supervised turn holds back for its plan and review steps, so an
/// executor that fills its trace cannot cost the reader the verdict. Opt-in
/// ([`ActivityLog::with_supervision_reserve`]): an ordinary turn keeps its
/// whole budget. Sized for one plan and one review step at their stored
/// detail bounds (`runs::supervision`), with room for JSON escaping.
pub(crate) const SUPERVISION_RESERVE: usize = 6_144;
/// Every supervision step's id starts with this. Only the server writes
/// these steps; a provider stream cannot produce one.
pub(crate) const SUPERVISION_ID_PREFIX: &str = "supervision:";
/// The planner is running (revised in place to [`STATE_PLAN`]).
pub(crate) const STATE_PLAN_PENDING: &str = "plan_pending";
/// The planner finished: `category` is `ready`, `unavailable` or `canceled`.
pub(crate) const STATE_PLAN: &str = "plan";
/// The executor COMPLETED and its reply is final; the review is running
/// (revised in place to [`STATE_REVIEW`]). A running row whose trace ends in
/// this state holds a finished execution, which is what crash recovery needs
/// to know.
pub(crate) const STATE_REVIEW_PENDING: &str = "review_pending";
/// The review finished: `category` is `pass`, `concerns`, `fail` or
/// `unavailable`. Display only; it never changes the turn's status.
pub(crate) const STATE_REVIEW: &str = "review";

pub(crate) fn is_supervision_state(state: &str) -> bool {
    matches!(
        state,
        STATE_PLAN_PENDING | STATE_PLAN | STATE_REVIEW_PENDING | STATE_REVIEW
    )
}

fn is_supervision_step(id: &str, kind: &ActivityKind) -> bool {
    id.starts_with(SUPERVISION_ID_PREFIX)
        && matches!(kind, ActivityKind::Status { state, .. } if is_supervision_state(state))
}

/// Steps a trimmed trace must keep: the terminal error and the supervision
/// steps, each of which is the stored answer to a question the reader asked.
fn is_pinned(item: &ActivityItem) -> bool {
    is_error_step(&item.kind) || is_supervision_step(&item.id, &item.kind)
}

/// What one recorded step was.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ActivityKind {
    #[serde(rename_all = "camelCase")]
    ToolCall {
        name: String,
        /// Already-serialised JSON, kept as a STRING: a tool input streamed in
        /// fragments is not always valid JSON when the block ends, and
        /// forwarding what arrived is honest where re-parsing would guess.
        input: String,
        input_truncated: bool,
    },
    #[serde(rename_all = "camelCase")]
    ToolResult {
        call_id: String,
        output: String,
        output_truncated: bool,
        is_error: bool,
    },
    #[serde(rename_all = "camelCase")]
    Status {
        state: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// Only on `state: "error"`: the failure's class (see
        /// `provider_error::Category`). Optional and defaulted, so a trace
        /// stored before it existed still reads, and an older reader ignores it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        category: Option<String>,
    },
}

/// A recorded step, positioned in the turn.
///
/// `seq` is the identity: monotonic within a turn and the client's dedupe key,
/// because a reattach replays the stored prefix before streaming live items and
/// must not produce two of anything. `offset` is where in the reply text this
/// happened, so the trace interleaves with the prose instead of piling up at
/// the end.
///
/// **`offset` counts Unicode scalar values** — Rust `chars()`, the same unit
/// the turn's stored length cap uses. A JavaScript string is UTF-16, so a
/// client slicing by `String.prototype.slice` will be wrong past the first
/// astral character (an emoji is one scalar here and two UTF-16 units there).
/// Clients must convert, e.g. `[...text].slice(0, offset).join("")`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivityItem {
    pub(crate) seq: u32,
    pub(crate) offset: usize,
    pub(crate) id: String,
    #[serde(flatten)]
    pub(crate) kind: ActivityKind,
}

/// A parsed step before the accumulator gives it a `seq` and an `offset`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivityDraft {
    pub(crate) id: String,
    pub(crate) kind: ActivityKind,
}

impl ActivityKind {
    /// The step with every payload removed: tool name, status state and error
    /// class are kept; tool input, tool output and status detail are emptied
    /// (the truncation flags are kept). Used for org-knowledge runs, whose
    /// tool traffic is source text that must not be persisted (Amendment 1, A1).
    pub(crate) fn names_only(&self) -> Self {
        match self {
            Self::ToolCall { name, input_truncated, .. } => Self::ToolCall {
                name: name.clone(),
                input: String::new(),
                input_truncated: *input_truncated,
            },
            Self::ToolResult { call_id, output_truncated, is_error, .. } => Self::ToolResult {
                call_id: call_id.clone(),
                output: String::new(),
                output_truncated: *output_truncated,
                is_error: *is_error,
            },
            Self::Status { state, category, .. } => Self::Status {
                state: state.clone(),
                detail: None,
                category: category.clone(),
            },
        }
    }
}

/// One mapped piece of a provider's stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StreamItem {
    /// Part of a message still being written. Joined to whatever came before it
    /// exactly as it arrives — these are fragments of one sentence and anything
    /// inserted between them would land mid-word.
    Text(String),
    /// A WHOLE message, complete when it arrived.
    ///
    /// A turn usually contains more than one: the provider says what it is
    /// about to do, runs its tools, then answers. Appending those the way
    /// fragments are appended ran the last word of one into the first word of
    /// the next — "…improvements.Yes…" — which reads as a rendering fault and
    /// was in the stored reply itself. Kept apart from `Text` because only a
    /// whole message is a place where a break belongs.
    Message(String),
    Activity(ActivityDraft),
    /// Usage evidence for Auto routing. Never stored, broadcast or shown.
    Usage(UsageSignal),
}

/// What a provider's own stream said about the caller's usage. Tagged with
/// the provider that emitted it, so a hint can only ever count against the
/// account that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageSignal {
    pub(crate) provider: String,
    pub(crate) kind: UsageKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UsageKind {
    /// The adapter's whitelisted `account/rateLimits` payload (Codex only).
    RateLimits(serde_json::Value),
    /// The raw STRUCTURED class the provider attached to a failure (a Codex
    /// `turn.failed.code`, a Claude assistant `error`). Never message text.
    FailureHint(String),
    /// The provider reported a clean end of turn.
    Finished,
}

/// The largest usage payload accepted from the adapter.
const MAX_USAGE_PAYLOAD_BYTES: usize = 8 * 1024;

/// Statuses the server itself emits, as opposed to relaying.
pub(crate) const STATE_TRUNCATED: &str = "truncated";
pub(crate) const STATE_UNSUPPORTED: &str = "unsupported";
/// A shared-library tool this run went ahead without (the agent cannot load it,
/// or the sandbox cannot reach it). `detail` is the tool's name.
pub(crate) const STATE_LIBRARY_TOOL_SKIPPED: &str = "library_tool_skipped";

fn truncate(value: &str, limit: usize) -> (String, bool) {
    if value.chars().count() <= limit {
        return (value.to_owned(), false);
    }
    (value.chars().take(limit).collect(), true)
}

/// Remove anything the server itself knows to be secret.
///
/// Only values we supplied — the credential/storage handle that names a user's
/// volume — are scrubbed. Tool input is model-authored content and is NOT
/// semantically scanned: pretending otherwise would be a false assurance. It is
/// carried as untrusted display text and the contract says so.
fn scrub(value: &str, needles: &[String]) -> String {
    let mut out = value.to_owned();
    for needle in needles {
        if needle.len() >= 8 && out.contains(needle.as_str()) {
            out = out.replace(needle.as_str(), "[redacted]");
        }
    }
    out
}

/// A tool block that has started but whose input is still streaming.
#[derive(Clone, Debug)]
struct PendingTool {
    id: String,
    name: String,
    input: String,
}

/// Items tracked as open at once, per kind. A message completes and is forgotten
/// straight away, so one or two entries is the real steady state; this is the
/// bound that stops a malformed stream growing the process, not a working limit.
const MAX_OPEN_ITEMS: usize = 64;

/// Maps one provider's `stream-json` lines to [`StreamItem`]s.
///
/// Stateful because a Claude tool input arrives as `input_json_delta` fragments
/// across several lines and is only complete at `content_block_stop`.
pub(crate) struct StreamParser {
    provider: String,
    open: HashMap<u64, PendingTool>,
    /// `unsupported` is reported at most once per turn; a stream we cannot read
    /// would otherwise produce one marker per line.
    unsupported_reported: bool,
    redact: Vec<String>,
    /// Codex message ids that have delivered at least one delta. Their
    /// `item.completed` carries the SAME text again, so it is dropped — without
    /// this every streamed reply would be shown twice.
    streamed: HashSet<String>,
    /// Codex command ids already announced at `item.started` with a command to
    /// show. Their `item.completed` contributes only the result.
    announced: HashSet<String>,
    /// The most recent provider-reported error class and text seen BEFORE the
    /// turn failed (Codex `error` events, Claude's assistant `error` field).
    /// Used when the terminal failure itself carries less.
    error_hint: Option<String>,
    error_message: Option<String>,
    /// Usage evidence produced while mapping the current line; appended after
    /// that line's items.
    usage: Vec<UsageSignal>,
}

impl StreamParser {
    pub(crate) fn new(provider: &str, redact: Vec<String>) -> Self {
        Self {
            provider: provider.to_owned(),
            open: HashMap::new(),
            unsupported_reported: false,
            redact: redact.into_iter().filter(|value| !value.is_empty()).collect(),
            streamed: HashSet::new(),
            announced: HashSet::new(),
            error_hint: None,
            error_message: None,
            usage: Vec::new(),
        }
    }

    fn usage_signal(&mut self, kind: UsageKind) {
        self.usage.push(UsageSignal {
            provider: self.provider.clone(),
            kind,
        });
    }

    /// The provider's own failure, classified, redacted and bounded.
    fn provider_error(&mut self, hint: Option<&str>, message: &str) -> StreamItem {
        let message = if message.trim().is_empty() {
            self.error_message.take().unwrap_or_default()
        } else {
            message.to_owned()
        };
        let hint = hint.map(str::to_owned).or_else(|| self.error_hint.take());
        if let Some(hint) = hint.as_deref().filter(|value| !value.is_empty()) {
            self.usage_signal(UsageKind::FailureHint(hint.chars().take(64).collect()));
        }
        let category = super::provider_error::classify(hint.as_deref(), &message);
        let safe = super::provider_error::sanitize(&message, &self.redact);
        StreamItem::Activity(super::provider_error::draft(category, safe))
    }

    /// A `turn.failed` envelope, in either dialect: the adapters write
    /// `reason` (+ optional `code`), `codex exec` writes `error.message`.
    /// The `result`/`failed` marker is kept for readers that know only it.
    fn turn_failed(&mut self, value: &serde_json::Value) -> Vec<StreamItem> {
        let reason = value
            .get("reason")
            .and_then(|v| v.as_str())
            .or_else(|| {
                value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or_default()
            .to_owned();
        let hint = value.get("code").and_then(|v| v.as_str()).map(str::to_owned);
        vec![
            self.status("result", "result", Some("failed".to_owned())),
            self.provider_error(hint.as_deref(), &reason),
        ]
    }

    /// Remember an error the provider reported before (or instead of) failing.
    fn remember_error(&mut self, hint: Option<&str>, message: Option<&str>) {
        if let Some(hint) = hint.filter(|value| !value.is_empty()) {
            self.error_hint = Some(hint.chars().take(64).collect());
        }
        if let Some(message) = message.filter(|value| !value.trim().is_empty()) {
            self.error_message = Some(message.chars().take(4_096).collect());
        }
    }

    fn call(&self, id: String, name: String, raw_input: &str) -> StreamItem {
        let (input, input_truncated) =
            truncate(&scrub(raw_input, &self.redact), MAX_INPUT_CHARS);
        StreamItem::Activity(ActivityDraft {
            id: id.clone(),
            kind: ActivityKind::ToolCall {
                name: scrub(&name, &self.redact),
                input,
                input_truncated,
            },
        })
    }

    fn result(&self, call_id: String, raw_output: &str, is_error: bool) -> StreamItem {
        let (output, output_truncated) =
            truncate(&scrub(raw_output, &self.redact), MAX_OUTPUT_CHARS);
        StreamItem::Activity(ActivityDraft {
            id: format!("result:{call_id}"),
            kind: ActivityKind::ToolResult {
                call_id,
                output,
                output_truncated,
                is_error,
            },
        })
    }

    fn status(&self, id: &str, state: &str, detail: Option<String>) -> StreamItem {
        StreamItem::Activity(ActivityDraft {
            id: id.to_owned(),
            kind: ActivityKind::Status {
                state: state.to_owned(),
                detail: detail.map(|value| {
                    let (short, _) = truncate(&scrub(&value, &self.redact), 200);
                    short
                }),
                category: None,
            },
        })
    }

    /// Map one line. A line may produce nothing (noise, or hidden reasoning we
    /// deliberately drop), one item, or several.
    pub(crate) fn push(&mut self, line: &str) -> Vec<StreamItem> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            // Not JSON at all: provider chatter, not a trace. Ignored rather
            // than surfaced, because it is not activity and not text either.
            return Vec::new();
        };
        let mapped = match self.provider.as_str() {
            "claude" => self.claude(&value),
            // The ACP adapter (`neev_acp_stream_adapter.py`) writes the Codex
            // item dialect, so Cursor, Gemini CLI and OpenCode share its mapping.
            "codex" | "cursor" | "gemini" | "opencode" => self.codex(&value),
            _ => None,
        };
        match mapped {
            Some(mut items) => {
                items.extend(self.usage.drain(..).map(StreamItem::Usage));
                items
            }
            None => {
                // Nothing known matched. Say so once, so the UI can report
                // "no trace available" instead of showing an empty one.
                if self.unsupported_reported {
                    return Vec::new();
                }
                self.unsupported_reported = true;
                vec![self.status(
                    "unsupported",
                    STATE_UNSUPPORTED,
                    Some(self.provider.clone()),
                )]
            }
        }
    }

    /// Anything still open when the stream ends: a tool whose block never
    /// closed still ran, so it is reported with the input we saw.
    pub(crate) fn finish(&mut self) -> Vec<StreamItem> {
        let mut pending: Vec<_> = self.open.drain().collect();
        pending.sort_by_key(|(index, _)| *index);
        pending
            .into_iter()
            .map(|(_, tool)| self.call(tool.id, tool.name, &tool.input))
            .collect()
    }

    /// Claude Code headless: `--output-format stream-json
    /// --include-partial-messages --verbose`.
    ///
    /// `None` means "not a shape this branch knows"; `Some(vec![])` means
    /// "known and deliberately carries nothing" — the difference is what keeps
    /// an ordinary `assistant` envelope from being reported as unsupported.
    fn claude(&mut self, value: &serde_json::Value) -> Option<Vec<StreamItem>> {
        let outer = value.get("type")?.as_str()?;
        match outer {
            "stream_event" => {
                let event = value.get("event")?;
                let kind = event.get("type")?.as_str()?;
                match kind {
                    "content_block_start" => {
                        let index = event.get("index")?.as_u64()?;
                        let block = event.get("content_block")?;
                        if block.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                            // Text or thinking: nothing to record here.
                            return Some(Vec::new());
                        }
                        self.open.insert(
                            index,
                            PendingTool {
                                id: block
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("tool")
                                    .to_owned(),
                                name: block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("tool")
                                    .to_owned(),
                                input: String::new(),
                            },
                        );
                        // Announced NOW, with no input yet. A tool that runs
                        // for thirty seconds was previously invisible until it
                        // finished, which is the whole of "nothing is
                        // happening" from the reader's side. The same id is
                        // re-emitted with the full input once the block closes.
                        let tool = self.open.get(&index)?;
                        Some(vec![self.call(tool.id.clone(), tool.name.clone(), "")])
                    }
                    "content_block_delta" => {
                        let delta = event.get("delta")?;
                        match delta.get("type")?.as_str()? {
                            "text_delta" => Some(vec![StreamItem::Text(
                                delta.get("text")?.as_str()?.to_owned(),
                            )]),
                            "input_json_delta" => {
                                let index = event.get("index")?.as_u64()?;
                                let fragment =
                                    delta.get("partial_json").and_then(|v| v.as_str())?;
                                if let Some(tool) = self.open.get_mut(&index) {
                                    // Bounded while accumulating: a runaway
                                    // input must not grow the process either.
                                    if tool.input.chars().count() < MAX_INPUT_CHARS * 2 {
                                        tool.input.push_str(fragment);
                                    }
                                }
                                Some(Vec::new())
                            }
                            // Hidden reasoning: recognised so it is not
                            // "unsupported", and dropped on purpose.
                            "thinking_delta" | "signature_delta" => Some(Vec::new()),
                            _ => Some(Vec::new()),
                        }
                    }
                    "content_block_stop" => {
                        let index = event.get("index")?.as_u64()?;
                        let Some(tool) = self.open.remove(&index) else {
                            return Some(Vec::new());
                        };
                        Some(vec![self.call(tool.id, tool.name, &tool.input)])
                    }
                    "message_delta" => {
                        let reason = event
                            .get("delta")
                            .and_then(|delta| delta.get("stop_reason"))
                            .and_then(|value| value.as_str());
                        Some(match reason {
                            Some(reason) => {
                                vec![self.status("stop", "stop", Some(reason.to_owned()))]
                            }
                            None => Vec::new(),
                        })
                    }
                    _ => Some(Vec::new()),
                }
            }
            "system" => {
                if value.get("subtype").and_then(|v| v.as_str()) != Some("init") {
                    return Some(Vec::new());
                }
                let model = value
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
                Some(vec![self.status("init", "model", model)])
            }
            // A tool's output comes back as a synthetic user turn.
            "user" => {
                let content = value.get("message")?.get("content")?.as_array()?;
                let mut items = Vec::new();
                for entry in content {
                    if entry.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
                        continue;
                    }
                    let call_id = entry
                        .get("tool_use_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool")
                        .to_owned();
                    let is_error = entry
                        .get("is_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    items.push(self.result(call_id, &flatten_content(entry.get("content")), is_error));
                }
                Some(items)
            }
            "result" => {
                let subtype = value
                    .get("subtype")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
                let failed = value.get("is_error").and_then(|v| v.as_bool()) == Some(true)
                    || subtype.as_deref().is_some_and(|kind| kind.starts_with("error"));
                let mut items = vec![self.status("result", "result", subtype)];
                if !failed {
                    self.usage_signal(UsageKind::Finished);
                }
                if failed {
                    // Claude's own words for why: `result` on an API error
                    // ("API Error: 401 … OAuth token has expired"), `errors[]`
                    // on an execution error.
                    let message = value
                        .get("result")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                        .or_else(|| {
                            value.get("errors").and_then(|v| v.as_array()).map(|errors| {
                                errors
                                    .iter()
                                    .filter_map(|e| e.as_str())
                                    .collect::<Vec<_>>()
                                    .join("; ")
                            })
                        })
                        .unwrap_or_default();
                    items.push(self.provider_error(None, &message));
                }
                Some(items)
            }
            // The assembled assistant message; its text already arrived as
            // deltas, so recording it again would duplicate the reply. An
            // assistant message that IS an error (`error` set, e.g.
            // `authentication_failed`) is remembered so the failure can say so.
            "assistant" => {
                if let Some(kind) = value.get("error").and_then(|v| v.as_str()) {
                    let text = value
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                        .and_then(|blocks| {
                            blocks
                                .iter()
                                .find_map(|b| b.get("text").and_then(|t| t.as_str()))
                        });
                    self.remember_error(Some(kind), text);
                }
                Some(Vec::new())
            }
            // The streaming adapter's own failure envelope.
            "turn.failed" => Some(self.turn_failed(value)),
            // A Claude stream we can read, carrying an event this version does
            // not map — a heartbeat, a new envelope type. Known-and-ignored,
            // NOT unsupported: telling the reader their provider is unsupported
            // because of a keepalive would be false.
            _ => Some(Vec::new()),
        }
    }

    /// Codex CLI, in both of the shapes this deployment runs.
    ///
    /// `codex exec --json` emits an envelope per finished item, so a reply
    /// appears all at once when the turn is already over. The app-server
    /// adapter (`neev_codex_stream_adapter.py`, inside the sandbox) speaks the
    /// SAME envelopes plus `item.delta`, so the two paths share one mapping
    /// here and the flag that chooses between them changes nothing else.
    ///
    /// Still conservative: only items whose meaning is unambiguous are mapped,
    /// and anything unknown falls through to `unsupported` rather than being
    /// guessed into a trace that reads as authoritative.
    fn codex(&mut self, value: &serde_json::Value) -> Option<Vec<StreamItem>> {
        // Codex may also be run with the Claude-compatible stream; if it looks
        // like one, read it as one.
        if matches!(
            value.get("type").and_then(|v| v.as_str()),
            Some("stream_event" | "system" | "user" | "result" | "assistant")
        ) {
            return self.claude(value);
        }
        let outer = value.get("type")?.as_str()?;
        match outer {
            // A fragment of a message still being written. Only the adapter
            // produces these; `exec` has no token-level output at all.
            "item.delta" => {
                let item = value.get("item")?;
                if item.get("type")?.as_str()? != "agent_message" {
                    // A delta for something we do not render as prose (a plan,
                    // a reasoning summary). Recognised and dropped.
                    return Some(Vec::new());
                }
                let delta = item.get("delta")?.as_str()?;
                if delta.is_empty() {
                    // Nothing to show, and marking the message as started on an
                    // empty fragment would cost the next one its paragraph break.
                    return Some(Vec::new());
                }
                let id = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned();
                if self.streamed.contains(&id) {
                    // Mid-message: appended exactly as it arrives, because these
                    // are fragments of one sentence.
                    return Some(vec![StreamItem::Text(delta.to_owned())]);
                }
                if self.streamed.len() < MAX_OPEN_ITEMS {
                    self.streamed.insert(id);
                }
                // The FIRST fragment of a message begins where the last one
                // ended rather than running on from it — the same break a whole
                // message gets, taken at the moment the message starts.
                Some(vec![StreamItem::Message(delta.to_owned())])
            }
            "item.started" | "item.completed" | "item.updated" => {
                let item = value.get("item")?;
                let item_type = item.get("type")?.as_str()?;
                if outer == "item.updated" {
                    // Nothing here is an outcome: an updated item is re-sent
                    // whole at completion.
                    return Some(Vec::new());
                }
                let id = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned();
                if outer == "item.started" {
                    // A command that runs for thirty seconds is announced NOW,
                    // with no output yet; the same id is re-sent at completion
                    // and the log revises that row in place rather than adding
                    // a second one. Everything else waits for its outcome.
                    if item_type != "command_execution" {
                        return Some(Vec::new());
                    }
                    let command = item
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    if !command.is_empty() && self.announced.len() < MAX_OPEN_ITEMS {
                        self.announced.insert(id.clone());
                    }
                    let announced = if id.is_empty() { "command".to_owned() } else { id };
                    return Some(vec![self.call(announced, "command".to_owned(), command)]);
                }
                match item_type {
                    "agent_message" => {
                        // Already delivered fragment by fragment: the completed
                        // item repeats it in full, and sending it again would
                        // show the reader their reply twice.
                        if self.streamed.remove(&id) {
                            return Some(Vec::new());
                        }
                        let text = item.get("text").and_then(|v| v.as_str())?;
                        // `item.completed` means this message is finished, so
                        // it is a whole one and the next is a new one.
                        Some(vec![StreamItem::Message(text.to_owned())])
                    }
                    "command_execution" => {
                        let announced = self.announced.remove(&id);
                        let id = if id.is_empty() { "command".to_owned() } else { id };
                        let command = item
                            .get("command")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        let exit = item.get("exit_code").and_then(|v| v.as_i64());
                        let output = item
                            .get("aggregated_output")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        let mut items = Vec::new();
                        if !announced {
                            items.push(self.call(id.clone(), "command".to_owned(), command));
                        }
                        items.push(self.result(id, output, exit.unwrap_or(0) != 0));
                        Some(items)
                    }
                    // Reasoning items are recognised and dropped.
                    "reasoning" => Some(Vec::new()),
                    _ => Some(Vec::new()),
                }
            }
            // The ACP adapter's own notice. Only the known state is recorded,
            // and only for a name in the adapter's tool-name alphabet.
            "neev.notice" => {
                if value.get("state").and_then(|v| v.as_str()) != Some(STATE_LIBRARY_TOOL_SKIPPED) {
                    return Some(Vec::new());
                }
                let name = value.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                if name.is_empty()
                    || name.len() > 200
                    || !name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                {
                    return Some(Vec::new());
                }
                Some(vec![self.status(
                    &format!("library-tool-{name}"),
                    STATE_LIBRARY_TOOL_SKIPPED,
                    Some(name.to_owned()),
                )])
            }
            "thread.started" | "turn.started" => Some(Vec::new()),
            "turn.completed" => {
                self.usage_signal(UsageKind::Finished);
                Some(vec![self.status("result", "result", None)])
            }
            // The Codex adapter's whitelisted rate-limit report. Accepted only
            // from Codex and only from its documented source; it is evidence
            // for routing, never activity.
            "neev.usage" => {
                let payload = value.get("payload");
                let accepted = self.provider == "codex"
                    && value.get("source").and_then(|v| v.as_str()) == Some("codex.rateLimits")
                    && payload.is_some_and(|payload| {
                        payload.is_object()
                            && serde_json::to_vec(payload)
                                .map(|bytes| bytes.len() <= MAX_USAGE_PAYLOAD_BYTES)
                                .unwrap_or(false)
                    });
                if accepted {
                    if let Some(payload) = payload {
                        self.usage_signal(UsageKind::RateLimits(payload.clone()));
                    }
                }
                Some(Vec::new())
            }
            "turn.failed" => Some(self.turn_failed(value)),
            // `codex exec` reports errors (including transient reconnects) as
            // their own envelope before `turn.failed`. Remembered, not shown:
            // only a failure that ends the turn is the turn's error.
            "error" => {
                let message = value.get("message").and_then(|v| v.as_str());
                self.remember_error(None, message);
                Some(Vec::new())
            }
            _ => None,
        }
    }
}

/// A tool result's content is a string, or a list of content blocks. Flattened
/// to the text a reader would want to see; non-text blocks are named, not
/// rendered, so an image block does not become a wall of base64.
fn flatten_content(value: Option<&serde_json::Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    let Some(entries) = value.as_array() else {
        return value.to_string();
    };
    let mut parts = Vec::new();
    for entry in entries {
        match entry.get("type").and_then(|v| v.as_str()) {
            Some("text") => parts.push(
                entry
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            ),
            Some(other) => parts.push(format!("[{other}]")),
            None => {}
        }
    }
    parts.join("\n")
}

/// The turn's activity as it accumulates, with every cap enforced here rather
/// than trusted to the callers.
#[derive(Debug, Default)]
pub(crate) struct ActivityLog {
    items: Vec<ActivityItem>,
    next_seq: u32,
    chars: usize,
    truncated: bool,
    /// The one terminal error step has been recorded (it is never capped out,
    /// and never recorded twice).
    errored: bool,
    /// Bytes still held back for supervision steps. Zero for ordinary turns.
    reserve: usize,
}

impl ActivityLog {
    pub(crate) fn new() -> Self {
        Self {
            items: Vec::new(),
            next_seq: 1,
            // The empty array's own two bytes, so the budget counts the
            // document rather than its contents.
            chars: 2,
            truncated: false,
            errored: false,
            reserve: 0,
        }
    }

    /// A supervised turn's log: [`SUPERVISION_RESERVE`] bytes are held back
    /// from every ordinary step and spent only by [`Self::push_supervision`].
    pub(crate) fn with_supervision_reserve() -> Self {
        Self {
            reserve: SUPERVISION_RESERVE,
            ..Self::new()
        }
    }

    /// Restore a stored log so a reattached run keeps numbering where it left
    /// off instead of repeating `seq` values the client already has.
    pub(crate) fn resume(items: Vec<ActivityItem>) -> Self {
        let next_seq = items.iter().map(|item| item.seq).max().unwrap_or(0) + 1;
        let chars = serialize(&items).len();
        let truncated = items.iter().any(|item| {
            matches!(&item.kind, ActivityKind::Status { state, .. } if state == STATE_TRUNCATED)
        });
        let errored = items.iter().any(|item| is_error_step(&item.kind));
        Self {
            items,
            next_seq,
            chars,
            truncated,
            errored,
            reserve: 0,
        }
    }

    pub(crate) fn items(&self) -> &[ActivityItem] {
        &self.items
    }

    /// This log with every item reduced to `ActivityKind::names_only` (kind,
    /// tool name, status, seq kept; input/output emptied; `truncated` kept).
    pub(crate) fn names_only(&self) -> ActivityLog {
        let items: Vec<ActivityItem> = self
            .items
            .iter()
            .map(|item| ActivityItem {
                seq: item.seq,
                offset: 0,
                id: item.id.clone(),
                kind: item.kind.names_only(),
            })
            .collect();
        let chars = serialize(&items).len();
        ActivityLog {
            items,
            next_seq: self.next_seq,
            chars,
            truncated: self.truncated,
            errored: self.errored,
            reserve: self.reserve,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Record one step at `offset`, or return `None` when a cap refuses it.
    ///
    /// A tool call announced at its start and re-sent with its arguments when
    /// the block closes is ONE step, not two: the second push finds the first
    /// by id and revises it in place, keeping the original `seq` and `offset`.
    /// The client upserts by `seq`, so it sees the row it already drew fill in
    /// rather than a duplicate appearing beneath it.
    ///
    /// The first refusal appends a `truncated` marker (itself the last item),
    /// so a trace that stops early always says that it did.
    ///
    /// Budgeting is deliberately pessimistic. `chars` tracks the size of the
    /// serialised ARRAY, not the sum of its items — the brackets and the commas
    /// between them are real bytes, and undercounting them is how a log that
    /// believes it fits produces a document that does not. A marker-sized
    /// reserve is held back on top, so the `truncated` marker is always
    /// affordable at the moment it is needed.
    pub(crate) fn push(&mut self, draft: ActivityDraft, offset: usize) -> Option<ActivityItem> {
        if is_error_step(&draft.kind) {
            return self.push_error(draft, offset);
        }
        if self.truncated {
            return None;
        }
        if let ActivityKind::ToolCall { .. } = &draft.kind {
            if let Some(position) = self.items.iter().position(|item| {
                item.id == draft.id && matches!(item.kind, ActivityKind::ToolCall { .. })
            }) {
                // An update GROWS the document: the announcement carried no
                // arguments and the revision carries all of them. Budgeting the
                // delta is what stops a chatty tool walking past the cap
                // through a door the append path guards and this one did not.
                let existing = &self.items[position];
                let before = serde_json::to_string(existing).map(|t| t.len()).unwrap_or(0);
                let revised = ActivityItem {
                    seq: existing.seq,
                    offset: existing.offset,
                    id: existing.id.clone(),
                    kind: draft.kind,
                };
                let after = serde_json::to_string(&revised).map(|t| t.len()).unwrap_or(0);
                let grown = after.saturating_sub(before);
                // The error reserve is held back here too: a revision grows
                // a step by up to `MAX_INPUT_CHARS`, and must not spend the
                // space the terminal error is promised.
                if self.chars + grown + MARKER_RESERVE + ERROR_RESERVE + self.reserve
                    > MAX_ACTIVITY_CHARS
                {
                    // Keep the announcement, which is delivered and still true,
                    // and record that the trace stopped growing.
                    return self.mark_truncated(offset);
                }
                self.chars += grown;
                self.items[position] = revised.clone();
                return Some(revised);
            }
        }
        let item = ActivityItem {
            seq: self.next_seq,
            offset,
            id: draft.id,
            kind: draft.kind,
        };
        // `+ 1` is the separator this item needs once it is not the first.
        let cost = serde_json::to_string(&item)
            .map(|text| text.len() + 1)
            .unwrap_or(0);
        if self.items.len() + 1 >= MAX_ITEMS
            || self.chars + cost + MARKER_RESERVE + ERROR_RESERVE + self.reserve
                > MAX_ACTIVITY_CHARS
        {
            return self.mark_truncated(offset);
        }
        self.next_seq += 1;
        self.chars += cost;
        self.items.push(item.clone());
        Some(item)
    }

    /// The terminal provider error: recorded even after the trace was capped,
    /// out of the reserve every other step leaves untouched. Past the item cap
    /// by at most one, which is ours to allow — the stored CHECK is on size.
    fn push_error(&mut self, draft: ActivityDraft, offset: usize) -> Option<ActivityItem> {
        if self.errored {
            return None;
        }
        let item = ActivityItem {
            seq: self.next_seq,
            offset,
            id: draft.id,
            kind: draft.kind,
        };
        let cost = serde_json::to_string(&item)
            .map(|text| text.len() + 1)
            .unwrap_or(0);
        // Never refused for size: every other path leaves `ERROR_RESERVE`, and
        // should the budget still be over, `stored()` trims OTHER items and
        // keeps this one. Losing the reason is the one outcome not allowed.
        self.errored = true;
        self.next_seq += 1;
        self.chars += cost;
        self.items.push(item.clone());
        Some(item)
    }

    /// Record or revise one supervision step (see [`STATE_PLAN`] and friends).
    ///
    /// One step per phase: a second push with the same id revises the first in
    /// place, keeping its `seq` and `offset`, so `plan_pending` BECOMES `plan`
    /// rather than leaving a stale "running" row behind it. Like the terminal
    /// error, it ignores `truncated` and the item cap and spends the reserve the
    /// other steps left alone. A detail too large for what remains is
    /// shortened, never dropped: the step itself (and its `category`, the
    /// verdict) is what must survive. Returns `None` only for a draft that is
    /// not a supervision step, or if not even the bare step fits.
    pub(crate) fn push_supervision(
        &mut self,
        draft: ActivityDraft,
        offset: usize,
    ) -> Option<ActivityItem> {
        if !is_supervision_step(&draft.id, &draft.kind) {
            return None;
        }
        let ActivityKind::Status {
            state,
            mut detail,
            category,
        } = draft.kind
        else {
            return None;
        };
        let existing = self.items.iter().position(|item| item.id == draft.id);
        let (seq, at, before) = match existing {
            Some(position) => {
                let item = &self.items[position];
                (item.seq, item.offset, item_cost(item))
            }
            None => (self.next_seq, offset, 0),
        };
        // The terminal error keeps its promise; the marker reserve is kept too
        // so an executor still running (plan step) can still say it was cut.
        let held = MARKER_RESERVE + if self.errored { 0 } else { ERROR_RESERVE };
        loop {
            let item = ActivityItem {
                seq,
                offset: at,
                id: draft.id.clone(),
                kind: ActivityKind::Status {
                    state: state.clone(),
                    detail: detail.clone(),
                    category: category.clone(),
                },
            };
            let cost = item_cost(&item);
            let after = self.chars.saturating_sub(before) + cost;
            if after + held <= MAX_ACTIVITY_CHARS {
                self.reserve = self.reserve.saturating_sub(cost.saturating_sub(before));
                self.chars = after;
                match existing {
                    Some(position) => self.items[position] = item.clone(),
                    None => {
                        self.next_seq += 1;
                        self.items.push(item.clone());
                    }
                }
                return Some(item);
            }
            detail = match detail {
                Some(text) if text.chars().count() > 16 => {
                    let keep = text.chars().count() / 2;
                    Some(format!("{}…", text.chars().take(keep).collect::<String>()))
                }
                Some(_) => None,
                None => return None,
            };
        }
    }

    fn mark_truncated(&mut self, offset: usize) -> Option<ActivityItem> {
        self.truncated = true;
        let item = ActivityItem {
            seq: self.next_seq,
            offset,
            id: STATE_TRUNCATED.to_owned(),
            kind: ActivityKind::Status {
                state: STATE_TRUNCATED.to_owned(),
                detail: None,
                category: None,
            },
        };
        self.next_seq += 1;
        self.items.push(item.clone());
        Some(item)
    }

    /// The stored form: `None` when there is nothing, so a turn without tools
    /// stores NULL rather than an empty array.
    ///
    /// A non-empty log ALWAYS stores something. If the budget was somehow still
    /// exceeded, the newest items are dropped — deterministically, from the end
    /// — and the `truncated` marker is re-attached, because a reader who saw a
    /// trace live must not find it gone after a reload. Returning `None` here
    /// would discard the whole thing to save the tail of it.
    pub(crate) fn stored(&self) -> Option<String> {
        if self.items.is_empty() {
            return None;
        }
        let text = serialize(&self.items);
        if text.len() <= MAX_ACTIVITY_CHARS {
            return Some(text);
        }
        // The terminal error is a step a trimmed trace must keep: it is the
        // stored reason the turn failed. Supervision steps likewise: they are
        // the plan and the verdict the reader was shown.
        let pinned: Vec<ActivityItem> = self
            .items
            .iter()
            .filter(|item| is_pinned(item))
            .cloned()
            .collect();
        let pinned_seq = pinned.iter().map(|item| item.seq).max().unwrap_or(0);
        let mut kept: Vec<ActivityItem> = self
            .items
            .iter()
            .filter(|item| !is_pinned(item))
            .cloned()
            .collect();
        while !kept.is_empty() {
            kept.pop();
            let mut candidate = kept.clone();
            candidate.push(ActivityItem {
                // Numbered past everything kept, so a client that already has
                // the earlier items still upserts this cleanly.
                seq: kept
                    .last()
                    .map(|item| item.seq)
                    .unwrap_or(0)
                    .max(pinned_seq)
                    + 1,
                offset: kept.last().map(|item| item.offset).unwrap_or(0),
                id: STATE_TRUNCATED.to_owned(),
                kind: ActivityKind::Status {
                    state: STATE_TRUNCATED.to_owned(),
                    detail: None,
                    category: None,
                },
            });
            candidate.extend(pinned.iter().cloned());
            let text = serialize(&candidate);
            if text.len() <= MAX_ACTIVITY_CHARS {
                return Some(text);
            }
        }
        // Even one item plus a marker does not fit: store the marker alone, so
        // the turn still reports that a trace existed and was cut.
        let mut marker = vec![ActivityItem {
            seq: 1,
            offset: 0,
            id: STATE_TRUNCATED.to_owned(),
            kind: ActivityKind::Status {
                state: STATE_TRUNCATED.to_owned(),
                detail: None,
                category: None,
            },
        }];
        if !pinned.is_empty() {
            // `seq` is the client's dedupe key: the marker must never share one.
            marker[0].seq = pinned_seq + 1;
            let mut with_pinned = pinned;
            with_pinned.extend(marker);
            marker = with_pinned;
        }
        Some(serialize(&marker))
    }
}

/// What one item adds to the stored array: itself plus its separator.
fn item_cost(item: &ActivityItem) -> usize {
    serde_json::to_string(item)
        .map(|text| text.len() + 1)
        .unwrap_or(0)
}

fn serialize(items: &[ActivityItem]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".to_owned())
}

/// Read a stored log back. Corrupt or oversized storage reads as no activity:
/// a turn's text is the thing that matters and must never fail to load because
/// its decoration did.
pub(crate) fn parse_stored(value: Option<&str>) -> Vec<ActivityItem> {
    let Some(text) = value else {
        return Vec::new();
    };
    if text.len() > MAX_ACTIVITY_CHARS {
        return Vec::new();
    }
    serde_json::from_str::<Vec<ActivityItem>>(text).unwrap_or_default()
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod activity_tests;
