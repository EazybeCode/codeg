use super::*;
use crate::neev::agents::activity::{StreamItem, StreamParser};

fn errors(provider: &str, lines: &[&str]) -> Vec<(String, Option<String>)> {
    let mut parser = StreamParser::new(provider, vec!["handle-0123456789abcdef".into()]);
    lines
        .iter()
        .flat_map(|line| parser.push(line))
        .filter_map(|item| match item {
            StreamItem::Activity(draft) => match draft.kind {
                ActivityKind::Status {
                    state,
                    detail,
                    category: Some(category),
                } if state == STATE_ERROR => Some((category, detail)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn codex_classes_come_from_the_structured_hint_first() {
    assert_eq!(
        classify(Some("usageLimitExceeded"), "whatever"),
        Category::Quota
    );
    assert_eq!(
        classify(Some("unauthorized"), "401"),
        Category::SigninRequired
    );
    assert_eq!(
        classify(
            Some("unauthorized"),
            "Your access token could not be refreshed"
        ),
        Category::AuthExpired
    );
    assert_eq!(classify(Some("serverOverloaded"), ""), Category::RateLimit);
    assert_eq!(
        classify(Some("responseStreamDisconnected"), ""),
        Category::Network
    );
    assert_eq!(
        classify(Some("contextWindowExceeded"), ""),
        Category::ProviderRefusal
    );
    assert_eq!(classify(Some("handshake_timeout"), ""), Category::Timeout);
    assert_eq!(classify(Some("closed"), ""), Category::RuntimeUnavailable);
}

#[test]
fn text_is_classified_when_there_is_no_hint() {
    let cases = [
        (
            "API Error: 401 OAuth token has expired. Please obtain a new token",
            Category::AuthExpired,
        ),
        (
            "Invalid API key · Please run /login",
            Category::SigninRequired,
        ),
        ("Not logged in", Category::SigninRequired),
        (
            "You've hit your usage limit. Try again at 3:05 PM.",
            Category::Quota,
        ),
        ("exceeded your current quota", Category::Quota),
        ("429 Too Many Requests", Category::RateLimit),
        ("API Error: 529 Overloaded", Category::RateLimit),
        ("request timed out", Category::Timeout),
        (
            "stream disconnected before completion: error sending request",
            Category::Network,
        ),
        (
            "400 Bad Request: model not supported",
            Category::ProviderRefusal,
        ),
        (
            "provider ignored: serviceTier=flex",
            Category::ProviderRefusal,
        ),
        ("provider closed the stream", Category::RuntimeUnavailable),
        ("something odd happened", Category::Provider),
    ];
    for (text, want) in cases {
        assert_eq!(classify(None, text), want, "{text}");
    }
}

#[test]
fn auth_classes_store_unauthorized_and_everything_else_provider() {
    assert_eq!(Category::AuthExpired.turn_error_code(), "unauthorized");
    assert_eq!(Category::SigninRequired.turn_error_code(), "unauthorized");
    for other in [
        Category::Quota,
        Category::RateLimit,
        Category::Network,
        Category::Timeout,
        Category::ProviderRefusal,
        Category::RuntimeUnavailable,
        Category::Provider,
    ] {
        assert_eq!(other.turn_error_code(), "provider");
        assert_eq!(Category::parse(other.as_str()), Some(other));
    }
}

#[test]
fn secrets_identities_and_private_paths_never_survive() {
    let raw = "failed for jane.doe@example.com with Authorization: Bearer abc.def-ghi \
               token=s3cr3tvalue api_key=\"sk-live-0123456789abcdef\" \
               eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl \
               sk-ant-api03-ABCDEFGHIJKLMNOP at /data/agents/run-1/home/.codex/auth.json \
               and /Users/sagar/.claude/.credentials.json see \
               https://chatgpt.com/codex/settings/usage?session=abcdef handle-0123456789abcdef \
               id 0123456789abcdef0123456789abcdef01";
    let safe = sanitize(raw, &["handle-0123456789abcdef".to_owned()]);
    for leaked in [
        "jane.doe",
        "example.com",
        "abc.def-ghi",
        "s3cr3tvalue",
        "sk-live",
        "eyJhbGci",
        "sk-ant",
        "/data/agents",
        "auth.json",
        "/Users/sagar",
        "credentials.json",
        "session=abcdef",
        "handle-0123",
        "0123456789abcdef0123456789abcdef01",
    ] {
        assert!(!safe.contains(leaked), "{leaked} leaked into: {safe}");
    }
    // What a person needs to act on is still there.
    assert!(
        safe.contains("https://chatgpt.com/codex/settings/usage"),
        "{safe}"
    );
    assert!(
        safe.contains("[path]") && safe.contains("[email]"),
        "{safe}"
    );
}

#[test]
fn a_json_error_body_is_reduced_to_its_message_and_bounded() {
    let raw = r#"API Error: 401 {"type":"error","error":{"type":"authentication_error","message":"OAuth token has expired. Please obtain a new token or refresh your existing token."},"request_id":"req_011CUxyz"}"#;
    assert_eq!(
        sanitize(raw, &[]),
        "API Error: 401 OAuth token has expired. Please obtain a new token or refresh your existing token."
    );
    let long = "x ".repeat(1_000);
    let safe = sanitize(&long, &[]);
    assert_eq!(safe.chars().count(), MAX_MESSAGE_CHARS);
    assert!(safe.ends_with('…'));
    assert_eq!(sanitize("line\none\r\ttwo", &[]), "line one two");
}

#[test]
fn a_codex_adapter_failure_keeps_its_reason_and_class() {
    let found = errors(
        "codex",
        &[
            r#"{"type":"turn.failed","reason":"You've hit your usage limit. Upgrade to Pro","code":"usageLimitExceeded"}"#,
        ],
    );
    assert_eq!(
        found,
        vec![(
            "quota".to_owned(),
            Some("You've hit your usage limit. Upgrade to Pro".to_owned())
        )]
    );
}

#[test]
fn codex_exec_errors_before_the_failure_supply_its_reason() {
    let found = errors(
        "codex",
        &[
            r#"{"type":"error","message":"unexpected status 401 Unauthorized: token expired"}"#,
            r#"{"type":"turn.failed","error":{}}"#,
        ],
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, "auth_expired");
    assert!(found[0].1.as_deref().unwrap().contains("401 Unauthorized"));
}

#[test]
fn the_old_result_marker_is_still_emitted_for_older_readers() {
    let mut parser = StreamParser::new("codex", vec![]);
    let items = parser.push(r#"{"type":"turn.failed","reason":"provider closed the stream"}"#);
    let states: Vec<_> = items
        .into_iter()
        .filter_map(|item| match item {
            StreamItem::Activity(draft) => match draft.kind {
                ActivityKind::Status { state, detail, .. } => Some((state, detail)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(states[0], ("result".to_owned(), Some("failed".to_owned())));
    assert_eq!(states[1].0, "error");
}

#[test]
fn a_claude_error_result_is_recorded_with_the_assistant_error_hint() {
    let found = errors(
        "claude",
        &[
            r#"{"type":"assistant","error":"authentication_failed","message":{"content":[{"type":"text","text":"Invalid API key · Please run /login"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login"}"#,
        ],
    );
    assert_eq!(
        found,
        vec![(
            "signin_required".to_owned(),
            Some("Invalid API key · Please run /login".to_owned())
        )]
    );
}

#[test]
fn a_claude_success_records_no_error() {
    assert!(errors(
        "claude",
        &[r#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#]
    )
    .is_empty());
}

#[test]
fn a_claude_adapter_failure_envelope_is_read_too() {
    let found = errors(
        "claude",
        &[r#"{"type":"turn.failed","reason":"provider closed the stream"}"#],
    );
    assert_eq!(found[0].0, "runtime_unavailable");
}

#[test]
fn the_category_field_is_additive_on_the_wire() {
    // A trace stored before `category` existed still parses…
    let old = r#"[{"seq":1,"offset":0,"id":"result","kind":"status","state":"result","detail":"failed"}]"#;
    let items = crate::neev::agents::activity::parse_stored(Some(old));
    assert_eq!(items.len(), 1);
    // …and a new error item serialises with it, and only it, added.
    let draft = draft(Category::Quota, "limit".into());
    let json = serde_json::to_value(crate::neev::agents::activity::ActivityItem {
        seq: 2,
        offset: 0,
        id: draft.id,
        kind: draft.kind,
    })
    .unwrap();
    assert_eq!(json["kind"], "status");
    assert_eq!(json["state"], "error");
    assert_eq!(json["category"], "quota");
    assert_eq!(json["detail"], "limit");
}

#[test]
fn a_file_uri_is_redacted_like_any_other_private_path() {
    let safe = sanitize(
        "cannot open file:///Users/sagar/.codex/auth.json or FILE:///data/agents/run-1/x",
        &[],
    );
    assert!(
        !safe.contains("sagar") && !safe.contains("auth.json"),
        "{safe}"
    );
    assert!(!safe.contains("/data/agents"), "{safe}");
    assert_eq!(safe, "cannot open [path] or [path]");
}

fn tool_call(index: usize) -> crate::neev::agents::activity::ActivityDraft {
    crate::neev::agents::activity::ActivityDraft {
        id: format!("call-{index}"),
        kind: ActivityKind::ToolCall {
            name: "Bash".into(),
            input: "x".repeat(900),
            input_truncated: false,
        },
    }
}

#[test]
fn a_failure_after_the_trace_hit_its_cap_is_still_stored_with_its_class() {
    use crate::neev::agents::activity::{parse_stored, ActivityLog, MAX_ACTIVITY_CHARS, MAX_ITEMS};
    let mut log = ActivityLog::new();
    // A long agentic run: far past both the item and the size cap.
    for index in 0..(MAX_ITEMS * 2) {
        log.push(tool_call(index), index);
    }
    let long = "You've hit your usage limit. ".repeat(40);
    let recorded = log.push(draft(Category::Quota, sanitize(&long, &[])), 9_999);
    assert!(
        recorded.is_some(),
        "the terminal error must not be capped out"
    );
    // Only ONE terminal error per turn.
    assert!(log
        .push(draft(Category::Network, "again".into()), 10_000)
        .is_none());
    let stored = log.stored().unwrap();
    assert!(stored.len() <= MAX_ACTIVITY_CHARS);
    let items = parse_stored(Some(&stored));
    let errors: Vec<_> = items
        .iter()
        .filter_map(|item| category_of(&item.kind))
        .collect();
    assert_eq!(
        errors,
        vec![Category::Quota],
        "survives reload with its class"
    );
    assert!(items.iter().any(|item| matches!(
        &item.kind,
        ActivityKind::Status { state, .. } if state == "truncated"
    )));
    let mut seqs: Vec<_> = items.iter().map(|item| item.seq).collect();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(
        seqs.len(),
        items.len(),
        "seq is the dedupe key and stays unique"
    );
}

#[test]
fn an_oversized_restored_trace_is_trimmed_but_keeps_its_error() {
    use crate::neev::agents::activity::{
        parse_stored, ActivityItem, ActivityLog, MAX_ACTIVITY_CHARS,
    };
    let mut items: Vec<ActivityItem> = (0..200)
        .map(|index| {
            let step = tool_call(index);
            ActivityItem {
                seq: index as u32 + 1,
                offset: 0,
                id: step.id,
                kind: step.kind,
            }
        })
        .collect();
    // Forced over budget (as a restored/legacy log could be), error in the middle.
    let error = draft(Category::RateLimit, "429 Too Many Requests".into());
    items.insert(
        50,
        ActivityItem {
            seq: 999,
            offset: 0,
            id: error.id,
            kind: error.kind,
        },
    );
    let stored = ActivityLog::resume(items).stored().unwrap();
    assert!(stored.len() <= MAX_ACTIVITY_CHARS);
    let kept = parse_stored(Some(&stored));
    assert_eq!(
        kept.iter()
            .filter_map(|item| category_of(&item.kind))
            .collect::<Vec<_>>(),
        vec![Category::RateLimit]
    );
    let mut seqs: Vec<_> = kept.iter().map(|item| item.seq).collect();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(seqs.len(), kept.len());
}

#[test]
fn a_tool_revision_cannot_spend_the_space_held_for_the_terminal_error() {
    use crate::neev::agents::activity::{
        parse_stored, ActivityDraft, ActivityItem, ActivityLog, ERROR_RESERVE, MAX_ACTIVITY_CHARS,
    };
    // A trace just under the APPEND limit, with one tool call announced but
    // not yet revised with its (large) input.
    let mut items = vec![ActivityItem {
        seq: 1,
        offset: 0,
        id: "call".into(),
        kind: ActivityKind::ToolCall {
            name: "Bash".into(),
            input: String::new(),
            input_truncated: false,
        },
    }];
    // Low enough that the OLD revision check (marker reserve only) accepted a
    // ~2 KB revision, high enough that the revision then ate into the space
    // the error needs — the exact R1 shape. Still inside the append limit.
    let limit = MAX_ACTIVITY_CHARS - 128 - 2_200;
    assert!(limit < MAX_ACTIVITY_CHARS - 128 - ERROR_RESERVE);
    loop {
        let next = ActivityItem {
            seq: items.len() as u32 + 1,
            offset: 0,
            id: format!("r{}", items.len()),
            kind: ActivityKind::ToolResult {
                call_id: "x".into(),
                output: "y".repeat(500),
                output_truncated: false,
                is_error: false,
            },
        };
        let mut probe = items.clone();
        probe.push(next.clone());
        if serde_json::to_string(&probe).unwrap().len() > limit {
            break;
        }
        items.push(next);
    }
    let mut log = ActivityLog::resume(items);
    // The revision that previously grew the trace into the error reserve.
    log.push(
        ActivityDraft {
            id: "call".into(),
            kind: ActivityKind::ToolCall {
                name: "Bash".into(),
                input: "z".repeat(2_048),
                input_truncated: false,
            },
        },
        1,
    );
    let long = "é".repeat(400); // multibyte: the costliest message there is
    assert!(log
        .push(draft(Category::Quota, sanitize(&long, &[])), 2)
        .is_some());
    let stored = log.stored().unwrap();
    assert!(stored.len() <= MAX_ACTIVITY_CHARS);
    let kept = parse_stored(Some(&stored));
    assert_eq!(
        kept.iter()
            .filter_map(|item| category_of(&item.kind))
            .collect::<Vec<_>>(),
        vec![Category::Quota],
        "the stored trace keeps the provider's class after reload"
    );
}
