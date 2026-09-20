//! Uploaded document images: content-sniffed, bound to a specific document, and
//! served only to callers currently authorized for THAT document. Ownership is not
//! "same org" — a private document's image is unreadable by a same-org member who
//! is not authorized for the document, even if they know the key. Cross-document
//! reference reuse is refused at save time so an image cannot widen its audience.
use super::{access, rich::RichBlock, rich::RichDocument, types::*};
use crate::neev::{db, error::database_error, startup::ServerState, types::OrgContext};
use sea_orm::Value;
use uuid::Uuid;

use super::blob::MAX_IMAGE_BYTES;

// Per-tenant storage bounds, enforced race-safe under the admission guard.
const MAX_IMAGES_PER_ORG: i64 = 5_000;
const MAX_IMAGE_BYTES_PER_ORG: i64 = 500 * 1024 * 1024;
// A document embedding more than this many distinct images is refused rather than
// building an unbounded IN clause.
const MAX_KEYS_PER_DOCUMENT: usize = 500;

pub(crate) struct StoredImage {
    pub(crate) key: String,
    pub(crate) content_type: &'static str,
}

/// Classify by leading magic bytes only; the client's declared type is ignored on
/// upload and the stored bytes are re-classified on serve, so a mislabeled or
/// polyglot upload can never be served as a different type.
pub(crate) fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Upload an image bound to `document_id`. The caller must be authorized to WRITE
/// that document (private ⇒ creator only); the image then inherits the document's
/// audience for every future read.
pub(crate) async fn store(
    state: &ServerState,
    actor: &OrgContext,
    document_id: &str,
    bytes: &[u8],
) -> Result<StoredImage, ContentError> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return Err(ContentError::Invalid);
    }
    let content_type = sniff_image(bytes).ok_or(ContentError::Invalid)?;
    access::canonical_id(document_id)?;
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid);
    }
    let tx = db::begin(&state.database).await?;
    // Serialize authorization + quota + insert so concurrent uploads cannot race
    // past the per-tenant caps.
    db::lock_admission_guard(&tx).await?;
    let resource = access::authorized(&tx, actor, document_id, true).await?;
    if resource.kind != ResourceKind::Document {
        return Err(ContentError::Invalid);
    }
    let usage = db::one(
        &tx,
        // PostgreSQL SUM(bigint) returns numeric, including the empty-table
        // COALESCE result. Decode the bounded quota total as bigint on both DBs.
        "SELECT COUNT(*) AS c, CAST(COALESCE(SUM(byte_length),0) AS BIGINT) AS b FROM neev_image_blobs WHERE org_id=$1",
        vec![actor.org_id.into()],
    )
    .await?
    .ok_or(ContentError::Storage)?;
    let count: i64 = usage.try_get("", "c").map_err(database_error)?;
    let used: i64 = usage.try_get("", "b").map_err(database_error)?;
    if count >= MAX_IMAGES_PER_ORG || used + bytes.len() as i64 > MAX_IMAGE_BYTES_PER_ORG {
        return Err(ContentError::Quota);
    }
    let key = Uuid::new_v4().hyphenated().to_string();
    // Write the blob before the row: a failed commit leaves a byte-bounded orphan
    // blob (never served, GC-able), never a row pointing at a missing file.
    let info = state.blobs.put_image(actor.org_id, &key, bytes).await?;
    db::execute(
        &tx,
        "INSERT INTO neev_image_blobs(org_id,key,document_id,content_type,byte_length,sha256,created_by,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
        vec![
            actor.org_id.into(),
            key.clone().into(),
            document_id.to_owned().into(),
            content_type.into(),
            info.byte_length.into(),
            info.sha256.into(),
            actor.identity.user_id.into(),
            now.into(),
        ],
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    Ok(StoredImage { key, content_type })
}

/// Read an image, enforcing the bound document's CURRENT read ACL. Returns the
/// bytes and the freshly re-sniffed content type (never the stored label).
pub(crate) async fn load(
    state: &ServerState,
    actor: &OrgContext,
    key: &str,
) -> Result<(Vec<u8>, &'static str), ContentError> {
    if !uuid::Uuid::parse_str(key).is_ok_and(|parsed| parsed.hyphenated().to_string() == key) {
        return Err(ContentError::NotFound);
    }
    let tx = db::begin(&state.database).await?;
    let row = db::one(
        &tx,
        "SELECT document_id FROM neev_image_blobs WHERE org_id=$1 AND key=$2",
        vec![actor.org_id.into(), key.to_owned().into()],
    )
    .await?
    .ok_or(ContentError::NotFound)?;
    let document_id: String = row.try_get("", "document_id").map_err(database_error)?;
    // Re-check the owning document's audience at read time: revoked access or a
    // document turned private denies the image immediately (uniform NotFound).
    access::authorized(&tx, actor, &document_id, false).await?;
    tx.commit().await.map_err(database_error)?;
    let bytes = state.blobs.read_image(actor.org_id, key).await?;
    let content_type = sniff_image(&bytes).ok_or(ContentError::Storage)?;
    Ok((bytes, content_type))
}

/// Save-time guard: every image embedded in `document` must have been uploaded for
/// THIS document. A key bound to another document (or unknown) is refused so a
/// private image cannot be re-embedded into a broader-audience document.
pub(crate) async fn verify_document_images(
    state: &ServerState,
    actor: &OrgContext,
    document_id: &str,
    document: &RichDocument,
) -> Result<(), ContentError> {
    let mut keys = collect_image_keys(document);
    if keys.is_empty() {
        return Ok(());
    }
    keys.sort();
    keys.dedup();
    if keys.len() > MAX_KEYS_PER_DOCUMENT {
        return Err(ContentError::Invalid);
    }
    let mut sql = String::from(
        "SELECT COUNT(DISTINCT key) AS c FROM neev_image_blobs WHERE org_id=$1 AND document_id=$2 AND key IN (",
    );
    let mut args: Vec<Value> = vec![actor.org_id.into(), document_id.to_owned().into()];
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            sql.push(',');
        }
        sql.push_str(&format!("${}", index + 3));
        args.push(key.clone().into());
    }
    sql.push(')');
    let tx = db::begin(&state.database).await?;
    let row = db::one(&tx, &sql, args)
        .await?
        .ok_or(ContentError::Storage)?;
    let matched: i64 = row.try_get("", "c").map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    if matched as usize != keys.len() {
        return Err(ContentError::Invalid);
    }
    Ok(())
}

fn collect_image_keys(document: &RichDocument) -> Vec<String> {
    let mut keys = Vec::new();
    let mut stack: Vec<&RichBlock> = document.blocks.iter().collect();
    while let Some(block) = stack.pop() {
        if let RichBlock::Image { url, .. } = block {
            if let Some(key) = url.strip_prefix("/api/neev/images/") {
                keys.push(key.to_owned());
            }
        }
        stack.extend(block.identity_children().1.iter());
    }
    keys
}
