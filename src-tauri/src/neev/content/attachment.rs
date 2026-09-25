//! Chat attachments: content-sniffed, bound to a specific chat, and served only
//! to callers currently authorized for THAT chat. Ownership is not "same org" —
//! a private chat's attachment is unreadable by a same-org member who is not
//! authorized for the chat, even if they know the key. The same rule document
//! images follow, one resource kind over.
//!
//! What an attachment is FOR is the agent: a run stages these bytes into its own
//! sandbox working directory so the provider can actually open them. Nothing
//! here sends a filename in place of content — if the bytes cannot be staged,
//! the run is refused rather than quietly mentioning a file that is not there.
use super::{access, blob::ATTACHMENT_STORAGE_CEILING, types::*};
use crate::neev::{db, error::database_error, startup::ServerState, types::OrgContext};
use sea_orm::ConnectionTrait;
use uuid::Uuid;

/// Per-tenant storage bounds, enforced race-safe under the admission guard.
const MAX_ATTACHMENTS_PER_ORG: i64 = 20_000;
/// Deliberately modest while the host is storage-constrained. This is the one
/// bound a reader can actually hit, so it is reported as a quota error they can
/// act on (remove something, or ask for more) rather than a generic failure.
///
/// With no per-file limit this is also the largest one attachment can be — the
/// arithmetic of a shared budget, not a rule about files. Defined as
/// `blob::ATTACHMENT_STORAGE_CEILING` so the buffer bounds cannot drift from it.
pub(super) const MAX_ATTACHMENT_BYTES_PER_ORG: i64 = ATTACHMENT_STORAGE_CEILING as i64;
/// Per message, and therefore per run.
pub(crate) const MAX_ATTACHMENTS_PER_MESSAGE: usize = 5;
const MAX_NAME_CHARS: usize = 200;

/// What an attachment hangs on.
///
/// A team chat is a `neev_resources` row with a shared audience; an Agent Chat
/// is a `neev_agent_sessions` row private to its owner. They are different
/// entities with different authorization, so the owner is named explicitly
/// rather than inferred from the shape of an id.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Owner<'a> {
    Chat(&'a str),
    Session(&'a str),
}

impl Owner<'_> {
    fn id(&self) -> &str {
        match self {
            Owner::Chat(id) | Owner::Session(id) => id,
        }
    }
}

/// Prove the caller may use this conversation, for reading or for writing.
///
/// A chat defers to the resource ACL. A session is private to its owner and has
/// no sharing, so ownership IS the check — and the uniform `NotFound` keeps the
/// existence of someone else's session unobservable.
async fn authorize(
    db: &sea_orm::DatabaseTransaction,
    actor: &OrgContext,
    owner: Owner<'_>,
    write: bool,
) -> Result<(), ContentError> {
    match owner {
        Owner::Chat(id) => {
            let resource = access::authorized(db, actor, id, write).await?;
            if resource.kind != ResourceKind::Chat {
                return Err(ContentError::Invalid);
            }
            Ok(())
        }
        Owner::Session(id) => {
            db::one(
                db,
                "SELECT id FROM neev_agent_sessions WHERE org_id=$1 AND id=$2 AND owner_id=$3",
                vec![
                    actor.org_id.into(),
                    id.to_owned().into(),
                    actor.identity.user_id.into(),
                ],
            )
            .await?
            .ok_or(ContentError::NotFound)?;
            Ok(())
        }
    }
}

pub(crate) struct StoredAttachment {
    pub(crate) key: String,
    pub(crate) file_name: String,
    pub(crate) content_type: &'static str,
    pub(crate) byte_length: i64,
}

/// An attachment resolved for a run: metadata plus the bytes to stage.
pub(crate) struct RunAttachment {
    pub(crate) key: String,
    pub(crate) file_name: String,
    pub(crate) content_type: &'static str,
    pub(crate) bytes: Vec<u8>,
}

impl RunAttachment {
    /// Whether this is something a provider can take as a native image input.
    pub(crate) fn is_image(&self) -> bool {
        self.content_type.starts_with("image/")
    }
}

/// Classify by leading magic bytes, falling back to UTF-8 text.
///
/// The client's declared type is ignored on upload and the stored bytes are
/// re-classified on serve, so a mislabelled or polyglot upload can never be
/// served — or staged — as a different type. Text is the only type without a
/// magic number, so it is accepted only when the whole file is valid UTF-8 and
/// nothing else claimed it; that is also what makes source code work without
/// enumerating every language's extension.
pub(crate) fn sniff_attachment(bytes: &[u8]) -> Option<&'static str> {
    // Empty content has no type. The UTF-8 fallback below would otherwise claim
    // it as text — an empty slice is trivially valid UTF-8 with no NUL — and a
    // zero-byte file would be classified rather than refused. `store` rejects
    // empty separately, but a classifier that answers for nothing is wrong on
    // its own terms and would mislead any future caller.
    if bytes.is_empty() {
        return None;
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else if std::str::from_utf8(bytes).is_ok() && !bytes.contains(&0) {
        Some("text/plain")
    } else {
        None
    }
}

/// The name an attachment is stored and staged under.
///
/// Reduced to a single path segment with a conservative character set. The name
/// reaches a filesystem inside the sandbox, so anything that could climb out of
/// the staging directory — separators, `..`, control characters, NUL — is not
/// sanitised into something plausible but refused outright.
fn canonical_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_NAME_CHARS {
        return None;
    }
    if trimmed == "." || trimmed == ".." || trimmed.starts_with('.') {
        return None;
    }
    if trimmed.chars().any(|character| {
        character.is_control()
            || character == '/'
            || character == '\\'
            || character == '\0'
            || character == ':'
    }) {
        return None;
    }
    Some(trimmed.to_owned())
}

/// Upload an attachment bound to `chat_id`. The caller must be authorized to
/// WRITE that chat; the attachment then inherits the chat's audience for every
/// future read.
pub(crate) async fn store(
    state: &ServerState,
    actor: &OrgContext,
    owner: Owner<'_>,
    file_name: &str,
    bytes: &[u8],
) -> Result<StoredAttachment, ContentError> {
    // No per-file size rule. An upload is bounded by the tenant's remaining
    // storage, checked under the admission guard below and reported as `Quota`;
    // `Invalid` here means only "no bytes", which no quota would fix.
    if bytes.is_empty() {
        return Err(ContentError::Invalid);
    }
    let content_type = sniff_attachment(bytes).ok_or(ContentError::Invalid)?;
    let file_name = canonical_name(file_name).ok_or(ContentError::Invalid)?;
    access::canonical_id(owner.id())?;
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid);
    }
    let tx = db::begin(&state.database).await?;
    // Serialize authorization + quota + insert so concurrent uploads cannot race
    // past the per-tenant caps.
    db::lock_admission_guard(&tx).await?;
    authorize(&tx, actor, owner, true).await?;
    let usage = db::one(
        &tx,
        "SELECT COUNT(*) AS c, CAST(COALESCE(SUM(byte_length),0) AS BIGINT) AS b FROM neev_attachments WHERE org_id=$1",
        vec![actor.org_id.into()],
    )
    .await?
    .ok_or(ContentError::Storage)?;
    let count: i64 = usage.try_get("", "c").map_err(database_error)?;
    let used: i64 = usage.try_get("", "b").map_err(database_error)?;
    if count >= MAX_ATTACHMENTS_PER_ORG
        || used + bytes.len() as i64 > MAX_ATTACHMENT_BYTES_PER_ORG
    {
        return Err(ContentError::Quota);
    }
    let key = Uuid::new_v4().hyphenated().to_string();
    // Write the blob before the row: a failed commit leaves a byte-bounded
    // orphan blob (never served, GC-able), never a row pointing at no file.
    let info = state.blobs.put_attachment(actor.org_id, &key, bytes).await?;
    db::execute(
        &tx,
        "INSERT INTO neev_attachments(org_id,key,chat_id,session_id,file_name,content_type,byte_length,sha256,created_by,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        vec![
            actor.org_id.into(),
            key.clone().into(),
            match owner {
                Owner::Chat(id) => id.to_owned().into(),
                Owner::Session(_) => sea_orm::Value::String(None),
            },
            match owner {
                Owner::Session(id) => id.to_owned().into(),
                Owner::Chat(_) => sea_orm::Value::String(None),
            },
            file_name.clone().into(),
            content_type.into(),
            info.byte_length.into(),
            info.sha256.into(),
            actor.identity.user_id.into(),
            now.into(),
        ],
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    Ok(StoredAttachment {
        key,
        file_name,
        content_type,
        byte_length: info.byte_length,
    })
}

/// Read an attachment, enforcing the bound chat's CURRENT read ACL. Returns the
/// bytes, the freshly re-sniffed content type (never the stored label) and the
/// stored name.
pub(crate) async fn load(
    state: &ServerState,
    actor: &OrgContext,
    key: &str,
) -> Result<(Vec<u8>, &'static str, String), ContentError> {
    if !uuid::Uuid::parse_str(key).is_ok_and(|parsed| parsed.hyphenated().to_string() == key) {
        return Err(ContentError::NotFound);
    }
    let tx = db::begin(&state.database).await?;
    let row = db::one(
        &tx,
        "SELECT chat_id,session_id,file_name FROM neev_attachments WHERE org_id=$1 AND key=$2",
        vec![actor.org_id.into(), key.to_owned().into()],
    )
    .await?
    .ok_or(ContentError::NotFound)?;
    let chat_id: Option<String> = row.try_get("", "chat_id").map_err(database_error)?;
    let session_id: Option<String> = row.try_get("", "session_id").map_err(database_error)?;
    let file_name: String = row.try_get("", "file_name").map_err(database_error)?;
    // Re-check the owner's audience at READ time: revoked access, a chat turned
    // private, or a session that is not the caller's denies immediately and
    // uniformly as NotFound.
    let owner = match (chat_id.as_deref(), session_id.as_deref()) {
        (Some(id), None) => Owner::Chat(id),
        (None, Some(id)) => Owner::Session(id),
        // Exactly one is a stored CHECK; a row that broke it is not served.
        _ => return Err(ContentError::Storage),
    };
    authorize(&tx, actor, owner, false).await?;
    tx.commit().await.map_err(database_error)?;
    let bytes = state.blobs.read_attachment(actor.org_id, key).await?;
    let content_type = sniff_attachment(&bytes).ok_or(ContentError::Storage)?;
    Ok((bytes, content_type, file_name))
}

/// Resolve the attachments a run was asked to carry.
///
/// Every key is re-authorized against the chat the run belongs to, so a key
/// uploaded to a different chat — or to one the caller has since lost access to
/// — cannot be smuggled into this turn. Returns the bytes, because staging is
/// the only honest way to give an agent a file.
pub(crate) async fn resolve_for_run(
    state: &ServerState,
    actor: &OrgContext,
    owner: Owner<'_>,
    keys: &[String],
) -> Result<Vec<RunAttachment>, ContentError> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    if keys.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(ContentError::Invalid);
    }
    let mut seen = std::collections::BTreeSet::new();
    for key in keys {
        if !seen.insert(key.as_str()) {
            return Err(ContentError::Invalid);
        }
    }
    // No aggregate byte ceiling for a turn: the count cap bounds this loop and the
    // storage quota already admitted every file, so the staged total cannot pass
    // the org budget however large the individual files are.
    let mut resolved = Vec::with_capacity(keys.len());
    for key in keys {
        let (bytes, content_type, file_name) = load(state, actor, key).await?;
        // The chat is re-checked here rather than trusted from `load`: `load`
        // proves the caller may read it, this proves it belongs to THIS turn.
        let tx = db::begin(&state.database).await?;
        let row = db::one(
            &tx,
            "SELECT chat_id,session_id FROM neev_attachments WHERE org_id=$1 AND key=$2",
            vec![actor.org_id.into(), key.to_owned().into()],
        )
        .await?
        .ok_or(ContentError::NotFound)?;
        let stored_chat: Option<String> = row.try_get("", "chat_id").map_err(database_error)?;
        let stored_session: Option<String> =
            row.try_get("", "session_id").map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        // Belongs to THIS conversation, not merely to one the caller may read.
        let same = match owner {
            Owner::Chat(id) => stored_chat.as_deref() == Some(id),
            Owner::Session(id) => stored_session.as_deref() == Some(id),
        };
        if !same {
            return Err(ContentError::Invalid);
        }
        resolved.push(RunAttachment {
            key: key.clone(),
            file_name,
            content_type,
            bytes,
        });
    }
    Ok(resolved)
}

/// Record which attachments went with a message, so a reload or a later turn
/// still shows them instead of a dangling mention in the text.
pub(crate) async fn link_to_message(
    tx: &sea_orm::DatabaseTransaction,
    actor: &OrgContext,
    message_id: &str,
    keys: &[String],
) -> Result<(), ContentError> {
    if keys.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(ContentError::Invalid);
    }
    for (position, key) in keys.iter().enumerate() {
        db::execute(
            tx,
            "INSERT INTO neev_attachment_refs(org_id,attachment_key,message_id,position) VALUES($1,$2,$3,$4)",
            vec![
                actor.org_id.into(),
                key.clone().into(),
                message_id.to_owned().into(),
                (position as i32).into(),
            ],
        )
        .await?;
    }
    Ok(())
}

/// How many turn or message ids one lookup will answer for.
///
/// A page of history is the caller's own bound; this refuses anything larger
/// rather than building an unbounded `IN` clause.
const MAX_REF_LOOKUP: usize = 200;

fn placeholders(count: usize, first: usize) -> String {
    (0..count)
        .map(|index| format!("${}", first + index))
        .collect::<Vec<_>>()
        .join(",")
}

fn read_stored(row: &sea_orm::QueryResult) -> Result<StoredAttachment, ContentError> {
    let key: String = row.try_get("", "key").map_err(database_error)?;
    let file_name: String = row.try_get("", "file_name").map_err(database_error)?;
    let content_type: String = row.try_get("", "content_type").map_err(database_error)?;
    let byte_length: i64 = row.try_get("", "byte_length").map_err(database_error)?;
    // Re-map to the static set so a tampered row cannot introduce a new type.
    let content_type = match content_type.as_str() {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/webp" => "image/webp",
        "application/pdf" => "application/pdf",
        "text/plain" => "text/plain",
        _ => return Err(ContentError::Storage),
    };
    Ok(StoredAttachment {
        key,
        file_name,
        content_type,
        byte_length,
    })
}

/// The attachments carried by each of `message_ids`, for listing a thread.
///
/// ONE query for the whole page. A statement per message was the obvious
/// shape and the wrong one: a hundred-message page became a hundred round
/// trips on a surface people already find slow to open.
pub(crate) async fn for_messages(
    state: &ServerState,
    actor: &OrgContext,
    message_ids: &[String],
) -> Result<std::collections::BTreeMap<String, Vec<StoredAttachment>>, ContentError> {
    let mut out: std::collections::BTreeMap<String, Vec<StoredAttachment>> =
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
        "SELECT l.message_id AS owner,a.key,a.file_name,a.content_type,a.byte_length FROM neev_attachment_refs l JOIN neev_attachments a ON a.org_id=l.org_id AND a.key=l.attachment_key WHERE l.org_id=$1 AND l.message_id IN ({}) ORDER BY l.message_id,l.position",
        placeholders(message_ids.len(), 2)
    );
    let rows = tx
        .query_all(db::statement(&tx, &sql, args))
        .await
        .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    for row in &rows {
        let owner: String = row.try_get("", "owner").map_err(database_error)?;
        out.entry(owner).or_default().push(read_stored(row)?);
    }
    Ok(out)
}

/// Record which attachments went with an agent turn, keyed by session and
/// sequence — an Agent Chat has turns, not messages.
pub(crate) async fn link_to_turn(
    tx: &sea_orm::DatabaseTransaction,
    actor: &OrgContext,
    session_id: &str,
    sequence: i64,
    keys: &[String],
) -> Result<(), ContentError> {
    if keys.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(ContentError::Invalid);
    }
    for (position, key) in keys.iter().enumerate() {
        db::execute(
            tx,
            "INSERT INTO neev_attachment_refs(org_id,attachment_key,turn_session_id,turn_sequence,position) VALUES($1,$2,$3,$4,$5)",
            vec![
                actor.org_id.into(),
                key.clone().into(),
                session_id.to_owned().into(),
                sequence.into(),
                (position as i32).into(),
            ],
        )
        .await?;
    }
    Ok(())
}

/// The attachments carried by each of a session's turns, by sequence. One query
/// for the page, for the same reason as `for_messages`.
pub(crate) async fn for_turns(
    state: &ServerState,
    actor: &OrgContext,
    session_id: &str,
    sequences: &[i64],
) -> Result<std::collections::BTreeMap<i64, Vec<StoredAttachment>>, ContentError> {
    let mut out: std::collections::BTreeMap<i64, Vec<StoredAttachment>> =
        std::collections::BTreeMap::new();
    if sequences.is_empty() {
        return Ok(out);
    }
    if sequences.len() > MAX_REF_LOOKUP {
        return Err(ContentError::Invalid);
    }
    let tx = db::begin(&state.database).await?;
    let mut args: Vec<sea_orm::Value> = vec![actor.org_id.into(), session_id.to_owned().into()];
    for sequence in sequences {
        args.push((*sequence).into());
    }
    let sql = format!(
        "SELECT l.turn_sequence AS owner,a.key,a.file_name,a.content_type,a.byte_length FROM neev_attachment_refs l JOIN neev_attachments a ON a.org_id=l.org_id AND a.key=l.attachment_key WHERE l.org_id=$1 AND l.turn_session_id=$2 AND l.turn_sequence IN ({}) ORDER BY l.turn_sequence,l.position",
        placeholders(sequences.len(), 3)
    );
    let rows = tx
        .query_all(db::statement(&tx, &sql, args))
        .await
        .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    for row in &rows {
        let owner: i64 = row.try_get("", "owner").map_err(database_error)?;
        out.entry(owner).or_default().push(read_stored(row)?);
    }
    Ok(out)
}

/// How long an unreferenced upload is kept before it is reclaimed.
///
/// An upload becomes referenced when the message or turn it was attached to is
/// sent. Until then it is a draft someone may still be composing, so it is not
/// reclaimed immediately — but a draft abandoned a day ago is not coming back,
/// and without this every cancelled compose would spend the tenant's quota
/// permanently.
pub(crate) const ORPHAN_RETENTION_SECONDS: i64 = 24 * 60 * 60;
/// Reclaimed per sweep, so one pass cannot monopolise the database.
const ORPHAN_BATCH: usize = 200;

/// Reclaim uploads that were never attached to anything.
///
/// Only rows with NO reference and older than the retention window. An
/// attachment carried by a persisted message or turn is never touched, however
/// old — it is part of that conversation's history.
///
/// The row goes first, then the blob: a crash between the two leaves a file
/// nobody can reach (reclaimed by the next sweep's blob pass or by hand), never
/// a row pointing at bytes that are gone.
pub(crate) async fn sweep_orphans(state: &ServerState) -> Result<u64, ContentError> {
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid);
    }
    let cutoff = now - ORPHAN_RETENTION_SECONDS;
    let tx = db::begin(&state.database).await?;
    let rows = tx
        .query_all(db::statement(
            &tx,
            &format!("SELECT a.org_id,a.key FROM neev_attachments a LEFT JOIN neev_attachment_refs l ON l.org_id=a.org_id AND l.attachment_key=a.key WHERE l.attachment_key IS NULL AND a.created_at<$1 ORDER BY a.created_at LIMIT {ORPHAN_BATCH}"),
            vec![cutoff.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut doomed = Vec::with_capacity(rows.len());
    for row in &rows {
        let org_id: i32 = row.try_get("", "org_id").map_err(database_error)?;
        let key: String = row.try_get("", "key").map_err(database_error)?;
        doomed.push((org_id, key));
    }
    for (org_id, key) in &doomed {
        db::execute(
            &tx,
            "DELETE FROM neev_attachments WHERE org_id=$1 AND key=$2",
            vec![(*org_id).into(), key.clone().into()],
        )
        .await?;
    }
    tx.commit().await.map_err(database_error)?;
    for (org_id, key) in &doomed {
        // A blob that cannot be removed is left for the next pass rather than
        // failing the sweep: the row is already gone, so it is unreachable.
        let _ = state.blobs.remove_attachment(*org_id, key).await;
    }
    Ok(doomed.len() as u64)
}

/// `canonical_name`, for the tests that prove what it refuses.
#[cfg(test)]
pub(crate) fn canonical_name_for_test(raw: &str) -> Option<String> {
    canonical_name(raw)
}
