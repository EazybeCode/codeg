//! Parser fixtures and the caps, driven by the line shapes the provider CLIs
//! actually emit.
//!
//! The Claude fixtures are taken from `--output-format stream-json
//! --include-partial-messages --verbose`. The Codex fixtures are the documented
//! `item.*` envelope shape and are explicitly UNVERIFIED against a live signed-in
//! Codex — see the note on [`StreamParser::codex`]; they pin our mapping, not
//! the provider's contract.

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
            StreamItem::Text(text) => Some(text.as_str()),
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
    assert_eq!(drafts.len(), 1, "one call, emitted once the block closed");
    assert_eq!(drafts[0].id, "toolu_01");
    assert_eq!(
        drafts[0].kind,
        ActivityKind::ToolCall {
            name: "Bash".into(),
            input: "{\"command\":\"ls -la\"}".into(),
            input_truncated: false,
        }
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
    match &activity(&items)[0].kind {
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
    assert_eq!(offset, 10);
    assert_eq!(reply.len(), 13, "bytes differ from scalars");
    assert_eq!(reply.encode_utf16().count(), 11, "UTF-16 differs too");
    let item = log
        .push(
            ActivityDraft {
                id: "t".into(),
                kind: ActivityKind::Status {
                    state: "stop".into(),
                    detail: None,
                },
            },
            offset,
        )
        .unwrap();
    assert_eq!(item.offset, 10);
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

#[test]
fn a_turn_without_tools_stores_nothing_at_all() {
    let log = ActivityLog::new();
    assert!(log.is_empty());
    assert_eq!(log.stored(), None, "NULL, not an empty array");
}
