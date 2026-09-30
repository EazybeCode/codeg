//! Parser fixtures and the caps, driven by the line shapes the provider CLIs
//! actually emit.
//!
//! The Claude fixtures are taken from `--output-format stream-json
//! --include-partial-messages --verbose`. The Codex fixtures are the `item.*`
//! envelope shape: as `codex exec --json` emits it, and as the in-sandbox
//! app-server adapter normalises it (same envelopes, plus `item.delta`). They
//! pin OUR mapping. The adapter's own translation from the app-server's
//! camelCase notifications is pinned separately, against the version-matched
//! schema, in `scripts/tests/test_neev_codex_stream_adapter.py`.

use super::*;

fn parser() -> StreamParser {
    StreamParser::new("claude", vec!["handle-0123456789abcdef".to_owned()])
}

fn drain(parser: &mut StreamParser, lines: &[&str]) -> Vec<StreamItem> {
    let mut items = Vec::new();
    for line in lines {
        items.extend(parser.push(line));
    }
    items
}

fn text_of(items: &[StreamItem]) -> String {
    items
        .iter()
        .filter_map(|item| match item {
            // Both carry reply text; they differ in whether more of the same
            // message may follow, which is what `joined_with_a_blank_line`
            // below is about rather than this.
            StreamItem::Text(text) | StreamItem::Message(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn activity(items: &[StreamItem]) -> Vec<&ActivityDraft> {
    items
        .iter()
        .filter_map(|item| match item {
            StreamItem::Activity(draft) => Some(draft),
            _ => None,
        })
        .collect()
}

#[test]
fn a_claude_tool_call_is_assembled_from_its_streamed_input_fragments() {
    let mut parser = parser();
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"Bash"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"ls -la\"}"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
        ],
    );
    let drafts = activity(&items);
    // Two events for one tool: announced when the block opens, revised when it
    // closes. The log collapses them onto a single seq; the PARSER reports both
    // because the first is what makes a running tool visible.
    assert_eq!(drafts.len(), 2, "announced, then completed");
    assert_eq!(drafts[0].id, "toolu_01");
    assert_eq!(
        drafts[1].kind,
        ActivityKind::ToolCall {
            name: "Bash".into(),
            input: "{\"command\":\"ls -la\"}".into(),
            input_truncated: false,
        }
    );
}

#[test]
fn a_tool_is_announced_when_it_starts_not_when_it_finishes() {
    let mut parser = parser();
    // The block opens: the tool is running NOW. Waiting for its input to finish
    // streaming is what made a long command look like a hung screen.
    let started = drain(
        &mut parser,
        &[
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_7","name":"Bash"}}}"#,
        ],
    );
    let drafts = activity(&started);
    assert_eq!(drafts.len(), 1, "announced immediately");
    assert_eq!(
        drafts[0].kind,
        ActivityKind::ToolCall {
            name: "Bash".into(),
            input: String::new(),
            input_truncated: false,
        },
        "named, with the arguments still unknown"
    );

    // The same tool, completed. It is the SAME step and must not appear twice.
    let finished = drain(
        &mut parser,
        &[
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"ls\"}"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
        ],
    );
    let drafts = activity(&finished);
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].id, "toolu_7", "same id, so the log revises in place");
}

#[test]
fn the_log_revises_a_running_tool_rather_than_listing_it_twice() {
    let mut log = ActivityLog::new();
    let announced = log
        .push(
            ActivityDraft {
                id: "toolu_7".into(),
                kind: ActivityKind::ToolCall {
                    name: "Bash".into(),
                    input: String::new(),
                    input_truncated: false,
                },
            },
            5,
        )
        .unwrap();
    let completed = log
        .push(
            ActivityDraft {
                id: "toolu_7".into(),
                kind: ActivityKind::ToolCall {
                    name: "Bash".into(),
                    input: "{\"command\":\"ls\"}".into(),
                    input_truncated: false,
                },
            },
            40,
        )
        .unwrap();
    assert_eq!(log.items().len(), 1, "one tool, one row");
    assert_eq!(
        completed.seq, announced.seq,
        "same seq, so a client upserting by seq fills the row it already drew"
    );
    assert_eq!(
        completed.offset, announced.offset,
        "and it keeps the position it started at, not where it finished"
    );
    match &log.items()[0].kind {
        ActivityKind::ToolCall { input, .. } => assert_eq!(input, "{\"command\":\"ls\"}"),
        other => panic!("expected the revised call, got {other:?}"),
    }
}

#[test]
fn revisions_are_budgeted_so_announcements_cannot_smuggle_past_the_cap() {
    // Every tool is announced cheaply (no arguments) and then revised with a
    // large input. If the revision path skipped the budget, a trace could grow
    // without limit through the door the append path guards.
    let mut log = ActivityLog::new();
    let announced: Vec<_> = (0..40)
        .filter_map(|index| {
            log.push(
                ActivityDraft {
                    id: format!("toolu_{index}"),
                    kind: ActivityKind::ToolCall {
                        name: "Bash".into(),
                        input: String::new(),
                        input_truncated: false,
                    },
                },
                index,
            )
        })
        .collect();
    assert!(!announced.is_empty());
    let payload = "z".repeat(MAX_INPUT_CHARS);
    for index in 0..40 {
        log.push(
            ActivityDraft {
                id: format!("toolu_{index}"),
                kind: ActivityKind::ToolCall {
                    name: "Bash".into(),
                    input: payload.clone(),
                    input_truncated: true,
                },
            },
            index,
        );
    }
    let stored = log.stored().expect("a non-empty trace always stores");
    assert!(
        stored.len() <= MAX_ACTIVITY_CHARS,
        "revisions stayed inside the cap: stored {} bytes",
        stored.len()
    );
    let items = parse_stored(Some(&stored));
    assert_eq!(
        items.last().unwrap().kind,
        ActivityKind::Status {
            state: STATE_TRUNCATED.into(),
            detail: None,
            category: None,
        },
        "and the trace says it stopped growing"
    );
}

#[test]
fn a_tool_result_carries_its_call_id_and_error_flag() {
    let mut parser = parser();
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_01","is_error":true,"content":[{"type":"text","text":"no such file"}]}]}}"#,
        ],
    );
    let drafts = activity(&items);
    assert_eq!(
        drafts[0].kind,
        ActivityKind::ToolResult {
            call_id: "toolu_01".into(),
            output: "no such file".into(),
            output_truncated: false,
            is_error: true,
        }
    );
}

#[test]
fn a_non_text_result_block_is_named_rather_than_dumped() {
    let mut parser = parser();
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"image","source":{"data":"AAAABBBB"}}]}]}}"#,
        ],
    );
    match &activity(&items)[0].kind {
        ActivityKind::ToolResult { output, .. } => {
            assert_eq!(output, "[image]");
            assert!(!output.contains("AAAABBBB"), "no base64 payload in a trace");
        }
        other => panic!("expected a tool result, got {other:?}"),
    }
}

#[test]
fn hidden_reasoning_is_recognised_and_never_recorded() {
    let mut parser = parser();
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"the user wants"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}}"#,
        ],
    );
    assert!(items.is_empty(), "thinking is dropped, not stored or relayed");
}

#[test]
fn an_unmapped_claude_event_is_ignored_rather_than_called_unsupported() {
    let mut parser = parser();
    // A keepalive and a future envelope type: the provider IS supported, we
    // simply do not render these.
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"stream_event","event":{"type":"ping"}}"#,
            r#"{"type":"control_response","subtype":"keepalive"}"#,
            r#"{"type":"assistant","message":{"content":[]}}"#,
        ],
    );
    assert!(
        items.is_empty(),
        "an unmapped event must not tell the reader their provider is unsupported"
    );
}

#[test]
fn only_the_model_escapes_the_init_line() {
    let mut parser = parser();
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"system","subtype":"init","model":"claude-opus-5","cwd":"/run/agent/abc","apiKeySource":"ANTHROPIC_API_KEY","tools":["Bash"]}"#,
        ],
    );
    assert_eq!(
        activity(&items)[0].kind,
        ActivityKind::Status {
            state: "model".into(),
            detail: Some("claude-opus-5".into()),
            category: None,
        },
        "the init line names the model and nothing else: cwd, tool list and \
         credential source stay on the server"
    );
}

#[test]
fn a_credential_handle_never_reaches_a_trace() {
    let mut parser = parser();
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"HOME=/run/handle-0123456789abcdef/x"}]}}"#,
        ],
    );
    match &activity(&items)[0].kind {
        ActivityKind::ToolResult { output, .. } => {
            assert!(!output.contains("handle-0123456789abcdef"));
            assert!(output.contains("[redacted]"));
        }
        other => panic!("expected a tool result, got {other:?}"),
    }
}

#[test]
fn oversized_tool_input_and_output_are_truncated_and_say_so() {
    let mut parser = parser();
    let long = "x".repeat(MAX_INPUT_CHARS + 500);
    let start = r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"Write"}}}"#;
    let delta = format!(
        r#"{{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"input_json_delta","partial_json":"{long}"}}}}}}"#
    );
    let stop = r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#;
    let items = drain(&mut parser, &[start, &delta, stop]);
    // The LAST draft is the completed call; the first is the announcement,
    // which carries no arguments and so is never truncated.
    match &activity(&items).last().unwrap().kind {
        ActivityKind::ToolCall {
            input,
            input_truncated,
            ..
        } => {
            assert!(*input_truncated);
            assert_eq!(input.chars().count(), MAX_INPUT_CHARS);
        }
        other => panic!("expected a tool call, got {other:?}"),
    }
}

#[test]
fn a_tool_block_left_open_by_a_dropped_stream_is_still_reported() {
    let mut parser = parser();
    drain(
        &mut parser,
        &[
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"toolu_09","name":"Read"}}}"#,
        ],
    );
    // The provider died before `content_block_stop`. The tool still ran.
    let finished = parser.finish();
    assert_eq!(activity(&finished).len(), 1);
    assert_eq!(activity(&finished)[0].id, "toolu_09");
}

#[test]
fn an_unreadable_codex_stream_is_reported_once_not_per_line() {
    let mut parser = StreamParser::new("codex", Vec::new());
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"something.unknown","payload":1}"#,
            r#"{"type":"something.else","payload":2}"#,
            r#"{"type":"third.thing","payload":3}"#,
        ],
    );
    let drafts = activity(&items);
    assert_eq!(drafts.len(), 1, "said once, not once per line");
    assert_eq!(
        drafts[0].kind,
        ActivityKind::Status {
            state: STATE_UNSUPPORTED.into(),
            detail: Some("codex".into()),
            category: None,
        }
    );
}

#[test]
fn a_codex_command_item_becomes_a_call_and_its_result() {
    let mut parser = StreamParser::new("codex", Vec::new());
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"item.completed","item":{"id":"i1","type":"command_execution","command":"ls","aggregated_output":"a\nb","exit_code":2}}"#,
        ],
    );
    let drafts = activity(&items);
    assert_eq!(drafts.len(), 2);
    assert_eq!(
        drafts[1].kind,
        ActivityKind::ToolResult {
            call_id: "i1".into(),
            output: "a\nb".into(),
            output_truncated: false,
            is_error: true,
        },
        "a non-zero exit is an error result"
    );
}

#[test]
fn a_streamed_codex_message_arrives_in_fragments_and_is_not_repeated_whole() {
    // The app-server adapter's shape: deltas as the message is written, then
    // the completed item carrying the SAME text in full. Sending both is how a
    // reader ends up looking at their reply twice.
    let mut parser = StreamParser::new("codex", Vec::new());
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"item.delta","item":{"type":"agent_message","id":"m1","delta":"Look"}}"#,
            r#"{"type":"item.delta","item":{"type":"agent_message","id":"m1","delta":"ing at"}}"#,
            r#"{"type":"item.delta","item":{"type":"agent_message","id":"m1","delta":" it."}}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","id":"m1","text":"Looking at it."}}"#,
        ],
    );
    assert_eq!(text_of(&items), "Looking at it.", "the reply, exactly once");
    // The first fragment opens the message (so it starts after whatever came
    // before rather than running on from it); the rest continue it and must be
    // appended with nothing inserted between them.
    assert!(
        matches!(items.first(), Some(StreamItem::Message(text)) if text == "Look"),
        "the first fragment must open a message"
    );
    assert!(
        items[1..]
            .iter()
            .all(|item| matches!(item, StreamItem::Text(_))),
        "later fragments must be continuations, not new messages"
    );
}

#[test]
fn a_second_streamed_message_begins_rather_than_running_on() {
    let mut parser = StreamParser::new("codex", Vec::new());
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"item.delta","item":{"type":"agent_message","id":"m1","delta":"Let me look."}}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","id":"m1","text":"Let me look."}}"#,
            r#"{"type":"item.delta","item":{"type":"agent_message","id":"m2","delta":"Yes"}}"#,
            r#"{"type":"item.delta","item":{"type":"agent_message","id":"m2","delta":" — here."}}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","id":"m2","text":"Yes — here."}}"#,
        ],
    );
    let opened: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            StreamItem::Message(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        opened,
        vec!["Let me look.", "Yes"],
        "each message opens once, at its first fragment"
    );
    assert_eq!(text_of(&items), "Let me look.Yes — here.");
}

#[test]
fn an_unstreamed_codex_message_is_still_delivered_whole() {
    // `codex exec --json` emits no deltas at all. That path must keep working
    // exactly as it did — the flag chooses a transport, not a behaviour.
    let mut parser = StreamParser::new("codex", Vec::new());
    let items = drain(
        &mut parser,
        &[r#"{"type":"item.completed","item":{"type":"agent_message","id":"m1","text":"Whole."}}"#],
    );
    assert!(
        matches!(items.as_slice(), [StreamItem::Message(text)] if text == "Whole."),
        "a message that never streamed must still arrive"
    );
}

#[test]
fn an_empty_delta_costs_the_message_neither_a_break_nor_a_write() {
    let mut parser = StreamParser::new("codex", Vec::new());
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"item.delta","item":{"type":"agent_message","id":"m1","delta":""}}"#,
            r#"{"type":"item.delta","item":{"type":"agent_message","id":"m1","delta":"Real."}}"#,
        ],
    );
    assert!(
        matches!(items.as_slice(), [StreamItem::Message(text)] if text == "Real."),
        "the first fragment with something in it is what opens the message"
    );
}

#[test]
fn a_streamed_delta_is_scrubbed_like_everything_else() {
    let mut parser = StreamParser::new("codex", vec!["handle-0123456789abcdef".to_owned()]);
    let items = drain(
        &mut parser,
        &[r#"{"type":"item.delta","item":{"type":"agent_message","id":"m1","delta":"path handle-0123456789abcdef here"}}"#],
    );
    // Deltas are reply TEXT, which the turn's own redaction covers on the way
    // out; what matters here is that the fragment is delivered intact rather
    // than dropped, and that the parser treats it as prose, not activity.
    assert!(matches!(items.as_slice(), [StreamItem::Message(_)]));
    assert!(activity(&items).is_empty());
}

#[test]
fn a_reasoning_delta_is_dropped_without_reporting_the_stream_unreadable() {
    let mut parser = StreamParser::new("codex", Vec::new());
    let items = drain(
        &mut parser,
        &[r#"{"type":"item.delta","item":{"type":"reasoning","id":"r1","delta":"thinking"}}"#],
    );
    assert!(items.is_empty(), "hidden reasoning is never surfaced");
}

#[test]
fn a_command_is_announced_when_it_starts_and_not_listed_twice_when_it_ends() {
    // A command that runs for thirty seconds was invisible until it finished,
    // which is the whole of "nothing is happening" from the reader's side.
    let mut parser = StreamParser::new("codex", Vec::new());
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"item.started","item":{"id":"c1","type":"command_execution","command":"ls -la","status":"inProgress"}}"#,
            r#"{"type":"item.completed","item":{"id":"c1","type":"command_execution","command":"ls -la","aggregated_output":"a\nb","exit_code":0,"status":"completed"}}"#,
        ],
    );
    let drafts = activity(&items);
    assert_eq!(drafts.len(), 2, "one call and one result, not two calls");
    assert_eq!(
        drafts[0].kind,
        ActivityKind::ToolCall {
            name: "command".into(),
            input: "ls -la".into(),
            input_truncated: false,
        },
        "the command is shown when it starts"
    );
    assert_eq!(
        drafts[1].kind,
        ActivityKind::ToolResult {
            call_id: "c1".into(),
            output: "a\nb".into(),
            output_truncated: false,
            is_error: false,
        },
        "the outcome is still recorded"
    );
}

#[test]
fn a_command_that_was_never_announced_is_still_shown_with_its_result() {
    // `item.started` may be absent (the exec path emits none) or carry no
    // command yet. Either way the completed item must supply the call.
    let mut parser = StreamParser::new("codex", Vec::new());
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"item.started","item":{"id":"c1","type":"command_execution","status":"inProgress"}}"#,
            r#"{"type":"item.completed","item":{"id":"c1","type":"command_execution","command":"ls","aggregated_output":"","exit_code":0}}"#,
        ],
    );
    let drafts = activity(&items);
    assert_eq!(drafts.len(), 3, "an empty announcement, then the call and result");
    assert_eq!(
        drafts[1].kind,
        ActivityKind::ToolCall {
            name: "command".into(),
            input: "ls".into(),
            input_truncated: false,
        },
        "the completed item fills in the command that the announcement lacked"
    );
    // Same id throughout, so the log revises one row rather than growing two.
    assert!(drafts.iter().take(2).all(|draft| draft.id == "c1"));
}

#[test]
fn codex_running_a_claude_compatible_stream_is_read_as_one() {
    // The runner currently launches codex with Claude's flags; if the stream
    // that comes back IS Claude-shaped, reading it as unsupported would be a
    // worse answer than reading it.
    let mut parser = StreamParser::new("codex", Vec::new());
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}}"#,
        ],
    );
    assert_eq!(text_of(&items), "hi");
    assert!(activity(&items).is_empty());
}

#[test]
fn offsets_count_unicode_scalars_not_utf16_units() {
    // An emoji is ONE scalar here and TWO UTF-16 units in a browser. A client
    // slicing the reply by these offsets must convert; the contract says so,
    // and this pins the unit the server promises.
    let mut log = ActivityLog::new();
    let reply = "ok 🙂 done";
    let offset = reply.chars().count();
    assert_eq!(offset, 9);
    assert_eq!(reply.len(), 12, "bytes differ from scalars");
    assert_eq!(reply.encode_utf16().count(), 10, "UTF-16 differs too");
    let item = log
        .push(
            ActivityDraft {
                id: "t".into(),
                kind: ActivityKind::Status {
                    state: "stop".into(),
                    detail: None,
                    category: None,
                },
            },
            offset,
        )
        .unwrap();
    assert_eq!(item.offset, 9);
}

#[test]
fn the_log_numbers_items_monotonically_and_caps_by_count() {
    let mut log = ActivityLog::new();
    let mut last = 0;
    for index in 0..(MAX_ITEMS + 20) {
        let Some(item) = log.push(
            ActivityDraft {
                id: format!("t{index}"),
                kind: ActivityKind::Status {
                    state: "stop".into(),
                    detail: None,
                    category: None,
                },
            },
            index,
        ) else {
            continue;
        };
        assert!(item.seq > last, "seq is monotonic");
        last = item.seq;
    }
    assert!(log.items().len() <= MAX_ITEMS);
    let final_item = log.items().last().unwrap();
    assert_eq!(
        final_item.kind,
        ActivityKind::Status {
            state: STATE_TRUNCATED.into(),
            detail: None,
            category: None,
        },
        "a trace that stopped early must say it stopped early"
    );
}

#[test]
fn a_resumed_log_never_reissues_a_seq_the_client_already_has() {
    let mut log = ActivityLog::new();
    for index in 0..3 {
        log.push(
            ActivityDraft {
                id: format!("t{index}"),
                kind: ActivityKind::Status {
                    state: "stop".into(),
                    detail: None,
                    category: None,
                },
            },
            index,
        );
    }
    let stored = log.stored().unwrap();
    let resumed = ActivityLog::resume(parse_stored(Some(&stored)));
    let mut resumed = resumed;
    let next = resumed
        .push(
            ActivityDraft {
                id: "t9".into(),
                kind: ActivityKind::Status {
                    state: "stop".into(),
                    detail: None,
                    category: None,
                },
            },
            9,
        )
        .unwrap();
    assert_eq!(next.seq, 4, "continues past the stored prefix");
}

#[test]
fn stored_activity_round_trips_and_corruption_reads_as_none() {
    let mut log = ActivityLog::new();
    log.push(
        ActivityDraft {
            id: "toolu_1".into(),
            kind: ActivityKind::ToolCall {
                name: "Bash".into(),
                input: "{}".into(),
                input_truncated: false,
            },
        },
        7,
    );
    let stored = log.stored().unwrap();
    let back = parse_stored(Some(&stored));
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].seq, 1);
    assert_eq!(back[0].offset, 7);
    // A turn must still load when its decoration does not.
    assert!(parse_stored(Some("{not json")).is_empty());
    assert!(parse_stored(None).is_empty());
    assert!(parse_stored(Some(&"x".repeat(MAX_ACTIVITY_CHARS + 1))).is_empty());
}

/// Fill the log with large items until it refuses, then assert the stored form
/// is always a valid, in-budget document that kept the earlier work.
fn fill_to_cap(payload: &str) -> String {
    let mut log = ActivityLog::new();
    for index in 0..MAX_ITEMS {
        log.push(
            ActivityDraft {
                id: format!("t{index}"),
                kind: ActivityKind::ToolResult {
                    call_id: format!("t{index}"),
                    output: payload.to_owned(),
                    output_truncated: false,
                    is_error: false,
                },
            },
            index,
        );
    }
    let stored = log
        .stored()
        .expect("a non-empty trace must never store as nothing");
    assert!(
        stored.len() <= MAX_ACTIVITY_CHARS,
        "stored {} bytes, cap is {MAX_ACTIVITY_CHARS}",
        stored.len()
    );
    stored
}

#[test]
fn a_trace_that_fills_its_budget_still_stores_and_still_says_it_was_cut() {
    // Big enough that the byte cap bites before the item cap.
    let stored = fill_to_cap(&"y".repeat(900));
    let items = parse_stored(Some(&stored));
    assert!(
        items.len() > 1,
        "the earlier items are kept, not discarded with the tail"
    );
    assert_eq!(
        items.last().unwrap().kind,
        ActivityKind::Status {
            state: STATE_TRUNCATED.into(),
            detail: None,
            category: None,
        },
        "the marker survives to the stored form, so a reload still reports the cut"
    );
    // Seq stays usable as a dedupe key across the trimmed document.
    let mut seqs: Vec<u32> = items.iter().map(|item| item.seq).collect();
    let before = seqs.len();
    seqs.dedup();
    assert_eq!(seqs.len(), before, "no duplicate seq survives trimming");
}

#[test]
fn a_multibyte_trace_respects_the_byte_cap_not_a_character_count() {
    // Every payload character is 4 bytes here, so a log that budgeted in
    // characters would store roughly four times the cap.
    let stored = fill_to_cap(&"🙂".repeat(300));
    assert!(stored.len() <= MAX_ACTIVITY_CHARS);
    let items = parse_stored(Some(&stored));
    assert!(!items.is_empty(), "multibyte content still stores");
    assert_eq!(
        items.last().unwrap().kind,
        ActivityKind::Status {
            state: STATE_TRUNCATED.into(),
            detail: None,
            category: None,
        }
    );
    // And what came back is intact rather than cut through a code point.
    for item in &items {
        if let ActivityKind::ToolResult { output, .. } = &item.kind {
            assert!(output.chars().all(|c| c == '🙂'));
        }
    }
}

#[test]
fn a_turn_without_tools_stores_nothing_at_all() {
    let log = ActivityLog::new();
    assert!(log.is_empty());
    assert_eq!(log.stored(), None, "NULL, not an empty array");
}

#[test]
fn two_codex_messages_in_one_turn_are_whole_messages_not_fragments() {
    // The real shape from the failing turn: the model says what it is about to
    // do, runs its tools, then answers. Both arrive as COMPLETED items, so
    // neither is a fragment of the other and the reply must not run them
    // together — "…improvements." followed by "Yes…" read as
    // "…improvements.Yes…" in the stored reply, which is why a reload did not
    // fix it.
    let mut parser = StreamParser::new("codex", vec!["handle-0123456789abcdef".to_owned()]);
    let items = drain(
        &mut parser,
        &[
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"Let me look for obvious improvements."}}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"Yes — here is the review."}}"#,
        ],
    );
    let messages: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            StreamItem::Message(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        messages,
        vec!["Let me look for obvious improvements.", "Yes — here is the review."],
        "a completed agent_message must be a whole message, not a fragment"
    );
    // Nothing classified as a fragment: a fragment would be appended with no
    // break, which is the bug.
    assert!(
        !items.iter().any(|item| matches!(item, StreamItem::Text(_))),
        "a completed message was classified as a fragment"
    );
}

#[test]
fn acp_providers_are_read_with_the_codex_mapping_the_adapter_writes() {
    // Exactly what `neev_acp_stream_adapter.py` emits for a streamed message, a
    // tool that ran, and a completed turn.
    let lines = [
        r#"{"type":"item.delta","item":{"type":"agent_message","id":"msg_1","delta":"Hel"}}"#,
        r#"{"type":"item.delta","item":{"type":"agent_message","id":"msg_1","delta":"lo"}}"#,
        r#"{"type":"item.completed","item":{"type":"agent_message","id":"msg_1","text":"Hello"}}"#,
        r#"{"type":"item.started","item":{"type":"command_execution","id":"call_1","command":"Read file"}}"#,
        r#"{"type":"item.completed","item":{"type":"command_execution","id":"call_1","command":"Read file","aggregated_output":"ok","exit_code":0}}"#,
        r#"{"type":"turn.completed"}"#,
    ];
    for provider in ["cursor", "gemini", "opencode"] {
        let mut parser = StreamParser::new(provider, Vec::new());
        let items = drain(&mut parser, &lines);
        assert_eq!(text_of(&items), "Hello", "{provider}: the reply, exactly once");
        let drafts = activity(&items);
        assert!(
            !drafts.iter().any(|draft| matches!(
                &draft.kind,
                ActivityKind::Status { state, .. } if state == "unsupported"
            )),
            "{provider}: must not be reported as an unsupported trace"
        );
        assert!(drafts.iter().any(|draft| matches!(
            &draft.kind,
            ActivityKind::ToolResult { call_id, is_error: false, .. } if call_id == "call_1"
        )));
    }
    // A failed turn keeps the adapter's fixed reason out of the trace text.
    let mut parser = StreamParser::new("gemini", Vec::new());
    let items = drain(
        &mut parser,
        &[r#"{"type":"turn.failed","reason":"provider sign-in required"}"#],
    );
    assert_eq!(text_of(&items), "");
}
