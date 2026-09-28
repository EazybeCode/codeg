//! Shared organization library: skills and MCP configurations.
//!
//! Org admins can import skills (SKILL.md + supporting files) and MCP JSON
//! configurations. All members can read; enabled items reach agent runs.
//!
//! ACL: admin writes, member reads. No secrets in listings. Stale membership
//! checks on every operation via proper transaction pattern.

use crate::neev::{
    auth::session,
    db,
    error::{database_error, AuthError},
    startup::ServerState,
    types::{OrgContext, Role},
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use zip::ZipArchive;

/// Maximum file size for individual files (1MB).
const MAX_FILE_SIZE: usize = 1_048_576;
/// Maximum total uncompressed size for a ZIP import (10MB).
const MAX_ZIP_UNCOMPRESSED_SIZE: usize = 10 * 1024 * 1024;
/// Maximum number of files in a ZIP.
const MAX_ZIP_FILES: usize = 100;
/// Maximum library items per organization.
const MAX_ITEMS_PER_ORG: i64 = 100;
/// Maximum files per library item.
const MAX_FILES_PER_ITEM: usize = 50;
/// Maximum per-org storage in bytes (64 MiB).
const MAX_ORG_STORAGE_BYTES: i64 = 64 * 1024 * 1024;
/// Maximum MCP server name length.
const MAX_MCP_NAME_LEN: usize = 100;
/// Maximum MCP command length.
const MAX_MCP_COMMAND_LEN: usize = 500;
/// Maximum MCP args count.
const MAX_MCP_ARGS_COUNT: usize = 50;
/// Maximum MCP arg length.
const MAX_MCP_ARG_LEN: usize = 1000;

/// A library item (skill or MCP config).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryItem {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) kind: String,
    pub(crate) enabled: bool,
    #[serde(serialize_with = "serialize_revision")]
    pub(crate) revision: i64,
    pub(crate) files: Vec<String>,
    pub(crate) setup_note: Option<String>,
}

// JavaScript cannot represent all i64 revisions exactly. Keep the native API's
// string revision contract, while database operations use checked integers.
fn serialize_revision<S: serde::Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}

/// Request to import a skill or MCP config.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportRequest {
    pub(crate) file_name: String,
    pub(crate) data: String, // base64
}

/// Request to update a library item (enable/disable).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PatchRequest {
    pub(crate) expected_revision: i64,
    pub(crate) enabled: bool,
}

/// Request to delete a library item.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeleteRequest {
    pub(crate) expected_revision: i64,
}

/// Response with all library items.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryResponse {
    pub(crate) items: Vec<LibraryItem>,
}

/// Result of successful deletion.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DeleteResponse {
    pub(crate) ok: bool,
}

/// Internal file for storage.
#[derive(Clone)]
struct ImportedFile {
    path: String,
    content: Vec<u8>,
}

/// Imported skill before storage.
struct ImportedSkill {
    name: String,
    description: String,
    files: Vec<ImportedFile>,
}

/// Normalized MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NormalizedMcpServer {
    pub(crate) name: String,
    #[serde(flatten)]
    pub(crate) config: McpServerConfig,
}

/// MCP server transport configuration (stdio or HTTP).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum McpServerConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Http {
        url: String,
    },
}

/// Begin a library transaction with proper ACL checks.
async fn begin(state: &ServerState, actor: &OrgContext) -> Result<DatabaseTransaction, AuthError> {
    let tx = db::begin(&state.database).await?;
    db::lock_admission_guard(&tx).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    Ok(tx)
}

/// List all library items for the organization.
/// Any member can read; membership is validated in transaction.
pub(crate) async fn list(
    state: &ServerState,
    actor: &OrgContext,
) -> Result<Vec<LibraryItem>, AuthError> {
    let tx = begin(state, actor).await?;
    let items = list_internal(&tx, actor.org_id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(items)
}

/// Internal list without transaction management.
async fn list_internal<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
) -> Result<Vec<LibraryItem>, AuthError> {
    let rows = db
        .query_all(db::statement(
            db,
            "SELECT id, name, description, kind, enabled, revision, setup_note
             FROM neev_library_items
             WHERE org_id=$1
             ORDER BY created_at DESC",
            vec![org_id.into()],
        ))
        .await
        .map_err(database_error)?;

    let mut items = Vec::new();
    for row in &rows {
        let id: String = row.try_get("", "id").map_err(database_error)?;
        let name: String = row.try_get("", "name").map_err(database_error)?;
        let description: String = row.try_get("", "description").map_err(database_error)?;
        let kind: String = row.try_get("", "kind").map_err(database_error)?;
        let enabled: bool = row.try_get("", "enabled").map_err(database_error)?;
        let revision: i64 = row.try_get("", "revision").map_err(database_error)?;
        let setup_note: Option<String> = row.try_get("", "setup_note").map_err(database_error)?;

        // Load file paths for this item
        let file_rows = db
            .query_all(db::statement(
                db,
                "SELECT path FROM neev_library_files WHERE item_id=$1 AND org_id=$2 ORDER BY path",
                vec![id.clone().into(), org_id.into()],
            ))
            .await
            .map_err(database_error)?;
        let files: Vec<String> = file_rows
            .iter()
            .map(|r| r.try_get("", "path").map_err(database_error))
            .collect::<Result<_, _>>()?;

        items.push(LibraryItem {
            id,
            name,
            description,
            kind,
            enabled,
            revision,
            files,
            setup_note,
        });
    }
    Ok(items)
}

/// Get current storage usage for an org.
async fn get_org_storage_bytes<C: ConnectionTrait>(db: &C, org_id: i32) -> Result<i64, AuthError> {
    let row = db::one(
        db,
        "SELECT COALESCE(SUM(LENGTH(content)), 0) as total FROM neev_library_files WHERE org_id=$1",
        vec![org_id.into()],
    )
    .await?;
    row.ok_or(AuthError::Unavailable)?
        .try_get::<i64>("", "total")
        .map_err(database_error)
}

/// Import a skill ZIP/MD or MCP JSON file. Admin only.
pub(crate) async fn import(
    state: &ServerState,
    actor: &OrgContext,
    request: ImportRequest,
) -> Result<Vec<LibraryItem>, AuthError> {
    // Admin only
    if actor.role != Role::Admin {
        return Err(AuthError::Forbidden);
    }

    // Decode base64
    let data = BASE64
        .decode(&request.data)
        .map_err(|_| AuthError::Invalid)?;

    // Determine type from filename and content
    let file_name = request.file_name.to_lowercase();
    let (skills, mcp_servers) = if file_name.ends_with(".zip") {
        (parse_skill_zip(&data)?, Vec::new())
    } else if file_name.ends_with(".json") {
        (Vec::new(), parse_mcp_json(&data)?)
    } else if file_name.ends_with(".md") {
        (vec![parse_skill_md(&request.file_name, &data)?], Vec::new())
    } else {
        return Err(AuthError::Invalid);
    };

    let total_items = skills.len() + mcp_servers.len();
    if total_items == 0 {
        return Err(AuthError::Invalid);
    }

    // Calculate total bytes to store
    let mut new_bytes: i64 = 0;
    for skill in &skills {
        for file in &skill.files {
            new_bytes += file.content.len() as i64;
        }
    }
    for server in &mcp_servers {
        // Estimate JSON size
        new_bytes += serde_json::to_vec(&server)
            .map(|v| v.len() as i64)
            .unwrap_or(1000);
    }

    // Begin transaction with ACL checks
    let tx = begin(state, actor).await?;

    // Check org item count limit
    let current_count = db::one(
        &tx,
        "SELECT COUNT(*) as cnt FROM neev_library_items WHERE org_id=$1",
        vec![actor.org_id.into()],
    )
    .await?
    .map(|r| r.try_get::<i64>("", "cnt").unwrap_or(0))
    .unwrap_or(0);

    if current_count + total_items as i64 > MAX_ITEMS_PER_ORG {
        return Err(AuthError::Limited);
    }

    // Check per-org storage limit
    let current_storage = get_org_storage_bytes(&tx, actor.org_id).await?;
    if current_storage + new_bytes > MAX_ORG_STORAGE_BYTES {
        return Err(AuthError::Limited);
    }

    let mut stored_ids = Vec::new();

    // Store skills
    for skill in skills {
        if skill.files.len() > MAX_FILES_PER_ITEM {
            return Err(AuthError::Limited);
        }

        let id = uuid::Uuid::new_v4().to_string();
        db::execute(
            &tx,
            "INSERT INTO neev_library_items (id, org_id, kind, name, description, enabled, creator_id, revision, setup_note)
             VALUES ($1, $2, 'skill', $3, $4, false, $5, 1, NULL)",
            vec![
                id.clone().into(),
                actor.org_id.into(),
                skill.name.into(),
                skill.description.into(),
                actor.identity.user_id.into(),
            ],
        )
        .await?;

        // Store files
        for file in skill.files {
            let file_id = uuid::Uuid::new_v4().to_string();
            let hash = format!("{:x}", Sha256::digest(&file.content));
            db::execute(
                &tx,
                "INSERT INTO neev_library_files (id, item_id, org_id, path, content_hash, content)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                vec![
                    file_id.into(),
                    id.clone().into(),
                    actor.org_id.into(),
                    file.path.into(),
                    hash.into(),
                    file.content.into(),
                ],
            )
            .await?;
        }

        stored_ids.push(id);
    }

    // Store MCP servers (each as separate item)
    for server in mcp_servers {
        let id = uuid::Uuid::new_v4().to_string();
        let setup_note = match &server.config {
            McpServerConfig::Http { url } => Some(format!(
                "This MCP server connects to {}. Ensure destination is on your network allowlist.",
                url
            )),
            McpServerConfig::Stdio { command, .. } => Some(format!(
                "This MCP server requires '{}' to be installed and in PATH.",
                command
            )),
        };

        db::execute(
            &tx,
            "INSERT INTO neev_library_items (id, org_id, kind, name, description, enabled, creator_id, revision, setup_note)
             VALUES ($1, $2, 'mcp', $3, '', false, $4, 1, $5)",
            vec![
                id.clone().into(),
                actor.org_id.into(),
                server.name.clone().into(),
                actor.identity.user_id.into(),
                setup_note.into(),
            ],
        )
        .await?;

        // Store normalized config as JSON file
        let config_json = serde_json::to_vec_pretty(&server).map_err(|_| AuthError::Invalid)?;
        let file_id = uuid::Uuid::new_v4().to_string();
        let hash = format!("{:x}", Sha256::digest(&config_json));
        db::execute(
            &tx,
            "INSERT INTO neev_library_files (id, item_id, org_id, path, content_hash, content)
             VALUES ($1, $2, $3, 'mcp.json', $4, $5)",
            vec![
                file_id.into(),
                id.clone().into(),
                actor.org_id.into(),
                hash.into(),
                config_json.into(),
            ],
        )
        .await?;

        stored_ids.push(id);
    }

    // Final recheck before commit
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;

    // Return only newly imported items (separate read transaction)
    let tx = begin(state, actor).await?;
    let all_items = list_internal(&tx, actor.org_id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;

    let stored_set: HashSet<_> = stored_ids.iter().collect();
    Ok(all_items
        .into_iter()
        .filter(|item| stored_set.contains(&item.id))
        .collect())
}

/// Update a library item (enable/disable). Admin only.
pub(crate) async fn patch(
    state: &ServerState,
    actor: &OrgContext,
    item_id: &str,
    request: PatchRequest,
) -> Result<LibraryItem, AuthError> {
    // Admin only
    if actor.role != Role::Admin {
        return Err(AuthError::Forbidden);
    }

    if request.expected_revision < 1 {
        return Err(AuthError::Invalid);
    }

    let tx = begin(state, actor).await?;

    // Check item exists and belongs to org
    let row = db::one(
        &tx,
        "SELECT revision FROM neev_library_items WHERE id=$1 AND org_id=$2",
        vec![item_id.into(), actor.org_id.into()],
    )
    .await?
    .ok_or(AuthError::Forbidden)?; // Not found = Forbidden (cross-org)

    let current_rev: i64 = row.try_get("", "revision").map_err(database_error)?;
    if current_rev != request.expected_revision {
        return Err(AuthError::Conflict); // Stale revision
    }

    // Update
    db::execute(
        &tx,
        "UPDATE neev_library_items SET enabled=$1, revision=revision+1, updated_at=CURRENT_TIMESTAMP WHERE id=$2",
        vec![request.enabled.into(), item_id.into()],
    )
    .await?;

    // Re-fetch updated item
    let items = list_internal(&tx, actor.org_id).await?;
    let updated = items
        .into_iter()
        .find(|i| i.id == item_id)
        .ok_or(AuthError::Unavailable)?;

    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;

    Ok(updated)
}

/// Delete a library item. Admin only.
pub(crate) async fn delete(
    state: &ServerState,
    actor: &OrgContext,
    item_id: &str,
    request: DeleteRequest,
) -> Result<DeleteResponse, AuthError> {
    // Admin only
    if actor.role != Role::Admin {
        return Err(AuthError::Forbidden);
    }

    if request.expected_revision < 1 {
        return Err(AuthError::Invalid);
    }

    let tx = begin(state, actor).await?;

    // Check item exists and belongs to org
    let row = db::one(
        &tx,
        "SELECT revision FROM neev_library_items WHERE id=$1 AND org_id=$2",
        vec![item_id.into(), actor.org_id.into()],
    )
    .await?
    .ok_or(AuthError::Forbidden)?;

    let current_rev: i64 = row.try_get("", "revision").map_err(database_error)?;
    if current_rev != request.expected_revision {
        return Err(AuthError::Conflict);
    }

    // Delete (cascade deletes files)
    db::execute(
        &tx,
        "DELETE FROM neev_library_items WHERE id=$1",
        vec![item_id.into()],
    )
    .await?;

    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;

    Ok(DeleteResponse { ok: true })
}

/// Load enabled items for an organization with their file contents.
/// Used by agent execution to stage skills/configs.
/// Note: This requires the caller to have already validated org membership.
pub(crate) async fn load_enabled(
    state: &ServerState,
    actor: &OrgContext,
) -> Result<Vec<EnabledItem>, AuthError> {
    let tx = begin(state, actor).await?;
    let items = load_enabled_internal(&tx, actor.org_id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(items)
}

/// Internal load without transaction management.
async fn load_enabled_internal<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
) -> Result<Vec<EnabledItem>, AuthError> {
    let rows = db
        .query_all(db::statement(
            db,
            "SELECT id, name, kind, setup_note FROM neev_library_items
             WHERE org_id=$1 AND enabled=true",
            vec![org_id.into()],
        ))
        .await
        .map_err(database_error)?;

    let mut items = Vec::new();
    for row in &rows {
        let id: String = row.try_get("", "id").map_err(database_error)?;
        let name: String = row.try_get("", "name").map_err(database_error)?;
        let kind: String = row.try_get("", "kind").map_err(database_error)?;
        let setup_note: Option<String> = row.try_get("", "setup_note").map_err(database_error)?;

        // Load files with content
        let file_rows = db
            .query_all(db::statement(
                db,
                "SELECT path, content, content_hash FROM neev_library_files WHERE item_id=$1 AND org_id=$2",
                vec![id.clone().into(), org_id.into()],
            ))
            .await
            .map_err(database_error)?;

        let mut files = Vec::new();
        for fr in &file_rows {
            let path: String = fr.try_get("", "path").map_err(database_error)?;
            let content: Vec<u8> = fr.try_get("", "content").map_err(database_error)?;
            let stored_hash: String = fr.try_get("", "content_hash").map_err(database_error)?;
            if format!("{:x}", Sha256::digest(&content)) != stored_hash {
                return Err(AuthError::Unavailable);
            }
            files.push(EnabledFile { path, content });
        }

        items.push(EnabledItem {
            id,
            name,
            kind,
            files,
            setup_note,
        });
    }
    Ok(items)
}

/// An enabled library item with full file contents.
#[derive(Debug, Clone)]
pub(crate) struct EnabledItem {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) files: Vec<EnabledFile>,
    pub(crate) setup_note: Option<String>,
}

/// A file from an enabled item.
#[derive(Debug, Clone)]
pub(crate) struct EnabledFile {
    pub(crate) path: String,
    pub(crate) content: Vec<u8>,
}

// --- Path validation ---

/// Check if a path component is invalid.
fn is_invalid_path_component(component: &str) -> bool {
    // Empty, current dir, parent dir
    if component.is_empty() || component == "." || component == ".." {
        return true;
    }
    // Hidden files (except . itself which we already rejected)
    if component.starts_with('.') {
        return true;
    }
    // Control characters or NUL
    if component.bytes().any(|b| b < 32 || b == 0) {
        return true;
    }
    // Windows drive letters (e.g., "C:")
    if component.len() >= 2 && component.as_bytes()[1] == b':' {
        return true;
    }
    // Colon anywhere (Windows path separator)
    if component.contains(':') {
        return true;
    }
    false
}

/// Validate and canonicalize a ZIP path. Returns None if invalid.
fn validate_zip_path(raw: &str) -> Option<String> {
    // Reject absolute paths
    if raw.starts_with('/') || raw.starts_with('\\') {
        return None;
    }
    // Reject parent traversal anywhere
    if raw.contains("..") {
        return None;
    }
    // Normalize separators
    let normalized = raw.replace('\\', "/");
    // Split and validate each component
    let components: Vec<&str> = normalized.split('/').collect();
    for component in &components {
        if is_invalid_path_component(component) {
            return None;
        }
    }
    // Rebuild canonical path
    let canonical = components.join("/");
    if canonical.is_empty() {
        return None;
    }
    Some(canonical)
}

/// Find the skill root for a given path. Returns the directory containing SKILL.md.
fn find_skill_root(path: &str, skill_roots: &HashSet<String>) -> Option<String> {
    // Check if path itself is a SKILL.md
    let lower = path.to_lowercase();
    if lower == "skill.md" || lower.ends_with("/skill.md") {
        // Return the directory (or empty for root)
        if let Some(idx) = path.rfind('/') {
            return Some(path[..idx].to_string());
        } else {
            return Some(String::new());
        }
    }
    // Find the nearest containing skill root
    let mut best_match: Option<&String> = None;
    let mut best_len = 0;
    for root in skill_roots {
        if root.is_empty() {
            // Root skill matches everything
            if best_match.is_none() {
                best_match = Some(root);
            }
        } else if path.starts_with(root)
            && path.len() > root.len()
            && path.as_bytes()[root.len()] == b'/'
        {
            // This root is a prefix of the path
            if root.len() > best_len {
                best_len = root.len();
                best_match = Some(root);
            }
        }
    }
    best_match.cloned()
}

// --- Parsing functions ---

fn parse_skill_zip(data: &[u8]) -> Result<Vec<ImportedSkill>, AuthError> {
    let cursor = std::io::Cursor::new(data);
    let mut archive = ZipArchive::new(cursor).map_err(|_| AuthError::Invalid)?;

    if archive.len() > MAX_ZIP_FILES {
        return Err(AuthError::Limited);
    }

    // Track seen canonical paths to reject duplicates
    let mut seen_paths: HashSet<String> = HashSet::new();
    // Track total uncompressed size
    let mut total_uncompressed: usize = 0;

    // First pass: find all SKILL.md files and their parent directories
    let mut skill_roots: HashSet<String> = HashSet::new();
    let mut all_files: Vec<(String, Vec<u8>)> = Vec::new();

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|_| AuthError::Invalid)?;

        // Reject symlinks
        if file.is_symlink() {
            return Err(AuthError::Invalid);
        }

        // Skip directories
        if file.is_dir() {
            continue;
        }

        let raw_name = file.name().to_string();

        // Validate and canonicalize path
        let canonical = validate_zip_path(&raw_name).ok_or(AuthError::Invalid)?;

        // Reject duplicates
        let lower_canonical = canonical.to_lowercase();
        if seen_paths.contains(&lower_canonical) {
            return Err(AuthError::Invalid);
        }
        seen_paths.insert(lower_canonical.clone());

        // Check individual file size limit with bounded read
        let mut content = Vec::new();
        let mut limited_reader = file.take(MAX_FILE_SIZE as u64 + 1);
        limited_reader
            .read_to_end(&mut content)
            .map_err(|_| AuthError::Invalid)?;
        if content.len() > MAX_FILE_SIZE {
            return Err(AuthError::Limited);
        }

        // Track cumulative uncompressed size
        total_uncompressed += content.len();
        if total_uncompressed > MAX_ZIP_UNCOMPRESSED_SIZE {
            return Err(AuthError::Limited);
        }

        // Check if this is a SKILL.md
        if lower_canonical == "skill.md" || lower_canonical.ends_with("/skill.md") {
            // Record the skill root directory
            let root = if let Some(idx) = canonical.rfind('/') {
                canonical[..idx].to_string()
            } else {
                String::new() // root level
            };
            skill_roots.insert(root);
        }

        // Skip known metadata directories
        if canonical.starts_with("__MACOSX/") || canonical.starts_with("__MACOSX") {
            continue;
        }

        all_files.push((canonical, content));
    }

    // Must have at least one SKILL.md
    if skill_roots.is_empty() {
        return Err(AuthError::Invalid);
    }

    // Second pass: assign files to skill roots
    let mut skill_files: HashMap<String, Vec<ImportedFile>> = HashMap::new();
    for root in &skill_roots {
        skill_files.insert(root.clone(), Vec::new());
    }

    for (path, content) in all_files {
        // Find the skill root this file belongs to
        let skill_root = find_skill_root(&path, &skill_roots);

        match skill_root {
            Some(root) => {
                // Calculate relative path within skill
                let relative_path = if root.is_empty() {
                    path.clone()
                } else if path.len() > root.len() + 1 {
                    path[root.len() + 1..].to_string()
                } else {
                    path.clone()
                };

                // Normalize SKILL.md case
                let normalized_path = if relative_path.to_lowercase() == "skill.md" {
                    "SKILL.md".to_string()
                } else {
                    relative_path
                };

                skill_files.get_mut(&root).unwrap().push(ImportedFile {
                    path: normalized_path,
                    content,
                });
            }
            None => {
                // Orphan file - reject unless it's metadata we already filtered
                return Err(AuthError::Invalid);
            }
        }
    }

    // Build skills from collected files
    let mut skills = Vec::new();
    for (root, files) in skill_files {
        if files.is_empty() {
            continue;
        }

        // Find SKILL.md content
        let skill_md = files.iter().find(|f| f.path == "SKILL.md");
        let skill_md = match skill_md {
            Some(f) => f,
            None => continue, // Should not happen, but skip if it does
        };

        let content_str = String::from_utf8_lossy(&skill_md.content);
        let (name, description) = parse_skill_header(&content_str);
        let name = if name.is_empty() {
            if root.is_empty() {
                "Imported Skill".to_string()
            } else {
                // Use the last component of the root path
                root.rsplit('/')
                    .next()
                    .unwrap_or("Imported Skill")
                    .to_string()
            }
        } else {
            name
        };

        skills.push(ImportedSkill {
            name,
            description,
            files,
        });
    }

    if skills.is_empty() {
        return Err(AuthError::Invalid);
    }

    Ok(skills)
}

fn parse_skill_md(file_name: &str, data: &[u8]) -> Result<ImportedSkill, AuthError> {
    if data.len() > MAX_FILE_SIZE {
        return Err(AuthError::Limited);
    }

    let content_str = String::from_utf8_lossy(data);
    let (name, description) = parse_skill_header(&content_str);
    let name = if name.is_empty() {
        file_name
            .trim_end_matches(".md")
            .trim_end_matches(".MD")
            .to_string()
    } else {
        name
    };

    Ok(ImportedSkill {
        name,
        description,
        files: vec![ImportedFile {
            path: "SKILL.md".to_string(),
            content: data.to_vec(),
        }],
    })
}

fn parse_skill_header(content: &str) -> (String, String) {
    let mut name = String::new();
    let mut description = String::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !name.is_empty() && description.is_empty() {
                continue; // Skip blank line after title
            }
            if !description.is_empty() {
                break; // End of header
            }
        } else if name.is_empty() && trimmed.starts_with('#') {
            name = trimmed.trim_start_matches('#').trim().to_string();
        } else if !name.is_empty() && !trimmed.starts_with('#') {
            if description.is_empty() {
                description = trimmed.to_string();
            } else {
                description.push(' ');
                description.push_str(trimmed);
            }
        } else if !name.is_empty() && trimmed.starts_with('#') {
            break; // Next section
        }
    }

    // Truncate to schema limits
    if name.len() > 200 {
        name = name.chars().take(200).collect();
    }
    if description.len() > 2000 {
        description = description.chars().take(2000).collect();
    }

    (name, description)
}

/// Validate MCP server name (slug-ish: alphanumeric, underscore, hyphen).
fn validate_mcp_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_MCP_NAME_LEN {
        return false;
    }
    name.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Validate MCP command string.
fn validate_mcp_command(cmd: &str) -> bool {
    if cmd.is_empty() || cmd.len() > MAX_MCP_COMMAND_LEN {
        return false;
    }
    // No NUL bytes
    if cmd.bytes().any(|b| b == 0) {
        return false;
    }
    // No credential patterns
    !contains_credential_pattern(cmd)
}

/// Parse MCP JSON config and normalize to individual server entries.
/// Strictly validates mcpServers map format, only allowing:
/// - stdio: {command, args} or {type: "stdio", command, args}
/// - http: {type: "http", url}
fn parse_mcp_json(data: &[u8]) -> Result<Vec<NormalizedMcpServer>, AuthError> {
    if data.len() > MAX_FILE_SIZE {
        return Err(AuthError::Limited);
    }

    let json: serde_json::Value = serde_json::from_slice(data).map_err(|_| AuthError::Invalid)?;

    // Must be an object with exactly mcpServers key
    let obj = json.as_object().ok_or(AuthError::Invalid)?;
    if obj.len() != 1 || !obj.contains_key("mcpServers") {
        return Err(AuthError::Invalid);
    }

    let servers_map = obj
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .ok_or(AuthError::Invalid)?;

    if servers_map.is_empty() {
        return Err(AuthError::Invalid);
    }

    let mut servers = Vec::new();

    for (name, config) in servers_map {
        // Validate name (slug-ish)
        if !validate_mcp_name(name) {
            return Err(AuthError::Invalid);
        }

        let config_obj = config.as_object().ok_or(AuthError::Invalid)?;

        // Determine transport type
        let type_val = config_obj.get("type").and_then(|v| v.as_str());

        let server_config = match type_val {
            Some("http") => {
                // HTTP transport: exactly {type, url}
                if config_obj.len() != 2 {
                    return Err(AuthError::Invalid);
                }
                let url = config_obj
                    .get("url")
                    .and_then(|v| v.as_str())
                    .ok_or(AuthError::Invalid)?;
                validate_mcp_url(url)?;
                McpServerConfig::Http {
                    url: url.to_string(),
                }
            }
            Some("stdio") | None => {
                // stdio transport
                let expected_keys: HashSet<&str> = if type_val.is_some() {
                    ["type", "command", "args"].iter().cloned().collect()
                } else {
                    ["command", "args"].iter().cloned().collect()
                };

                // Reject unknown keys
                for key in config_obj.keys() {
                    if !expected_keys.contains(key.as_str()) {
                        return Err(AuthError::Invalid);
                    }
                }

                // Command is required
                let command = config_obj
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or(AuthError::Invalid)?;

                if !validate_mcp_command(command) {
                    return Err(AuthError::Invalid);
                }

                // Parse args - must all be strings, reject non-strings
                let args: Vec<String> = if let Some(args_val) = config_obj.get("args") {
                    let arr = args_val.as_array().ok_or(AuthError::Invalid)?;
                    if arr.len() > MAX_MCP_ARGS_COUNT {
                        return Err(AuthError::Limited);
                    }
                    let mut result = Vec::with_capacity(arr.len());
                    for arg in arr {
                        let s = arg.as_str().ok_or(AuthError::Invalid)?; // Reject non-string
                        if s.len() > MAX_MCP_ARG_LEN {
                            return Err(AuthError::Limited);
                        }
                        if s.bytes().any(|b| b == 0) {
                            return Err(AuthError::Invalid); // No NUL
                        }
                        if contains_credential_pattern(s) {
                            return Err(AuthError::Invalid);
                        }
                        result.push(s.to_string());
                    }
                    result
                } else {
                    Vec::new()
                };

                McpServerConfig::Stdio {
                    command: command.to_string(),
                    args,
                }
            }
            Some(_) => {
                // Unknown type
                return Err(AuthError::Invalid);
            }
        };

        servers.push(NormalizedMcpServer {
            name: name.clone(),
            config: server_config,
        });
    }

    Ok(servers)
}

/// Validate MCP URL - must be HTTPS, no credentials, no query/fragment.
fn validate_mcp_url(url_str: &str) -> Result<(), AuthError> {
    let url = url::Url::parse(url_str).map_err(|_| AuthError::Invalid)?;

    // Must be HTTPS
    if url.scheme() != "https" {
        return Err(AuthError::Invalid);
    }

    // No username/password
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AuthError::Invalid);
    }

    // No query string (could contain tokens)
    if url.query().is_some() {
        return Err(AuthError::Invalid);
    }

    // No fragment
    if url.fragment().is_some() {
        return Err(AuthError::Invalid);
    }

    // Check for private/local hosts
    if let Some(host) = url.host_str() {
        let host_lower = host.to_lowercase();
        if host_lower == "localhost"
            || host_lower.starts_with("127.")
            || host_lower.starts_with("192.168.")
            || host_lower.starts_with("10.")
            || host_lower.ends_with(".local")
            || host_lower.ends_with(".internal")
        {
            return Err(AuthError::Invalid);
        }
    }

    Ok(())
}

/// Check if a string contains credential-like patterns.
fn contains_credential_pattern(s: &str) -> bool {
    let patterns = [
        // API keys
        "sk-",
        "api_key=",
        "apikey=",
        "api-key=",
        // Tokens
        "token=",
        "bearer ",
        "authorization:",
        // AWS
        "AKIA",
        "aws_access_key",
        "aws_secret",
        // Generic
        "password=",
        "secret=",
        "credential",
    ];

    let lower = s.to_lowercase();
    patterns.iter().any(|p| lower.contains(&p.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neev::test_support as h;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    /// Create a minimal valid SKILL.md content.
    fn skill_md_content(name: &str, desc: &str) -> String {
        format!("# {name}\n\n{desc}\n")
    }

    /// Create a ZIP with a single SKILL.md.
    fn single_skill_zip(name: &str, desc: &str) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = SimpleFileOptions::default();
            zip.start_file("SKILL.md", options).unwrap();
            zip.write_all(skill_md_content(name, desc).as_bytes())
                .unwrap();
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    /// Create a ZIP with root SKILL.md + references/ folder.
    fn root_skill_with_references_zip() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = SimpleFileOptions::default();

            // Root SKILL.md
            zip.start_file("SKILL.md", options).unwrap();
            zip.write_all(skill_md_content("Root Skill", "A skill with references").as_bytes())
                .unwrap();

            // Supporting file in references/
            zip.add_directory("references/", options).unwrap();
            zip.start_file("references/tone.md", options).unwrap();
            zip.write_all(b"# Tone Guide\n\nBe friendly.").unwrap();

            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    /// Create a ZIP with wrapper folder containing multiple skills.
    fn wrapper_folder_multi_skill_zip() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = SimpleFileOptions::default();

            // Wrapper/skill1/SKILL.md
            zip.add_directory("myskills/", options).unwrap();
            zip.add_directory("myskills/skill1/", options).unwrap();
            zip.start_file("myskills/skill1/SKILL.md", options).unwrap();
            zip.write_all(skill_md_content("Skill One", "First skill").as_bytes())
                .unwrap();
            zip.start_file("myskills/skill1/helper.py", options)
                .unwrap();
            zip.write_all(b"# helper code").unwrap();

            // Wrapper/skill2/SKILL.md
            zip.add_directory("myskills/skill2/", options).unwrap();
            zip.start_file("myskills/skill2/SKILL.md", options).unwrap();
            zip.write_all(skill_md_content("Skill Two", "Second skill").as_bytes())
                .unwrap();

            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    /// Create a ZIP with multiple skills in subdirectories.
    fn multi_skill_zip() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = SimpleFileOptions::default();

            // Skill 1
            zip.add_directory("skill1/", options).unwrap();
            zip.start_file("skill1/SKILL.md", options).unwrap();
            zip.write_all(skill_md_content("Skill One", "First skill").as_bytes())
                .unwrap();
            zip.start_file("skill1/helper.py", options).unwrap();
            zip.write_all(b"# helper code").unwrap();

            // Skill 2
            zip.add_directory("skill2/", options).unwrap();
            zip.start_file("skill2/SKILL.md", options).unwrap();
            zip.write_all(skill_md_content("Skill Two", "Second skill").as_bytes())
                .unwrap();

            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    /// Create a valid MCP JSON config with mcpServers map (no secrets).
    fn valid_mcp_json() -> Vec<u8> {
        serde_json::json!({
            "mcpServers": {
                "test-mcp": {
                    "command": "node",
                    "args": ["server.js"]
                }
            }
        })
        .to_string()
        .into_bytes()
    }

    /// Create an MCP JSON with explicit type: "stdio".
    fn explicit_stdio_mcp_json() -> Vec<u8> {
        serde_json::json!({
            "mcpServers": {
                "explicit-stdio": {
                    "type": "stdio",
                    "command": "python",
                    "args": ["-m", "mcp_server"]
                }
            }
        })
        .to_string()
        .into_bytes()
    }

    /// Create an MCP JSON with disallowed env field.
    fn env_mcp_json() -> Vec<u8> {
        serde_json::json!({
            "mcpServers": {
                "test-mcp": {
                    "command": "node",
                    "args": ["server.js"],
                    "env": {"API_KEY": "secret"}
                }
            }
        })
        .to_string()
        .into_bytes()
    }

    /// Create an MCP JSON with credential in command args.
    fn credential_args_mcp_json() -> Vec<u8> {
        serde_json::json!({
            "mcpServers": {
                "test-mcp": {
                    "command": "node",
                    "args": ["--token=sk-1234567890"]
                }
            }
        })
        .to_string()
        .into_bytes()
    }

    /// Create an MCP JSON with non-string arg.
    fn malformed_args_mcp_json() -> Vec<u8> {
        serde_json::json!({
            "mcpServers": {
                "test-mcp": {
                    "command": "node",
                    "args": ["server.js", 123, true]
                }
            }
        })
        .to_string()
        .into_bytes()
    }

    /// Create an MCP JSON with HTTPS URL.
    fn https_mcp_json() -> Vec<u8> {
        serde_json::json!({
            "mcpServers": {
                "remote-mcp": {
                    "type": "http",
                    "url": "https://api.example.com/mcp"
                }
            }
        })
        .to_string()
        .into_bytes()
    }

    /// Create an MCP JSON with HTTP (not HTTPS) URL.
    fn http_mcp_json() -> Vec<u8> {
        serde_json::json!({
            "mcpServers": {
                "insecure-mcp": {
                    "type": "http",
                    "url": "http://api.example.com/mcp"
                }
            }
        })
        .to_string()
        .into_bytes()
    }

    /// Create an MCP JSON with unknown HTTP field.
    fn unknown_http_field_mcp_json() -> Vec<u8> {
        serde_json::json!({
            "mcpServers": {
                "bad-http": {
                    "type": "http",
                    "url": "https://api.example.com/mcp",
                    "headers": {"Authorization": "Bearer xyz"}
                }
            }
        })
        .to_string()
        .into_bytes()
    }

    /// Create an MCP JSON with extra top-level key.
    fn extra_toplevel_key_mcp_json() -> Vec<u8> {
        serde_json::json!({
            "mcpServers": {
                "test-mcp": {
                    "command": "node",
                    "args": []
                }
            },
            "extraField": "should fail"
        })
        .to_string()
        .into_bytes()
    }

    /// Base64 encode for import requests.
    fn b64(data: &[u8]) -> String {
        BASE64.encode(data)
    }

    #[tokio::test]
    async fn member_can_list_empty_library() {
        let harness = h::OrgHarness::new().await;
        let items = list(harness.state(), harness.member_context())
            .await
            .unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn admin_can_import_skill_zip() {
        let harness = h::OrgHarness::new().await;
        let zip_data = single_skill_zip("Test Skill", "A test skill description");
        let request = ImportRequest {
            file_name: "skill.zip".to_string(),
            data: b64(&zip_data),
        };
        let items = import(harness.state(), harness.owner_context(), request)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Test Skill");
        assert_eq!(items[0].kind, "skill");
        assert!(!items[0].enabled); // Default disabled
        assert_eq!(items[0].revision, 1);
    }

    #[tokio::test]
    async fn admin_can_import_root_skill_with_references() {
        let harness = h::OrgHarness::new().await;
        let zip_data = root_skill_with_references_zip();
        let request = ImportRequest {
            file_name: "skill.zip".to_string(),
            data: b64(&zip_data),
        };
        let items = import(harness.state(), harness.owner_context(), request)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Root Skill");
        // Should have both SKILL.md and references/tone.md
        assert_eq!(items[0].files.len(), 2);
        assert!(items[0].files.contains(&"SKILL.md".to_string()));
        assert!(items[0].files.contains(&"references/tone.md".to_string()));
    }

    #[tokio::test]
    async fn admin_can_import_wrapper_folder_multi_skill() {
        let harness = h::OrgHarness::new().await;
        let zip_data = wrapper_folder_multi_skill_zip();
        let request = ImportRequest {
            file_name: "skills.zip".to_string(),
            data: b64(&zip_data),
        };
        let items = import(harness.state(), harness.owner_context(), request)
            .await
            .unwrap();
        assert_eq!(items.len(), 2);
        let names: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"Skill One"));
        assert!(names.contains(&"Skill Two"));
    }

    #[tokio::test]
    async fn admin_can_import_skill_md() {
        let harness = h::OrgHarness::new().await;
        let content = skill_md_content("Direct Skill", "Imported directly");
        let request = ImportRequest {
            file_name: "my-skill.md".to_string(),
            data: b64(content.as_bytes()),
        };
        let items = import(harness.state(), harness.owner_context(), request)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Direct Skill");
        assert_eq!(items[0].files, vec!["SKILL.md".to_string()]);
    }

    #[tokio::test]
    async fn admin_can_import_multi_skill_zip() {
        let harness = h::OrgHarness::new().await;
        let zip_data = multi_skill_zip();
        let request = ImportRequest {
            file_name: "skills.zip".to_string(),
            data: b64(&zip_data),
        };
        let items = import(harness.state(), harness.owner_context(), request)
            .await
            .unwrap();
        assert_eq!(items.len(), 2);
        let names: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"Skill One"));
        assert!(names.contains(&"Skill Two"));
    }

    #[tokio::test]
    async fn admin_can_import_valid_mcp_json() {
        let harness = h::OrgHarness::new().await;
        let json_data = valid_mcp_json();
        let request = ImportRequest {
            file_name: "mcp.json".to_string(),
            data: b64(&json_data),
        };
        let items = import(harness.state(), harness.owner_context(), request)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "test-mcp");
        assert_eq!(items[0].kind, "mcp");
        // stdio has setup note about command requirement
        assert!(items[0].setup_note.is_some());
    }

    #[tokio::test]
    async fn admin_can_import_explicit_stdio_mcp() {
        let harness = h::OrgHarness::new().await;
        let json_data = explicit_stdio_mcp_json();
        let request = ImportRequest {
            file_name: "mcp.json".to_string(),
            data: b64(&json_data),
        };
        let items = import(harness.state(), harness.owner_context(), request)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "explicit-stdio");
        assert_eq!(items[0].kind, "mcp");
    }

    #[tokio::test]
    async fn https_mcp_gets_setup_note() {
        let harness = h::OrgHarness::new().await;
        let json_data = https_mcp_json();
        let request = ImportRequest {
            file_name: "remote-mcp.json".to_string(),
            data: b64(&json_data),
        };
        let items = import(harness.state(), harness.owner_context(), request)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].setup_note.is_some());
        assert!(items[0].setup_note.as_ref().unwrap().contains("allowlist"));
    }

    #[tokio::test]
    async fn mcp_with_env_field_rejected() {
        let harness = h::OrgHarness::new().await;
        let json_data = env_mcp_json();
        let request = ImportRequest {
            file_name: "env-mcp.json".to_string(),
            data: b64(&json_data),
        };
        let result = import(harness.state(), harness.owner_context(), request).await;
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[tokio::test]
    async fn mcp_with_credential_args_rejected() {
        let harness = h::OrgHarness::new().await;
        let json_data = credential_args_mcp_json();
        let request = ImportRequest {
            file_name: "cred-mcp.json".to_string(),
            data: b64(&json_data),
        };
        let result = import(harness.state(), harness.owner_context(), request).await;
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[tokio::test]
    async fn mcp_with_malformed_args_rejected() {
        let harness = h::OrgHarness::new().await;
        let json_data = malformed_args_mcp_json();
        let request = ImportRequest {
            file_name: "bad-args.json".to_string(),
            data: b64(&json_data),
        };
        let result = import(harness.state(), harness.owner_context(), request).await;
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[tokio::test]
    async fn mcp_with_http_url_rejected() {
        let harness = h::OrgHarness::new().await;
        let json_data = http_mcp_json();
        let request = ImportRequest {
            file_name: "http-mcp.json".to_string(),
            data: b64(&json_data),
        };
        let result = import(harness.state(), harness.owner_context(), request).await;
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[tokio::test]
    async fn mcp_with_unknown_http_field_rejected() {
        let harness = h::OrgHarness::new().await;
        let json_data = unknown_http_field_mcp_json();
        let request = ImportRequest {
            file_name: "bad-http.json".to_string(),
            data: b64(&json_data),
        };
        let result = import(harness.state(), harness.owner_context(), request).await;
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[tokio::test]
    async fn mcp_with_extra_toplevel_key_rejected() {
        let harness = h::OrgHarness::new().await;
        let json_data = extra_toplevel_key_mcp_json();
        let request = ImportRequest {
            file_name: "extra-key.json".to_string(),
            data: b64(&json_data),
        };
        let result = import(harness.state(), harness.owner_context(), request).await;
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[tokio::test]
    async fn member_cannot_import() {
        let harness = h::OrgHarness::new().await;
        let request = ImportRequest {
            file_name: "skill.md".to_string(),
            data: b64(b"# Test\n\nDesc"),
        };
        let result = import(harness.state(), harness.member_context(), request).await;
        assert_eq!(result, Err(AuthError::Forbidden));
    }

    #[tokio::test]
    async fn admin_can_enable_item() {
        let harness = h::OrgHarness::new().await;

        // Import a skill
        let import_req = ImportRequest {
            file_name: "skill.md".to_string(),
            data: b64(skill_md_content("Enable Test", "Test").as_bytes()),
        };
        let items = import(harness.state(), harness.owner_context(), import_req)
            .await
            .unwrap();
        assert!(!items[0].enabled);
        let item_id = items[0].id.clone();

        // Enable it
        let patch_req = PatchRequest {
            expected_revision: 1,
            enabled: true,
        };
        let updated = patch(
            harness.state(),
            harness.owner_context(),
            &item_id,
            patch_req,
        )
        .await
        .unwrap();
        assert!(updated.enabled);
        assert_eq!(updated.revision, 2);
    }

    #[tokio::test]
    async fn stale_revision_rejected() {
        let harness = h::OrgHarness::new().await;

        // Import a skill
        let import_req = ImportRequest {
            file_name: "skill.md".to_string(),
            data: b64(skill_md_content("Stale Test", "Test").as_bytes()),
        };
        let items = import(harness.state(), harness.owner_context(), import_req)
            .await
            .unwrap();
        let item_id = items[0].id.clone();

        // Enable it first
        let patch_req = PatchRequest {
            expected_revision: 1,
            enabled: true,
        };
        patch(
            harness.state(),
            harness.owner_context(),
            &item_id,
            patch_req,
        )
        .await
        .unwrap();

        // Try to use old revision - should get Conflict
        let stale_req = PatchRequest {
            expected_revision: 1, // Stale!
            enabled: false,
        };
        let result = patch(
            harness.state(),
            harness.owner_context(),
            &item_id,
            stale_req,
        )
        .await;
        assert_eq!(result, Err(AuthError::Conflict));
    }

    #[tokio::test]
    async fn member_cannot_enable() {
        let harness = h::OrgHarness::new().await;

        // Admin imports a skill
        let import_req = ImportRequest {
            file_name: "skill.md".to_string(),
            data: b64(skill_md_content("Member Test", "Test").as_bytes()),
        };
        let items = import(harness.state(), harness.owner_context(), import_req)
            .await
            .unwrap();
        let item_id = items[0].id.clone();

        // Member tries to enable
        let patch_req = PatchRequest {
            expected_revision: 1,
            enabled: true,
        };
        let result = patch(
            harness.state(),
            harness.member_context(),
            &item_id,
            patch_req,
        )
        .await;
        assert_eq!(result, Err(AuthError::Forbidden));
    }

    #[tokio::test]
    async fn admin_can_delete_item() {
        let harness = h::OrgHarness::new().await;

        // Import a skill
        let import_req = ImportRequest {
            file_name: "skill.md".to_string(),
            data: b64(skill_md_content("Delete Test", "Test").as_bytes()),
        };
        let items = import(harness.state(), harness.owner_context(), import_req)
            .await
            .unwrap();
        let item_id = items[0].id.clone();

        // Delete it
        let delete_req = DeleteRequest {
            expected_revision: 1,
        };
        let result = delete(
            harness.state(),
            harness.owner_context(),
            &item_id,
            delete_req,
        )
        .await
        .unwrap();
        assert!(result.ok);

        // Verify it's gone
        let items = list(harness.state(), harness.owner_context())
            .await
            .unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn member_cannot_delete() {
        let harness = h::OrgHarness::new().await;

        // Admin imports a skill
        let import_req = ImportRequest {
            file_name: "skill.md".to_string(),
            data: b64(skill_md_content("No Delete", "Test").as_bytes()),
        };
        let items = import(harness.state(), harness.owner_context(), import_req)
            .await
            .unwrap();
        let item_id = items[0].id.clone();

        // Member tries to delete
        let delete_req = DeleteRequest {
            expected_revision: 1,
        };
        let result = delete(
            harness.state(),
            harness.member_context(),
            &item_id,
            delete_req,
        )
        .await;
        assert_eq!(result, Err(AuthError::Forbidden));
    }

    #[tokio::test]
    async fn cross_org_access_denied() {
        let harness = h::OrgHarness::new().await;

        // Admin imports a skill
        let import_req = ImportRequest {
            file_name: "skill.md".to_string(),
            data: b64(skill_md_content("Org Test", "Test").as_bytes()),
        };
        let items = import(harness.state(), harness.owner_context(), import_req)
            .await
            .unwrap();
        let item_id = items[0].id.clone();

        // Other org admin tries to list - should see empty
        let other_org = harness.other_org_context();
        let items = list(harness.state(), other_org).await.unwrap();
        assert!(items.is_empty());

        // Other org admin tries to patch - should get Forbidden
        let patch_req = PatchRequest {
            expected_revision: 1,
            enabled: true,
        };
        let result = patch(harness.state(), other_org, &item_id, patch_req).await;
        assert_eq!(result, Err(AuthError::Forbidden));
    }

    #[tokio::test]
    async fn load_enabled_returns_only_enabled() {
        let harness = h::OrgHarness::new().await;

        // Import two skills
        let import1 = ImportRequest {
            file_name: "skill1.md".to_string(),
            data: b64(skill_md_content("Skill 1", "First").as_bytes()),
        };
        let items1 = import(harness.state(), harness.owner_context(), import1)
            .await
            .unwrap();

        let import2 = ImportRequest {
            file_name: "skill2.md".to_string(),
            data: b64(skill_md_content("Skill 2", "Second").as_bytes()),
        };
        import(harness.state(), harness.owner_context(), import2)
            .await
            .unwrap();

        // Enable only the first one
        let patch_req = PatchRequest {
            expected_revision: 1,
            enabled: true,
        };
        patch(
            harness.state(),
            harness.owner_context(),
            &items1[0].id,
            patch_req,
        )
        .await
        .unwrap();

        // Load enabled
        let enabled = load_enabled(harness.state(), harness.owner_context())
            .await
            .unwrap();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "Skill 1");
        assert!(!enabled[0].files.is_empty());
    }

    #[tokio::test]
    async fn zip_path_traversal_rejected() {
        // Create a ZIP with path traversal attempt
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = SimpleFileOptions::default();
            zip.start_file("../../../etc/passwd", options).unwrap();
            zip.write_all(b"malicious content").unwrap();
            zip.finish().unwrap();
        }
        let zip_data = buf.into_inner();

        let harness = h::OrgHarness::new().await;
        let request = ImportRequest {
            file_name: "evil.zip".to_string(),
            data: b64(&zip_data),
        };
        let result = import(harness.state(), harness.owner_context(), request).await;
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[tokio::test]
    async fn zip_empty_component_rejected() {
        // Create a ZIP with empty path component (foo//bar.md)
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = SimpleFileOptions::default();
            zip.start_file("foo//bar.md", options).unwrap();
            zip.write_all(b"content").unwrap();
            zip.finish().unwrap();
        }
        let zip_data = buf.into_inner();

        let harness = h::OrgHarness::new().await;
        let request = ImportRequest {
            file_name: "empty-component.zip".to_string(),
            data: b64(&zip_data),
        };
        let result = import(harness.state(), harness.owner_context(), request).await;
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[tokio::test]
    async fn zip_orphan_file_rejected() {
        // Create a ZIP with a file that has no associated SKILL.md
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = SimpleFileOptions::default();

            // Skill in skill1/
            zip.start_file("skill1/SKILL.md", options).unwrap();
            zip.write_all(skill_md_content("Skill One", "Test").as_bytes())
                .unwrap();

            // Orphan file in orphan/ (no SKILL.md there)
            zip.start_file("orphan/lonely.txt", options).unwrap();
            zip.write_all(b"I have no skill").unwrap();

            zip.finish().unwrap();
        }
        let zip_data = buf.into_inner();

        let harness = h::OrgHarness::new().await;
        let request = ImportRequest {
            file_name: "orphan.zip".to_string(),
            data: b64(&zip_data),
        };
        let result = import(harness.state(), harness.owner_context(), request).await;
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[tokio::test]
    async fn invalid_base64_rejected() {
        let harness = h::OrgHarness::new().await;
        let request = ImportRequest {
            file_name: "skill.md".to_string(),
            data: "not-valid-base64!!!".to_string(),
        };
        let result = import(harness.state(), harness.owner_context(), request).await;
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[tokio::test]
    async fn unsupported_file_type_rejected() {
        let harness = h::OrgHarness::new().await;
        let request = ImportRequest {
            file_name: "file.txt".to_string(),
            data: b64(b"hello"),
        };
        let result = import(harness.state(), harness.owner_context(), request).await;
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[tokio::test]
    async fn mcp_without_mcpservers_rejected() {
        let harness = h::OrgHarness::new().await;
        // Missing mcpServers key
        let json_data = serde_json::json!({
            "name": "Bad Config",
            "command": "node"
        })
        .to_string()
        .into_bytes();
        let request = ImportRequest {
            file_name: "bad.json".to_string(),
            data: b64(&json_data),
        };
        let result = import(harness.state(), harness.owner_context(), request).await;
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[tokio::test]
    async fn mcp_name_with_special_chars_rejected() {
        let harness = h::OrgHarness::new().await;
        let json_data = serde_json::json!({
            "mcpServers": {
                "bad/name": {
                    "command": "node",
                    "args": []
                }
            }
        })
        .to_string()
        .into_bytes();
        let request = ImportRequest {
            file_name: "bad-name.json".to_string(),
            data: b64(&json_data),
        };
        let result = import(harness.state(), harness.owner_context(), request).await;
        assert_eq!(result, Err(AuthError::Invalid));
    }

    #[test]
    fn credential_pattern_detection() {
        // Test the contains_credential_pattern function
        assert!(contains_credential_pattern("sk-1234567890"));
        assert!(contains_credential_pattern("--api_key=xyz"));
        assert!(contains_credential_pattern("bearer token123"));
        assert!(contains_credential_pattern("AKIAIOSFODNN7EXAMPLE"));
        assert!(contains_credential_pattern("--password=secret"));

        // These should not match
        assert!(!contains_credential_pattern("node"));
        assert!(!contains_credential_pattern("server.js"));
        assert!(!contains_credential_pattern("--port=3000"));
    }

    #[test]
    fn url_validation() {
        // Valid HTTPS URL
        assert!(validate_mcp_url("https://api.example.com/mcp").is_ok());

        // HTTP not allowed
        assert_eq!(
            validate_mcp_url("http://api.example.com/mcp"),
            Err(AuthError::Invalid)
        );

        // URL with credentials not allowed
        assert_eq!(
            validate_mcp_url("https://user:pass@api.example.com/mcp"),
            Err(AuthError::Invalid)
        );

        // URL with query not allowed
        assert_eq!(
            validate_mcp_url("https://api.example.com/mcp?token=123"),
            Err(AuthError::Invalid)
        );

        // Localhost not allowed
        assert_eq!(
            validate_mcp_url("https://localhost/mcp"),
            Err(AuthError::Invalid)
        );

        // Private IP not allowed
        assert_eq!(
            validate_mcp_url("https://192.168.1.1/mcp"),
            Err(AuthError::Invalid)
        );
    }

    #[test]
    fn path_validation() {
        // Valid paths
        assert!(validate_zip_path("SKILL.md").is_some());
        assert!(validate_zip_path("skill1/SKILL.md").is_some());
        assert!(validate_zip_path("myskills/skill1/helper.py").is_some());

        // Invalid paths
        assert!(validate_zip_path("../etc/passwd").is_none());
        assert!(validate_zip_path("/absolute/path").is_none());
        assert!(validate_zip_path("foo//bar").is_none()); // empty component
        assert!(validate_zip_path("foo/./bar").is_none()); // . component
        assert!(validate_zip_path(".hidden").is_none());
        assert!(validate_zip_path("C:file").is_none()); // Windows drive
        assert!(validate_zip_path("foo:bar").is_none()); // colon
    }

    #[test]
    fn mcp_name_validation() {
        // Valid names
        assert!(validate_mcp_name("test-mcp"));
        assert!(validate_mcp_name("my_server"));
        assert!(validate_mcp_name("server123"));

        // Invalid names
        assert!(!validate_mcp_name("")); // empty
        assert!(!validate_mcp_name("bad/name")); // slash
        assert!(!validate_mcp_name("bad name")); // space
        assert!(!validate_mcp_name("bad.name")); // dot
    }
    #[tokio::test]
    async fn library_revision_wire_contract_is_a_string() {
        let harness = h::OrgHarness::new().await;
        let items = import(
            harness.state(),
            harness.owner_context(),
            ImportRequest {
                file_name: "SKILL.md".into(),
                data: b64(b"# Team writing\nUse clear prose."),
            },
        )
        .await
        .unwrap();
        let wire = serde_json::to_value(&items[0]).unwrap();
        assert_eq!(wire["revision"], "1");
    }

    #[tokio::test]
    async fn removed_member_cannot_read_or_load_enabled_library() {
        let harness = h::OrgHarness::new().await;
        let member = harness.member_context().clone();
        crate::neev::org::remove(harness.state(), harness.owner_context(), member.member_id)
            .await
            .unwrap();
        assert!(list(harness.state(), &member).await.is_err());
        assert!(load_enabled(harness.state(), &member).await.is_err());
    }
}
