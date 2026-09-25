use super::{access, attachment, mentions, resources, types::*};
use crate::neev::{
    auth::session, db, error::database_error, org, startup::ServerState, types::OrgContext,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, QueryResult};

fn message(row: &QueryResult) -> Result<Message, ContentError> {
    let author_kind: String = row.try_get("", "author_kind").map_err(database_error)?;
    let agent_label: Option<String> = row.try_get("", "agent_label").map_err(database_error)?;
    let author_kind = AuthorKind::parse(&author_kind).ok_or(ContentError::Storage)?;
    // The pairing SQLite cannot express as a table CHECK: an agent message names
    // its provider and a human one never does. A row that breaks it is storage
    // corruption, not something to render as "some agent".
    if (author_kind == AuthorKind::Agent) != agent_label.is_some() {
        return Err(ContentError::Storage);
    }
    Ok(Message {
        id: row.try_get("", "id").map_err(database_error)?,
        sequence: row.try_get("", "sequence").map_err(database_error)?,
        author_id: row.try_get("", "author_id").map_err(database_error)?,
        author_name: row.try_get("", "author_name").map_err(database_error)?,
        body: row.try_get("", "body").map_err(database_error)?,
        created_at: row.try_get("", "created_at").map_err(database_error)?,
        author_kind,
        agent_label,
    })
}

/// Every message read selects the same projection, so a new column cannot be
/// exposed on one path and silently missing on another.
const MESSAGE_COLUMNS: &str = "m.id,m.sequence,m.author_id,a.display_name AS author_name,m.body,m.created_at,m.author_kind,m.agent_label";

async fn load(
    tx: &DatabaseTransaction,
    org_id: i32,
    chat_id: &str,
    id: &str,
) -> Result<Message, ContentError> {
    let row=db::one(tx,&format!("SELECT {MESSAGE_COLUMNS} FROM neev_messages m JOIN neev_accounts a ON a.id=m.author_id WHERE m.org_id=$1 AND m.chat_id=$2 AND m.id=$3"),vec![org_id.into(),chat_id.into(),id.into()]).await?.ok_or(ContentError::NotFound)?;
    message(&row)
}

/// Prove every key was uploaded to THIS chat, then bind it to the message —
/// inside the transaction that creates the message.
///
/// Atomic on purpose: a message sent WITH files must never become durable
/// without them, and a reference must never outlive a rolled-back message. The
/// ownership re-check is what stops a sender carrying a file across from
/// another conversation they happen to be able to read — the attachment would
/// otherwise inherit THIS chat's audience.
async fn link_attachments(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    chat_id: &str,
    message_id: &str,
    keys: &[String],
) -> Result<(), ContentError> {
    if keys.is_empty() {
        return Ok(());
    }
    if keys.len() > attachment::MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(ContentError::Invalid);
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut total: i64 = 0;
    for key in keys {
        if !seen.insert(key.as_str()) {
            return Err(ContentError::Invalid);
        }
        let row = db::one(
            tx,
            "SELECT chat_id,byte_length FROM neev_attachments WHERE org_id=$1 AND key=$2",
            vec![actor.org_id.into(), key.clone().into()],
        )
        .await?
        .ok_or(ContentError::Invalid)?;
        let owner: Option<String> = row.try_get("", "chat_id").map_err(database_error)?;
        if owner.as_deref() != Some(chat_id) {
            return Err(ContentError::Invalid);
        }
        // The same ceiling the run enforces, applied where the files become
        // durable: a message that could never be sent to an agent must not be
        // storable either, or the thread accumulates sends nothing can act on.
        total += row
            .try_get::<i64>("", "byte_length")
            .map_err(database_error)?;
        if total > attachment::MAX_RUN_ATTACHMENT_BYTES {
            return Err(ContentError::Quota);
        }
    }
    attachment::link_to_message(tx, actor, message_id, keys).await
}

pub(crate) async fn append(
    state: &ServerState,
    actor: &OrgContext,
    chat_id: &str,
    request_id: &str,
    expected_auth_revision: i64,
    body: &str,
) -> Result<Message, ContentError> {
    append_with_attachments(
        state,
        actor,
        chat_id,
        request_id,
        expected_auth_revision,
        body,
        &[],
        &[],
    )
    .await
}

/// Append a message that carries uploaded files.
///
/// The keys are part of the idempotency hash, so a retry of the same request id
/// replays the same message, and a retry that changed the files is a conflict
/// rather than a second, quietly different meaning for one request id.
pub(crate) async fn append_with_attachments(
    state: &ServerState,
    actor: &OrgContext,
    chat_id: &str,
    request_id: &str,
    expected_auth_revision: i64,
    body: &str,
    attachment_keys: &[String],
    mentions: &[mentions::MentionInput],
) -> Result<Message, ContentError> {
    if body.is_empty() || body.len() > 32768 || expected_auth_revision < 1 {
        return Err(ContentError::Invalid);
    }
    if attachment_keys.len() > attachment::MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(ContentError::Invalid);
    }
    if mentions.len() > mentions::MAX_MENTIONS_PER_MESSAGE {
        return Err(ContentError::Invalid);
    }
    let request = org::normalize_request_id(request_id).map_err(|_| ContentError::Invalid)?;
    let tenant = actor.org_id.to_string();
    let revision = expected_auth_revision.to_string();
    // The files are part of what the request MEANT, so a retry that changed
    // them is a conflict rather than a replay of a different send. Appended
    // only when there are any: a send without files has to hash exactly as it
    // did before attachments existed, or every receipt already in storage would
    // start reading as a conflict.
    let attached = attachment_keys.join(",");
    let mut parts = vec![
        "content.message.append",
        tenant.as_str(),
        chat_id,
        revision.as_str(),
        body,
    ];
    if !attachment_keys.is_empty() {
        parts.push(attached.as_str());
    }
    // The people named are part of what the request meant too, so a retry that
    // changed them is a conflict rather than a replay. Appended only when there
    // are any, for the same reason the files are: a send without mentions has
    // to hash exactly as it did before mentions existed, or every receipt
    // already in storage would start reading as a conflict.
    let named = mentions::digest_part(mentions);
    if !mentions.is_empty() {
        parts.push(named.as_str());
    }
    let hash = org::digest_parts(&parts);
    let tx = resources::begin(state, actor).await?;
    access::lock_resources(&tx, actor, chat_id, false).await?;
    let chat = access::authorized(&tx, actor, chat_id, false).await?;
    if chat.kind != ResourceKind::Chat {
        return Err(ContentError::NotFound);
    }
    // A committed retry returns the existing message even after a rename/archive,
    // but only while the actor retains current read authority over this chat.
    if let Some(id) =
        resources::receipt(&tx, actor, "content.message.append", &request, &hash).await?
    {
        let result = load(&tx, actor.org_id, chat_id, &id).await?;
        session::recheck(&tx, actor, state.clock.now_unix()).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(result);
    }
    access::authorized(&tx, actor, chat_id, true).await?;
    if chat.auth_revision != expected_auth_revision {
        return Err(ContentError::Conflict);
    }
    let sequence = chat
        .content_revision
        .checked_add(1)
        .ok_or(ContentError::Conflict)?;
    let bytes = i64::try_from(body.len()).map_err(|_| ContentError::Invalid)?;
    resources::initialize_usage(&tx, actor.org_id).await?;
    let charged=db::execute(&tx,"UPDATE neev_content_usage SET charged_bytes=charged_bytes+$1,messages=messages+1 WHERE org_id=$2 AND messages<100000 AND charged_bytes<=1073741824-$1",vec![bytes.into(),actor.org_id.into()]).await?;
    if charged.rows_affected() != 1 {
        return Err(ContentError::Quota);
    }
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid);
    }
    let id = uuid::Uuid::new_v4().to_string();
    db::execute(&tx,"INSERT INTO neev_messages(org_id,chat_id,resource_kind,id,sequence,author_id,body,byte_length,created_at,author_kind,agent_label) VALUES($1,$2,'chat',$3,$4,$5,$6,$7,$8,'human',NULL)",vec![actor.org_id.into(),chat_id.into(),id.clone().into(),sequence.into(),actor.identity.user_id.into(),body.into(),bytes.into(),now.into()]).await?;
    db::execute(
        &tx,
        "UPDATE neev_resources SET content_revision=$1,updated_at=$2 WHERE org_id=$3 AND id=$4",
        vec![
            sequence.into(),
            now.into(),
            actor.org_id.into(),
            chat_id.into(),
        ],
    )
    .await?;
    // Before the receipt: if a key does not belong here the whole append rolls
    // back, so no request id is ever spent on a message that was refused.
    link_attachments(&tx, actor, chat_id, &id, attachment_keys).await?;
    // Before the receipt as well: a mention this chat may not carry takes the
    // whole append back with it, so no request id is ever spent on a message
    // that was refused — and a retry can never leave a second notice.
    mentions::link_to_message(&tx, actor, &chat, &id, body, mentions).await?;
    resources::record_receipt(&tx, actor, "content.message.append", &request, &hash, &id).await?;
    let result = load(&tx, actor.org_id, chat_id, &id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    // Only after the row is durable: a rolled-back append must never wake a
    // reader. The notice carries the sequence alone — subscribers re-read the
    // conversation through `list`, which re-runs this same ACL.
    state.realtime.publish(actor.org_id, chat_id, sequence);
    Ok(result)
}

/// Publish an agent-authored reply into a team chat (spec §5.3).
///
/// Called by the run driver *after* the run finished, not by the request that
/// started it, so it deliberately does NOT recheck the browser session: the
/// invocation was authorized when it was made, the run is server-owned, and the
/// answer belongs to the team even if the invoker has since closed their tab.
///
/// What it does still re-prove, against current storage:
/// - the invoker retains **write** authority over this chat (a member removed
///   from the org, or a chat that became private to someone else, publishes
///   nothing);
/// - the chat is not archived;
/// - the org's content quota admits the message.
///
/// The row is written with `author_kind='agent'` and `agent_label=<provider>`,
/// while `author_id` stays the human invoker — the message is *attributable*
/// ("Claude, invoked by Sam") without ever being *attributed* to a human.
pub(crate) async fn publish_agent_reply(
    state: &ServerState,
    invoker: &OrgContext,
    chat_id: &str,
    body: &str,
    agent_label: &str,
    turn_id: &str,
) -> Result<Message, ContentError> {
    if body.is_empty() || body.len() > 32768 {
        return Err(ContentError::Invalid);
    }
    if !matches!(agent_label, "claude" | "codex") {
        return Err(ContentError::Invalid);
    }
    // The turn is the identity of this reply. Recording a receipt against it
    // means a replay — a retried invocation, or a second settlement attempt
    // after a partial failure — returns the message that already exists instead
    // of posting the agent's answer twice.
    let request = org::normalize_request_id(turn_id).map_err(|_| ContentError::Invalid)?;
    let hash = org::digest_parts(&[
        "agent.team.publish",
        &invoker.org_id.to_string(),
        chat_id,
        agent_label,
        body,
    ]);
    let tx = db::begin(&state.database).await?;
    db::lock_admission_guard(&tx).await?;
    if let Some(id) = resources::receipt(&tx, invoker, "agent.team.publish", &request, &hash).await?
    {
        let existing = load(&tx, invoker.org_id, chat_id, &id).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(existing);
    }
    access::lock_resources(&tx, invoker, chat_id, false).await?;
    // `write = true`: publishing on someone's behalf requires that they could
    // still post here themselves.
    let chat = access::authorized(&tx, invoker, chat_id, true).await?;
    if chat.kind != ResourceKind::Chat {
        return Err(ContentError::NotFound);
    }
    let sequence = chat
        .content_revision
        .checked_add(1)
        .ok_or(ContentError::Conflict)?;
    let bytes = i64::try_from(body.len()).map_err(|_| ContentError::Invalid)?;
    resources::initialize_usage(&tx, invoker.org_id).await?;
    let charged=db::execute(&tx,"UPDATE neev_content_usage SET charged_bytes=charged_bytes+$1,messages=messages+1 WHERE org_id=$2 AND messages<100000 AND charged_bytes<=1073741824-$1",vec![bytes.into(),invoker.org_id.into()]).await?;
    if charged.rows_affected() != 1 {
        return Err(ContentError::Quota);
    }
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid);
    }
    let id = uuid::Uuid::new_v4().to_string();
    db::execute(&tx,"INSERT INTO neev_messages(org_id,chat_id,resource_kind,id,sequence,author_id,body,byte_length,created_at,author_kind,agent_label) VALUES($1,$2,'chat',$3,$4,$5,$6,$7,$8,'agent',$9)",vec![invoker.org_id.into(),chat_id.into(),id.clone().into(),sequence.into(),invoker.identity.user_id.into(),body.into(),bytes.into(),now.into(),agent_label.into()]).await?;
    db::execute(
        &tx,
        "UPDATE neev_resources SET content_revision=$1,updated_at=$2 WHERE org_id=$3 AND id=$4",
        vec![
            sequence.into(),
            now.into(),
            invoker.org_id.into(),
            chat_id.into(),
        ],
    )
    .await?;
    resources::record_receipt(&tx, invoker, "agent.team.publish", &request, &hash, &id).await?;
    let result = load(&tx, invoker.org_id, chat_id, &id).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

pub(crate) async fn list(
    state: &ServerState,
    actor: &OrgContext,
    chat_id: &str,
    after: Option<i64>,
) -> Result<Page<Message>, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let chat = access::authorized(&tx, actor, chat_id, false).await?;
    if chat.kind != ResourceKind::Chat {
        return Err(ContentError::NotFound);
    }
    if let Some(cursor) = after {
        if cursor < 1
            || db::one(
                &tx,
                "SELECT id FROM neev_messages WHERE org_id=$1 AND chat_id=$2 AND sequence=$3",
                vec![actor.org_id.into(), chat_id.into(), cursor.into()],
            )
            .await?
            .is_none()
        {
            return Err(ContentError::NotFound);
        }
    }
    let rows=tx.query_all(db::statement(&tx,&format!("SELECT {MESSAGE_COLUMNS} FROM neev_messages m JOIN neev_accounts a ON a.id=m.author_id WHERE m.org_id=$1 AND m.chat_id=$2 AND m.sequence>$3 ORDER BY m.sequence LIMIT 101"),vec![actor.org_id.into(),chat_id.into(),after.unwrap_or(0).into()])).await.map_err(database_error)?;
    let has_more = rows.len() > 100;
    let items = rows
        .iter()
        .take(100)
        .map(message)
        .collect::<Result<Vec<_>, _>>()?;
    let next_after = if has_more {
        items.last().map(|message| message.sequence.to_string())
    } else {
        None
    };
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(Page { items, next_after })
}
