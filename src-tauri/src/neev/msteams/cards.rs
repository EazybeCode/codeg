//! Adaptive Cards Neev sends to Teams.
//!
//! Cards carry no identity: every action names only a run id, and the action
//! handler re-proves the clicker, their access AND that the run was launched
//! from this very conversation before answering. Buttons are `Action.Execute`
//! (Universal Actions, answered in the invoke response) with an
//! `Action.Submit` fallback for clients that predate it; both carry
//! `"neev": <verb>` so `activity::normalize` reads them the same way. Invoke
//! answers are short messages to the clicker ([`invoke_message`]), never a
//! replacement card: in a channel a replaced card would change what everyone
//! sees because one person clicked.

use serde_json::{json, Value};

use super::codes::Refusal;

pub(crate) const CARD_CONTENT_TYPE: &str = "application/vnd.microsoft.card.adaptive";
/// "What is the state of this run?" (any authorized member of the origin).
pub(crate) const STATUS_VERB: &str = "neev.status";
/// "Stop this run" (only the member it runs as).
pub(crate) const STOP_VERB: &str = "neev.stop";
/// Teams caps a message near 28 KB; leave room for the card around the text.
pub(crate) const MAX_CARD_TEXT_CHARS: usize = 20_000;

fn card(body: Vec<Value>, actions: Vec<Value>) -> Value {
    let mut card = json!({
        "type": "AdaptiveCard",
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "version": "1.5",
        "body": body,
    });
    if !actions.is_empty() {
        card["actions"] = Value::Array(actions);
    }
    card
}

fn text(value: &str) -> Value {
    json!({"type": "TextBlock", "text": value, "wrap": true})
}

fn heading(value: &str) -> Value {
    json!({"type": "TextBlock", "text": value, "wrap": true, "weight": "Bolder", "size": "Medium"})
}

fn subtle(value: &str) -> Value {
    json!({"type": "TextBlock", "text": value, "wrap": true, "isSubtle": true, "size": "Small"})
}

fn action(verb: &str, title: &str, run_id: &str) -> Value {
    let data = json!({"neev": verb, "runId": run_id});
    json!({
        "type": "Action.Execute",
        "title": title,
        "verb": verb,
        "data": data,
        "fallback": {"type": "Action.Submit", "title": title, "data": data},
    })
}

fn run_actions(run_id: &str) -> Vec<Value> {
    vec![action(STATUS_VERB, "Status", run_id), action(STOP_VERB, "Stop", run_id)]
}

/// Cut long agent output on a character boundary and say that it was cut.
pub(crate) fn clip(value: &str) -> String {
    if value.chars().count() <= MAX_CARD_TEXT_CHARS {
        return value.to_owned();
    }
    let mut clipped: String = value.chars().take(MAX_CARD_TEXT_CHARS).collect();
    clipped.push_str("\n\n…(truncated — open the chat in Neev for the full reply)");
    clipped
}

pub(crate) fn refusal(reason: Refusal) -> Value {
    card(
        vec![
            text(reason.message()),
            subtle(&format!("Code: {}", reason.code())),
        ],
        Vec::new(),
    )
}

/// What a person sees right after their message became a Neev turn.
pub(crate) fn started(session_title: &str, run_id: &str) -> Value {
    card(
        vec![
            heading("Neev is working on it"),
            subtle(&format!("Saved to your Neev chat “{session_title}”. The reply will be posted here when it is ready.")),
        ],
        run_actions(run_id),
    )
}

/// A shared channel thread's run started. Says whose Neev account and
/// connection it runs as: in a shared thread that is the person who asked, not
/// whoever started the thread.
pub(crate) fn thread_started(requester: &str, run_id: &str) -> Value {
    let requester = if requester.trim().is_empty() {
        "the person who asked"
    } else {
        requester
    };
    card(
        vec![
            heading("Neev is working on it"),
            subtle(&format!(
                "Running as {requester}, with their Neev access and connection. Earlier messages in this thread are included as context. The answer will be posted in this thread."
            )),
        ],
        run_actions(run_id),
    )
}

/// A message recorded while another request in the same thread is running. It
/// did NOT change that request; nothing pretends otherwise.
pub(crate) fn queued() -> Value {
    card(
        vec![
            heading("Noted in this thread"),
            text("Neev is still working on an earlier request in this thread. Your message did not change that request. It will run next, as you, with this thread (including that answer) as context."),
        ],
        Vec::new(),
    )
}

/// The run's settled outcome, delivered automatically into its origin.
pub(crate) fn final_answer(status: &str, response: &str, error_code: Option<&str>, requester: Option<&str>) -> Value {
    final_with_files(status, response, error_code, requester, None)
}

/// [`final_answer`] plus an honest line about the run's generated files.
pub(crate) fn final_with_files(status: &str, response: &str, error_code: Option<&str>, requester: Option<&str>, files: Option<&str>) -> Value {
    let mut body = Vec::new();
    if let Some(name) = requester.filter(|name| !name.trim().is_empty()) {
        body.push(subtle(&format!("For {name}")));
    }
    match status {
        "completed" if !response.trim().is_empty() => body.push(text(&clip(response))),
        "completed" => body.push(text("The agent finished without a reply.")),
        "canceled" => body.push(text("This request was stopped.")),
        _ => {
            body.push(heading("The agent could not answer"));
            body.push(subtle(&format!("Reason: {}", error_code.unwrap_or("failed"))));
        }
    }
    if let Some(files) = files {
        body.push(subtle(files));
    }
    card(body, Vec::new())
}

/// A channel answer that used sources not everyone in the channel may see
/// (a personal connector, an unknown tool, an incomplete trace). The content
/// stays in the requester's own Neev chat.
pub(crate) fn withheld(requester: &str) -> Value {
    let requester = if requester.trim().is_empty() { "The requester" } else { requester };
    card(
        vec![
            heading("Answer kept private"),
            text(&format!(
                "{requester}'s answer used tools or sources Neev cannot confirm are open to everyone in this channel (for example files, connectors or a shell), so Neev did not post it here. {requester} can read it in their Neev chat."
            )),
        ],
        Vec::new(),
    )
}

/// A generated file now in the channel's Files, linked by its SharePoint URL
/// (existing permissions apply; no sharing link is created).
pub(crate) fn document(name: &str, web_url: &str) -> Value {
    card(
        vec![text(&format!("Saved “{name}” to this channel's Files."))],
        vec![json!({"type": "Action.OpenUrl", "title": "Open", "url": web_url})],
    )
}

/// A generated file that could not be saved to the channel's Files.
pub(crate) fn document_not_saved(name: &str) -> Value {
    card(
        vec![text(&format!("“{name}” was not saved to this channel's Files. It stays in the requester's Neev chat."))],
        Vec::new(),
    )
}

/// A short reply to the person who clicked a card button, shown to them
/// without replacing the card everyone else sees.
pub(crate) fn invoke_message(text: &str) -> Value {
    json!({"statusCode": 200, "type": "application/vnd.microsoft.activity.message", "value": text})
}

/// A plain one-line card (an answer to a fallback `Action.Submit` click).
pub(crate) fn notice(value: &str) -> Value {
    card(vec![text(value)], Vec::new())
}

/// The human sentence for a run's state, for the Status button.
pub(crate) fn status_text(status: &str) -> &'static str {
    match status {
        "running" => "Neev is still working on this request.",
        "completed" => "Finished. The answer is posted in this conversation.",
        "canceled" => "This request was stopped.",
        _ => "This request ended without an answer.",
    }
}

/// Sent once when the app is added. States the CURRENT mapping honestly
/// rather than promising access the operator has not granted.
pub(crate) fn welcome(greeting_name: &str, tenant_connected: bool) -> Value {
    let greeting = if greeting_name.is_empty() {
        "Hi! I'm Neev.".to_owned()
    } else {
        format!("Hi {greeting_name}! I'm Neev.")
    };
    let status = if tenant_connected {
        "Message me directly, or mention @Neev in a connected channel, and I will run your request as a Neev agent chat."
    } else {
        Refusal::UnmappedTenant.message()
    };
    card(vec![heading(&greeting), text(status)], Vec::new())
}

/// A message activity carrying one card, for the Bot Connector send API.
pub(crate) fn message(card: Value, reply_to_id: Option<&str>) -> Value {
    let mut activity = json!({
        "type": "message",
        "attachments": [{"contentType": CARD_CONTENT_TYPE, "content": card}],
    });
    if let Some(id) = reply_to_id {
        activity["replyToId"] = Value::String(id.to_owned());
    }
    activity
}
