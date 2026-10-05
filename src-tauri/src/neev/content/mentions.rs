//! Naming a coworker in a message, and the inbox that is nothing more than the
//! rows it wrote.
//!
//! Three rules hold this together, and each one is enforced here rather than
//! trusted from the client:
//!
//!  - **A mention names an identity.** The row carries the account id and the
//!    span of the body that says so, so reading a message back never has to
//!    match a name against a directory and guess.
//!  - **A mention can never widen an audience.** Only a chat the whole
//!    organization can already read accepts them — or a shared chat, where
//!    every person named must already be able to read it — and only an ACTIVE
//!    member of the sender's own organization can be named. A private conversation
//!    notifies nobody, because there is nobody in it to notify.
//!  - **Only a person can write one.** This is called from the human append
//!    path alone; `publish_agent_reply` records nothing here, so text an agent
//!    produces containing `@Ana` is exactly that — text.
//!
//! Offsets are Unicode code points into the stored body. The span is proved
//! against the body before it is stored: the characters it covers must read as
//! `@` followed by that account's display name, so a span can neither point at
//! somebody else's words nor at nothing at all.

use super::{access, resources, types::*};
use crate::neev::{
    auth::session, db, error::database_error, startup::ServerState, types::OrgContext,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction};

/// As many people as one message may name. Mirrored by the composer and by the
/// schema's own CHECK.
pub(crate) const MAX_MENTIONS_PER_MESSAGE: usize = 10;
/// How many message ids one lookup may ask about; mirrors `attachment`.
const MAX_REF_LOOKUP: usize = 200;
/// How many notices the inbox hands back at once.
pub(crate) const INBOX_LIMIT: u32 = 50;
/// How much of a message the inbox quotes, in characters.
const EXCERPT_CHARS: usize = 160;

/// What a send claims: who, and where they are said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MentionInput {
    pub user_id: i32,
    pub start: i64,
    pub length: i64,
}

/// What a reader gets back: the same span, proved and stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StoredMention {
    pub user_id: i32,
    pub start: i64,
    pub length: i64,
}

/// One notice in somebody's inbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub message_id: String,
    pub chat_id: String,
    pub chat_title: String,
    pub author_name: String,
    pub excerpt: String,
    pub created_at: i64,
    pub read: bool,
}

/// The part of the body a span covers, if the span fits it.
fn spanned(body: &str, start: i64, length: i64) -> Option<String> {
    let start = usize::try_from(start).ok()?;
    let length = usize::try_from(length).ok()?;
    let mut chars = body.chars().skip(start);
    let taken: String = chars.by_ref().take(length).collect();
    if taken.chars().count() != length {
        return None;
    }
    Some(taken)
}

/// Fold the mentions into the request's meaning, so a retry that changed them
/// is a conflict rather than a second, quietly different send.
pub(crate) fn digest_part(mentions: &[MentionInput]) -> String {
    mentions
        .iter()
        .map(|mention| format!("{}:{}:{}", mention.user_id, mention.start, mention.length))
        .collect::<Vec<_>>()
        .join(",")
}

/// Prove every mention, then bind it to the message — inside the transaction
/// that creates the message.
///
/// Atomic for the same reason attachments are: a notice must never become
/// durable without the message it points at, and a message that was refused
/// must never leave a notice behind.
pub(super) async fn link_to_message(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    chat: &Resource,
    message_id: &str,
    body: &str,
    mentions: &[MentionInput],
) -> Result<(), ContentError> {
    if mentions.is_empty() {
        return Ok(());
    }
    if mentions.len() > MAX_MENTIONS_PER_MESSAGE {
        return Err(ContentError::Invalid);
    }
    // The audience rule, stated once: a mention may only exist where everybody
    // named can already read what they are being named in. An organization
    // chat is read by every member; a shared one only by its grantees, so each
    // person named is checked against the chat's own rule below. A private
    // chat has nobody else in it.
    if !matches!(chat.visibility, Visibility::Organization | Visibility::Shared)
        || chat.origin_chat_id.is_some()
    {
        return Err(ContentError::Invalid);
    }
    let mut reach = 0i64;
    for (position, mention) in mentions.iter().enumerate() {
        // Ascending and non-overlapping: two spans over the same characters
        // would make the message read differently depending on which was drawn
        // first, and a reader cannot be shown two answers.
        if mention.start < reach || mention.length < 2 || mention.user_id <= 0 {
            return Err(ContentError::Invalid);
        }
        let text = spanned(body, mention.start, mention.length).ok_or(ContentError::Invalid)?;
        // An ACTIVE membership of the SENDER's own organization, and an account
        // that is not disabled. Anyone else — another tenant's member, someone
        // removed, a deactivated account — is not somebody this message may
        // name, and the whole send is refused rather than quietly stripped.
        let row = db::one(
            tx,
            "SELECT a.display_name FROM neev_memberships m JOIN neev_accounts a ON a.id=m.user_id WHERE m.org_id=$1 AND m.user_id=$2 AND m.active=true AND a.disabled=false",
            vec![actor.org_id.into(), mention.user_id.into()],
        )
        .await?
        .ok_or(ContentError::Invalid)?;
        let display_name: String = row.try_get("", "display_name").map_err(database_error)?;
        if super::grants::level(tx, actor.org_id, mention.user_id, chat)
            .await?
            .is_none()
        {
            return Err(ContentError::Invalid);
        }
        // The body has to actually say it. A span that covers anything but this
        // person's own name is a claim about somebody else's words.
        if text != format!("@{display_name}") {
            return Err(ContentError::Invalid);
        }
        db::execute(
            tx,
            "INSERT INTO neev_message_mentions(org_id,message_id,position,user_id,start_offset,length,read_at) VALUES($1,$2,$3,$4,$5,$6,NULL)",
            vec![
                actor.org_id.into(),
                message_id.into(),
                i32::try_from(position)
                    .map_err(|_| ContentError::Invalid)?
                    .into(),
                mention.user_id.into(),
                mention.start.into(),
                mention.length.into(),
            ],
        )
        .await?;
        reach = mention
            .start
            .checked_add(mention.length)
            .ok_or(ContentError::Invalid)?;
    }
    Ok(())
}

fn placeholders(count: usize, first: usize) -> String {
    (0..count)
        .map(|index| format!("${}", first + index))
        .collect::<Vec<_>>()
        .join(",")
}

/// The spans carried by a page of messages, for the reader who is looking at
/// them. The caller has already proved its authority over the conversation
/// those messages belong to.
pub(crate) async fn for_messages(
    state: &ServerState,
    actor: &OrgContext,
    message_ids: &[String],
) -> Result<std::collections::BTreeMap<String, Vec<StoredMention>>, ContentError> {
    let mut out: std::collections::BTreeMap<String, Vec<StoredMention>> =
        std::collections::BTreeMap::new();
    if message_ids.is_empty() {
        return Ok(out);
    }
    if message_ids.len() > MAX_REF_LOOKUP {
        return Err(ContentError::Invalid);
    }
    let tx = db::begin(&state.database).await?;
    let mut args: Vec<sea_orm::Value> = vec![actor.org_id.into()];
    for id in message_ids {
        args.push(id.clone().into());
    }
    let sql = format!(
        "SELECT message_id,user_id,start_offset,length FROM neev_message_mentions WHERE org_id=$1 AND message_id IN ({}) ORDER BY message_id,position",
        placeholders(message_ids.len(), 2)
    );
    let rows = tx
        .query_all(db::statement(&tx, &sql, args))
        .await
        .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    for row in &rows {
        let owner: String = row.try_get("", "message_id").map_err(database_error)?;
        out.entry(owner).or_default().push(StoredMention {
            user_id: row.try_get("", "user_id").map_err(database_error)?,
            start: row.try_get("", "start_offset").map_err(database_error)?,
            length: row.try_get("", "length").map_err(database_error)?,
        });
    }
    Ok(out)
}

/// One reader's notices, newest first.
///
/// The ACL is re-applied HERE, on every read, rather than captured when the
/// mention was written: a conversation that has since been made private, or an
/// organization somebody has left, shows nothing. `session::recheck` proves the
/// membership in the same transaction.
pub(crate) async fn inbox(
    state: &ServerState,
    actor: &OrgContext,
) -> Result<(Vec<Notice>, i64), ContentError> {
    let tx = resources::begin(state, actor).await?;
    let readable = super::grants::Audience::load(&tx, actor.org_id, actor.identity.user_id)
        .await?
        .readable_unbound("r");
    let rows = tx
        .query_all(db::statement(
            &tx,
            &format!("SELECT m.id AS message_id,m.chat_id,m.body,m.created_at,a.display_name AS author_name,r.title AS chat_title,SUM(CASE WHEN n.read_at IS NULL THEN 1 ELSE 0 END) AS unread FROM neev_message_mentions n JOIN neev_messages m ON m.org_id=n.org_id AND m.id=n.message_id JOIN neev_resources r ON r.org_id=m.org_id AND r.id=m.chat_id JOIN neev_accounts a ON a.id=m.author_id WHERE n.org_id=$1 AND n.user_id=$2 AND m.author_id<>$2 AND r.kind='chat' AND {readable} GROUP BY m.id,m.chat_id,m.body,m.created_at,a.display_name,r.title ORDER BY (SUM(CASE WHEN n.read_at IS NULL THEN 1 ELSE 0 END)>0) DESC,m.created_at DESC,m.id LIMIT $3"),
            vec![
                actor.org_id.into(),
                actor.identity.user_id.into(),
                i64::from(INBOX_LIMIT).into(),
            ],
        ))
        .await
        .map_err(database_error)?;
    // Counted over EVERY notice this reader still has, not over the page: an
    // indicator that only counts the newest fifty goes quiet exactly when
    // somebody is behind. Same joins, same ACL, so the two can never disagree
    // about what is visible.
    let counted = db::one(
        &tx,
        &format!("SELECT COUNT(DISTINCT m.id) AS c FROM neev_message_mentions n JOIN neev_messages m ON m.org_id=n.org_id AND m.id=n.message_id JOIN neev_resources r ON r.org_id=m.org_id AND r.id=m.chat_id WHERE n.org_id=$1 AND n.user_id=$2 AND n.read_at IS NULL AND m.author_id<>$2 AND r.kind='chat' AND {readable}"),
        vec![actor.org_id.into(), actor.identity.user_id.into()],
    )
    .await?
    .ok_or(ContentError::Storage)?;
    let unread_total: i64 = counted.try_get("", "c").map_err(database_error)?;
    let mut notices = Vec::with_capacity(rows.len());
    for row in &rows {
        let body: String = row.try_get("", "body").map_err(database_error)?;
        let unread: i64 = row.try_get("", "unread").map_err(database_error)?;
        notices.push(Notice {
            message_id: row.try_get("", "message_id").map_err(database_error)?,
            chat_id: row.try_get("", "chat_id").map_err(database_error)?,
            chat_title: row.try_get("", "chat_title").map_err(database_error)?,
            author_name: row.try_get("", "author_name").map_err(database_error)?,
            excerpt: excerpt(&body),
            created_at: row.try_get("", "created_at").map_err(database_error)?,
            read: unread == 0,
        });
    }
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok((notices, unread_total))
}

/// What the notice quotes. A single line, bounded: this is a pointer to a
/// conversation, not a copy of it.
fn excerpt(body: &str) -> String {
    let flat = body
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let trimmed = flat.trim();
    if trimmed.chars().count() <= EXCERPT_CHARS {
        return trimmed.to_owned();
    }
    trimmed.chars().take(EXCERPT_CHARS).collect::<String>()
}

/// Mark notices read. Idempotent: a second call over the same messages changes
/// nothing, which is what makes it safe to call from a link a reader follows.
pub(crate) async fn mark_read(
    state: &ServerState,
    actor: &OrgContext,
    message_ids: &[String],
) -> Result<(), ContentError> {
    if message_ids.is_empty() {
        return Ok(());
    }
    if message_ids.len() > INBOX_LIMIT as usize {
        return Err(ContentError::Invalid);
    }
    for id in message_ids {
        access::canonical_id(id)?;
    }
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid);
    }
    let tx = resources::begin(state, actor).await?;
    let readable = super::grants::Audience::load(&tx, actor.org_id, actor.identity.user_id)
        .await?
        .readable_unbound("r");
    let mut args: Vec<sea_orm::Value> = vec![
        now.into(),
        actor.org_id.into(),
        actor.identity.user_id.into(),
    ];
    for id in message_ids {
        args.push(id.clone().into());
    }
    // Only this reader's own rows, only the unread ones — read_at is when a
    // notice was first seen, not when it was last looked at — and only where
    // the conversation is one they can still read. A stale link to a chat that
    // has since been made private changes nothing, exactly as reading it shows
    // nothing.
    let sql = format!(
        "UPDATE neev_message_mentions SET read_at=$1 WHERE org_id=$2 AND user_id=$3 AND read_at IS NULL AND message_id IN (SELECT m.id FROM neev_messages m JOIN neev_resources r ON r.org_id=m.org_id AND r.id=m.chat_id WHERE m.org_id=$2 AND r.kind='chat' AND {readable} AND m.id IN ({}))",
        placeholders(message_ids.len(), 4)
    );
    db::execute(&tx, &sql, args).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(())
}
