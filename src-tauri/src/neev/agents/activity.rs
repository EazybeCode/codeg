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

use std::collections::HashMap;

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
}

/// Statuses the server itself emits, as opposed to relaying.
pub(crate) const STATE_TRUNCATED: &str = "truncated";
pub(crate) const STATE_UNSUPPORTED: &str = "unsupported";

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
}

impl StreamParser {
    pub(crate) fn new(provider: &str, redact: Vec<String>) -> Self {
        Self {
            provider: provider.to_owned(),
            open: HashMap::new(),
            unsupported_reported: false,
            redact: redact.into_iter().filter(|value| !value.is_empty()).collect(),
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
            "codex" => self.codex(&value),
            _ => None,
        };
        match mapped {
            Some(items) => items,
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
                Some(vec![self.status("result", "result", subtype)])
            }
            // The assembled assistant message; its text already arrived as
            // deltas, so recording it again would duplicate the reply.
            "assistant" => Some(Vec::new()),
            // A Claude stream we can read, carrying an event this version does
            // not map — a heartbeat, a new envelope type. Known-and-ignored,
            // NOT unsupported: telling the reader their provider is unsupported
            // because of a keepalive would be false.
            _ => Some(Vec::new()),
        }
    }

    /// Codex CLI — BEST EFFORT, and deliberately conservative.
    ///
    /// The runner currently launches the codex binary with Claude's flags, so
    /// this shape is unverified against a real stream. Only items whose meaning
    /// is unambiguous are mapped; anything else falls through to `unsupported`
    /// rather than being guessed into a trace that reads as authoritative.
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
            "item.started" | "item.completed" | "item.updated" => {
                let item = value.get("item")?;
                let item_type = item.get("type")?.as_str()?;
                // Only completed items carry an outcome worth recording; a
                // started one would duplicate into a second entry.
                if outer != "item.completed" {
                    return Some(Vec::new());
                }
                match item_type {
                    "agent_message" => {
                        let text = item.get("text").and_then(|v| v.as_str())?;
                        // `item.completed` means this message is finished, so
                        // it is a whole one and the next is a new one.
                        Some(vec![StreamItem::Message(text.to_owned())])
                    }
                    "command_execution" => {
                        let id = item
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("command")
                            .to_owned();
                        let command = item
                            .get("command")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        let exit = item.get("exit_code").and_then(|v| v.as_i64());
                        let output = item
                            .get("aggregated_output")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        Some(vec![
                            self.call(id.clone(), "command".to_owned(), command),
                            self.result(id, output, exit.unwrap_or(0) != 0),
                        ])
                    }
                    // Reasoning items are recognised and dropped.
                    "reasoning" => Some(Vec::new()),
                    _ => Some(Vec::new()),
                }
            }
            "thread.started" | "turn.started" => Some(Vec::new()),
            "turn.completed" => Some(vec![self.status("result", "result", None)]),
            "turn.failed" => Some(vec![self.status(
                "result",
                "result",
                Some("failed".to_owned()),
            )]),
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
        Self {
            items,
            next_seq,
            chars,
            truncated,
        }
    }

    pub(crate) fn items(&self) -> &[ActivityItem] {
        &self.items
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
                if self.chars + grown + MARKER_RESERVE > MAX_ACTIVITY_CHARS {
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
            || self.chars + cost + MARKER_RESERVE > MAX_ACTIVITY_CHARS
        {
            return self.mark_truncated(offset);
        }
        self.next_seq += 1;
        self.chars += cost;
        self.items.push(item.clone());
        Some(item)
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
        let mut kept: Vec<ActivityItem> = self.items.clone();
        while !kept.is_empty() {
            kept.pop();
            let mut candidate = kept.clone();
            candidate.push(ActivityItem {
                // Numbered past everything kept, so a client that already has
                // the earlier items still upserts this cleanly.
                seq: kept.last().map(|item| item.seq).unwrap_or(0) + 1,
                offset: kept.last().map(|item| item.offset).unwrap_or(0),
                id: STATE_TRUNCATED.to_owned(),
                kind: ActivityKind::Status {
                    state: STATE_TRUNCATED.to_owned(),
                    detail: None,
                },
            });
            let text = serialize(&candidate);
            if text.len() <= MAX_ACTIVITY_CHARS {
                return Some(text);
            }
        }
        // Even one item plus a marker does not fit: store the marker alone, so
        // the turn still reports that a trace existed and was cut.
        let marker = vec![ActivityItem {
            seq: 1,
            offset: 0,
            id: STATE_TRUNCATED.to_owned(),
            kind: ActivityKind::Status {
                state: STATE_TRUNCATED.to_owned(),
                detail: None,
            },
        }];
        Some(serialize(&marker))
    }
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
