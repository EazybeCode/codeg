//! Shared organization library: skills and MCP configurations.
//!
//! Org admins can import skills (SKILL.md + supporting files) and MCP JSON
//! configurations. All members can read; enabled items reach agent runs.
//!
//! ACL: admin writes, member reads. No secrets in listings. Stale membership
//! checks on every operation via proper transaction pattern.

use crate::neev::{
    agents::execution::ExecutionError,
    auth::session,
    db,
    error::{database_error, AuthError},
    integrations::{
        BrokeredIntegration, IntegrationCheckRequest, IntegrationSecrets, IntegrationSpec,
        IntegrationStatus, Transport, SECRETS_PATH, SPEC_PATH, STATUS_PATH,
    },
    library_integrations as vault,
    startup::ServerState,
    types::{OrgContext, Role},
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
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
    /// Present for shared (brokered) integrations: names, hosts and the last
    /// test only — never a secret value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) integration: Option<IntegrationView>,
}

/// What members see of a shared integration (names only, never values).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IntegrationView {
    pub(crate) transport: Transport,
    pub(crate) hosts: Vec<String>,
    /// `secret_env` names, then `secret_headers` names.
    pub(crate) secret_names: Vec<String>,
    pub(crate) source: String,
    pub(crate) notes: Vec<String>,
    /// The last admin test, only when it ran against the current revision.
    pub(crate) status: Option<IntegrationStatus>,
}

/// An MCP server the import did not store, and why (a code, never a value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SkippedServer {
    pub(crate) name: String,
    pub(crate) reason: String,
}

/// Result of an import: newly stored items plus per-server skips.
#[derive(Debug)]
pub(crate) struct ImportOutcome {
    pub(crate) items: Vec<LibraryItem>,
    pub(crate) skipped: Vec<SkippedServer>,
}

/// Write-only replacement of some or all of an integration's credentials.
pub(crate) struct SecretsRequest {
    pub(crate) expected_revision: i64,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) headers: BTreeMap<String, String>,
}

impl std::fmt::Debug for SecretsRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretsRequest")
            .field("expected_revision", &self.expected_revision)
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}

// JavaScript cannot represent all i64 revisions exactly. Keep the native API's
// string revision contract, while database operations use checked integers.
fn serialize_revision<S: serde::Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}

/// Request to import a skill or MCP config.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportRequest {
    pub(crate) file_name: String,
    pub(crate) data: String, // base64
}

// The payload is an MCP export with plaintext tokens: never print it.
impl std::fmt::Debug for ImportRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportRequest")
            .field("file_name", &self.file_name)
            .field("data", &format_args!("<{} base64 bytes>", self.data.len()))
            .finish()
    }
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
                "SELECT path FROM neev_library_files WHERE item_id=$1 AND org_id=$2 AND path<>$3 ORDER BY path",
                vec![id.clone().into(), org_id.into(), SECRETS_PATH.into()],
            ))
            .await
            .map_err(database_error)?;
        let files: Vec<String> = file_rows
            .iter()
            .map(|r| r.try_get("", "path").map_err(database_error))
            .collect::<Result<_, _>>()?;

        let integration = if kind == "mcp" && files.iter().any(|f| f == SPEC_PATH) {
            integration_view(db, &id, org_id, revision).await?
        } else {
            None
        };

        items.push(LibraryItem {
            id,
            name,
            description,
            kind,
            enabled,
            revision,
            files,
            setup_note,
            integration,
        });
    }
    Ok(items)
}

/// The member-visible view of an integration item (None if its spec is unreadable).
async fn integration_view<C: ConnectionTrait>(
    db: &C,
    item_id: &str,
    org_id: i32,
    revision: i64,
) -> Result<Option<IntegrationView>, AuthError> {
    let Some(spec) = read_spec(db, item_id, org_id).await?.and_then(Result::ok) else {
        return Ok(None);
    };
    let status = read_file(db, item_id, org_id, STATUS_PATH)
        .await?
        .and_then(|bytes| serde_json::from_slice::<IntegrationStatus>(&bytes).ok())
        .filter(|status| status.revision == revision.to_string());
    let mut secret_names = spec.secret_env.clone();
    secret_names.extend(spec.secret_headers.iter().cloned());
    Ok(Some(IntegrationView {
        transport: spec.transport,
        hosts: spec.hosts,
        secret_names,
        source: spec.source,
        notes: spec.notes,
        status,
    }))
}

/// Read one stored file of an item (org-scoped).
async fn read_file<C: ConnectionTrait>(
    db: &C,
    item_id: &str,
    org_id: i32,
    path: &str,
) -> Result<Option<Vec<u8>>, AuthError> {
    db::one(
        db,
        "SELECT content FROM neev_library_files WHERE item_id=$1 AND org_id=$2 AND path=$3",
        vec![item_id.into(), org_id.into(), path.into()],
    )
    .await?
    .map(|row| row.try_get::<Vec<u8>>("", "content").map_err(database_error))
    .transpose()
}

/// `Some(Err(()))` when `integration.json` exists but does not parse.
async fn read_spec<C: ConnectionTrait>(
    db: &C,
    item_id: &str,
    org_id: i32,
) -> Result<Option<Result<IntegrationSpec, ()>>, AuthError> {
    Ok(read_file(db, item_id, org_id, SPEC_PATH)
        .await?
        .map(|bytes| parse_spec(&bytes)))
}

fn parse_spec(bytes: &[u8]) -> Result<IntegrationSpec, ()> {
    serde_json::from_slice::<IntegrationSpec>(bytes)
        .ok()
        .filter(|spec| spec.v == crate::neev::integrations::SPEC_VERSION)
        .ok_or(())
}

async fn insert_file<C: ConnectionTrait>(
    db: &C,
    item_id: &str,
    org_id: i32,
    path: &str,
    content: Vec<u8>,
) -> Result<(), AuthError> {
    let hash = format!("{:x}", Sha256::digest(&content));
    db::execute(
        db,
        "INSERT INTO neev_library_files (id, item_id, org_id, path, content_hash, content)
         VALUES ($1, $2, $3, $4, $5, $6)",
        vec![
            uuid::Uuid::new_v4().to_string().into(),
            item_id.into(),
            org_id.into(),
            path.into(),
            hash.into(),
            content.into(),
        ],
    )
    .await?;
    Ok(())
}

async fn delete_file<C: ConnectionTrait>(
    db: &C,
    item_id: &str,
    org_id: i32,
    path: &str,
) -> Result<(), AuthError> {
    db::execute(
        db,
        "DELETE FROM neev_library_files WHERE item_id=$1 AND org_id=$2 AND path=$3",
        vec![item_id.into(), org_id.into(), path.into()],
    )
    .await?;
    Ok(())
}

/// Replace (delete + insert) one file of an item.
async fn upsert_file<C: ConnectionTrait>(
    db: &C,
    item_id: &str,
    org_id: i32,
    path: &str,
    content: Vec<u8>,
) -> Result<(), AuthError> {
    delete_file(db, item_id, org_id, path).await?;
    insert_file(db, item_id, org_id, path, content).await
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
///
/// An MCP file is imported per server: credential-free stdio servers become
/// legacy items, authenticated ones become shared integrations (secrets
/// sealed), and anything unsupported is reported in `skipped` (never a value).
pub(crate) async fn import(
    state: &ServerState,
    actor: &OrgContext,
    request: ImportRequest,
) -> Result<ImportOutcome, AuthError> {
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
    let (skills, parsed) = if file_name.ends_with(".zip") {
        (parse_skill_zip(&data)?, vault::ParsedImport::default())
    } else if file_name.ends_with(".json") {
        (Vec::new(), parse_mcp_json(&data)?)
    } else if file_name.ends_with(".md") {
        (
            vec![parse_skill_md(&request.file_name, &data)?],
            vault::ParsedImport::default(),
        )
    } else {
        return Err(AuthError::Invalid);
    };
    let vault::ParsedImport {
        servers: mut mcp_servers,
        mut skipped,
    } = parsed;

    if skills.is_empty() && mcp_servers.is_empty() && skipped.is_empty() {
        return Err(AuthError::Invalid);
    }

    // Begin transaction with ACL checks
    let tx = begin(state, actor).await?;

    // MCP names are unique per org (case-insensitive): they become tool names.
    if !mcp_servers.is_empty() {
        let rows = tx
            .query_all(db::statement(
                &tx,
                "SELECT name FROM neev_library_items WHERE org_id=$1 AND kind='mcp'",
                vec![actor.org_id.into()],
            ))
            .await
            .map_err(database_error)?;
        let existing: HashSet<String> = rows
            .iter()
            .map(|r| r.try_get::<String>("", "name").map(|n| n.to_lowercase()))
            .collect::<Result<_, _>>()
            .map_err(database_error)?;
        mcp_servers.retain(|server| {
            if existing.contains(&server.key()) {
                skipped.push(SkippedServer {
                    name: server.display_name().to_string(),
                    reason: "duplicate_name".into(),
                });
                false
            } else {
                true
            }
        });
    }

    let total_items = skills.len() + mcp_servers.len();
    if total_items == 0 {
        // Nothing importable; the caller decides how to present the skips.
        session::recheck(&tx, actor, state.clock.now_unix()).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(ImportOutcome {
            items: Vec::new(),
            skipped,
        });
    }

    // Calculate total bytes to store
    let mut new_bytes: i64 = 0;
    for skill in &skills {
        for file in &skill.files {
            new_bytes += file.content.len() as i64;
        }
    }
    for server in &mcp_servers {
        new_bytes += match server {
            vault::ParsedServer::Legacy(server) => serde_json::to_vec(server)
                .map(|v| v.len() as i64)
                .unwrap_or(1000),
            vault::ParsedServer::Integration(parsed) => {
                let legacy = serde_json::to_vec_pretty(&parsed.legacy).map_or(1000, |v| v.len());
                let spec = serde_json::to_vec(&parsed.spec).map_or(1000, |v| v.len());
                let secrets = serde_json::to_vec(&parsed.secrets).map_or(1000, |v| v.len());
                (legacy + spec + secrets + 31) as i64
            }
        };
    }

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

        for file in skill.files {
            insert_file(&tx, &id, actor.org_id, &file.path, file.content).await?;
        }

        stored_ids.push(id);
    }

    // Store MCP servers (each as separate item)
    let vault_key = vault::vault_key(&state.config.encryption_key);
    for server in mcp_servers {
        let id = uuid::Uuid::new_v4().to_string();
        let (name, setup_note, legacy) = match &server {
            vault::ParsedServer::Legacy(server) => {
                let note = match &server.config {
                    McpServerConfig::Http { url } => format!(
                        "This MCP server connects to {}. Ensure destination is on your network allowlist.",
                        url
                    ),
                    McpServerConfig::Stdio { command, .. } => format!(
                        "This MCP server requires '{}' to be installed and in PATH.",
                        command
                    ),
                };
                (server.name.clone(), note, server)
            }
            vault::ParsedServer::Integration(parsed) => (
                parsed.spec.key.clone(),
                vault::SETUP_NOTE.to_string(),
                &parsed.legacy,
            ),
        };

        db::execute(
            &tx,
            "INSERT INTO neev_library_items (id, org_id, kind, name, description, enabled, creator_id, revision, setup_note)
             VALUES ($1, $2, 'mcp', $3, '', false, $4, 1, $5)",
            vec![
                id.clone().into(),
                actor.org_id.into(),
                name.into(),
                actor.identity.user_id.into(),
                Some(setup_note).into(),
            ],
        )
        .await?;

        // Legacy-compatible, credential-free config
        let config_json = serde_json::to_vec_pretty(legacy).map_err(|_| AuthError::Invalid)?;
        insert_file(&tx, &id, actor.org_id, "mcp.json", config_json).await?;

        if let vault::ParsedServer::Integration(parsed) = &server {
            let spec = serde_json::to_vec(&parsed.spec).map_err(|_| AuthError::Unavailable)?;
            insert_file(&tx, &id, actor.org_id, SPEC_PATH, spec).await?;
            let sealed = vault::seal(&vault_key, actor.org_id, &id, &parsed.secrets)
                .map_err(|_| AuthError::Unavailable)?;
            insert_file(&tx, &id, actor.org_id, SECRETS_PATH, sealed).await?;
        }

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
    Ok(ImportOutcome {
        items: all_items
            .into_iter()
            .filter(|item| stored_set.contains(&item.id))
            .collect(),
        skipped,
    })
}

/// Load an org's integration item inside `tx`: (revision, spec). Not found →
/// Forbidden (cross-org), stale → Conflict, not an integration → Invalid.
async fn load_integration_item<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    item_id: &str,
    expected_revision: i64,
) -> Result<IntegrationSpec, AuthError> {
    let row = db::one(
        db,
        "SELECT revision, kind FROM neev_library_items WHERE id=$1 AND org_id=$2",
        vec![item_id.into(), org_id.into()],
    )
    .await?
    .ok_or(AuthError::Forbidden)?;
    let current_rev: i64 = row.try_get("", "revision").map_err(database_error)?;
    let kind: String = row.try_get("", "kind").map_err(database_error)?;
    if current_rev != expected_revision {
        return Err(AuthError::Conflict);
    }
    if kind != "mcp" {
        return Err(AuthError::Invalid);
    }
    read_spec(db, item_id, org_id)
        .await?
        .ok_or(AuthError::Invalid)?
        .map_err(|()| AuthError::Invalid)
}

/// Open an item's sealed secrets; any failure is `None` (vault unavailable).
async fn open_secrets<C: ConnectionTrait>(
    db: &C,
    key: &[u8; 32],
    org_id: i32,
    item_id: &str,
) -> Result<Option<IntegrationSecrets>, AuthError> {
    Ok(read_file(db, item_id, org_id, SECRETS_PATH)
        .await?
        .and_then(|sealed| vault::open(key, org_id, item_id, &sealed).ok()))
}

/// Replace some or all credentials of a shared integration. Admin only.
/// Write-only: values are never returned. Bumps the revision (hiding any
/// earlier test result).
pub(crate) async fn update_secrets(
    state: &ServerState,
    actor: &OrgContext,
    item_id: &str,
    request: SecretsRequest,
) -> Result<LibraryItem, AuthError> {
    if actor.role != Role::Admin {
        return Err(AuthError::Forbidden);
    }
    if request.expected_revision < 1 || (request.env.is_empty() && request.headers.is_empty()) {
        return Err(AuthError::Invalid);
    }
    if request
        .env
        .values()
        .chain(request.headers.values())
        .any(|value| vault::check_secret_value(value).is_err())
    {
        return Err(AuthError::Invalid);
    }
    let key = vault::vault_key(&state.config.encryption_key);

    let tx = begin(state, actor).await?;
    let spec = load_integration_item(&tx, actor.org_id, item_id, request.expected_revision).await?;
    if !request.env.keys().all(|name| spec.secret_env.contains(name))
        || !request
            .headers
            .keys()
            .all(|name| spec.secret_headers.contains(name))
    {
        return Err(AuthError::Invalid);
    }
    let mut secrets = match open_secrets(&tx, &key, actor.org_id, item_id).await? {
        Some(existing) => existing,
        None => {
            // Nothing to merge into: every declared secret must be provided.
            if !spec.secret_env.iter().all(|n| request.env.contains_key(n))
                || !spec
                    .secret_headers
                    .iter()
                    .all(|n| request.headers.contains_key(n))
            {
                return Err(AuthError::Invalid);
            }
            IntegrationSecrets::default()
        }
    };
    secrets.env.extend(request.env);
    secrets.headers.extend(request.headers);
    // Keep only declared names (a stale extra name is never carried forward).
    secrets.env.retain(|name, _| spec.secret_env.contains(name));
    secrets.headers.retain(|name, _| spec.secret_headers.contains(name));
    let sealed = vault::seal(&key, actor.org_id, item_id, &secrets)
        .map_err(|_| AuthError::Unavailable)?;
    upsert_file(&tx, item_id, actor.org_id, SECRETS_PATH, sealed).await?;
    delete_file(&tx, item_id, actor.org_id, STATUS_PATH).await?;
    db::execute(
        &tx,
        "UPDATE neev_library_items SET revision=revision+1, updated_at=CURRENT_TIMESTAMP WHERE id=$1 AND org_id=$2",
        vec![item_id.into(), actor.org_id.into()],
    )
    .await?;

    let updated = list_internal(&tx, actor.org_id)
        .await?
        .into_iter()
        .find(|i| i.id == item_id)
        .ok_or(AuthError::Unavailable)?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(updated)
}

/// Test a shared integration through the runtime and record the result in
/// `status.json`. Admin only. No transaction is held during the test; a
/// result for a revision that changed meanwhile is dropped (Conflict).
pub(crate) async fn test_integration(
    state: &ServerState,
    actor: &OrgContext,
    item_id: &str,
    expected_revision: i64,
) -> Result<LibraryItem, AuthError> {
    if actor.role != Role::Admin {
        return Err(AuthError::Forbidden);
    }
    if expected_revision < 1 {
        return Err(AuthError::Invalid);
    }
    let key = vault::vault_key(&state.config.encryption_key);

    // tx #1: authorize and read.
    let tx = begin(state, actor).await?;
    let spec = load_integration_item(&tx, actor.org_id, item_id, expected_revision).await?;
    let secrets = open_secrets(&tx, &key, actor.org_id, item_id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;

    // The test itself, outside any transaction.
    let integration_key = spec.key.clone();
    let (check_state, tool_count, http_status) = match secrets {
        None => ("vault_unavailable".to_string(), None, None),
        Some(secrets) => match state
            .agents
            .check_integrations(IntegrationCheckRequest {
                user_id: actor.identity.user_id,
                org_id: actor.org_id,
                integrations: vec![BrokeredIntegration {
                    spec,
                    secrets: Some(secrets),
                }],
            })
            .await
        {
            // Keep the failures an admin can act on distinct: both lanes in
            // use ("try again shortly") and a runtime that is off here.
            Err(ExecutionError::LanesBusy) => ("busy".to_string(), None, None),
            Err(ExecutionError::Disabled | ExecutionError::Unsupported) => {
                ("runtime_disabled".to_string(), None, None)
            }
            Err(_) => ("runtime_unavailable".to_string(), None, None),
            Ok(results) => results
                .into_iter()
                .find(|r| r.key == integration_key)
                .map(|r| (r.state, r.tool_count, r.http_status))
                .unwrap_or_else(|| ("protocol_error".to_string(), None, None)),
        },
    };

    // tx #2: record, only if the item is unchanged.
    let tx = begin(state, actor).await?;
    let row = db::one(
        &tx,
        "SELECT revision FROM neev_library_items WHERE id=$1 AND org_id=$2",
        vec![item_id.into(), actor.org_id.into()],
    )
    .await?
    .ok_or(AuthError::Forbidden)?;
    let current_rev: i64 = row.try_get("", "revision").map_err(database_error)?;
    if current_rev != expected_revision {
        return Err(AuthError::Conflict);
    }
    let status = IntegrationStatus {
        state: check_state,
        checked_at: state.clock.now_unix().to_string(),
        tool_count,
        detail: http_status.map(|s| format!("HTTP {s}")),
        revision: current_rev.to_string(),
    };
    let status = serde_json::to_vec(&status).map_err(|_| AuthError::Unavailable)?;
    upsert_file(&tx, item_id, actor.org_id, STATUS_PATH, status).await?;
    let updated = list_internal(&tx, actor.org_id)
        .await?
        .into_iter()
        .find(|i| i.id == item_id)
        .ok_or(AuthError::Unavailable)?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(updated)
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

    // Enabling or disabling changes neither the credentials nor the config, so
    // a test result for the revision just replaced still describes the item:
    // carry it to the new revision instead of showing a tested integration as
    // "untested" the moment it is enabled. Credential updates still clear it.
    if let Some(mut status) = read_file(&tx, item_id, actor.org_id, STATUS_PATH)
        .await?
        .and_then(|bytes| serde_json::from_slice::<IntegrationStatus>(&bytes).ok())
    {
        if status.revision == current_rev.to_string() {
            status.revision = (current_rev + 1).to_string();
            let bytes = serde_json::to_vec(&status).map_err(|_| AuthError::Unavailable)?;
            upsert_file(&tx, item_id, actor.org_id, STATUS_PATH, bytes).await?;
        }
    }

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
    let key = vault::vault_key(&state.config.encryption_key);
    let items = load_enabled_internal(&tx, actor.org_id, &key).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(items)
}

/// Internal load without transaction management.
async fn load_enabled_internal<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    vault_key: &[u8; 32],
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
        let mut sealed: Option<Vec<u8>> = None;
        for fr in &file_rows {
            let path: String = fr.try_get("", "path").map_err(database_error)?;
            let content: Vec<u8> = fr.try_get("", "content").map_err(database_error)?;
            let stored_hash: String = fr.try_get("", "content_hash").map_err(database_error)?;
            let intact = format!("{:x}", Sha256::digest(&content)) == stored_hash;
            if path == SECRETS_PATH {
                // Never handed out as a file. A damaged blob only makes this
                // integration unavailable (the vault rejects it below).
                if intact {
                    sealed = Some(content);
                }
                continue;
            }
            if !intact {
                return Err(AuthError::Unavailable);
            }
            files.push(EnabledFile { path, content });
        }

        let integration = match files.iter().find(|f| f.path == SPEC_PATH) {
            Some(spec_file) if kind == "mcp" => {
                // One damaged spec makes THAT integration unavailable, never the
                // whole library (and never stages it as a legacy item).
                let Ok(spec) = parse_spec(&spec_file.content) else {
                    continue;
                };
                let secrets = sealed
                    .as_deref()
                    .and_then(|bytes| vault::open(vault_key, org_id, &id, bytes).ok());
                Some(BrokeredIntegration { spec, secrets })
            }
            _ => None,
        };

        items.push(EnabledItem {
            id,
            name,
            kind,
            files,
            setup_note,
            integration,
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
    /// Shared integration (spec + opened secrets; `secrets: None` when the
    /// vault could not open them). `None` for skills and legacy MCP items.
    pub(crate) integration: Option<BrokeredIntegration>,
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
        let file = archive.by_index(i).map_err(|_| AuthError::Invalid)?;

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

/// Parse an MCP JSON file (`{"mcpServers": {...}}`) into legacy items,
/// shared integrations and per-server skips. Legacy stdio entries keep the
/// existing rules (slug name, command and args checks, no credentials).
fn parse_mcp_json(data: &[u8]) -> Result<vault::ParsedImport, AuthError> {
    if data.len() > MAX_FILE_SIZE {
        return Err(AuthError::Limited);
    }
    vault::parse_import(data, |name, command, args| {
        validate_mcp_name(name)
            && validate_mcp_command(command)
            && args.len() <= MAX_MCP_ARGS_COUNT
            && args.iter().all(|arg| {
                arg.len() <= MAX_MCP_ARG_LEN
                    && !arg.bytes().any(|b| b == 0)
                    && !contains_credential_pattern(arg)
            })
    })
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

    /// The pre-integration call shape: stored items only.
    async fn import(
        state: &ServerState,
        actor: &OrgContext,
        request: ImportRequest,
    ) -> Result<Vec<LibraryItem>, AuthError> {
        super::import(state, actor, request).await.map(|o| o.items)
    }

    /// Import an MCP JSON value; returns the full outcome.
    async fn import_json(
        state: &ServerState,
        actor: &OrgContext,
        json: serde_json::Value,
    ) -> Result<ImportOutcome, AuthError> {
        super::import(
            state,
            actor,
            ImportRequest {
                file_name: "mcp.json".into(),
                data: b64(json.to_string().as_bytes()),
            },
        )
        .await
    }

    fn skip_reasons(outcome: &ImportOutcome) -> Vec<(String, String)> {
        outcome
            .skipped
            .iter()
            .map(|s| (s.name.clone(), s.reason.clone()))
            .collect()
    }

    async fn import_skip(json: Vec<u8>) -> Vec<(String, String)> {
        let harness = h::OrgHarness::new().await;
        let outcome = super::import(
            harness.state(),
            harness.owner_context(),
            ImportRequest {
                file_name: "mcp.json".into(),
                data: b64(&json),
            },
        )
        .await
        .unwrap();
        assert!(outcome.items.is_empty());
        skip_reasons(&outcome)
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
    async fn https_mcp_becomes_shared_integration() {
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
        assert_eq!(items[0].setup_note.as_deref(), Some(vault::SETUP_NOTE));
        let view = items[0].integration.as_ref().unwrap();
        assert_eq!(view.transport, Transport::Remote);
        assert_eq!(view.hosts, vec!["api.example.com".to_string()]);
        assert!(view.secret_names.is_empty());
    }

    #[tokio::test]
    async fn mcp_with_env_field_skipped() {
        assert_eq!(
            import_skip(env_mcp_json()).await,
            vec![("test-mcp".into(), "unsupported_package".into())]
        );
    }

    #[tokio::test]
    async fn mcp_with_credential_args_skipped() {
        assert_eq!(
            import_skip(credential_args_mcp_json()).await,
            vec![("test-mcp".into(), "invalid_config".into())]
        );
    }

    #[tokio::test]
    async fn mcp_with_malformed_args_skipped() {
        assert_eq!(
            import_skip(malformed_args_mcp_json()).await,
            vec![("test-mcp".into(), "invalid_config".into())]
        );
    }

    #[tokio::test]
    async fn mcp_with_http_url_skipped() {
        assert_eq!(
            import_skip(http_mcp_json()).await,
            vec![("insecure-mcp".into(), "invalid_url".into())]
        );
    }

    #[tokio::test]
    async fn http_mcp_with_headers_becomes_integration() {
        let harness = h::OrgHarness::new().await;
        let outcome = super::import(
            harness.state(),
            harness.owner_context(),
            ImportRequest {
                file_name: "bad-http.json".to_string(),
                data: b64(&unknown_http_field_mcp_json()),
            },
        )
        .await
        .unwrap();
        assert!(outcome.skipped.is_empty());
        let view = outcome.items[0].integration.as_ref().unwrap();
        assert_eq!(view.secret_names, vec!["Authorization".to_string()]);
        assert!(!serde_json::to_string(&outcome.items).unwrap().contains("xyz"));
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
    async fn mcp_name_with_special_chars_skipped() {
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
        assert_eq!(
            import_skip(json_data).await,
            vec![("bad/name".into(), "invalid_name".into())]
        );
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
        assert!(vault::validate_remote_url("https://api.example.com/mcp").is_some());
        for bad in [
            "http://api.example.com/mcp",
            "https://user:pass@api.example.com/mcp",
            "https://api.example.com/mcp?token=123",
            "https://localhost/mcp",
            "https://192.168.1.1/mcp",
        ] {
            assert!(vault::validate_remote_url(bad).is_none(), "{bad}");
        }
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

    /// The authorized export SHAPE (synthetic values only).
    fn export_shape_json() -> Vec<u8> {
        serde_json::json!({"mcpServers": {
            "hubspot": {"type": "stdio", "command": "npx", "args": ["-y", "@hubspot/mcp-server"],
                "env": {"HUBSPOT_ACCESS_TOKEN": "synthetic-hubspot-0001"}},
            "fireflies": {"type": "http", "url": "https://api.fireflies.ai/mcp",
                "headers": {"Authorization": "Bearer synthetic-fireflies-0002"}},
            "github": {"type": "stdio", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"],
                "env": {"GITHUB_PERSONAL_ACCESS_TOKEN": "synthetic-github-0003"}},
            "notion": {"type": "stdio", "command": "npx", "args": ["-y", "@suekou/mcp-notion-server"],
                "env": {"NOTION_API_TOKEN": "synthetic-notion-0004"}},
            "sanity": {"type": "stdio", "command": "npx", "args": ["-y", "@sanity/mcp-server@latest"],
                "env": {"SANITY_PROJECT_ID": "synthproj", "SANITY_DATASET": "production",
                        "SANITY_API_TOKEN": "synthetic-sanity-0005", "MCP_USER_ROLE": "developer"}},
            "aimfox": {"type": "http", "url": "https://mcp.aimfox.com/mcp",
                "headers": {"Authorization": "Bearer synthetic-aimfox-0006"}},
            "unblocked": {"type": "http", "url": "https://getunblocked.com/api/mcpsse",
                "headers": {"Authorization": "Bearer synthetic-unblocked-0007"}}
        }})
        .to_string()
        .into_bytes()
    }

    // --- Shared integrations -------------------------------------------------

    const SYNTHETIC_SECRETS: &[&str] = &[
        "synthetic-hubspot-0001",
        "synthetic-fireflies-0002",
        "synthetic-github-0003",
        "synthetic-notion-0004",
        "synthetic-sanity-0005",
        "synthetic-aimfox-0006",
        "synthetic-unblocked-0007",
    ];

    fn assert_no_secret(text: &str) {
        for secret in SYNTHETIC_SECRETS {
            assert!(!text.contains(secret), "secret value leaked");
        }
        assert!(!text.contains("synthetic-rotated"), "secret value leaked");
    }

    fn view<'a>(items: &'a [LibraryItem], name: &str) -> &'a IntegrationView {
        items
            .iter()
            .find(|i| i.name == name)
            .and_then(|i| i.integration.as_ref())
            .unwrap_or_else(|| panic!("integration {name}"))
    }

    fn set_runtime(
        harness: &mut h::OrgHarness,
        runtime: crate::neev::agents::execution::AgentRuntime,
    ) {
        std::sync::Arc::get_mut(&mut harness.state)
            .expect("owned test state")
            .set_agent_runtime(runtime);
    }

    fn fake(
        runtime: crate::neev::agents::execution::FakeRuntime,
    ) -> crate::neev::agents::execution::AgentRuntime {
        crate::neev::agents::execution::AgentRuntime::Fake(runtime)
    }

    fn new_fake() -> crate::neev::agents::execution::FakeRuntime {
        crate::neev::agents::execution::FakeRuntime::new(vec![])
    }

    /// Import the export shape and return the stored items.
    async fn import_export(harness: &h::OrgHarness) -> Vec<LibraryItem> {
        let outcome = super::import(
            harness.state(),
            harness.owner_context(),
            ImportRequest {
                file_name: "mcp.json".into(),
                data: b64(&export_shape_json()),
            },
        )
        .await
        .unwrap();
        assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
        outcome.items
    }

    fn item_id(items: &[LibraryItem], name: &str) -> String {
        items.iter().find(|i| i.name == name).unwrap().id.clone()
    }

    async fn enable(harness: &h::OrgHarness, id: &str) {
        patch(
            harness.state(),
            harness.owner_context(),
            id,
            PatchRequest {
                expected_revision: 1,
                enabled: true,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn export_shape_imports_seven_integrations() {
        let harness = h::OrgHarness::new().await;
        let items = import_export(&harness).await;
        assert_eq!(items.len(), 7);
        for item in &items {
            assert_eq!(item.kind, "mcp");
            assert!(!item.enabled);
            assert_eq!(item.setup_note.as_deref(), Some(vault::SETUP_NOTE));
            assert_eq!(item.files, vec!["integration.json".to_string(), "mcp.json".to_string()]);
            assert!(item.integration.as_ref().unwrap().status.is_none());
        }
        let hub = view(&items, "hubspot");
        assert_eq!(hub.transport, Transport::Package);
        assert_eq!(hub.hosts, vec!["api.hubspot.com".to_string()]);
        assert_eq!(hub.secret_names, vec!["PRIVATE_APP_ACCESS_TOKEN".to_string()]);
        assert_eq!(hub.source, "stdio:@hubspot/mcp-server");
        let notion = view(&items, "notion");
        assert_eq!(notion.transport, Transport::Package);
        assert_eq!(notion.hosts, vec!["api.notion.com".to_string()]);
        assert_eq!(notion.secret_names, vec!["NOTION_API_TOKEN".to_string()]);
        let github = view(&items, "github");
        assert_eq!(github.transport, Transport::Remote);
        assert_eq!(github.hosts, vec!["api.githubcopilot.com".to_string()]);
        assert_eq!(github.secret_names, vec!["Authorization".to_string()]);
        assert_eq!(github.source, "stdio:@modelcontextprotocol/server-github");
        assert_eq!(github.notes.len(), 1);
        let sanity = view(&items, "sanity");
        assert_eq!(sanity.transport, Transport::Remote);
        assert_eq!(sanity.hosts, vec!["mcp.sanity.io".to_string()]);
        assert_eq!(sanity.secret_names, vec!["Authorization".to_string()]);
        assert!(sanity.notes[0].contains("dropped"));
        for (name, host) in [
            ("fireflies", "api.fireflies.ai"),
            ("aimfox", "mcp.aimfox.com"),
            ("unblocked", "getunblocked.com"),
        ] {
            let v = view(&items, name);
            assert_eq!(v.transport, Transport::Remote);
            assert_eq!(v.hosts, vec![host.to_string()]);
            assert_eq!(v.secret_names, vec!["Authorization".to_string()]);
            assert!(v.source.starts_with("http:https://"));
        }
        // The legacy mcp.json stays credential-free and old-binary compatible.
        let row = db::one(
            &harness.database,
            "SELECT f.content FROM neev_library_files f JOIN neev_library_items i ON i.id=f.item_id
             WHERE i.name='hubspot' AND f.path='mcp.json'",
            vec![],
        )
        .await
        .unwrap()
        .unwrap();
        let legacy: serde_json::Value =
            serde_json::from_slice(&row.try_get::<Vec<u8>>("", "content").unwrap()).unwrap();
        assert_eq!(
            legacy,
            serde_json::json!({"name": "hubspot", "type": "stdio", "command": "npx",
                "args": ["-y", "@hubspot/mcp-server@0.4.0"]})
        );
    }

    #[tokio::test]
    async fn import_skip_reasons_never_carry_values() {
        let harness = h::OrgHarness::new().await;
        let remote = |url: &str| serde_json::json!({"type": "http", "url": url,
            "headers": {"Authorization": "Bearer synthetic-fireflies-0002"}});
        let outcome = import_json(
            harness.state(),
            harness.owner_context(),
            serde_json::json!({"mcpServers": {
                "placeholder": {"type": "http", "url": "https://api.example.com/mcp",
                    "headers": {"Authorization": "Bearer ${TOKEN}"}},
                "unknownpkg": {"command": "npx", "args": ["-y", "some-mcp"]},
                "stdioenv": {"command": "/usr/local/bin/tool", "env": {"API": "synthetic-notion-0004"}},
                "hubnode": {"command": "npx", "args": ["-y", "@hubspot/mcp-server"],
                    "env": {"PRIVATE_APP_ACCESS_TOKEN": "synthetic-hubspot-0001",
                            "NODE_OPTIONS": "--require /tmp/x.js"}},
                "plain": remote("http://api.example.com/mcp"),
                "ipv4": remote("https://8.8.8.8/mcp"),
                "local": remote("https://localhost/mcp"),
                "private": remote("https://10.0.0.1/mcp"),
                "metadata": remote("https://169.254.169.254/latest"),
                "loop6": remote("https://[::1]/mcp"),
                "userinfo": remote("https://u:p@api.example.com/mcp"),
                "query": remote("https://api.example.com/mcp?k=v"),
                "port": remote("https://api.example.com:8443/mcp"),
                "crlf": {"type": "http", "url": "https://api.example.com/mcp",
                    "headers": {"Authorization": "Bearer synthetic-aimfox-0006\r\nX: y"}},
                "hosthdr": {"type": "http", "url": "https://api.example.com/mcp",
                    "headers": {"Host": "synthetic-unblocked-0007"}},
                "approve": {"type": "http", "url": "https://api.example.com/mcp",
                    "autoApprove": ["x"]},
                "Dup": remote("https://api.example.com/a"),
                "dup": remote("https://api.example.com/b"),
            }}),
        )
        .await
        .unwrap();
        let reasons: HashMap<String, String> = skip_reasons(&outcome).into_iter().collect();
        let expect = [
            ("placeholder", "unresolved_placeholder"),
            ("unknownpkg", "unsupported_package"),
            ("stdioenv", "unsupported_package"),
            ("hubnode", "unsupported_env"),
            ("plain", "invalid_url"),
            ("ipv4", "invalid_url"),
            ("local", "invalid_url"),
            ("private", "invalid_url"),
            ("metadata", "invalid_url"),
            ("loop6", "invalid_url"),
            ("userinfo", "invalid_url"),
            ("query", "invalid_url"),
            ("port", "invalid_url"),
            ("crlf", "invalid_secret"),
            ("hosthdr", "invalid_secret"),
            ("approve", "unsupported_field"),
        ];
        for (name, reason) in expect {
            assert_eq!(reasons.get(name).map(String::as_str), Some(reason), "{name}");
        }
        // Exactly one of Dup/dup is stored, the other is a duplicate.
        assert_eq!(outcome.items.len(), 1);
        assert_eq!(reasons.values().filter(|r| *r == "duplicate_name").count(), 1);
        assert_no_secret(&format!("{outcome:?}"));
        assert_no_secret(&serde_json::to_string(&outcome.skipped).unwrap());
        assert_no_secret(&serde_json::to_string(&outcome.items).unwrap());

        // A second import of an existing name is a duplicate (case-insensitive).
        let again = import_json(
            harness.state(),
            harness.owner_context(),
            serde_json::json!({"mcpServers": {"DUP": remote("https://api.example.com/c")}}),
        )
        .await
        .unwrap();
        assert!(again.items.is_empty());
        assert_eq!(skip_reasons(&again), vec![("DUP".into(), "duplicate_name".into())]);
    }

    #[tokio::test]
    async fn malformed_files_are_still_rejected_whole() {
        let harness = h::OrgHarness::new().await;
        for json in [
            serde_json::json!({"mcpServers": {}}),
            serde_json::json!({"mcpServers": {"a": {"url": "https://a.example.com"}}, "x": 1}),
            serde_json::json!(["mcpServers"]),
        ] {
            assert!(matches!(
                import_json(harness.state(), harness.owner_context(), json).await,
                Err(AuthError::Invalid)
            ));
        }
    }

    #[tokio::test]
    async fn secrets_never_exposed_in_responses_or_rows() {
        let harness = h::OrgHarness::new().await;
        let items = import_export(&harness).await;
        assert_no_secret(&serde_json::to_string(&items).unwrap());
        let listed = list(harness.state(), harness.member_context()).await.unwrap();
        assert_no_secret(&serde_json::to_string(&listed).unwrap());
        assert_no_secret(&format!("{listed:?}"));
        assert!(listed.iter().all(|i| !i.files.iter().any(|f| f == SECRETS_PATH)));

        let github = item_id(&items, "github");
        let updated = update_secrets(
            harness.state(),
            harness.owner_context(),
            &github,
            SecretsRequest {
                expected_revision: 1,
                env: BTreeMap::new(),
                headers: BTreeMap::from([(
                    "Authorization".into(),
                    "Bearer synthetic-rotated-9".into(),
                )]),
            },
        )
        .await
        .unwrap();
        assert_eq!(updated.revision, 2);
        assert_no_secret(&serde_json::to_string(&updated).unwrap());
        assert!(!updated.files.iter().any(|f| f == SECRETS_PATH));

        let rows = harness
            .database
            .query_all(db::statement(
                &harness.database,
                "SELECT content FROM neev_library_files WHERE path=$1",
                vec![SECRETS_PATH.into()],
            ))
            .await
            .unwrap();
        assert_eq!(rows.len(), 7);
        for row in rows {
            let content: Vec<u8> = row.try_get("", "content").unwrap();
            assert_no_secret(&String::from_utf8_lossy(&content));
        }
    }

    #[tokio::test]
    async fn org_members_share_integrations_and_admins_alone_manage_them() {
        let harness = h::OrgHarness::new().await;
        let items = import_export(&harness).await;
        let notion = item_id(&items, "notion");
        enable(&harness, &notion).await;

        for actor in [harness.owner_context(), harness.member_context()] {
            let enabled = load_enabled(harness.state(), actor).await.unwrap();
            assert_eq!(enabled.len(), 1);
            let integration = enabled[0].integration.as_ref().unwrap();
            assert_eq!(integration.spec.key, "notion");
            let secrets = integration.secrets.as_ref().unwrap();
            assert_eq!(secrets.env["NOTION_API_TOKEN"], "synthetic-notion-0004");
            assert!(enabled[0].files.iter().all(|f| f.path != SECRETS_PATH));
            assert_no_secret(&format!("{enabled:?}"));
        }
        assert!(load_enabled(harness.state(), harness.other_org_context())
            .await
            .unwrap()
            .is_empty());

        let secrets = || SecretsRequest {
            expected_revision: 2,
            env: BTreeMap::from([("NOTION_API_TOKEN".into(), "synthetic-rotated-1".into())]),
            headers: BTreeMap::new(),
        };
        for actor in [harness.member_context(), harness.other_org_context()] {
            assert_eq!(
                update_secrets(harness.state(), actor, &notion, secrets()).await,
                Err(AuthError::Forbidden)
            );
            assert_eq!(
                test_integration(harness.state(), actor, &notion, 2).await,
                Err(AuthError::Forbidden)
            );
        }
    }

    #[tokio::test]
    async fn moved_ciphertext_does_not_open_and_does_not_fail_the_load() {
        let harness = h::OrgHarness::new().await;
        let items = import_export(&harness).await;
        let (hub, notion) = (item_id(&items, "hubspot"), item_id(&items, "notion"));
        enable(&harness, &hub).await;
        enable(&harness, &notion).await;
        // Copy hubspot's sealed blob (and its hash) onto notion.
        let row = db::one(
            &harness.database,
            "SELECT content, content_hash FROM neev_library_files WHERE item_id=$1 AND path=$2",
            vec![hub.clone().into(), SECRETS_PATH.into()],
        )
        .await
        .unwrap()
        .unwrap();
        let content: Vec<u8> = row.try_get("", "content").unwrap();
        let hash: String = row.try_get("", "content_hash").unwrap();
        db::execute(
            &harness.database,
            "UPDATE neev_library_files SET content=$1, content_hash=$2 WHERE item_id=$3 AND path=$4",
            vec![content.into(), hash.into(), notion.clone().into(), SECRETS_PATH.into()],
        )
        .await
        .unwrap();

        let enabled = load_enabled(harness.state(), harness.member_context()).await.unwrap();
        let by_name = |n: &str| {
            enabled
                .iter()
                .find(|i| i.name == n)
                .and_then(|i| i.integration.clone())
                .unwrap()
        };
        assert!(by_name("hubspot").secrets.is_some());
        assert!(by_name("notion").secrets.is_none());

        // A vault failure is reported by the test as vault_unavailable.
        let tested = test_integration(harness.state(), harness.owner_context(), &notion, 2)
            .await
            .unwrap();
        assert_eq!(
            tested.integration.unwrap().status.unwrap().state,
            "vault_unavailable"
        );
    }

    #[test]
    fn import_request_debug_never_prints_the_payload() {
        let request = ImportRequest {
            file_name: "conductor-mcp.json".into(),
            data: "eyJzZWNyZXQiOiJzeW50aGV0aWMtdG9rZW4ifQ==".into(),
        };
        let text = format!("{request:?}");
        assert!(text.contains("conductor-mcp.json"));
        assert!(!text.contains("eyJzZWNyZXQi"));
    }

    #[tokio::test]
    async fn one_damaged_spec_does_not_take_down_the_library() {
        let harness = h::OrgHarness::new().await;
        let items = import_export(&harness).await;
        let owner = harness.owner_context();
        for item in &items {
            patch(harness.state(), owner, &item.id, PatchRequest { expected_revision: 1, enabled: true })
                .await
                .unwrap();
        }
        let broken = item_id(&items, "fireflies");
        let garbage = b"{not json".to_vec();
        let hash = format!("{:x}", Sha256::digest(&garbage));
        db::execute(
            &harness.state().database,
            "UPDATE neev_library_files SET content=$1, content_hash=$2 WHERE item_id=$3 AND path=$4",
            vec![garbage.into(), hash.into(), broken.clone().into(), SPEC_PATH.into()],
        )
        .await
        .unwrap();
        let enabled = load_enabled(harness.state(), harness.member_context()).await.unwrap();
        assert!(!enabled.iter().any(|item| item.id == broken), "the damaged one is left out");
        assert_eq!(enabled.len(), items.len() - 1, "every other integration still loads");
    }

    #[tokio::test]
    async fn a_tested_integration_stays_tested_when_enabled_or_disabled() {
        let mut harness = h::OrgHarness::new().await;
        set_runtime(&mut harness, fake(new_fake()));
        let items = import_export(&harness).await;
        let owner = harness.owner_context();
        let fireflies = item_id(&items, "fireflies");
        test_integration(harness.state(), owner, &fireflies, 1).await.unwrap();
        let enabled = patch(
            harness.state(),
            owner,
            &fireflies,
            PatchRequest { expected_revision: 1, enabled: true },
        )
        .await
        .unwrap();
        assert_eq!(enabled.revision, 2);
        let status = enabled.integration.unwrap().status.expect("still tested after enable");
        assert_eq!(status.state, "ready");
        assert_eq!(status.revision, "2");
        let disabled = patch(
            harness.state(),
            owner,
            &fireflies,
            PatchRequest { expected_revision: 2, enabled: false },
        )
        .await
        .unwrap();
        assert_eq!(disabled.integration.unwrap().status.unwrap().state, "ready");
        // An untested item stays untested; a credential update still clears it.
        let github = item_id(&items, "github");
        let untested = patch(
            harness.state(),
            owner,
            &github,
            PatchRequest { expected_revision: 1, enabled: true },
        )
        .await
        .unwrap();
        assert!(untested.integration.unwrap().status.is_none());
    }

    #[tokio::test]
    async fn integration_test_records_status_for_the_current_revision() {
        let mut harness = h::OrgHarness::new().await;
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        set_runtime(
            &mut harness,
            fake(
                new_fake()
                    .with_check_state("github", "auth_failed")
                    .with_check_state("aimfox", "forbidden")
                    .recording_checks(log.clone()),
            ),
        );
        let items = import_export(&harness).await;
        let owner = harness.owner_context();

        let fireflies = item_id(&items, "fireflies");
        let ready = test_integration(harness.state(), owner, &fireflies, 1).await.unwrap();
        assert_eq!(ready.revision, 1);
        let status = ready.integration.as_ref().unwrap().status.clone().unwrap();
        assert_eq!(status.state, "ready");
        assert_eq!(status.tool_count, Some(3));
        assert_eq!(status.revision, "1");
        assert_eq!(status.checked_at, "1700000000");
        let wire = serde_json::to_value(&ready).unwrap();
        assert_eq!(wire["integration"]["status"]["toolCount"], 3);
        {
            let log = log.lock().unwrap();
            assert_eq!(log.len(), 1);
            assert_eq!(log[0].org_id, owner.org_id);
            assert_eq!(log[0].user_id, owner.identity.user_id);
            assert_eq!(log[0].integrations[0].spec.key, "fireflies");
            assert!(log[0].integrations[0].secrets.is_some());
        }

        let github = item_id(&items, "github");
        let failed = test_integration(harness.state(), owner, &github, 1).await.unwrap();
        let status = failed.integration.unwrap().status.unwrap();
        assert_eq!(status.state, "auth_failed");
        assert_eq!(status.detail.as_deref(), Some("HTTP 401"));
        assert_eq!(status.tool_count, None);

        let aimfox = item_id(&items, "aimfox");
        let forbidden = test_integration(harness.state(), owner, &aimfox, 1).await.unwrap();
        let status = forbidden.integration.unwrap().status.unwrap();
        assert_eq!(status.state, "forbidden");
        assert_eq!(status.detail.as_deref(), Some("HTTP 403"));

        // Members see the status read-only.
        let listed = list(harness.state(), harness.member_context()).await.unwrap();
        assert_eq!(view(&listed, "fireflies").status.as_ref().unwrap().state, "ready");

        assert_eq!(
            test_integration(harness.state(), owner, &fireflies, 2).await,
            Err(AuthError::Conflict)
        );

        // New credentials bump the revision and hide the old result.
        update_secrets(
            harness.state(),
            owner,
            &fireflies,
            SecretsRequest {
                expected_revision: 1,
                env: BTreeMap::new(),
                headers: BTreeMap::from([(
                    "Authorization".into(),
                    "Bearer synthetic-rotated-2".into(),
                )]),
            },
        )
        .await
        .unwrap();
        let listed = list(harness.state(), owner).await.unwrap();
        assert!(view(&listed, "fireflies").status.is_none());
    }

    #[tokio::test]
    async fn integration_test_without_runtime_is_runtime_unavailable() {
        let mut harness = h::OrgHarness::new().await;
        set_runtime(
            &mut harness,
            fake(new_fake().failing(crate::neev::agents::execution::ExecutionError::Unavailable)),
        );
        let items = import_export(&harness).await;
        let id = item_id(&items, "unblocked");
        let item = test_integration(harness.state(), harness.owner_context(), &id, 1)
            .await
            .unwrap();
        assert_eq!(item.integration.unwrap().status.unwrap().state, "runtime_unavailable");

        // The production default (disabled runtime) says so, not "try later".
        let harness = h::OrgHarness::new().await;
        let items = import_export(&harness).await;
        let id = item_id(&items, "unblocked");
        let item = test_integration(harness.state(), harness.owner_context(), &id, 1)
            .await
            .unwrap();
        assert_eq!(item.integration.unwrap().status.unwrap().state, "runtime_disabled");
    }

    #[tokio::test]
    async fn integration_test_keeps_meaningful_runtime_errors() {
        use crate::neev::agents::execution::ExecutionError as E;
        for (error, expected) in [
            (E::LanesBusy, "busy"),
            (E::Disabled, "runtime_disabled"),
            (E::Unsupported, "runtime_disabled"),
            (E::Isolation, "runtime_unavailable"),
            (E::Provider, "runtime_unavailable"),
            (E::Unauthorized, "runtime_unavailable"),
        ] {
            let mut harness = h::OrgHarness::new().await;
            set_runtime(&mut harness, fake(new_fake().failing(error.clone())));
            let items = import_export(&harness).await;
            let id = item_id(&items, "unblocked");
            let item = test_integration(harness.state(), harness.owner_context(), &id, 1)
                .await
                .unwrap();
            let status = item.integration.unwrap().status.unwrap();
            assert_eq!(status.state, expected, "{error:?}");
            assert_eq!(status.detail, None);
        }
    }

    #[tokio::test]
    async fn update_secrets_validates_names_values_and_revision() {
        let harness = h::OrgHarness::new().await;
        let items = import_export(&harness).await;
        let hub = item_id(&items, "hubspot");
        let owner = harness.owner_context();
        let request = |rev: i64, name: &str, value: &str| SecretsRequest {
            expected_revision: rev,
            env: BTreeMap::from([(name.to_string(), value.to_string())]),
            headers: BTreeMap::new(),
        };
        for bad in [
            request(1, "NODE_OPTIONS", "--require x"),
            request(1, "HUBSPOT_ACCESS_TOKEN", "synthetic-rotated-3"),
            request(1, "PRIVATE_APP_ACCESS_TOKEN", "a\r\nb"),
            request(1, "PRIVATE_APP_ACCESS_TOKEN", ""),
            request(1, "PRIVATE_APP_ACCESS_TOKEN", "non-ascii-\u{e9}"),
            request(0, "PRIVATE_APP_ACCESS_TOKEN", "synthetic-rotated-3"),
        ] {
            assert_eq!(
                update_secrets(harness.state(), owner, &hub, bad).await,
                Err(AuthError::Invalid)
            );
        }
        assert_eq!(
            update_secrets(
                harness.state(),
                owner,
                &hub,
                request(5, "PRIVATE_APP_ACCESS_TOKEN", "synthetic-rotated-3")
            )
            .await,
            Err(AuthError::Conflict)
        );
        // A skill is not an integration.
        let skill = import(
            harness.state(),
            owner,
            ImportRequest {
                file_name: "SKILL.md".into(),
                data: b64(b"# Skill\nText."),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            update_secrets(
                harness.state(),
                owner,
                &skill[0].id,
                request(1, "PRIVATE_APP_ACCESS_TOKEN", "synthetic-rotated-3")
            )
            .await,
            Err(AuthError::Invalid)
        );
        let ok = update_secrets(
            harness.state(),
            owner,
            &hub,
            request(1, "PRIVATE_APP_ACCESS_TOKEN", "synthetic-rotated-3"),
        )
        .await
        .unwrap();
        assert_eq!(ok.revision, 2);
    }

    #[tokio::test]
    async fn updated_secrets_reach_the_runtime() {
        let harness = h::OrgHarness::new().await;
        let items = import_export(&harness).await;
        let hub = item_id(&items, "hubspot");
        update_secrets(
            harness.state(),
            harness.owner_context(),
            &hub,
            SecretsRequest {
                expected_revision: 1,
                env: BTreeMap::from([(
                    "PRIVATE_APP_ACCESS_TOKEN".into(),
                    "synthetic-rotated-4".into(),
                )]),
                headers: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
        patch(
            harness.state(),
            harness.owner_context(),
            &hub,
            PatchRequest {
                expected_revision: 2,
                enabled: true,
            },
        )
        .await
        .unwrap();
        let enabled = load_enabled(harness.state(), harness.member_context()).await.unwrap();
        let secrets = enabled[0].integration.as_ref().unwrap().secrets.as_ref().unwrap();
        assert_eq!(secrets.env.len(), 1);
        assert_eq!(secrets.env["PRIVATE_APP_ACCESS_TOKEN"], "synthetic-rotated-4");
    }

    #[tokio::test]
    async fn legacy_stdio_items_are_unchanged() {
        let harness = h::OrgHarness::new().await;
        let items = import(
            harness.state(),
            harness.owner_context(),
            ImportRequest {
                file_name: "mcp.json".into(),
                data: b64(&valid_mcp_json()),
            },
        )
        .await
        .unwrap();
        assert!(items[0].integration.is_none());
        assert_eq!(items[0].files, vec!["mcp.json".to_string()]);
        let wire = serde_json::to_value(&items[0]).unwrap();
        assert!(wire.get("integration").is_none());
        enable(&harness, &items[0].id).await;
        let enabled = load_enabled(harness.state(), harness.owner_context()).await.unwrap();
        assert!(enabled[0].integration.is_none());
        assert_eq!(enabled[0].files.len(), 1);
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
