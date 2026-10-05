//! Fork a resource chat into a NEW private chat, up through one agent reply.
//!
//! The fork is a copy, never a link: every message up through the selected
//! reply is written again into a chat the forker owns, with its original
//! author, author kind, agent label and time, and every file those messages
//! carry is copied byte-for-byte to a fresh attachment key owned by the new
//! chat. Nothing is shared afterwards — no row, no blob, no provider session —
//! so continuing either conversation can never change the other. The original
//! is only read.
//!
//! Audience never widens. The fork is `private` to the forker whatever its
//! folder's default (an explicit audience wins over the folder default and is
//! materialized, `content::sharing`), and it copies only what the forker can
//! read now. Mentions are not copied: a notice about a private chat the person
//! cannot open would be a broken promise. Bound documents stay with the
//! original.
//!
//! Three phases keep the global admission guard short:
//! 1. a guarded snapshot of what to copy (ACL, selection, bounds, quota hint);
//! 2. the bytes, copied with NO transaction open, each verified against the
//!    stored digest;
//! 3. a guarded commit that re-proves the ACL and the snapshot, charges the
//!    quotas and writes everything atomically. Any failure after phase 2
//!    removes the fresh blobs again.

use std::collections::{BTreeMap, BTreeSet};

use sea_orm::{ConnectionTrait, DatabaseTransaction, Value};

use super::{access, attachment, resources, types::*};
use crate::neev::{
    auth::session, db, error::database_error, org, startup::ServerState, types::OrgContext,
};

/// The most messages one fork copies. A longer history is refused with a
/// reason rather than cut short: a fork that silently lost its start would
/// misrepresent the conversation it claims to continue.
pub(crate) const MAX_FORK_MESSAGES: usize = 1_000;
/// The most message text one fork copies.
pub(crate) const MAX_FORK_TEXT_BYTES: i64 = 8 * 1024 * 1024;
/// The most files one fork copies, and their total size. Over either, the
/// fork is refused (`AttachmentsTooLarge`) — never made without them.
pub(crate) const MAX_FORK_ATTACHMENTS: usize = 200;
pub(crate) const MAX_FORK_ATTACHMENT_BYTES: i64 = 256 * 1024 * 1024;

/// The link form the server writes for an attachment (`generated::rewrite_output_links`,
/// and what `NativeMarkdown` accepts). Copied bodies are pointed at the copy.
const ATTACHMENT_PATH: &str = "/api/neev/attachments/";

/// Why a fork was refused before anything was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForkError {
    Content(ContentError),
    /// The selection is not a completed reply (a person's message, a running
    /// or failed turn).
    NotComplete,
    /// More history than one fork copies.
    TooLarge,
    /// More files, or more bytes of files, than one fork copies.
    AttachmentsTooLarge,
    /// The source sits in a folder that is in the trash.
    SourceTrashed,
    /// An Agent Chat bound to a chat the forker can no longer read.
    BoundChatUnavailable,
}

impl From<ContentError> for ForkError {
    fn from(value: ContentError) -> Self {
        Self::Content(value)
    }
}

impl From<crate::neev::error::AuthError> for ForkError {
    fn from(value: crate::neev::error::AuthError) -> Self {
        Self::Content(ContentError::Auth(value))
    }
}

/// One stored file to copy.
#[derive(Clone, Debug)]
pub(crate) struct FileSnapshot {
    pub key: String,
    pub file_name: String,
    pub content_type: String,
    pub byte_length: i64,
    pub sha256: String,
    pub created_by: i32,
    pub created_at: i64,
}

/// A copied file: the old key and the fresh one, with what was written.
#[derive(Clone, Debug)]
pub(crate) struct CopiedFile {
    pub old_key: String,
    pub new_key: String,
    pub file: FileSnapshot,
}

/// Read a file column set as `FileSnapshot`, refusing anything the store would
/// not have written.
pub(crate) fn file_row(row: &sea_orm::QueryResult) -> Result<FileSnapshot, ContentError> {
    Ok(FileSnapshot {
        key: row.try_get("", "key").map_err(database_error)?,
        file_name: row.try_get("", "file_name").map_err(database_error)?,
        content_type: row.try_get("", "content_type").map_err(database_error)?,
        byte_length: row.try_get("", "byte_length").map_err(database_error)?,
        sha256: row.try_get("", "sha256").map_err(database_error)?,
        created_by: row.try_get("", "created_by").map_err(database_error)?,
        created_at: row.try_get("", "created_at").map_err(database_error)?,
    })
}

/// The bounds every fork applies to the files it carries.
pub(crate) fn check_file_bounds(files: &BTreeMap<String, FileSnapshot>) -> Result<(), ForkError> {
    let bytes: i64 = files.values().map(|file| file.byte_length).sum();
    if files.len() > MAX_FORK_ATTACHMENTS || bytes > MAX_FORK_ATTACHMENT_BYTES {
        return Err(ForkError::AttachmentsTooLarge);
    }
    Ok(())
}

/// Copy each file's bytes to a fresh key with NO transaction open. Every read
/// is verified against the stored length and digest; every write is checked
/// to have produced the same digest. On any failure the fresh blobs written so
/// far are removed and nothing is left behind.
pub(crate) async fn copy_files(
    state: &ServerState,
    org_id: i32,
    files: &BTreeMap<String, FileSnapshot>,
) -> Result<Vec<CopiedFile>, ContentError> {
    let mut copied = Vec::with_capacity(files.len());
    for file in files.values() {
        let result = async {
            let bytes = state
                .blobs
                .read_attachment(org_id, &file.key, file.byte_length, &file.sha256)
                .await?;
            let new_key = uuid::Uuid::new_v4().hyphenated().to_string();
            let info = state.blobs.put_attachment(org_id, &new_key, &bytes).await?;
            if info.sha256 != file.sha256 || info.byte_length != file.byte_length {
                let _ = state.blobs.remove_attachment(org_id, &new_key).await;
                return Err(ContentError::Storage);
            }
            Ok(new_key)
        }
        .await;
        match result {
            Ok(new_key) => copied.push(CopiedFile {
                old_key: file.key.clone(),
                new_key,
                file: file.clone(),
            }),
            Err(error) => {
                discard_files(state, org_id, &copied).await;
                return Err(error);
            }
        }
    }
    Ok(copied)
}

/// Best-effort removal of fresh blobs whose rows never committed.
pub(crate) async fn discard_files(state: &ServerState, org_id: i32, copied: &[CopiedFile]) {
    for file in copied {
        let _ = state.blobs.remove_attachment(org_id, &file.new_key).await;
    }
}

/// Re-prove, inside the committing transaction, that every file still exists
/// unarchived with the same bytes it had when it was copied.
pub(crate) async fn verify_files(
    tx: &DatabaseTransaction,
    org_id: i32,
    copied: &[CopiedFile],
) -> Result<(), ForkError> {
    for file in copied {
        let row = db::one(
            tx,
            "SELECT sha256,byte_length,archived FROM neev_attachments WHERE org_id=$1 AND key=$2",
            vec![org_id.into(), file.old_key.clone().into()],
        )
        .await?
        .ok_or(ContentError::Conflict)?;
        let sha: String = row.try_get("", "sha256").map_err(database_error)?;
        let length: i64 = row.try_get("", "byte_length").map_err(database_error)?;
        let archived: bool = row.try_get("", "archived").map_err(database_error)?;
        if archived || sha != file.file.sha256 || length != file.file.byte_length {
            return Err(ForkError::Content(ContentError::Conflict));
        }
    }
    Ok(())
}

/// The org's attachment budget, charged for the copies (the same rule as an
/// upload, `attachment::store`).
pub(crate) async fn charge_files(
    tx: &DatabaseTransaction,
    org_id: i32,
    copied: &[CopiedFile],
) -> Result<(), ForkError> {
    if copied.is_empty() {
        return Ok(());
    }
    let usage = db::one(
        tx,
        "SELECT COUNT(*) AS c, CAST(COALESCE(SUM(byte_length),0) AS BIGINT) AS b FROM neev_attachments WHERE org_id=$1",
        vec![org_id.into()],
    )
    .await?
    .ok_or(ContentError::Storage)?;
    let count: i64 = usage.try_get("", "c").map_err(database_error)?;
    let used: i64 = usage.try_get("", "b").map_err(database_error)?;
    let adding: i64 = copied.iter().map(|file| file.file.byte_length).sum();
    if count + copied.len() as i64 > attachment::MAX_ATTACHMENTS_PER_ORG
        || used + adding > attachment::MAX_ATTACHMENT_BYTES_PER_ORG
    {
        return Err(ForkError::Content(ContentError::Quota));
    }
    Ok(())
}

/// Write the copied files' rows for their new owner.
pub(crate) async fn insert_files(
    tx: &DatabaseTransaction,
    org_id: i32,
    owner: attachment::Owner<'_>,
    copied: &[CopiedFile],
) -> Result<(), ContentError> {
    for file in copied {
        let (chat, session) = match owner {
            attachment::Owner::Chat(id) => (Value::from(id.to_owned()), Value::String(None)),
            attachment::Owner::Session(id) => (Value::String(None), Value::from(id.to_owned())),
        };
        db::execute(
            tx,
            "INSERT INTO neev_attachments(org_id,key,chat_id,session_id,file_name,content_type,byte_length,sha256,created_by,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
            vec![
                org_id.into(),
                file.new_key.clone().into(),
                chat,
                session,
                file.file.file_name.clone().into(),
                file.file.content_type.clone().into(),
                file.file.byte_length.into(),
                file.file.sha256.clone().into(),
                file.file.created_by.into(),
                file.file.created_at.into(),
            ],
        )
        .await?;
    }
    Ok(())
}

/// A copied body's attachment links, pointed at the copies. Only links to the
/// files this fork copied change; the text is otherwise exactly as written
/// (a key is a fixed-width UUID, so the byte length is unchanged too).
pub(crate) fn rewrite_links(text: &str, copied: &[CopiedFile]) -> String {
    let mut out = text.to_owned();
    for file in copied {
        let old = format!("{ATTACHMENT_PATH}{}", file.old_key);
        if out.contains(&old) {
            out = out.replace(&old, &format!("{ATTACHMENT_PATH}{}", file.new_key));
        }
    }
    out
}

/// The fork's title: the source's, marked, within the title bound.
pub(crate) fn fork_title(source: &str) -> String {
    const SUFFIX: &str = " (fork)";
    let room = 100 - SUFFIX.chars().count();
    let base: String = source.trim().chars().take(room).collect();
    format!("{}{SUFFIX}", base.trim_end())
}

/// What a chat fork produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChatFork {
    pub chat_id: String,
    pub source_chat_id: String,
    pub through_message_id: String,
    pub node_id: i32,
    pub messages: i64,
    pub attachments: i64,
    /// Files the source had already archived (soft-deleted): not visible in
    /// the original either, so not copied.
    pub archived_attachments: i64,
    /// `true` when this request had already produced the fork.
    pub replayed: bool,
}

#[derive(Clone, Debug)]
struct MessageSnapshot {
    id: String,
    sequence: i64,
    author_id: i32,
    author_kind: String,
    agent_label: Option<String>,
    body: String,
    created_at: i64,
    /// (position, attachment key), in order.
    files: Vec<(i32, String)>,
}

struct ChatSnapshot {
    title: String,
    node_id: i32,
    messages: Vec<MessageSnapshot>,
    files: BTreeMap<String, FileSnapshot>,
    archived: i64,
    text_bytes: i64,
}

const FORK_ACTION: &str = "content.chat.fork";
/// Provenance, schema-free: a durable receipt keyed by the FORK's id whose
/// resource is the source chat. Read with [`chat_fork_source`].
const SOURCE_ACTION: &str = "content.chat.fork.source";

fn fork_hash(actor: &OrgContext, chat_id: &str, through: &str) -> String {
    org::digest_parts(&[FORK_ACTION, &actor.org_id.to_string(), chat_id, through])
}

/// An existing fork made by this request, as its summary.
async fn replay(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    fork_id: &str,
    chat_id: &str,
    through: &str,
) -> Result<ChatFork, ForkError> {
    let fork = access::authorized(tx, actor, fork_id, false).await?;
    let attachments: i64 = db::one(
        tx,
        "SELECT COUNT(*) AS n FROM neev_attachments WHERE org_id=$1 AND chat_id=$2",
        vec![actor.org_id.into(), fork_id.into()],
    )
    .await?
    .ok_or(ContentError::Storage)?
    .try_get("", "n")
    .map_err(database_error)?;
    Ok(ChatFork {
        chat_id: fork.id,
        source_chat_id: chat_id.to_owned(),
        through_message_id: through.to_owned(),
        node_id: fork.node_id,
        messages: fork.content_revision,
        attachments,
        archived_attachments: 0,
        replayed: true,
    })
}

/// Everything up through `through`, under the caller's open transaction.
async fn snapshot(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    chat_id: &str,
    through: &str,
) -> Result<ChatSnapshot, ForkError> {
    let chat = access::authorized(tx, actor, chat_id, false).await?;
    if chat.kind != ResourceKind::Chat {
        return Err(ContentError::NotFound.into());
    }
    if crate::neev::catalog::trash::node_trashed(tx, actor.org_id, chat.node_id).await? {
        return Err(ForkError::SourceTrashed);
    }
    let selected = db::one(
        tx,
        "SELECT sequence,author_kind FROM neev_messages WHERE org_id=$1 AND chat_id=$2 AND id=$3",
        vec![actor.org_id.into(), chat_id.into(), through.into()],
    )
    .await?
    .ok_or(ContentError::NotFound)?;
    let through_sequence: i64 = selected.try_get("", "sequence").map_err(database_error)?;
    let kind: String = selected.try_get("", "author_kind").map_err(database_error)?;
    // A published agent reply is complete by construction (the run driver
    // writes it once, after the run settled). A person's message is not a reply.
    if kind != "agent" {
        return Err(ForkError::NotComplete);
    }
    if through_sequence > MAX_FORK_MESSAGES as i64 {
        return Err(ForkError::TooLarge);
    }
    let rows = tx
        .query_all(db::statement(
            tx,
            &format!(
                "SELECT id,sequence,author_id,author_kind,agent_label,body,byte_length,created_at FROM neev_messages WHERE org_id=$1 AND chat_id=$2 AND sequence<=$3 ORDER BY sequence LIMIT {}",
                MAX_FORK_MESSAGES + 1
            ),
            vec![actor.org_id.into(), chat_id.into(), through_sequence.into()],
        ))
        .await
        .map_err(database_error)?;
    if rows.len() > MAX_FORK_MESSAGES {
        return Err(ForkError::TooLarge);
    }
    let mut messages = Vec::with_capacity(rows.len());
    let mut text_bytes = 0i64;
    let mut index = BTreeMap::new();
    for row in &rows {
        let length: i64 = row.try_get("", "byte_length").map_err(database_error)?;
        text_bytes += length;
        let id: String = row.try_get("", "id").map_err(database_error)?;
        index.insert(id.clone(), messages.len());
        messages.push(MessageSnapshot {
            id,
            sequence: row.try_get("", "sequence").map_err(database_error)?,
            author_id: row.try_get("", "author_id").map_err(database_error)?,
            author_kind: row.try_get("", "author_kind").map_err(database_error)?,
            agent_label: row.try_get("", "agent_label").map_err(database_error)?,
            body: row.try_get("", "body").map_err(database_error)?,
            created_at: row.try_get("", "created_at").map_err(database_error)?,
            files: Vec::new(),
        });
    }
    if text_bytes > MAX_FORK_TEXT_BYTES {
        return Err(ForkError::TooLarge);
    }
    // The files those messages carry. Each must belong to THIS chat (it is
    // how they got this chat's audience); one that does not is corrupt
    // storage, not something to copy elsewhere.
    let refs = tx
        .query_all(db::statement(
            tx,
            "SELECT l.message_id,l.position,a.key,a.chat_id,a.archived,a.file_name,a.content_type,a.byte_length,a.sha256,a.created_by,a.created_at FROM neev_attachment_refs l JOIN neev_attachments a ON a.org_id=l.org_id AND a.key=l.attachment_key JOIN neev_messages m ON m.org_id=l.org_id AND m.id=l.message_id WHERE l.org_id=$1 AND m.chat_id=$2 AND m.sequence<=$3 ORDER BY m.sequence,l.position",
            vec![actor.org_id.into(), chat_id.into(), through_sequence.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut files = BTreeMap::new();
    let mut archived = BTreeSet::new();
    for row in &refs {
        let owner: Option<String> = row.try_get("", "chat_id").map_err(database_error)?;
        if owner.as_deref() != Some(chat_id) {
            return Err(ContentError::Storage.into());
        }
        let file = file_row(row)?;
        if row.try_get::<bool>("", "archived").map_err(database_error)? {
            archived.insert(file.key);
            continue;
        }
        let message: String = row.try_get("", "message_id").map_err(database_error)?;
        let position: i32 = row.try_get("", "position").map_err(database_error)?;
        let at = *index.get(&message).ok_or(ContentError::Storage)?;
        messages[at].files.push((position, file.key.clone()));
        files.insert(file.key.clone(), file);
    }
    check_file_bounds(&files)?;
    Ok(ChatSnapshot {
        title: chat.title,
        node_id: chat.node_id,
        messages,
        files,
        archived: archived.len() as i64,
        text_bytes,
    })
}

/// Fork `chat_id` up through the agent reply `through` into a new private
/// chat. Idempotent on `request_id`.
pub(crate) async fn fork_chat(
    state: &ServerState,
    actor: &OrgContext,
    chat_id: &str,
    through: &str,
    request_id: &str,
) -> Result<ChatFork, ForkError> {
    access::canonical_id(chat_id)?;
    access::canonical_id(through)?;
    let request = org::normalize_request_id(request_id).map_err(|_| ContentError::Invalid)?;
    let hash = fork_hash(actor, chat_id, through);

    // 1. Snapshot, guarded and short.
    let tx = resources::begin(state, actor).await?;
    if let Some(id) = resources::receipt(&tx, actor, FORK_ACTION, &request, &hash).await? {
        let done = replay(&tx, actor, &id, chat_id, through).await?;
        session::recheck(&tx, actor, state.clock.now_unix()).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(done);
    }
    let snap = snapshot(&tx, actor, chat_id, through).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;

    // 2. Bytes, with no transaction open.
    let copied = copy_files(state, actor.org_id, &snap.files).await?;

    // 3. Commit, guarded; any failure takes the fresh blobs back with it.
    match commit_chat(state, actor, chat_id, through, &request, &hash, &snap, &copied).await {
        Ok(fork) => {
            if fork.replayed {
                discard_files(state, actor.org_id, &copied).await;
            }
            Ok(fork)
        }
        Err(error) => {
            discard_files(state, actor.org_id, &copied).await;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn commit_chat(
    state: &ServerState,
    actor: &OrgContext,
    chat_id: &str,
    through: &str,
    request: &str,
    hash: &str,
    snap: &ChatSnapshot,
    copied: &[CopiedFile],
) -> Result<ChatFork, ForkError> {
    let tx = resources::begin(state, actor).await?;
    // A concurrent attempt with the same request may have committed meanwhile.
    if let Some(id) = resources::receipt(&tx, actor, FORK_ACTION, request, hash).await? {
        let done = replay(&tx, actor, &id, chat_id, through).await?;
        session::recheck(&tx, actor, state.clock.now_unix()).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(done);
    }
    // Re-prove everything the snapshot relied on: the caller can still read the
    // source, the history up through the selection is the one copied (messages
    // are append-only, so the same ids in the same order means the same text),
    // and every copied file still exists unarchived with the same bytes.
    let again = snapshot(&tx, actor, chat_id, through).await?;
    let same = again.messages.len() == snap.messages.len()
        && again
            .messages
            .iter()
            .zip(&snap.messages)
            .all(|(a, b)| a.id == b.id && a.files == b.files)
        && again.files.keys().eq(snap.files.keys());
    if !same {
        return Err(ContentError::Conflict.into());
    }
    verify_files(&tx, actor.org_id, copied).await?;
    // Serialize with concurrent writes to the SOURCE chat. (The fork's privacy
    // comes from the explicit `'private'` insert below, not from this lock.)
    access::lock_resources(&tx, actor, chat_id, false).await?;
    resources::initialize_usage(&tx, actor.org_id).await?;
    let count = snap.messages.len() as i64;
    let charged = db::execute(
        &tx,
        "UPDATE neev_content_usage SET chats=chats+1,messages=messages+$1,charged_bytes=charged_bytes+$2 WHERE org_id=$3 AND chats<10000 AND messages<=100000-$1 AND charged_bytes<=1073741824-$2",
        vec![count.into(), snap.text_bytes.into(), actor.org_id.into()],
    )
    .await?;
    if charged.rows_affected() != 1 {
        return Err(ContentError::Quota.into());
    }
    charge_files(&tx, actor.org_id, copied).await?;
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid.into());
    }
    let fork_id = uuid::Uuid::new_v4().to_string();
    let last = snap.messages.last().map_or(0, |message| message.sequence);
    db::execute(
        &tx,
        "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,'chat',$3,$4,$5,'private',NULL,NULL,1,$6,false,$7,$7)",
        vec![
            actor.org_id.into(),
            fork_id.clone().into(),
            snap.node_id.into(),
            actor.identity.user_id.into(),
            fork_title(&snap.title).into(),
            last.into(),
            now.into(),
        ],
    )
    .await?;
    insert_files(&tx, actor.org_id, attachment::Owner::Chat(&fork_id), copied).await?;
    let mapped: BTreeMap<&str, &str> = copied
        .iter()
        .map(|file| (file.old_key.as_str(), file.new_key.as_str()))
        .collect();
    for message in &snap.messages {
        let id = uuid::Uuid::new_v4().to_string();
        let body = rewrite_links(&message.body, copied);
        let bytes = i64::try_from(body.len()).map_err(|_| ContentError::Invalid)?;
        db::execute(
            &tx,
            "INSERT INTO neev_messages(org_id,chat_id,resource_kind,id,sequence,author_id,body,byte_length,created_at,author_kind,agent_label) VALUES($1,$2,'chat',$3,$4,$5,$6,$7,$8,$9,$10)",
            vec![
                actor.org_id.into(),
                fork_id.clone().into(),
                id.clone().into(),
                message.sequence.into(),
                message.author_id.into(),
                body.into(),
                bytes.into(),
                message.created_at.into(),
                message.author_kind.clone().into(),
                message.agent_label.clone().into(),
            ],
        )
        .await?;
        for (position, key) in &message.files {
            let new_key = mapped.get(key.as_str()).ok_or(ContentError::Storage)?;
            db::execute(
                &tx,
                "INSERT INTO neev_attachment_refs(org_id,attachment_key,message_id,position) VALUES($1,$2,$3,$4)",
                vec![
                    actor.org_id.into(),
                    (*new_key).to_owned().into(),
                    id.clone().into(),
                    (*position).into(),
                ],
            )
            .await?;
        }
    }
    resources::record_receipt(&tx, actor, FORK_ACTION, request, hash, &fork_id).await?;
    let provenance = org::digest_parts(&[SOURCE_ACTION, chat_id, through]);
    resources::record_receipt(&tx, actor, SOURCE_ACTION, &fork_id, &provenance, chat_id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(ChatFork {
        chat_id: fork_id,
        source_chat_id: chat_id.to_owned(),
        through_message_id: through.to_owned(),
        node_id: snap.node_id,
        messages: count,
        attachments: copied.len() as i64,
        archived_attachments: snap.archived,
        replayed: false,
    })
}

/// Which chat `fork_id` was forked from, if the caller made that fork. The
/// source is returned only while the caller can still read it.
pub(crate) async fn chat_fork_source(
    state: &ServerState,
    actor: &OrgContext,
    fork_id: &str,
) -> Result<Option<String>, ContentError> {
    access::canonical_id(fork_id)?;
    let tx = resources::begin(state, actor).await?;
    access::authorized(&tx, actor, fork_id, false).await?;
    let source = db::one(
        &tx,
        "SELECT resource_id FROM neev_requests WHERE user_id=$1 AND action=$2 AND request_id=$3",
        vec![
            actor.identity.user_id.into(),
            SOURCE_ACTION.into(),
            fork_id.into(),
        ],
    )
    .await?
    .map(|row| row.try_get::<String>("", "resource_id").map_err(database_error))
    .transpose()?;
    let readable = match &source {
        Some(id) => access::authorized(&tx, actor, id, false).await.is_ok(),
        None => false,
    };
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(source.filter(|_| readable))
}
