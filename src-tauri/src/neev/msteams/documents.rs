//! Release 4: generated files of a channel run, published into the channel's
//! own SharePoint folder and linked in the originating thread.
//!
//! Durable and re-authorized like the outbox:
//! - A row per generated file is enqueued when the run settles (same
//!   transaction as the final answer), only for a COMPLETED channel run and
//!   only when file publication is explicitly authorized
//!   (`NEEV_TEAMS_GRAPH_FILES=true`, see `config::GraphConfig::files`).
//! - Before every attempt the destination is rechecked exactly as for the
//!   final answer (binding enabled, channel audience fresh, the executor still
//!   a Team and Neev member) AND the run's sources are re-classified
//!   (`sources::classify`): a file is as private as the answer it came with.
//! - Bytes are read through the attachment API as the EXECUTOR (a short
//!   delegated session), so the file is only ever what that member could open,
//!   and the stored SHA-256 must still match.
//! - Destination: Graph `filesFolder` of the channel (never a guessed path) →
//!   `Neev/` → `Run <id>/` → create-only upload session. The session's upload
//!   URL is a secret capability: it is SEALED (AES-256-GCM, key derived from
//!   the server key, bound to org + row id) and stored BEFORE any byte is sent,
//!   so a restart RESUMES the same session.
//! - Uncertain completions (transport loss, unreadable final answer, missing
//!   session) are RECONCILED by reading the destination name. A file there is
//!   accepted only when its CURRENT content, downloaded through Graph
//!   (`graph_files::content_sha256`), has exactly the stored artifact's length
//!   and SHA-256, and its item and ETag did not change around that read: a
//!   same-named file a collaborator put or edited there is a conflict and is
//!   never overwritten or linked; a content read that cannot be completed
//!   leaves the row undecided (retried, then failed), never published.
//! - Once a capability has been stored, bytes may have been sent: a later
//!   "not there" is NOT proof that nothing committed (a commit can precede its
//!   metadata becoming readable). Such a row keeps its sealed session and is
//!   only ever resumed or reconciled — never given a second session — until
//!   it is proved published or fails as `teams_document_uncertain`.
//! - Graph's `Retry-After` (429/5xx) sets the next attempt.
//! - No application size cap: Graph's multipart session limits apply.
//! - Agent execution is never repeated by any of this.

use std::sync::Arc;

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use sea_orm::{ConnectionTrait, DatabaseConnection, QueryResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::access;
use super::activity;
use super::directory::Directory;
use super::files_policy::safe_name;
use super::graph::GraphAuthority;
use super::graph_files::{GraphError as FileError, GraphFiles, Item as DriveItem, UploadSession};
use super::graph::Membership;
use super::inbox::Item;
use super::codes::Refusal;
use super::sources;
use crate::neev::agents::sessions::{self, TurnStatus};
use crate::neev::content::types::ContentError;
use crate::neev::db;
use crate::neev::error::database_error;
use crate::neev::startup::ServerState;

/// Publication attempts before a document is marked `failed`.
pub(crate) const MAX_ATTEMPTS: i64 = 8;
const LEASE_SECS: i64 = 900;
const SEAL_MAGIC: &[u8] = b"nts1";
const NONCE_LEN: usize = 12;

/// The explicitly authorized file authority: the Graph app token (only what
/// the tenant admin granted) and the file transport.
pub(crate) struct FilesAuthority {
    pub(crate) graph: Arc<GraphAuthority>,
    pub(crate) files: GraphFiles,
}

// ---------------------------------------------------------------- sealing

fn seal_key(encryption_key: &[u8; 32]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(encryption_key).expect("HMAC accepts any key length");
    mac.update(b"neev-teams-upload-session-v1");
    mac.finalize().into_bytes().into()
}

fn seal_aad(org_id: i32, document_id: &str) -> Vec<u8> {
    format!("neev-teams-upload-session-v1\0{org_id}\0{document_id}").into_bytes()
}

/// Seal an upload session for one org's document row.
pub(crate) fn seal(encryption_key: &[u8; 32], org_id: i32, document_id: &str, session: &UploadSession) -> Result<String, ContentError> {
    let (url, expires, drive) = session.export_for_sealing();
    let plaintext = serde_json::to_vec(&json!({"u": url, "e": expires, "d": drive})).map_err(|_| ContentError::Storage)?;
    let cipher = Aes256Gcm::new_from_slice(&seal_key(encryption_key)).map_err(|_| ContentError::Storage)?;
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.try_fill_bytes(&mut nonce).map_err(|_| ContentError::Storage)?;
    let aad = seal_aad(org_id, document_id);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &plaintext, aad: &aad })
        .map_err(|_| ContentError::Storage)?;
    let mut out = Vec::with_capacity(SEAL_MAGIC.len() + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(SEAL_MAGIC);
    out.extend_from_slice(&nonce);
    out.extend(ciphertext);
    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}

/// Open a sealed session. A sealed value moved to another row or org, or
/// tampered with, is refused.
pub(crate) fn open(encryption_key: &[u8; 32], org_id: i32, document_id: &str, sealed: &str, loopback: bool) -> Result<UploadSession, ContentError> {
    let raw = base64::engine::general_purpose::STANDARD.decode(sealed).map_err(|_| ContentError::Storage)?;
    let rest = raw.strip_prefix(SEAL_MAGIC).ok_or(ContentError::Storage)?;
    let (nonce, ciphertext) = rest.split_at_checked(NONCE_LEN).ok_or(ContentError::Storage)?;
    let cipher = Aes256Gcm::new_from_slice(&seal_key(encryption_key)).map_err(|_| ContentError::Storage)?;
    let aad = seal_aad(org_id, document_id);
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), Payload { msg: ciphertext, aad: &aad })
        .map_err(|_| ContentError::Storage)?;
    let value: Value = serde_json::from_slice(&plaintext).map_err(|_| ContentError::Storage)?;
    let part = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned).ok_or(ContentError::Storage);
    UploadSession::restore(&part("u")?, &part("e")?, &part("d")?, loopback).map_err(|_| ContentError::Storage)
}

// ---------------------------------------------------------------- rows

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Document {
    pub(crate) id: String,
    pub(crate) org_id: i32,
    pub(crate) tenant_id: String,
    pub(crate) run_id: String,
    pub(crate) attachment_key: String,
    pub(crate) file_name: String,
    pub(crate) byte_length: i64,
    pub(crate) sha256: String,
    pub(crate) user_id: i32,
    pub(crate) service_url: String,
    pub(crate) conversation_id: String,
    pub(crate) reply_to_id: String,
    pub(crate) channel_id: String,
    pub(crate) root_id: String,
    pub(crate) sealed_session: Option<String>,
    pub(crate) attempts: i64,
}

const COLUMNS: &str = "id,org_id,tenant_id,run_id,attachment_key,file_name,byte_length,sha256,user_id,service_url,conversation_id,reply_to_id,channel_id,root_id,sealed_session,attempts";

fn get<T: sea_orm::TryGetable>(row: &QueryResult, column: &str) -> Result<T, ContentError> {
    row.try_get("", column).map_err(|error| database_error(error).into())
}

fn document(row: &QueryResult) -> Result<Document, ContentError> {
    Ok(Document {
        id: get(row, "id")?,
        org_id: get(row, "org_id")?,
        tenant_id: get(row, "tenant_id")?,
        run_id: get(row, "run_id")?,
        attachment_key: get(row, "attachment_key")?,
        file_name: get(row, "file_name")?,
        byte_length: get(row, "byte_length")?,
        sha256: get(row, "sha256")?,
        user_id: get(row, "user_id")?,
        service_url: get(row, "service_url")?,
        conversation_id: get(row, "conversation_id")?,
        reply_to_id: get(row, "reply_to_id")?,
        channel_id: get(row, "channel_id")?,
        root_id: get(row, "root_id")?,
        sealed_session: get(row, "sealed_session")?,
        attempts: get::<i32>(row, "attempts")? as i64,
    })
}

/// Enqueue every generated file of a settled channel run. Runs in the settle
/// transaction; idempotent per (run, file). Returns how many files the run
/// produced.
pub(crate) async fn enqueue_for_run<C: ConnectionTrait>(
    tx: &C,
    item: &Item,
    run_id: &str,
    session_id: &str,
    sequence: i64,
    now: i64,
) -> Result<usize, ContentError> {
    let Some(origin) = &item.channel else {
        return Ok(0);
    };
    let rows = tx
        .query_all(db::statement(
            tx,
            "SELECT a.key,a.file_name,a.byte_length,a.sha256 FROM neev_attachment_refs l JOIN neev_attachments a ON a.org_id=l.org_id AND a.key=l.attachment_key WHERE l.org_id=$1 AND l.turn_session_id=$2 AND l.turn_sequence=$3 AND a.archived=FALSE ORDER BY l.position",
            vec![item.org_id.into(), session_id.into(), sequence.into()],
        ))
        .await
        .map_err(database_error)?;
    for row in &rows {
        let key: String = get(row, "key")?;
        let id = activity::digest(&["msteams-document-v1", run_id, &key]);
        db::execute(
            tx,
            "INSERT INTO neev_msteams_documents(id,org_id,tenant_id,run_id,attachment_key,file_name,byte_length,sha256,user_id,service_url,conversation_id,reply_to_id,channel_id,root_id,state,sealed_session,drive_id,item_id,web_url,attempts,next_attempt_at,error_code,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,'pending',NULL,NULL,NULL,NULL,0,$15,NULL,$15,$15) ON CONFLICT DO NOTHING",
            vec![
                id.into(),
                item.org_id.into(),
                item.tenant_id.clone().into(),
                run_id.into(),
                key.into(),
                get::<String>(row, "file_name")?.into(),
                get::<i64>(row, "byte_length")?.into(),
                get::<String>(row, "sha256")?.into(),
                item.user_id.into(),
                item.service_url.clone().into(),
                item.conversation_id.clone().into(),
                item.activity_id.clone().into(),
                origin.key.channel_id.clone().into(),
                origin.key.root_id.clone().into(),
                now.into(),
            ],
        )
        .await?;
    }
    Ok(rows.len())
}

/// Claim due documents (conditional update per row).
pub(crate) async fn claim_due(database: &DatabaseConnection, now: i64, limit: u64) -> Result<Vec<Document>, ContentError> {
    let rows = database
        .query_all(db::statement(
            database,
            &format!("SELECT {COLUMNS} FROM neev_msteams_documents WHERE state IN ('pending','uploading') AND next_attempt_at<=$1 ORDER BY created_at,id LIMIT {limit}"),
            vec![now.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut claimed = Vec::new();
    for row in rows {
        let found = document(&row)?;
        let won = db::execute(
            database,
            "UPDATE neev_msteams_documents SET attempts=attempts+1,next_attempt_at=$1,updated_at=$2 WHERE id=$3 AND state IN ('pending','uploading') AND next_attempt_at<=$2",
            vec![(now + LEASE_SECS).into(), now.into(), found.id.clone().into()],
        )
        .await?;
        if won.rows_affected() == 1 {
            claimed.push(Document {
                attempts: found.attempts + 1,
                ..found
            });
        }
    }
    Ok(claimed)
}

async fn set_state(database: &DatabaseConnection, id: &str, state: &str, error: Option<&str>, at: i64, now: i64) -> Result<(), ContentError> {
    db::execute(
        database,
        "UPDATE neev_msteams_documents SET state=$1,error_code=$2,next_attempt_at=$3,updated_at=$4,sealed_session=CASE WHEN $1 IN ('withheld','failed') THEN NULL ELSE sealed_session END WHERE id=$5 AND state IN ('pending','uploading')",
        vec![state.into(), error.map(str::to_owned).into(), at.into(), now.into(), id.into()],
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------- publish

#[cfg(test)]
fn loopback(authority: &FilesAuthority) -> bool {
    authority.files.allow_loopback
}

#[cfg(not(test))]
fn loopback(_: &FilesAuthority) -> bool {
    false
}

/// How many files a run produced (its turn's stored outputs).
pub(crate) async fn outputs_of(database: &DatabaseConnection, org_id: i32, session_id: &str, sequence: i64) -> Result<i64, ContentError> {
    let row = db::one(
        database,
        "SELECT COUNT(*) AS n FROM neev_attachment_refs l JOIN neev_attachments a ON a.org_id=l.org_id AND a.key=l.attachment_key WHERE l.org_id=$1 AND l.turn_session_id=$2 AND l.turn_sequence=$3 AND a.archived=FALSE",
        vec![org_id.into(), session_id.into(), sequence.into()],
    )
    .await?
    .ok_or(ContentError::Storage)?;
    get(&row, "n")
}

/// What one publication attempt did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Published {
    /// In the channel's Files, at this web URL.
    Done { web_url: String },
    Withheld(&'static str),
    Failed(&'static str),
    /// Try again later: no sooner than `after` seconds when Graph said so.
    /// `uncertain` when bytes may already have committed: the stored session
    /// is kept and the row is only resumed or reconciled from here on.
    Retry { after: Option<u64>, uncertain: bool },
}

const RETRY: Published = Published::Retry { after: None, uncertain: false };

/// The destination name: the generated file's own name when SharePoint
/// accepts it, else a neutral one keeping a safe extension.
pub(crate) fn destination_name(file_name: &str, index_hint: &str) -> String {
    if safe_name(file_name).is_ok() {
        return file_name.to_owned();
    }
    let extension: String = file_name
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .filter(|ext| !ext.is_empty() && ext.len() <= 10 && ext.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|ext| format!(".{ext}"))
        .unwrap_or_default();
    format!("Neev output {index_hint}{extension}")
}

/// Destination + source recheck, then upload (or resume, or reconcile).
pub(crate) async fn publish_one(
    state: &Arc<ServerState>,
    directory: &Directory,
    authority: &FilesAuthority,
    doc: &Document,
) -> Result<Published, ContentError> {
    let loopback = loopback(authority);
    // Destination, exactly as for the final answer: binding live, audience
    // eligible NOW, the executor still a Team and Neev member.
    let access = match access::channel(&state.database, &doc.tenant_id, None, &doc.channel_id).await {
        Ok(access) if access.tenant.org_id == doc.org_id => access,
        Ok(_) => return Ok(Published::Withheld(Refusal::UnmappedChannel.code())),
        Err(Refusal::Unavailable) => return Ok(RETRY),
        Err(reason) => return Ok(Published::Withheld(reason.code())),
    };
    match crate::neev::auth::session::member_active(&state.database, doc.user_id, doc.org_id).await {
        Ok(()) => {}
        Err(crate::neev::error::AuthError::Unavailable) => return Ok(RETRY),
        Err(_) => return Ok(Published::Withheld(Refusal::NotAMember.code())),
    }
    let object: Option<String> = db::one(
        &state.database,
        "SELECT aad_object_id FROM neev_msteams_principals WHERE tenant_id=$1 AND org_id=$2 AND user_id=$3",
        vec![doc.tenant_id.clone().into(), doc.org_id.into(), doc.user_id.into()],
    )
    .await?
    .map(|row| get(&row, "aad_object_id"))
    .transpose()?;
    let Some(object) = object else {
        return Ok(Published::Withheld(Refusal::UnmappedUser.code()));
    };
    match access::teams_member(directory, &access, Some(&object), true).await {
        Ok(Membership::Owner | Membership::Member) => {}
        Ok(_) => return Ok(Published::Withheld(Refusal::NotATeamMember.code())),
        Err(Refusal::Unavailable) => return Ok(RETRY),
        Err(reason) => return Ok(Published::Withheld(reason.code())),
    }
    // A file is as private as the run that made it.
    let Some(turn) = sessions::turn_by_run_id(&state.database, &doc.run_id).await? else {
        return Ok(Published::Withheld("teams_document_unavailable"));
    };
    if turn.status != TurnStatus::Completed {
        return Ok(Published::Withheld("teams_document_unavailable"));
    }
    if let sources::Sources::Unclassified(code) = sources::classify(&turn.activity) {
        return Ok(Published::Withheld(code));
    }
    // The bytes, read as the executor through the attachment API.
    let actor = match super::worker::delegated(state, doc.org_id, doc.user_id, 300).await {
        Ok(actor) => actor,
        Err(Refusal::Unavailable | Refusal::Limited) => return Ok(RETRY),
        Err(reason) => return Ok(Published::Withheld(reason.code())),
    };
    let bytes = match crate::neev::content::attachment::load(state, &actor, &doc.attachment_key).await {
        Ok((bytes, _, _)) => bytes,
        Err(ContentError::NotFound | ContentError::Auth(_)) => return Ok(Published::Withheld("teams_document_unavailable")),
        Err(_) => return Ok(RETRY),
    };
    if format!("{:x}", Sha256::digest(&bytes)) != doc.sha256 || bytes.len() as i64 != doc.byte_length {
        return Ok(Published::Failed("teams_document_changed"));
    }
    // Destination folders.
    let token = match authority.graph.token(&doc.tenant_id).await {
        Ok(token) => token,
        Err(super::graph::GraphError::Unavailable) => return Ok(RETRY),
        Err(_) => return Ok(Published::Failed("teams_files_forbidden")),
    };
    let folder = match task_folder(authority, &token, &access.graph_group_id, &doc.channel_id, &doc.run_id).await {
        Ok(folder) => folder,
        Err(error) => return Ok(classify_error(&error)),
    };
    let name = destination_name(&doc.file_name, &doc.attachment_key[..8]);
    let now = state.clock.now_unix();
    let expected = Expected {
        sha256: &doc.sha256,
        length: bytes.len() as u64,
    };
    // Resume an in-flight session, or start a create-only one.
    let result = match &doc.sealed_session {
        // A stored capability means bytes may have been sent: only that same
        // session is resumed. One that cannot be opened (key rotated) is
        // decided from the destination alone, below — never replaced.
        Some(sealed) => match open(&state.config.encryption_key, doc.org_id, &doc.id, sealed, loopback) {
            Ok(session) => authority.files.resume(&session, &bytes).await,
            Err(_) => Err(FileError::UncertainTransport),
        },
        None => {
            // No capability was ever stored for this row, so none of its bytes
            // were sent: anything at the name is someone else's unless it is
            // provably these exact bytes.
            match reconcile(authority, &token, &folder, &name, &expected).await {
                Reconciled::Ours(item) => return finish(state, doc, &item, now).await,
                Reconciled::Conflict => return Ok(Published::Failed("teams_document_conflict")),
                Reconciled::Unknown(after) => return Ok(Published::Retry { after, uncertain: false }),
                Reconciled::Absent => {}
            }
            let session = match authority.files.begin(&token, &folder, &name).await {
                Ok(session) => session,
                Err(error) => return Ok(classify_error(&error)),
            };
            // The capability is sealed and stored BEFORE any byte is sent.
            let sealed = seal(&state.config.encryption_key, doc.org_id, &doc.id, &session)?;
            db::execute(
                &state.database,
                "UPDATE neev_msteams_documents SET state='uploading',sealed_session=$1,updated_at=$2 WHERE id=$3 AND state IN ('pending','uploading')",
                vec![sealed.into(), now.into(), doc.id.clone().into()],
            )
            .await?;
            authority.files.complete(&session, &bytes).await
        }
    };
    match result {
        Ok(item) => finish(state, doc, &item, now).await,
        // Possibly committed: decide from the destination itself. Absent is
        // NOT proof that nothing committed (metadata can lag a commit), so the
        // session is kept and the row stays undecided.
        Err(FileError::UncertainTransport | FileError::InvalidResponse | FileError::Conflict | FileError::NotFound) => {
            match reconcile(authority, &token, &folder, &name, &expected).await {
                Reconciled::Ours(item) => finish(state, doc, &item, now).await,
                Reconciled::Conflict => Ok(Published::Failed("teams_document_conflict")),
                Reconciled::Unknown(after) => Ok(Published::Retry { after, uncertain: true }),
                Reconciled::Absent => Ok(Published::Retry { after: None, uncertain: true }),
            }
        }
        Err(error) => Ok(classify_error(&error)),
    }
}

/// The artifact a remote file must provably be.
struct Expected<'a> {
    sha256: &'a str,
    length: u64,
}

enum Reconciled {
    Ours(DriveItem),
    Absent,
    Conflict,
    /// Not decidable now (Graph unavailable, or the file changed while it was
    /// read); retry no sooner than the given delay.
    Unknown(Option<u64>),
}

fn unknown(error: &FileError) -> Reconciled {
    match error {
        FileError::Retryable { retry_after_seconds, .. } => Reconciled::Unknown(*retry_after_seconds),
        _ => Reconciled::Unknown(None),
    }
}

/// Is the file at `name` exactly the artifact? Proven only by its downloaded
/// content's SHA-256 and length, with the same item and ETag before and after
/// the read; a size or content mismatch is someone else's file.
async fn reconcile(authority: &FilesAuthority, token: &str, folder: &DriveItem, name: &str, expected: &Expected<'_>) -> Reconciled {
    let before = match authority.files.child(token, folder, name).await {
        Ok(Some(remote)) => remote,
        Ok(None) => return Reconciled::Absent,
        Err(FileError::Conflict) => return Reconciled::Conflict,
        Err(error) => return unknown(&error),
    };
    if before.size != expected.length {
        return Reconciled::Conflict;
    }
    let content = match authority.files.content_sha256(token, &before.item, expected.length).await {
        Ok(content) => content,
        Err(error) => return unknown(&error),
    };
    let after = match authority.files.child(token, folder, name).await {
        Ok(Some(remote)) => remote,
        Ok(None) => return Reconciled::Unknown(None),
        Err(FileError::Conflict) => return Reconciled::Conflict,
        Err(error) => return unknown(&error),
    };
    // The bytes read must be ONE version of ONE item.
    if after.item.item_id != before.item.item_id || after.item.etag.is_empty() || after.item.etag != before.item.etag {
        return Reconciled::Unknown(None);
    }
    match content {
        Some((sha256, length)) if length == expected.length && sha256 == expected.sha256 => Reconciled::Ours(after.item),
        _ => Reconciled::Conflict,
    }
}

async fn task_folder(authority: &FilesAuthority, token: &str, group_id: &str, channel_id: &str, run_id: &str) -> Result<DriveItem, FileError> {
    let root = authority.files.channel_folder(token, group_id, channel_id).await?;
    let neev = authority.files.folder(token, &root, "Neev").await?;
    authority.files.folder(token, &neev, &format!("Run {}", &run_id[..12])).await
}

fn classify_error(error: &FileError) -> Published {
    match error {
        FileError::Retryable { retry_after_seconds, .. } => Published::Retry {
            after: *retry_after_seconds,
            uncertain: false,
        },
        FileError::UncertainTransport | FileError::NotFound => RETRY,
        FileError::Unauthorized | FileError::Forbidden => Published::Failed("teams_files_forbidden"),
        FileError::Conflict => Published::Failed("teams_document_conflict"),
        FileError::Policy(_) | FileError::InvalidResponse | FileError::Http(_) => Published::Failed("teams_files_error"),
    }
}

async fn finish(state: &Arc<ServerState>, doc: &Document, item: &DriveItem, now: i64) -> Result<Published, ContentError> {
    db::execute(
        &state.database,
        "UPDATE neev_msteams_documents SET state='published',sealed_session=NULL,drive_id=$1,item_id=$2,web_url=$3,error_code=NULL,updated_at=$4 WHERE id=$5 AND state IN ('pending','uploading')",
        vec![item.drive_id.clone().into(), item.item_id.clone().into(), item.web_url.clone().into(), now.into(), doc.id.clone().into()],
    )
    .await?;
    Ok(Published::Done {
        web_url: item.web_url.clone(),
    })
}

/// Record a non-success outcome: retry with backoff (honouring Graph's
/// Retry-After), or a final state. Returns the code when the row reached a
/// final unpublished state, so the thread can be told.
pub(crate) async fn record(database: &DatabaseConnection, doc: &Document, outcome: &Published, now: i64) -> Result<Option<&'static str>, ContentError> {
    // A row that already held a capability may have committed bytes.
    let undecided = doc.sealed_session.is_some() || matches!(outcome, Published::Retry { uncertain: true, .. });
    let terminal = match outcome {
        Published::Done { .. } => return Ok(None),
        Published::Withheld(code) => {
            set_state(database, &doc.id, "withheld", Some(code), now, now).await?;
            return Ok(Some(code));
        }
        Published::Failed(code) => Some(*code),
        Published::Retry { .. } if undecided && doc.attempts >= MAX_ATTEMPTS => Some("teams_document_uncertain"),
        Published::Retry { .. } if doc.attempts >= MAX_ATTEMPTS => Some("teams_files_unavailable"),
        Published::Retry { .. } => None,
    };
    if let Some(code) = terminal {
        set_state(database, &doc.id, "failed", Some(code), now, now).await?;
        return Ok(Some(code));
    }
    let Published::Retry { after, .. } = outcome else {
        unreachable!("every other outcome returned above");
    };
    let at = now + super::outbox::backoff(doc.attempts, *after);
    db::execute(
        database,
        "UPDATE neev_msteams_documents SET next_attempt_at=$1,error_code=$2,updated_at=$3 WHERE id=$4 AND state IN ('pending','uploading')",
        vec![at.into(), undecided.then_some("teams_document_uncertain").map(str::to_owned).into(), now.into(), doc.id.clone().into()],
    )
    .await?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_sessions_round_trip_only_for_their_own_row_and_key() {
        let key = [7u8; 32];
        let session = UploadSession::restore("https://contoso.sharepoint.com/upload?tempauth=secret", "2026-10-14T00:00:00Z", "drive1", false).unwrap();
        let sealed = seal(&key, 5, "doc-a", &session).unwrap();
        assert!(!sealed.contains("tempauth"), "the capability is never stored in clear");
        let opened = open(&key, 5, "doc-a", &sealed, false).unwrap();
        assert_eq!(opened.export_for_sealing(), session.export_for_sealing());
        assert!(open(&key, 5, "doc-b", &sealed, false).is_err(), "moved to another row");
        assert!(open(&key, 6, "doc-a", &sealed, false).is_err(), "moved to another org");
        assert!(open(&[8u8; 32], 5, "doc-a", &sealed, false).is_err(), "another server key");
        let mut tampered = base64::engine::general_purpose::STANDARD.decode(&sealed).unwrap();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(open(&key, 5, "doc-a", &base64::engine::general_purpose::STANDARD.encode(tampered), false).is_err());
        // A restored capability still obeys the host rules.
        assert!(UploadSession::restore("https://evil.example.com/upload", "x", "d", false).is_err());
        assert!(UploadSession::restore("http://127.0.0.1:9/upload", "x", "d", false).is_err(), "loopback only when asked, in tests");
    }

    #[test]
    fn destination_names_are_safe_and_keep_extensions() {
        assert_eq!(destination_name("Proposal 2026.docx", "abcd1234"), "Proposal 2026.docx");
        assert_eq!(destination_name("a:b.docx", "abcd1234"), "Neev output abcd1234.docx");
        assert_eq!(destination_name("CON", "abcd1234"), "Neev output abcd1234");
        assert_eq!(destination_name("../x.pdf", "abcd1234"), "Neev output abcd1234.pdf");
    }
}
