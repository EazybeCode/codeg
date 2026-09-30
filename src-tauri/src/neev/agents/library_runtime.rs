//! Runtime staging of shared library items (skills and MCP configurations).
//!
//! When a run starts, enabled library items are staged into the run's working
//! directory and made available to the agent. Skills become files the agent
//! can read and reference in its briefing. MCP servers get injected into the
//! provider's MCP configuration.
//!
//! Security:
//! - All paths are validated to prevent traversal attacks
//! - Symlinks are never created (content is copied)
//! - MCP configs are normalized and validated at import time
//! - No shell interpolation (command/args are passed to provider config)

use std::path::{Path, PathBuf};

use super::execution::ExecutionError;
use crate::neev::library::{EnabledFile, EnabledItem, McpServerConfig, NormalizedMcpServer};

/// Directory name for staged library items, under the run's workdir.
const LIBRARY_DIR: &str = "neev-library";

/// Directory name for skills within the library directory.
const SKILLS_DIR: &str = "skills";

/// An enabled library item ready for staging.
///
/// This is a clone-friendly version of [`EnabledItem`] for passing to the runtime.
#[derive(Debug, Clone)]
pub(crate) struct LibraryRunItem {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: LibraryItemKind,
    pub(crate) files: Vec<LibraryRunFile>,
    pub(crate) setup_note: Option<String>,
}

/// The kind of library item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LibraryItemKind {
    Skill,
    Mcp,
}

/// A file from a library item, ready for staging.
#[derive(Debug, Clone)]
pub(crate) struct LibraryRunFile {
    pub(crate) path: String,
    pub(crate) content: Vec<u8>,
}

/// Convert domain types to runtime types.
impl From<EnabledItem> for LibraryRunItem {
    fn from(item: EnabledItem) -> Self {
        Self {
            id: item.id,
            name: item.name,
            kind: match item.kind.as_str() {
                "mcp" => LibraryItemKind::Mcp,
                _ => LibraryItemKind::Skill,
            },
            files: item.files.into_iter().map(LibraryRunFile::from).collect(),
            setup_note: item.setup_note,
        }
    }
}

impl From<EnabledFile> for LibraryRunFile {
    fn from(file: EnabledFile) -> Self {
        Self {
            path: file.path,
            content: file.content,
        }
    }
}

/// Result of staging library items: skill paths for briefing and MCP configs for providers.
#[derive(Debug, Default)]
pub(crate) struct StagedLibrary {
    /// Staged skill directories, each containing their SKILL.md and supporting files.
    /// The path points to the skill's root directory.
    pub(crate) skill_dirs: Vec<StagedSkill>,
    /// MCP server configurations to inject into the provider.
    pub(crate) mcp_servers: Vec<StagedMcpServer>,
}

/// A staged skill with its directory path.
#[derive(Debug, Clone)]
pub(crate) struct StagedSkill {
    /// The skill's unique ID.
    pub(crate) id: String,
    /// The skill's display name.
    pub(crate) name: String,
    /// The directory containing the skill's files.
    pub(crate) directory: PathBuf,
    /// The skill's setup note, if any.
    pub(crate) setup_note: Option<String>,
}

/// An MCP server configuration ready for injection.
#[derive(Debug, Clone)]
pub(crate) struct StagedMcpServer {
    /// The item's unique ID (used for collision-safe naming).
    pub(crate) id: String,
    /// The server's name from the original config.
    pub(crate) name: String,
    /// The server configuration.
    pub(crate) config: McpServerConfig,
}

/// Validate that an item ID is safe for use in filesystem paths.
///
/// IDs must be ASCII alphanumeric with dashes/underscores only. This prevents
/// path traversal via IDs (e.g., "../evil") and ensures collision-free naming.
fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Stage library items into the run's working directory.
///
/// Skills are staged as directories under `{workdir}/neev-library/skills/{id}/`.
/// MCP configs are parsed and returned for provider-specific injection.
///
/// Returns `Err` if:
/// - Any item ID fails validation (prevents path traversal)
/// - Any MCP config is malformed (corrupt library item)
/// - Any file fails path validation (traversal, hidden files)
/// - Any filesystem operation fails
///
/// An empty library is fine and returns an empty result.
#[cfg(unix)]
pub(crate) fn stage_library(
    workdir: &Path,
    items: &[LibraryRunItem],
) -> Result<StagedLibrary, ExecutionError> {
    if items.is_empty() {
        return Ok(StagedLibrary::default());
    }

    let mut result = StagedLibrary::default();

    // Process each item
    for item in items {
        // Validate item ID before using it in any path
        if !is_safe_id(&item.id) {
            return Err(ExecutionError::Unauthorized);
        }

        match item.kind {
            LibraryItemKind::Skill => {
                let skill_dir = stage_skill_files(workdir, item)?;
                result.skill_dirs.push(StagedSkill {
                    id: item.id.clone(),
                    name: item.name.clone(),
                    directory: skill_dir,
                    setup_note: item.setup_note.clone(),
                });
            }
            LibraryItemKind::Mcp => {
                // MCP items have a single mcp.json file with normalized config.
                // A corrupt config is an error, not a silent drop — the library
                // import validated this, so corruption here is a storage bug.
                let mcp_file = item
                    .files
                    .iter()
                    .find(|f| f.path == "mcp.json")
                    .ok_or(ExecutionError::Unavailable)?;
                let server: NormalizedMcpServer = serde_json::from_slice(&mcp_file.content)
                    .map_err(|_| ExecutionError::Unavailable)?;
                result.mcp_servers.push(StagedMcpServer {
                    id: item.id.clone(),
                    name: server.name,
                    config: server.config,
                });
            }
        }
    }

    Ok(result)
}

/// Stage skill files into a subdirectory of the library.
#[cfg(unix)]
fn stage_skill_files(workdir: &Path, item: &LibraryRunItem) -> Result<PathBuf, ExecutionError> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    // Create library root if needed
    let library_root = workdir.join(LIBRARY_DIR);
    ensure_directory(&library_root)?;

    // Create skills directory if needed
    let skills_root = library_root.join(SKILLS_DIR);
    ensure_directory(&skills_root)?;

    // Create skill-specific directory using item ID (UUID, collision-free)
    let skill_dir = skills_root.join(&item.id);
    std::fs::create_dir(&skill_dir).map_err(|_| ExecutionError::Unavailable)?;
    std::fs::set_permissions(&skill_dir, std::fs::Permissions::from_mode(0o755))
        .map_err(|_| ExecutionError::Unavailable)?;

    // Stage each file
    for file in &item.files {
        // Validate path: must be a normal relative path, no traversal
        if !is_safe_path(&file.path) {
            return Err(ExecutionError::Unauthorized);
        }

        // Build target path
        let target = skill_dir.join(&file.path);

        // Create parent directories if needed (for nested files)
        if let Some(parent) = target.parent() {
            if parent != skill_dir {
                std::fs::create_dir_all(parent).map_err(|_| ExecutionError::Unavailable)?;
                // Set permissions on all created directories
                for ancestor in parent.ancestors() {
                    if ancestor == skill_dir {
                        break;
                    }
                    std::fs::set_permissions(ancestor, std::fs::Permissions::from_mode(0o755))
                        .map_err(|_| ExecutionError::Unavailable)?;
                }
            }
        }

        // Write file with safe flags
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o644)
            .open(&target)
            .map_err(|_| ExecutionError::Unavailable)?;

        use std::io::Write;
        f.write_all(&file.content)
            .map_err(|_| ExecutionError::Unavailable)?;
        f.sync_all().map_err(|_| ExecutionError::Unavailable)?;

        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))
            .map_err(|_| ExecutionError::Unavailable)?;
    }

    Ok(skill_dir)
}

/// Ensure a directory exists, handling the already-exists case safely.
#[cfg(unix)]
fn ensure_directory(path: &Path) -> Result<(), ExecutionError> {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::create_dir(path) {
        Ok(()) => {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .map_err(|_| ExecutionError::Unavailable)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // Verify it's a real directory, not a symlink
            let meta = std::fs::symlink_metadata(path).map_err(|_| ExecutionError::Unavailable)?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(ExecutionError::Unauthorized);
            }
            Ok(())
        }
        Err(_) => Err(ExecutionError::Unavailable),
    }
}

/// Non-unix stub: library staging requires unix filesystem semantics.
#[cfg(not(unix))]
pub(crate) fn stage_library(
    _workdir: &Path,
    items: &[LibraryRunItem],
) -> Result<StagedLibrary, ExecutionError> {
    if items.is_empty() {
        return Ok(StagedLibrary::default());
    }
    Err(ExecutionError::Unavailable)
}

/// Validate that a path is safe for staging (no traversal, no absolute paths).
fn is_safe_path(path: &str) -> bool {
    if path.is_empty()
        || path.contains(['\\', ':'])
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return false;
    }

    let p = std::path::Path::new(path);

    // Must be relative
    if p.is_absolute() {
        return false;
    }

    // Check each component
    for component in p.components() {
        match component {
            std::path::Component::Normal(name) => {
                let name = name.to_string_lossy();
                // No hidden files/directories (except current directory which isn't here)
                if name.starts_with('.') && name != "." {
                    return false;
                }
            }
            // Allow these
            std::path::Component::CurDir => {}
            // Reject traversal and absolute components
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                return false;
            }
        }
    }

    true
}

/// Generate a collision-safe MCP server name from item name and ID.
///
/// The name is used in provider configs and must be unique. We append the FULL
/// item ID to guarantee uniqueness — truncating to 8 chars would collide when
/// two items share a UUID prefix, which UUIDs do not prevent.
pub(crate) fn safe_mcp_name(name: &str, id: &str) -> String {
    // Sanitize name: only alphanumeric, dash, underscore
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();

    // Use the FULL ID for uniqueness
    format!("{}-{}", sanitized, id)
}

/// Build a briefing section describing staged skills.
///
/// This is appended to the agent's briefing so it knows about available skills.
/// The briefing includes usage instructions so the agent knows HOW to use them.
pub(crate) fn skills_briefing(skills: &[StagedSkill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut text = String::from("<library-skills>\n");
    text.push_str("Your organization has shared the following skills. ");
    text.push_str("Each skill directory contains a SKILL.md file describing what the skill does ");
    text.push_str("and how to use it. Read the SKILL.md file before invoking a skill.\n\n");

    for skill in skills {
        text.push_str(&format!(
            "- **{}**: {}/SKILL.md\n",
            skill.name,
            skill.directory.display()
        ));
        if let Some(note) = &skill.setup_note {
            text.push_str(&format!("  Setup: {}\n", note));
        }
    }

    text.push_str("\nTo use a skill, read its SKILL.md for instructions, then follow them.\n");
    text.push_str("</library-skills>\n");
    Some(text)
}

/// Generate Claude MCP config JSON for a set of servers.
///
/// Claude takes `--mcp-config <path>` with a JSON file containing the mcpServers map.
pub(crate) fn claude_mcp_config(servers: &[StagedMcpServer]) -> Option<serde_json::Value> {
    if servers.is_empty() {
        return None;
    }

    let mut mcp_servers = serde_json::Map::new();
    for server in servers {
        let safe_name = safe_mcp_name(&server.name, &server.id);
        let config = match &server.config {
            McpServerConfig::Stdio { command, args } => {
                serde_json::json!({
                    "command": command,
                    "args": args
                })
            }
            McpServerConfig::Http { url } => {
                serde_json::json!({
                    "type": "http",
                    "url": url
                })
            }
        };
        mcp_servers.insert(safe_name, config);
    }

    Some(serde_json::json!({
        "mcpServers": mcp_servers
    }))
}

/// Write Claude MCP config to a file in the library directory.
///
/// Returns the path to the config file, which should be passed to `--mcp-config`.
#[cfg(unix)]
pub(crate) fn write_claude_mcp_config(
    workdir: &Path,
    servers: &[StagedMcpServer],
) -> Result<Option<PathBuf>, ExecutionError> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let config = match claude_mcp_config(servers) {
        Some(config) => config,
        None => return Ok(None),
    };

    let library_root = workdir.join(LIBRARY_DIR);
    ensure_directory(&library_root)?;

    let config_path = library_root.join("mcp.json");
    let json = serde_json::to_vec_pretty(&config).map_err(|_| ExecutionError::Unavailable)?;

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o644)
        .open(&config_path)
        .map_err(|_| ExecutionError::Unavailable)?;

    use std::io::Write;
    f.write_all(&json)
        .map_err(|_| ExecutionError::Unavailable)?;
    f.sync_all().map_err(|_| ExecutionError::Unavailable)?;

    Ok(Some(config_path))
}

/// Non-unix stub for MCP config writing.
#[cfg(not(unix))]
pub(crate) fn write_claude_mcp_config(
    _workdir: &Path,
    servers: &[StagedMcpServer],
) -> Result<Option<PathBuf>, ExecutionError> {
    if servers.is_empty() {
        return Ok(None);
    }
    Err(ExecutionError::Unavailable)
}

/// One ACP server's JSON may be at most this many bytes: the adapter's own
/// bound (`neev_acp_stream_adapter.mcp_server`), and half the kernel's limit on
/// a single argv string, which it travels as.
pub(crate) const ACP_MCP_MAX_SERVER_BYTES: usize = 65_536;
/// All forwarded servers' JSON together: far inside the kernel's argv limit
/// whatever the prompt and the rest of the launch line take.
pub(crate) const ACP_MCP_MAX_TOTAL_BYTES: usize = 256 * 1024;

/// Library MCP servers for an ACP run: those forwarded, and those left out.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AcpMcpServers {
    /// One JSON object per server in the adapter's input shape.
    pub(crate) forwarded: Vec<String>,
    /// Names of servers over the argv budget, reported in the trace by the
    /// adapter rather than failing the launch.
    pub(crate) skipped: Vec<String>,
}

/// Shared-library MCP servers for an ACP agent's `session/new`, one JSON object
/// per server in the adapter's input shape (`neev_acp_stream_adapter.mcp_server`):
/// stdio `{name, command, args}` or `{name, type:"http", url}`. No env and no
/// headers: nothing secret travels this way.
///
/// Every library server is accounted for (the library holds at most 100 items,
/// the adapter's limit). One whose JSON would not fit the argv budget is named
/// in `skipped` instead: the run goes ahead without it, and says so.
pub(crate) fn acp_mcp_servers(servers: &[StagedMcpServer]) -> AcpMcpServers {
    let mut result = AcpMcpServers::default();
    let mut total = 0usize;
    for server in servers {
        let name = safe_mcp_name(&server.name, &server.id);
        let value = match &server.config {
            McpServerConfig::Stdio { command, args } => {
                serde_json::json!({ "name": name, "command": command, "args": args })
            }
            McpServerConfig::Http { url } => {
                serde_json::json!({ "name": name, "type": "http", "url": url })
            }
        }
        .to_string();
        if value.len() > ACP_MCP_MAX_SERVER_BYTES || total + value.len() > ACP_MCP_MAX_TOTAL_BYTES
        {
            result.skipped.push(name);
        } else {
            total += value.len();
            result.forwarded.push(value);
        }
    }
    result
}

/// Generate Codex MCP config overrides.
///
/// Codex takes `-c mcp_servers.<name>.<field>=<value>` for each config field.
/// Values must be TOML-safe. We use serde_json for quoting which produces
/// valid TOML string literals (JSON strings are a subset of TOML strings).
///
/// Returns a list of config override strings.
pub(crate) fn codex_mcp_config(servers: &[StagedMcpServer]) -> Vec<String> {
    let mut overrides = Vec::new();

    for server in servers {
        let safe_name = safe_mcp_name(&server.name, &server.id);
        match &server.config {
            McpServerConfig::Stdio { command, args } => {
                // Use serde_json::to_string for TOML-safe quoting
                let command_quoted = serde_json::to_string(command).expect("strings serialize");
                overrides.push(format!(
                    "mcp_servers.{}.command={}",
                    safe_name, command_quoted
                ));
                // Always override args, including an empty list, so inherited
                // per-user config cannot add arguments to a shared server.
                let args_json = serde_json::to_string(args).expect("string arrays serialize");
                overrides.push(format!("mcp_servers.{}.args={}", safe_name, args_json));
            }
            McpServerConfig::Http { url } => {
                let url_quoted = serde_json::to_string(url).expect("strings serialize");
                overrides.push(format!("mcp_servers.{}.url={}", safe_name, url_quoted));
            }
        }
    }

    overrides
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn skill_item(id: &str, name: &str, files: Vec<(&str, &[u8])>) -> LibraryRunItem {
        LibraryRunItem {
            id: id.to_string(),
            name: name.to_string(),
            kind: LibraryItemKind::Skill,
            files: files
                .into_iter()
                .map(|(path, content)| LibraryRunFile {
                    path: path.to_string(),
                    content: content.to_vec(),
                })
                .collect(),
            setup_note: None,
        }
    }

    fn mcp_item(id: &str, name: &str, config: McpServerConfig) -> LibraryRunItem {
        let normalized = NormalizedMcpServer {
            name: name.to_string(),
            config,
        };
        let content = serde_json::to_vec(&normalized).unwrap();
        LibraryRunItem {
            id: id.to_string(),
            name: name.to_string(),
            kind: LibraryItemKind::Mcp,
            files: vec![LibraryRunFile {
                path: "mcp.json".to_string(),
                content,
            }],
            setup_note: Some("Requires node".to_string()),
        }
    }

    #[test]
    fn empty_library_returns_empty_result() {
        let dir = TempDir::new().unwrap();
        let result = stage_library(dir.path(), &[]).unwrap();
        assert!(result.skill_dirs.is_empty());
        assert!(result.mcp_servers.is_empty());
    }

    #[test]
    fn stages_single_skill() {
        let dir = TempDir::new().unwrap();
        let items = vec![skill_item(
            "uuid-1234",
            "Test Skill",
            vec![("SKILL.md", b"# Test Skill\n\nDescription")],
        )];

        let result = stage_library(dir.path(), &items).unwrap();
        assert_eq!(result.skill_dirs.len(), 1);
        assert_eq!(result.skill_dirs[0].name, "Test Skill");
        assert!(result.skill_dirs[0].directory.exists());

        let skill_md = result.skill_dirs[0].directory.join("SKILL.md");
        assert!(skill_md.exists());
        let content = std::fs::read_to_string(skill_md).unwrap();
        assert_eq!(content, "# Test Skill\n\nDescription");
    }

    #[test]
    fn stages_skill_with_nested_files() {
        let dir = TempDir::new().unwrap();
        let items = vec![skill_item(
            "uuid-nested",
            "Nested Skill",
            vec![
                ("SKILL.md", b"# Nested\n"),
                ("lib/helper.py", b"# helper"),
                ("lib/utils/common.py", b"# common"),
            ],
        )];

        let result = stage_library(dir.path(), &items).unwrap();
        assert!(result.skill_dirs[0].directory.join("SKILL.md").exists());
        assert!(result.skill_dirs[0]
            .directory
            .join("lib/helper.py")
            .exists());
        assert!(result.skill_dirs[0]
            .directory
            .join("lib/utils/common.py")
            .exists());
    }

    #[test]
    fn rejects_path_traversal() {
        let dir = TempDir::new().unwrap();
        let items = vec![skill_item(
            "uuid-evil",
            "Evil Skill",
            vec![("../../../etc/passwd", b"malicious")],
        )];

        let result = stage_library(dir.path(), &items);
        assert!(matches!(result, Err(ExecutionError::Unauthorized)));
    }

    #[test]
    fn rejects_absolute_path() {
        let dir = TempDir::new().unwrap();
        let items = vec![skill_item(
            "uuid-abs",
            "Absolute Skill",
            vec![("/etc/passwd", b"malicious")],
        )];

        let result = stage_library(dir.path(), &items);
        assert!(matches!(result, Err(ExecutionError::Unauthorized)));
    }

    #[test]
    fn rejects_hidden_files() {
        let dir = TempDir::new().unwrap();
        let items = vec![skill_item(
            "uuid-hidden",
            "Hidden Skill",
            vec![(".secret", b"hidden")],
        )];

        let result = stage_library(dir.path(), &items);
        assert!(matches!(result, Err(ExecutionError::Unauthorized)));
    }

    #[test]
    fn stages_mcp_stdio_server() {
        let dir = TempDir::new().unwrap();
        let items = vec![mcp_item(
            "uuid-mcp",
            "test-server",
            McpServerConfig::Stdio {
                command: "node".to_string(),
                args: vec!["server.js".to_string()],
            },
        )];

        let result = stage_library(dir.path(), &items).unwrap();
        assert_eq!(result.mcp_servers.len(), 1);
        assert_eq!(result.mcp_servers[0].name, "test-server");
        match &result.mcp_servers[0].config {
            McpServerConfig::Stdio { command, args } => {
                assert_eq!(command, "node");
                assert_eq!(args, &["server.js"]);
            }
            _ => panic!("expected stdio config"),
        }
    }

    #[test]
    fn stages_mcp_http_server() {
        let dir = TempDir::new().unwrap();
        let items = vec![mcp_item(
            "uuid-http",
            "remote-api",
            McpServerConfig::Http {
                url: "https://api.example.com/mcp".to_string(),
            },
        )];

        let result = stage_library(dir.path(), &items).unwrap();
        assert_eq!(result.mcp_servers.len(), 1);
        match &result.mcp_servers[0].config {
            McpServerConfig::Http { url } => {
                assert_eq!(url, "https://api.example.com/mcp");
            }
            _ => panic!("expected http config"),
        }
    }

    #[test]
    fn safe_mcp_name_sanitizes_and_adds_full_id() {
        // Full ID is used for uniqueness, not truncated
        let name = safe_mcp_name("My Server!", "12345678-abcd-1234-5678");
        assert_eq!(name, "My-Server--12345678-abcd-1234-5678");

        let name = safe_mcp_name("test-server", "abcdef12");
        assert_eq!(name, "test-server-abcdef12");
    }

    #[test]
    fn claude_mcp_config_generates_valid_json() {
        let servers = vec![
            StagedMcpServer {
                id: "uuid-1234".to_string(),
                name: "stdio-server".to_string(),
                config: McpServerConfig::Stdio {
                    command: "node".to_string(),
                    args: vec!["index.js".to_string()],
                },
            },
            StagedMcpServer {
                id: "uuid-5678".to_string(),
                name: "http-server".to_string(),
                config: McpServerConfig::Http {
                    url: "https://api.example.com".to_string(),
                },
            },
        ];

        let config = claude_mcp_config(&servers).unwrap();
        let mcp_servers = config.get("mcpServers").unwrap().as_object().unwrap();

        // Check stdio server (full ID in key)
        let stdio = mcp_servers.get("stdio-server-uuid-1234").unwrap();
        assert_eq!(stdio["command"], "node");
        assert_eq!(stdio["args"], serde_json::json!(["index.js"]));

        // Check http server (full ID in key)
        let http = mcp_servers.get("http-server-uuid-5678").unwrap();
        assert_eq!(http["type"], "http");
        assert_eq!(http["url"], "https://api.example.com");
    }

    #[test]
    fn codex_mcp_config_generates_overrides() {
        let servers = vec![StagedMcpServer {
            id: "uuid-abcd".to_string(),
            name: "test".to_string(),
            config: McpServerConfig::Stdio {
                command: "python".to_string(),
                args: vec!["-m".to_string(), "server".to_string()],
            },
        }];

        let overrides = codex_mcp_config(&servers);
        // Full ID in key
        assert!(overrides
            .iter()
            .any(|o| o.contains("mcp_servers.test-uuid-abcd")));
        // TOML-safe quoting: uses serde_json::to_string
        assert!(overrides.iter().any(|o| o.contains("command=\"python\"")));
        assert!(overrides.iter().any(|o| o.contains("args=")));
    }

    #[test]
    fn codex_mcp_config_quotes_special_chars() {
        // Test that special characters are properly quoted
        let servers = vec![StagedMcpServer {
            id: "uuid-special".to_string(),
            name: "test".to_string(),
            config: McpServerConfig::Stdio {
                command: "/path/with spaces/cmd".to_string(),
                args: vec!["arg with\ttab".to_string()],
            },
        }];

        let overrides = codex_mcp_config(&servers);
        // serde_json properly escapes the tab character
        assert!(overrides.iter().any(|o| o.contains(r#"\t"#)));
    }

    #[test]
    fn is_safe_id_validation() {
        // Valid IDs
        assert!(is_safe_id("abc-123"));
        assert!(is_safe_id("test_item"));
        assert!(is_safe_id("a1b2c3d4-e5f6-7890-abcd-ef1234567890"));

        // Invalid IDs
        assert!(!is_safe_id("")); // empty
        assert!(!is_safe_id("../traversal")); // path traversal
        assert!(!is_safe_id("a".repeat(65).as_str())); // too long
        assert!(!is_safe_id("hello world")); // space
        assert!(!is_safe_id("test/path")); // slash
    }

    #[test]
    fn rejects_unsafe_item_id() {
        let dir = TempDir::new().unwrap();
        let items = vec![skill_item(
            "../evil",
            "Evil Skill",
            vec![("SKILL.md", b"harmless content")],
        )];

        let result = stage_library(dir.path(), &items);
        assert!(matches!(result, Err(ExecutionError::Unauthorized)));
    }

    #[test]
    fn rejects_corrupt_mcp_json() {
        let dir = TempDir::new().unwrap();
        // Manually create an MCP item with corrupt JSON
        let items = vec![LibraryRunItem {
            id: "uuid-corrupt".to_string(),
            name: "bad-mcp".to_string(),
            kind: LibraryItemKind::Mcp,
            files: vec![LibraryRunFile {
                path: "mcp.json".to_string(),
                content: b"not valid json".to_vec(),
            }],
            setup_note: None,
        }];

        let result = stage_library(dir.path(), &items);
        assert!(matches!(result, Err(ExecutionError::Unavailable)));
    }

    #[test]
    fn rejects_mcp_without_config_file() {
        let dir = TempDir::new().unwrap();
        // MCP item without the required mcp.json file
        let items = vec![LibraryRunItem {
            id: "uuid-missing".to_string(),
            name: "missing-mcp".to_string(),
            kind: LibraryItemKind::Mcp,
            files: vec![], // No files
            setup_note: None,
        }];

        let result = stage_library(dir.path(), &items);
        assert!(matches!(result, Err(ExecutionError::Unavailable)));
    }

    #[test]
    fn skills_briefing_generates_section() {
        let skills = vec![
            StagedSkill {
                id: "uuid-1".to_string(),
                name: "Skill One".to_string(),
                directory: PathBuf::from("/tmp/skills/uuid-1"),
                setup_note: None,
            },
            StagedSkill {
                id: "uuid-2".to_string(),
                name: "Skill Two".to_string(),
                directory: PathBuf::from("/tmp/skills/uuid-2"),
                setup_note: Some("Requires Python 3.10+".to_string()),
            },
        ];

        let briefing = skills_briefing(&skills).unwrap();
        assert!(briefing.contains("<library-skills>"));
        assert!(briefing.contains("Skill One"));
        assert!(briefing.contains("Skill Two"));
        assert!(briefing.contains("Requires Python 3.10+"));
    }

    #[test]
    fn is_safe_path_validation() {
        // Valid paths
        assert!(is_safe_path("SKILL.md"));
        assert!(is_safe_path("lib/helper.py"));
        assert!(is_safe_path("src/utils/common.rs"));

        // Invalid paths
        assert!(!is_safe_path(""));
        assert!(!is_safe_path("../parent"));
        assert!(!is_safe_path("/absolute"));
        assert!(!is_safe_path(".hidden"));
        assert!(!is_safe_path("dir/.hidden"));
        assert!(!is_safe_path(".."));
    }

    fn stdio(id: &str, args: Vec<String>) -> StagedMcpServer {
        StagedMcpServer {
            id: id.to_string(),
            name: "tool".to_string(),
            config: McpServerConfig::Stdio { command: "tool".to_string(), args },
        }
    }

    #[test]
    fn acp_servers_are_all_accounted_for_up_to_the_library_limit() {
        // 100 ordinary servers (the org library limit) all travel.
        let servers: Vec<_> = (0..100).map(|i| stdio(&format!("id{i}"), vec![])).collect();
        let acp = acp_mcp_servers(&servers);
        assert_eq!(acp.forwarded.len(), 100);
        assert!(acp.skipped.is_empty());
        assert!(acp.forwarded[0].contains("\"name\":\"tool-id0\""));
    }

    #[test]
    fn acp_servers_over_the_argv_budget_are_named_not_dropped_or_fatal() {
        // A server whose JSON exceeds one argv string's budget (50 args of 1000
        // 4-byte characters) and then enough large ones to pass the total.
        let huge = stdio("huge", vec!["\u{1F600}".repeat(1000); 50]);
        let big = |i: usize| stdio(&format!("big{i}"), vec!["x".repeat(1000); 50]);
        let mut servers = vec![huge];
        servers.extend((0..6).map(big));
        servers.push(stdio("small", vec![]));
        let acp = acp_mcp_servers(&servers);
        assert!(acp.skipped.contains(&"tool-huge".to_string()));
        assert!(acp.forwarded.iter().all(|s| s.len() <= ACP_MCP_MAX_SERVER_BYTES));
        let total: usize = acp.forwarded.iter().map(String::len).sum();
        assert!(total <= ACP_MCP_MAX_TOTAL_BYTES);
        assert!(!acp.skipped.is_empty() && acp.skipped.len() >= 2);
        // Every server is either forwarded or named; the small one still fits.
        assert_eq!(acp.forwarded.len() + acp.skipped.len(), servers.len());
        assert!(acp.forwarded.iter().any(|s| s.contains("tool-small")));
    }
}
