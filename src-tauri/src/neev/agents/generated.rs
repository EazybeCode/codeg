//! Generated output persistence: reads the spool left by the launcher after a
//! run completes and stores the files as attachments linked to the turn.
//!
//! The launcher writes to `{workspace_root}/spool/run-{run_id}/manifest.json` with
//! files in `{workspace_root}/spool/run-{run_id}/files/`. The server reads these
//! after the launcher exits, validates content types, stores them as attachments,
//! and cleans up the spool directory ONLY after successful storage.
//!
//! Security invariants:
//! - Manifest run_id MUST match the expected run_id
//! - All declared files must pass validation (no partial success)
//! - Spool is only deleted AFTER storage is confirmed
//! - Uses proper Owner type (Chat for team, Session for private)

use crate::neev::{
    content::{
        attachment::{self, sniff_attachment, Owner},
        resources,
        types::ContentError,
    },
    db,
    error::database_error,
    startup::ServerState,
};
use sea_orm::ConnectionTrait;
use std::path::Path;
use uuid::Uuid;

/// Spool directory name under workspace root.
const SPOOL_DIR: &str = "spool";

/// Maximum files to persist per run.
const MAX_GENERATED_FILES: usize = 100;

/// Manifest entry as written by the Python launcher.
#[derive(Debug, serde::Deserialize)]
struct ManifestEntry {
    key: String,
    name: String,
    byte_length: i64,
    sha256: String,
}

/// Manifest as written by the Python launcher.
#[derive(Debug, serde::Deserialize)]
struct Manifest {
    run_id: String,
    files: Vec<ManifestEntry>,
    #[serde(default)]
    errors: Vec<String>,
}

/// A generated file ready for persistence.
pub(crate) struct GeneratedFile {
    pub(crate) name: String,
    pub(crate) bytes: Vec<u8>,
    /// SHA256 from manifest, to verify integrity.
    pub(crate) sha256: String,
}

/// Error reading or validating the spool.
#[derive(Debug)]
pub(crate) enum SpoolError {
    /// No spool directory exists (normal case - no outputs were generated).
    NotFound,
    /// Manifest exists but is malformed or has wrong run_id.
    InvalidManifest,
    /// Manifest declares files that are missing or corrupted.
    FileError,
    /// Manifest explicitly declares export errors from the launcher.
    ExportFailed(Vec<String>),
}

/// Read the spool for a completed run and validate it.
///
/// Returns the list of generated files or an error. Does NOT clean up the spool -
/// that happens only after successful storage.
pub(crate) fn read_spool(
    workspace_root: &Path,
    run_id: &str,
) -> Result<Vec<GeneratedFile>, SpoolError> {
    let spool_dir = workspace_root.join(SPOOL_DIR).join(format!("run-{run_id}"));
    if !spool_dir.is_dir() {
        return Err(SpoolError::NotFound);
    }

    let manifest_path = spool_dir.join("manifest.json");
    let manifest: Manifest = match std::fs::read_to_string(&manifest_path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(m) => m,
            Err(_) => return Err(SpoolError::InvalidManifest),
        },
        Err(_) => return Err(SpoolError::InvalidManifest),
    };

    // Validate run_id matches - prevents using spool from a different run
    if manifest.run_id != format!("run-{run_id}") {
        return Err(SpoolError::InvalidManifest);
    }

    // If the launcher reported errors, fail the whole operation
    if !manifest.errors.is_empty() {
        return Err(SpoolError::ExportFailed(manifest.errors));
    }

    if manifest.files.is_empty() {
        return Err(SpoolError::NotFound);
    }

    if manifest.files.len() > MAX_GENERATED_FILES {
        return Err(SpoolError::InvalidManifest);
    }
    let files_dir = spool_dir.join("files");
    let mut total_bytes: i64 = 0;
    let mut result = Vec::with_capacity(manifest.files.len().min(MAX_GENERATED_FILES));

    for entry in manifest.files {
        if attachment::canonical_name(&entry.name).as_deref() != Some(entry.name.as_str())
            || entry.byte_length <= 0
        {
            return Err(SpoolError::FileError);
        }
        total_bytes = total_bytes
            .checked_add(entry.byte_length)
            .ok_or(SpoolError::FileError)?;
        if total_bytes > attachment::MAX_ATTACHMENT_BYTES_PER_ORG {
            return Err(SpoolError::FileError);
        }
        // Validate key is a UUID to prevent path traversal
        if Uuid::parse_str(&entry.key).is_err() {
            return Err(SpoolError::FileError);
        }

        let file_path = files_dir.join(format!("{}--{}", entry.key, entry.name));
        let metadata = std::fs::symlink_metadata(&file_path).map_err(|_| SpoolError::FileError)?;
        if !metadata.is_file() || metadata.len() != entry.byte_length as u64 {
            return Err(SpoolError::FileError);
        }
        let bytes = match std::fs::read(&file_path) {
            Ok(b) => b,
            Err(_) => return Err(SpoolError::FileError),
        };

        // Verify size matches manifest
        if bytes.len() as i64 != entry.byte_length {
            return Err(SpoolError::FileError);
        }

        // Verify SHA256 matches manifest
        let actual_hash = sha256_hex(&bytes);
        if actual_hash != entry.sha256 {
            return Err(SpoolError::FileError);
        }

        result.push(GeneratedFile {
            name: entry.name,
            bytes,
            sha256: entry.sha256,
        });
    }

    Ok(result)
}

/// Compute SHA256 hex digest.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Clean up the spool directory for a run.
pub(crate) fn cleanup_spool(workspace_root: &Path, run_id: &str) {
    let spool_dir = workspace_root.join(SPOOL_DIR).join(format!("run-{run_id}"));
    let _ = std::fs::remove_dir_all(spool_dir);
}

/// Context for persisting generated files.
///
/// For team chats: actor is the invoker's OrgContext, chat_id is set.
/// For private sessions: actor contains org_id and owner_id, chat_id is None.
pub(crate) struct PersistContext<'a> {
    pub(crate) actor: &'a crate::neev::types::OrgContext,
    pub(crate) session_id: &'a str,
    pub(crate) sequence: i64,
    /// For team chats: the chat_id to link attachments to.
    /// For private sessions: None.
    pub(crate) chat_id: Option<&'a str>,
}

/// Result of persistence with keys mapped to original filenames.
pub(crate) struct PersistResult {
    /// Map of original filename -> attachment key for link rewriting.
    pub(crate) stored: Vec<(String, String)>,
}

/// Persist generated files with explicit file list.
///
/// All files must succeed or none are stored. This is transactional at the
/// database level - blob writes that fail after DB commit leave orphans that
/// the sweep reclaims.
///
/// For team chats (chat_id is Some): attaches to the chat.
/// For private sessions (chat_id is None): attaches to the session.
pub(crate) async fn persist_generated_files(
    state: &ServerState,
    ctx: &PersistContext<'_>,
    files: Vec<GeneratedFile>,
) -> Result<PersistResult, ContentError> {
    if files.is_empty() {
        return Ok(PersistResult { stored: Vec::new() });
    }

    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid);
    }

    let tx = resources::begin(state, ctx.actor).await?;
    let owner = match ctx.chat_id {
        Some(id) => Owner::Chat(id),
        None => Owner::Session(ctx.session_id),
    };
    attachment::authorize(&tx, ctx.actor, owner, true).await?;
    // Validate every file before writing any bytes.
    for file in &files {
        sniff_attachment(&file.bytes).ok_or(ContentError::Invalid)?;
        attachment::canonical_name(&file.name).ok_or(ContentError::Invalid)?;
    }

    // Check quota
    let usage = db::one(
        &tx,
        "SELECT COUNT(*) AS c, CAST(COALESCE(SUM(byte_length),0) AS BIGINT) AS b FROM neev_attachments WHERE org_id=$1",
        vec![ctx.actor.org_id.into()],
    )
    .await?
    .ok_or(ContentError::Storage)?;
    let existing_count: i64 = usage.try_get("", "c").map_err(database_error)?;
    let existing_bytes: i64 = usage.try_get("", "b").map_err(database_error)?;

    // Pre-validate all files fit within quota
    let total_bytes: i64 = files.iter().map(|f| f.bytes.len() as i64).sum();
    let total_count = files.len() as i64;

    if existing_count + total_count > attachment::MAX_ATTACHMENTS_PER_ORG {
        return Err(ContentError::Quota);
    }
    if existing_bytes + total_bytes > attachment::MAX_ATTACHMENT_BYTES_PER_ORG {
        return Err(ContentError::Quota);
    }

    let mut written_keys: Vec<String> = Vec::new();
    let operation = async {
    let mut stored = Vec::with_capacity(files.len());

    // Determine owner type based on whether this is a team chat or private session
    let owner = match ctx.chat_id {
        Some(chat_id) => Owner::Chat(chat_id),
        None => Owner::Session(ctx.session_id),
    };

    for file in files {
        // Validate content type - reject unsupported types
        let content_type = sniff_attachment(&file.bytes).ok_or(ContentError::Invalid)?;

        // Validate name
        let file_name = attachment::canonical_name(&file.name).ok_or(ContentError::Invalid)?;

        let key = Uuid::new_v4().hyphenated().to_string();

        // Store blob
        let info = state
            .blobs
            .put_attachment(ctx.actor.org_id, &key, &file.bytes)
            .await?;

        written_keys.push(key.clone());

        // Insert attachment row
        db::execute(
            &tx,
            "INSERT INTO neev_attachments(org_id,key,chat_id,session_id,file_name,content_type,byte_length,sha256,created_by,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
            vec![
                ctx.actor.org_id.into(),
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
                ctx.actor.identity.user_id.into(),
                now.into(),
            ],
        )
        .await?;

        stored.push((file.name.clone(), key));
    }


    if ctx.chat_id.is_none() {
    // Link to turn with position offset to avoid colliding with input refs.
    // Query existing max position for this turn.
    let existing_max: i32 = match db::one(
        &tx,
        "SELECT COALESCE(MAX(position), -1) AS max_pos FROM neev_attachment_refs WHERE org_id=$1 AND turn_session_id=$2 AND turn_sequence=$3",
        vec![
            ctx.actor.org_id.into(),
            ctx.session_id.to_owned().into(),
            ctx.sequence.into(),
        ],
    )
    .await?
    {
        Some(row) => row.try_get("", "max_pos").map_err(database_error)?,
        None => -1,
    };

    let start_position = existing_max + 1;

    for (index, (_, key)) in stored.iter().enumerate() {
        db::execute(
            &tx,
            "INSERT INTO neev_attachment_refs(org_id,attachment_key,turn_session_id,turn_sequence,position) VALUES($1,$2,$3,$4,$5)",
            vec![
                ctx.actor.org_id.into(),
                key.clone().into(),
                ctx.session_id.to_owned().into(),
                ctx.sequence.into(),
                (start_position + index as i32).into(),
            ],
        )
        .await?;
    }

    }
    Ok::<_, ContentError>(stored)
    }.await;
    let stored = match operation {
        Ok(stored) => stored,
        Err(error) => {
            let _ = tx.rollback().await;
            for key in written_keys {
                let _ = state.blobs.remove_attachment(ctx.actor.org_id, &key).await;
            }
            return Err(error);
        }
    };
    // A commit error can be ambiguous; retain blobs for reconciliation rather
    // than deleting bytes that a committed row might already reference.
    tx.commit().await.map_err(database_error)?;
    Ok(PersistResult { stored })
}

/// Rewrite local output links in response text to attachment URLs.
///
/// Transforms `outputs/filename.html` and `neev-output/filename.html` patterns
/// to `/api/neev/attachments/{key}` URLs.
pub(crate) fn rewrite_output_links(text: &str, stored: &[(String, String)]) -> String {
    if stored.is_empty() {
        return text.to_owned();
    }

    let mut result = text.to_owned();
    for (filename, key) in stored {
        // Rewrite markdown links: [text](outputs/filename) -> [text](/api/neev/attachments/key)
        // Also handle neev-output/ directory
        for prefix in &["outputs/", "neev-output/"] {
            let old_path = format!("{prefix}{filename}");
            let new_path = format!("/api/neev/attachments/{key}");

            // Simple replacement - handles markdown links and raw URLs
            result = result.replace(&format!("({old_path})"), &format!("({new_path})"));
        }
        let url = format!("/api/neev/attachments/{key}");
        if !result.contains(&url) {
            let label = filename.replace('[', "\\[").replace(']', "\\]");
            result.push_str(&format!("\n\n[{label}]({url})"));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_rewrite_output_links() {
        let stored = vec![
            ("report.html".to_string(), "abc-123".to_string()),
            ("data.json".to_string(), "def-456".to_string()),
        ];

        let text = "See [the report](outputs/report.html) and [data](neev-output/data.json).";
        let result = rewrite_output_links(text, &stored);
        assert_eq!(
            result,
            "See [the report](/api/neev/attachments/abc-123) and [data](/api/neev/attachments/def-456)."
        );
    }

    #[test]
    fn test_rewrite_empty_stored() {
        let text = "No changes outputs/file.html";
        let result = rewrite_output_links(text, &[]);
        assert_eq!(result, text);
    }

    #[test]
    fn test_read_spool_not_found() {
        let temp = TempDir::new().unwrap();
        let result = read_spool(temp.path(), "nonexistent-run-id");
        assert!(matches!(result, Err(SpoolError::NotFound)));
    }

    #[test]
    fn test_read_spool_mismatched_run_id() {
        let temp = TempDir::new().unwrap();
        let spool_dir = temp.path().join("spool").join("run-actual-id");
        std::fs::create_dir_all(&spool_dir).unwrap();

        let manifest = serde_json::json!({
            "run_id": "different-id",
            "files": [],
            "errors": []
        });
        std::fs::write(spool_dir.join("manifest.json"), manifest.to_string()).unwrap();

        let result = read_spool(temp.path(), "actual-id");
        assert!(matches!(result, Err(SpoolError::InvalidManifest)));
    }

    #[test]
    fn test_read_spool_with_errors() {
        let temp = TempDir::new().unwrap();
        let run_id = "test-run-123";
        let spool_dir = temp.path().join("spool").join(format!("run-{run_id}"));
        std::fs::create_dir_all(&spool_dir).unwrap();

        let manifest = serde_json::json!({
            "run_id": format!("run-{run_id}"),
            "files": [],
            "errors": ["symlink rejected: bad-file"]
        });
        std::fs::write(spool_dir.join("manifest.json"), manifest.to_string()).unwrap();

        let result = read_spool(temp.path(), run_id);
        assert!(matches!(result, Err(SpoolError::ExportFailed(_))));
    }

    #[test]
    fn test_read_spool_valid_file() {
        let temp = TempDir::new().unwrap();
        let run_id = "test-run-456";
        let spool_dir = temp.path().join("spool").join(format!("run-{run_id}"));
        let files_dir = spool_dir.join("files");
        std::fs::create_dir_all(&files_dir).unwrap();

        let content = b"Hello, world!";
        let hash = sha256_hex(content);
        let key = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

        let manifest = serde_json::json!({
            "run_id": format!("run-{run_id}"),
            "files": [{
                "key": key,
                "name": "test.txt",
                "byte_length": content.len() as i64,
                "sha256": hash
            }],
            "errors": []
        });
        std::fs::write(spool_dir.join("manifest.json"), manifest.to_string()).unwrap();

        let file_path = files_dir.join(format!("{key}--test.txt"));
        let mut file = std::fs::File::create(&file_path).unwrap();
        file.write_all(content).unwrap();

        let result = read_spool(temp.path(), run_id);
        assert!(result.is_ok());
        let files = result.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "test.txt");
        assert_eq!(files[0].bytes, content);
        assert_eq!(files[0].sha256, hash);
    }

    #[test]
    fn test_read_spool_hash_mismatch() {
        let temp = TempDir::new().unwrap();
        let run_id = "test-run-789";
        let spool_dir = temp.path().join("spool").join(format!("run-{run_id}"));
        let files_dir = spool_dir.join("files");
        std::fs::create_dir_all(&files_dir).unwrap();

        let content = b"Hello, world!";
        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";
        let key = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

        let manifest = serde_json::json!({
            "run_id": format!("run-{run_id}"),
            "files": [{
                "key": key,
                "name": "test.txt",
                "byte_length": content.len() as i64,
                "sha256": wrong_hash
            }],
            "errors": []
        });
        std::fs::write(spool_dir.join("manifest.json"), manifest.to_string()).unwrap();

        let file_path = files_dir.join(format!("{key}--test.txt"));
        let mut file = std::fs::File::create(&file_path).unwrap();
        file.write_all(content).unwrap();

        let result = read_spool(temp.path(), run_id);
        assert!(matches!(result, Err(SpoolError::FileError)));
    }

    #[test]
    fn test_read_spool_invalid_key_path_traversal() {
        let temp = TempDir::new().unwrap();
        let run_id = "test-run-traversal";
        let spool_dir = temp.path().join("spool").join(format!("run-{run_id}"));
        std::fs::create_dir_all(&spool_dir).unwrap();

        // Key with path traversal attempt
        let manifest = serde_json::json!({
            "run_id": format!("run-{run_id}"),
            "files": [{
                "key": "../../../etc/passwd",
                "name": "test.txt",
                "byte_length": 10,
                "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
            }],
            "errors": []
        });
        std::fs::write(spool_dir.join("manifest.json"), manifest.to_string()).unwrap();

        let result = read_spool(temp.path(), run_id);
        // Should fail because the key is not a valid UUID
        assert!(matches!(result, Err(SpoolError::FileError)));
    }

    #[test]
    fn test_cleanup_spool() {
        let temp = TempDir::new().unwrap();
        let run_id = "cleanup-test";
        let spool_dir = temp.path().join("spool").join(format!("run-{run_id}"));
        std::fs::create_dir_all(&spool_dir).unwrap();
        std::fs::write(spool_dir.join("manifest.json"), "{}").unwrap();

        assert!(spool_dir.exists());
        cleanup_spool(temp.path(), run_id);
        assert!(!spool_dir.exists());
    }
    #[tokio::test]
    async fn generated_file_is_durable_scoped_and_restaged() {
        use crate::neev::{
            agents::sessions::{self, SessionInput},
            test_support as h,
        };
        use std::sync::Arc;
        let database = h::database().await;
        let uid = h::native_account(&database, "outputs", "outputs@example.test").await;
        let (org, _) = h::organization(&database, uid, "admin").await;
        let state = ServerState::new(
            database.clone(),
            h::preview_config(),
            Arc::new(h::TestClock::new(1000)),
        )
        .unwrap();
        let actor = h::scoped_context(&database, uid, org, 1000).await;
        let session = sessions::create(
            &state,
            &actor,
            SessionInput {
                title: "Outputs".into(),
                provider: "claude".into(),
                model: "default".into(),
                bound_chat_id: None,
                node_id: None,
                effort: None,
                fast: false,
                request_id: Uuid::new_v4().to_string(),
            },
        )
        .await
        .unwrap();
        let (turn, _) = sessions::begin_turn(&state, &actor, &session.id, "Make HTML", None)
            .await
            .unwrap();
        let bytes = b"<!doctype html><title>Saved output</title>".to_vec();
        let make_file = || GeneratedFile {
            name: "report.html".into(),
            sha256: sha256_hex(&bytes),
            bytes: bytes.clone(),
        };
        let ctx = PersistContext {
            actor: &actor,
            session_id: &session.id,
            sequence: turn.sequence,
            chat_id: None,
        };
        let saved = persist_generated_files(&state, &ctx, (0..6).map(|_| make_file()).collect())
            .await
            .unwrap();
        let key = &saved.stored[0].1;
        let loaded = attachment::load(&state, &actor, key).await.unwrap();
        assert_eq!(loaded.0, bytes);
        let row = db::one(&database, "SELECT attachment_key FROM neev_attachment_refs WHERE org_id=$1 AND turn_session_id=$2 AND turn_sequence=$3 ORDER BY position LIMIT 1", vec![org.into(), session.id.clone().into(), turn.sequence.into()]).await.unwrap().unwrap();
        assert_eq!(row.try_get::<String>("", "attachment_key").unwrap(), *key);
        let node = crate::neev::catalog::nodes::create(
            &state,
            &actor,
            crate::neev::types::NodeInput {
                parent_id: None,
                kind: crate::neev::types::NodeKind::Project,
                name: "Outputs".into(),
            },
        )
        .await
        .unwrap();
        let chat = resources::create(
            &state,
            &actor,
            crate::neev::content::types::ResourceInput {
                node_id: node.id,
                kind: crate::neev::content::types::ResourceKind::Chat,
                title: "Generated files".into(),
                origin_chat_id: None,
                request_id: Uuid::new_v4().to_string(),
            },
        )
        .await
        .unwrap();
        let team_ctx = PersistContext {
            actor: &actor,
            session_id: &session.id,
            sequence: turn.sequence,
            chat_id: Some(&chat.id),
        };
        let team_files =
            persist_generated_files(&state, &team_ctx, (0..6).map(|_| make_file()).collect())
                .await
                .unwrap();
        let team_keys: Vec<String> = team_files
            .stored
            .iter()
            .map(|(_, key)| key.clone())
            .collect();
        let message = crate::neev::content::messages::publish_agent_reply_with_attachments(
            &state, &actor, &chat.id, "Saved", "claude", &turn.id, &team_keys,
        )
        .await
        .unwrap();
        let linked = attachment::for_messages(&state, &actor, &[message.id.clone()])
            .await
            .unwrap();
        assert_eq!(linked[&message.id].len(), 6);
        // Startup/restore validation must accept the resulting chat and turn refs.
        db::initialize(&database).await.unwrap();
        let other = h::native_account(&database, "other", "other@example.test").await;
        h::add_membership(&database, org, other, "member").await;
        let outsider = h::scoped_context(&database, other, org, 1000).await;
        assert!(attachment::load(&state, &outsider, key).await.is_err());
        let denied = PersistContext {
            actor: &outsider,
            session_id: &session.id,
            sequence: turn.sequence,
            chat_id: None,
        };
        assert!(persist_generated_files(&state, &denied, vec![make_file()])
            .await
            .is_err());
    }
}
