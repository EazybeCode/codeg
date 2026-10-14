//! The Bot Framework Activity subset Neev reads, normalized into what the
//! service acts on. Everything identity-bearing comes from the activity the
//! Bot Connector signed for (see `jwt::verify_connector`); nothing here trusts
//! display names.

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) const MAX_TEXT_CHARS: usize = 8_000;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChannelAccount {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) aad_object_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationAccount {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) conversation_type: Option<String>,
    #[serde(default)]
    pub(crate) tenant_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Activity {
    #[serde(rename = "type", default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) channel_id: String,
    #[serde(default)]
    pub(crate) service_url: String,
    #[serde(default)]
    pub(crate) from: ChannelAccount,
    #[serde(default)]
    pub(crate) recipient: ChannelAccount,
    #[serde(default)]
    pub(crate) conversation: ConversationAccount,
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) value: Option<Value>,
    #[serde(default)]
    pub(crate) entities: Vec<Value>,
    #[serde(default)]
    pub(crate) channel_data: Option<Value>,
    #[serde(default)]
    pub(crate) members_added: Vec<ChannelAccount>,
    #[serde(default)]
    pub(crate) members_removed: Vec<ChannelAccount>,
    #[serde(default)]
    pub(crate) action: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    Personal,
    Channel {
        team_id: Option<String>,
        channel_id: String,
    },
    GroupChat,
}

/// Who said what, where — shared by every inbound kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Envelope {
    pub(crate) tenant_id: String,
    pub(crate) conversation_id: String,
    pub(crate) activity_id: String,
    pub(crate) service_url: String,
    /// Entra object id of the sender; `None` for guests/anonymous senders,
    /// who can never be mapped.
    pub(crate) from_aad: Option<String>,
    pub(crate) from_name: String,
    pub(crate) bot_id: String,
    pub(crate) scope: Scope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InboundMessage {
    pub(crate) envelope: Envelope,
    pub(crate) text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CardAction {
    pub(crate) envelope: Envelope,
    pub(crate) verb: String,
    pub(crate) data: Value,
    /// `Action.Execute` (an `invoke` answered in the HTTP response) rather
    /// than the `Action.Submit` fallback (a message we answer by sending).
    pub(crate) invoke: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Inbound {
    Message(InboundMessage),
    Action(CardAction),
    /// The bot was added to a personal chat, team or group chat.
    Installed(Envelope),
    /// Teams reported a change that must stop Neev from answering somewhere.
    Lifecycle(Lifecycle),
    Ignored,
}

/// A signed Teams event that suspends a binding. Neev never deletes anything
/// because of one; an admin re-enables deliberately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Lifecycle {
    pub(crate) tenant_id: String,
    pub(crate) event: LifecycleEvent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LifecycleEvent {
    /// The app was removed from the Team, or the Team was deleted/archived.
    TeamSuspended { team_id: String },
    /// A channel was deleted.
    ChannelDeleted { channel_id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ActivityError {
    #[error("activity is missing a required field")]
    Malformed,
    #[error("tenant is missing or inconsistent")]
    Tenant,
}

fn tenant_of(activity: &Activity) -> Result<String, ActivityError> {
    let from_channel_data = activity
        .channel_data
        .as_ref()
        .and_then(|data| data.pointer("/tenant/id"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    let from_conversation = activity
        .conversation
        .tenant_id
        .as_deref()
        .map(str::to_ascii_lowercase);
    let tenant = match (from_channel_data, from_conversation) {
        (Some(a), Some(b)) if a != b => return Err(ActivityError::Tenant),
        (Some(a), _) => a,
        (None, Some(b)) => b,
        (None, None) => return Err(ActivityError::Tenant),
    };
    if !super::config::is_guid(&tenant) {
        return Err(ActivityError::Tenant);
    }
    Ok(tenant)
}

fn scope_of(activity: &Activity) -> Scope {
    match activity.conversation.conversation_type.as_deref() {
        Some("channel") => {
            let data = activity.channel_data.as_ref();
            let channel_id = data
                .and_then(|data| data.pointer("/channel/id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                // A channel thread's conversation id is `<channel>;messageid=…`.
                .unwrap_or_else(|| {
                    activity
                        .conversation
                        .id
                        .split(';')
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                });
            let team_id = data
                .and_then(|data| data.pointer("/team/id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            Scope::Channel {
                team_id,
                channel_id,
            }
        }
        Some("groupChat") => Scope::GroupChat,
        _ => Scope::Personal,
    }
}

fn envelope(activity: &Activity) -> Result<Envelope, ActivityError> {
    let tenant_id = tenant_of(activity)?;
    let activity_id = activity
        .id
        .clone()
        .filter(|id| !id.is_empty() && id.len() <= 256)
        .ok_or(ActivityError::Malformed)?;
    if activity.conversation.id.is_empty()
        || activity.conversation.id.len() > 512
        || activity.recipient.id.is_empty()
        || activity.service_url.is_empty()
    {
        return Err(ActivityError::Malformed);
    }
    Ok(Envelope {
        tenant_id,
        conversation_id: activity.conversation.id.clone(),
        activity_id,
        service_url: activity.service_url.clone(),
        from_aad: activity
            .from
            .aad_object_id
            .as_deref()
            .filter(|oid| super::config::is_guid(oid))
            .map(str::to_ascii_lowercase),
        from_name: activity
            .from
            .name
            .clone()
            .unwrap_or_default()
            .chars()
            .take(128)
            .collect(),
        bot_id: activity.recipient.id.clone(),
        scope: scope_of(activity),
    })
}

/// Whether the message @-mentions the bot itself (not merely someone).
fn mentions_bot(activity: &Activity) -> bool {
    activity.entities.iter().any(|entity| {
        entity.get("type").and_then(Value::as_str) == Some("mention")
            && entity.pointer("/mentioned/id").and_then(Value::as_str)
                == Some(activity.recipient.id.as_str())
    })
}

/// Remove `<at>…</at>` mention spans and the few entities Teams escapes, then
/// trim. The prompt the agent sees is what the person typed, not markup.
pub(crate) fn clean_text(raw: &str) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("<at>") {
        text.push_str(&rest[..start]);
        match rest[start..].find("</at>") {
            Some(end) => rest = &rest[start + end + 5..],
            None => {
                rest = "";
            }
        }
    }
    text.push_str(rest);
    let text = text
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    text.trim().chars().take(MAX_TEXT_CHARS).collect()
}

/// A card action's verb and data, from either `Action.Execute` (invoke) or the
/// `Action.Submit` fallback (a message whose `value` is the action data).
fn submitted_action(value: &Value) -> Option<(String, Value)> {
    let verb = value.get("neev").and_then(Value::as_str)?;
    if verb.is_empty() || verb.len() > 64 {
        return None;
    }
    Some((verb.to_owned(), value.clone()))
}

pub(crate) fn normalize(activity: &Activity) -> Result<Inbound, ActivityError> {
    if activity.channel_id != "msteams" {
        return Ok(Inbound::Ignored);
    }
    match activity.kind.as_str() {
        "message" => {
            let envelope = envelope(activity)?;
            if let Some((verb, data)) = activity.value.as_ref().and_then(submitted_action) {
                return Ok(Inbound::Action(CardAction {
                    envelope,
                    verb,
                    data,
                    invoke: false,
                }));
            }
            // Channel and group traffic is mention-only: Neev never reads a
            // conversation it was not explicitly addressed in.
            if envelope.scope != Scope::Personal && !mentions_bot(activity) {
                return Ok(Inbound::Ignored);
            }
            let text = clean_text(activity.text.as_deref().unwrap_or_default());
            if text.is_empty() {
                return Ok(Inbound::Ignored);
            }
            Ok(Inbound::Message(InboundMessage { envelope, text }))
        }
        "invoke" if activity.name.as_deref() == Some("adaptiveCard/action") => {
            let envelope = envelope(activity)?;
            let action = activity
                .value
                .as_ref()
                .and_then(|value| value.get("action"))
                .ok_or(ActivityError::Malformed)?;
            let verb = action
                .get("verb")
                .and_then(Value::as_str)
                .filter(|verb| !verb.is_empty() && verb.len() <= 64)
                .ok_or(ActivityError::Malformed)?
                .to_owned();
            let data = action.get("data").cloned().unwrap_or(Value::Null);
            Ok(Inbound::Action(CardAction {
                envelope,
                verb,
                data,
                invoke: true,
            }))
        }
        "conversationUpdate"
            if activity
                .members_added
                .iter()
                .any(|member| member.id == activity.recipient.id) =>
        {
            Ok(Inbound::Installed(envelope(activity)?))
        }
        "installationUpdate" if activity.action.as_deref() == Some("add") => {
            Ok(Inbound::Installed(envelope(activity)?))
        }
        "conversationUpdate" | "installationUpdate" => Ok(lifecycle(activity)?
            .map(Inbound::Lifecycle)
            .unwrap_or(Inbound::Ignored)),
        _ => Ok(Inbound::Ignored),
    }
}

fn team_id_of(activity: &Activity) -> Option<String> {
    activity
        .channel_data
        .as_ref()
        .and_then(|data| data.pointer("/team/id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 256)
        .map(str::to_owned)
}

/// Removal and deletion events (Teams `channelData.eventType`, or the bot
/// itself in `membersRemoved` / an `installationUpdate` "remove").
fn lifecycle(activity: &Activity) -> Result<Option<Lifecycle>, ActivityError> {
    let tenant_id = tenant_of(activity)?;
    let event_type = activity
        .channel_data
        .as_ref()
        .and_then(|data| data.get("eventType"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let bot_removed = activity.kind == "conversationUpdate"
        && activity
            .members_removed
            .iter()
            .any(|member| member.id == activity.recipient.id);
    let uninstalled = activity.kind == "installationUpdate"
        && matches!(activity.action.as_deref(), Some("remove"));
    let event = match event_type {
        "channelDeleted" => activity
            .channel_data
            .as_ref()
            .and_then(|data| data.pointer("/channel/id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .map(|channel_id| LifecycleEvent::ChannelDeleted {
                channel_id: channel_id.to_owned(),
            }),
        "teamDeleted" | "teamHardDeleted" | "teamArchived" => team_id_of(activity)
            .map(|team_id| LifecycleEvent::TeamSuspended { team_id }),
        _ if bot_removed || uninstalled => team_id_of(activity)
            .map(|team_id| LifecycleEvent::TeamSuspended { team_id }),
        _ => None,
    };
    Ok(event.map(|event| Lifecycle { tenant_id, event }))
}

/// A stable digest naming one inbound activity. Bot Connector retries reuse
/// the activity id, so a retry digests identically.
pub(crate) fn idempotency_key(envelope: &Envelope) -> String {
    digest(&[
        "msteams-activity-v1",
        &envelope.tenant_id,
        &envelope.conversation_id,
        &envelope.activity_id,
    ])
}

pub(crate) fn digest(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

/// A UUID-shaped request id derived from a digest, for Neev's idempotent
/// request columns (`neev_agent_turns.request_id`, `neev_requests`).
pub(crate) fn request_uuid(hex_digest: &str) -> String {
    let mut bytes = [0u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex_digest.get(index * 2..index * 2 + 2).unwrap_or("00"), 16)
            .unwrap_or(0);
    }
    // RFC 4122 variant, version 8 ("custom"): deterministic, not random.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}
