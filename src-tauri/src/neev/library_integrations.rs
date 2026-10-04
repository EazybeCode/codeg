//! Shared integrations: MCP import parsing (pure) and the credential vault.
//!
//! Import turns an `mcpServers` JSON file into legacy stdio items, brokered
//! integrations ([`IntegrationSpec`] + sealed [`IntegrationSecrets`]) and
//! per-server skips (`{name, reason}`; a reason never carries a value).
//! The vault seals secrets with AES-256-GCM under a key derived from the
//! server's encryption key, bound (AAD) to the org and item.

use std::collections::{BTreeMap, HashSet};

use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use sha2::Sha256;

use crate::neev::{
    error::AuthError,
    integrations::{IntegrationSecrets, IntegrationSpec, Transport, SPEC_VERSION},
    library::{McpServerConfig, NormalizedMcpServer, SkippedServer},
};

/// `setup_note` of every brokered integration item.
pub(crate) const SETUP_NOTE: &str =
    "Shared integration: credentials are stored encrypted and are never given to agents.";

const VAULT_LABEL: &[u8] = b"neev-library-integration-secrets-v1";
const SEALED_MAGIC: &[u8] = b"nv1";
const NONCE_LEN: usize = 12;

const MAX_SECRET_VALUE_BYTES: usize = 8192;
const MAX_SECRETS: usize = 16;
const MAX_URL_LEN: usize = 2048;

const GITHUB_NOTE: &str = "Converted from the deprecated npm server to GitHub's official remote MCP server (api.githubcopilot.com). The stored token has not been verified there: run Test.";
const SANITY_NOTE: &str = "Converted from the deprecated npm server to Sanity's remote MCP server; project/dataset settings were dropped and token authentication there is unverified: run Test.";

// --- Vault ---------------------------------------------------------------

/// Why sealed secrets could not be opened (or sealed). Carries no data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VaultError {
    /// Not a sealed blob of this format (wrong magic / too short).
    Malformed,
    /// Authentication failed: tampered, moved to another org/item, or wrong key.
    Rejected,
    /// The plaintext is not a valid secrets document.
    Corrupt,
    /// Cipher or randomness unavailable.
    Unavailable,
}

/// The integration vault key: HMAC-SHA256(encryption_key, label).
pub(crate) fn vault_key(encryption_key: &[u8; 32]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(encryption_key)
        .expect("HMAC accepts any key length");
    mac.update(VAULT_LABEL);
    mac.finalize().into_bytes().into()
}

fn vault_aad(org_id: i32, item_id: &str) -> Vec<u8> {
    format!("neev-integration-v1\0{org_id}\0{item_id}").into_bytes()
}

/// Seal `secrets` for one org's item: `b"nv1" ‖ nonce ‖ AES-256-GCM`.
pub(crate) fn seal(
    key: &[u8; 32],
    org_id: i32,
    item_id: &str,
    secrets: &IntegrationSecrets,
) -> Result<Vec<u8>, VaultError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| VaultError::Unavailable)?;
    let mut nonce = [0u8; NONCE_LEN];
    OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| VaultError::Unavailable)?;
    let plaintext = serde_json::to_vec(secrets).map_err(|_| VaultError::Unavailable)?;
    let aad = vault_aad(org_id, item_id);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| VaultError::Unavailable)?;
    let mut out = Vec::with_capacity(SEALED_MAGIC.len() + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(SEALED_MAGIC);
    out.extend_from_slice(&nonce);
    out.extend(ciphertext);
    Ok(out)
}

/// Open sealed secrets of one org's item. Never panics.
pub(crate) fn open(
    key: &[u8; 32],
    org_id: i32,
    item_id: &str,
    sealed: &[u8],
) -> Result<IntegrationSecrets, VaultError> {
    let rest = sealed
        .strip_prefix(SEALED_MAGIC)
        .ok_or(VaultError::Malformed)?;
    let (nonce, ciphertext) = rest
        .split_at_checked(NONCE_LEN)
        .ok_or(VaultError::Malformed)?;
    if ciphertext.is_empty() {
        return Err(VaultError::Malformed);
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| VaultError::Unavailable)?;
    let aad = vault_aad(org_id, item_id);
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| VaultError::Rejected)?;
    serde_json::from_slice(&plaintext).map_err(|_| VaultError::Corrupt)
}

// --- Validation ------------------------------------------------------------

/// `^[a-z0-9][a-z0-9_-]{0,23}$`.
pub(crate) fn valid_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 24
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// Value check shared by import and `update_secrets`: non-empty, at most
/// 8 KiB, printable ASCII only (0x20–0x7E). `Err(reason)`.
pub(crate) fn check_secret_value(value: &str) -> Result<(), &'static str> {
    if value.contains("${") {
        return Err("unresolved_placeholder");
    }
    if value.is_empty()
        || value.len() > MAX_SECRET_VALUE_BYTES
        || !value.bytes().all(|b| (0x20..=0x7e).contains(&b))
    {
        return Err("invalid_secret");
    }
    Ok(())
}

const FORBIDDEN_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "accept",
    "connection",
    "transfer-encoding",
    "te",
    "upgrade",
    "keep-alive",
    "cookie",
    "mcp-session-id",
    "mcp-protocol-version",
    "last-event-id",
];

/// Header name check: `^[A-Za-z0-9][A-Za-z0-9-]{0,63}$`, not hop-by-hop or
/// protocol-owned, not `proxy-*` / `sec-*`.
pub(crate) fn valid_header_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 64
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-')
    {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    !(FORBIDDEN_HEADERS.contains(&lower.as_str())
        || lower.starts_with("proxy-")
        || lower.starts_with("sec-"))
}

const LOCAL_SUFFIXES: &[&str] = &[".local", ".localhost", ".internal", ".lan", ".home.arpa", ".onion"];

/// Validate a remote MCP URL; returns `(normalized url, host)`.
pub(crate) fn validate_remote_url(raw: &str) -> Option<(String, String)> {
    if raw.len() > MAX_URL_LEN {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some_and(|p| p != 443)
    {
        return None;
    }
    let host = match url.host()? {
        url::Host::Domain(domain) => domain.to_string(),
        url::Host::Ipv4(_) | url::Host::Ipv6(_) => return None,
    };
    if host != host.to_ascii_lowercase() || host == "localhost" {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return None;
    }
    for label in &labels {
        let b = label.as_bytes();
        if b.is_empty()
            || b.len() > 63
            || b[0] == b'-'
            || b[b.len() - 1] == b'-'
            || !b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        {
            return None;
        }
    }
    if labels.last()?.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if LOCAL_SUFFIXES.iter().any(|s| host.ends_with(s)) {
        return None;
    }
    Some((url.to_string(), host))
}

// --- Import ------------------------------------------------------------------

/// One parsed integration ready to store. `Debug` prints names only.
pub(crate) struct ParsedIntegration {
    /// The server name as written in the imported file (for skip reports).
    pub(crate) source_name: String,
    pub(crate) spec: IntegrationSpec,
    pub(crate) secrets: IntegrationSecrets,
    /// The legacy-compatible, credential-free `mcp.json` entry.
    pub(crate) legacy: NormalizedMcpServer,
}

impl std::fmt::Debug for ParsedIntegration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedIntegration")
            .field("spec", &self.spec)
            .field("secrets", &self.secrets)
            .finish()
    }
}

#[derive(Debug)]
pub(crate) enum ParsedServer {
    /// Credential-free stdio item, stored exactly as before.
    Legacy(NormalizedMcpServer),
    Integration(ParsedIntegration),
}

impl ParsedServer {
    /// The case-insensitive identity used for duplicate detection.
    pub(crate) fn key(&self) -> String {
        match self {
            Self::Legacy(server) => server.name.to_lowercase(),
            Self::Integration(parsed) => parsed.spec.key.clone(),
        }
    }

    pub(crate) fn display_name(&self) -> &str {
        match self {
            Self::Legacy(server) => &server.name,
            Self::Integration(parsed) => &parsed.source_name,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct ParsedImport {
    pub(crate) servers: Vec<ParsedServer>,
    pub(crate) skipped: Vec<SkippedServer>,
}

const ALLOWED_KEYS: &[&str] = &["type", "command", "args", "env", "url", "headers"];
const IGNORED_KEYS: &[&str] = &["description", "timeout", "disabled"];
const RUNNERS: &[&str] = &["npx", "uvx", "bunx", "pnpm", "pnpx", "yarn", "docker"];

/// Parse an `mcpServers` file. A malformed file is `Invalid`; a bad server is
/// a skip. Legacy stdio entries are validated by `legacy` (the existing rules).
pub(crate) fn parse_import(
    data: &[u8],
    legacy: impl Fn(&str, &str, &[String]) -> bool,
) -> Result<ParsedImport, AuthError> {
    let json: serde_json::Value = serde_json::from_slice(data).map_err(|_| AuthError::Invalid)?;
    let top = json.as_object().ok_or(AuthError::Invalid)?;
    if top.len() != 1 {
        return Err(AuthError::Invalid);
    }
    let servers = top
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .ok_or(AuthError::Invalid)?;
    if servers.is_empty() {
        return Err(AuthError::Invalid);
    }

    let mut out = ParsedImport::default();
    let mut seen: HashSet<String> = HashSet::new();
    for (name, config) in servers {
        let key = name.to_lowercase();
        let result = if !valid_key(&key) {
            Err("invalid_name")
        } else if !seen.insert(key.clone()) {
            Err("duplicate_name")
        } else {
            parse_server(name, &key, config, &legacy)
        };
        match result {
            Ok(mut server) => {
                if let ParsedServer::Integration(parsed) = &mut server {
                    parsed.source_name = name.clone();
                }
                out.servers.push(server)
            }
            Err(reason) => out.skipped.push(SkippedServer {
                name: name.clone(),
                reason: reason.to_string(),
            }),
        }
    }
    Ok(out)
}

fn string_map(
    value: Option<&serde_json::Value>,
) -> Result<BTreeMap<String, String>, &'static str> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let obj = value.as_object().ok_or("invalid_config")?;
    if obj.len() > MAX_SECRETS {
        return Err("invalid_secret");
    }
    let mut map = BTreeMap::new();
    for (k, v) in obj {
        let v = v.as_str().ok_or("invalid_secret")?;
        map.insert(k.clone(), v.to_string());
    }
    // Placeholders first (a clearer reason), then the value rules.
    if map.values().any(|v| v.contains("${")) {
        return Err("unresolved_placeholder");
    }
    for v in map.values() {
        check_secret_value(v)?;
    }
    Ok(map)
}

fn parse_server(
    name: &str,
    key: &str,
    config: &serde_json::Value,
    legacy: &impl Fn(&str, &str, &[String]) -> bool,
) -> Result<ParsedServer, &'static str> {
    let obj = config.as_object().ok_or("invalid_config")?;
    if obj
        .keys()
        .any(|k| !ALLOWED_KEYS.contains(&k.as_str()) && !IGNORED_KEYS.contains(&k.as_str()))
    {
        return Err("unsupported_field");
    }
    let kind = match obj.get("type") {
        None => None,
        Some(v) => Some(v.as_str().ok_or("invalid_config")?),
    };
    let remote = match kind {
        Some("http") | Some("sse") | Some("streamable-http") => true,
        Some("stdio") => false,
        Some(_) => return Err("invalid_config"),
        None => match (obj.contains_key("command"), obj.contains_key("url")) {
            (false, true) => true,
            (true, false) => false,
            _ => return Err("invalid_config"),
        },
    };
    let env = string_map(obj.get("env"))?;
    let headers = string_map(obj.get("headers"))?;
    if remote {
        parse_remote(key, obj, env, headers)
    } else {
        parse_stdio(name, key, obj, env, headers, legacy)
    }
}

fn parse_remote(
    key: &str,
    obj: &serde_json::Map<String, serde_json::Value>,
    env: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
) -> Result<ParsedServer, &'static str> {
    if obj.contains_key("command") || obj.contains_key("args") || !env.is_empty() {
        return Err("invalid_config");
    }
    let raw = obj
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("invalid_config")?;
    if raw.contains("${") {
        return Err("unresolved_placeholder");
    }
    let (url, host) = validate_remote_url(raw).ok_or("invalid_url")?;
    if !headers.keys().all(|name| valid_header_name(name)) {
        return Err("invalid_secret");
    }
    let mut lowered = HashSet::new();
    if !headers.keys().all(|name| lowered.insert(name.to_ascii_lowercase())) {
        return Err("invalid_secret");
    }
    Ok(remote_integration(
        key,
        url.clone(),
        host,
        headers,
        format!("http:{url}"),
        vec![],
    ))
}

fn remote_integration(
    key: &str,
    url: String,
    host: String,
    headers: BTreeMap<String, String>,
    source: String,
    notes: Vec<String>,
) -> ParsedServer {
    ParsedServer::Integration(ParsedIntegration {
        source_name: key.to_string(),
        spec: IntegrationSpec {
            v: SPEC_VERSION,
            key: key.to_string(),
            transport: Transport::Remote,
            url: Some(url.clone()),
            package: None,
            hosts: vec![host],
            secret_env: vec![],
            secret_headers: headers.keys().cloned().collect(),
            source,
            notes,
        },
        secrets: IntegrationSecrets {
            env: BTreeMap::new(),
            headers,
        },
        legacy: NormalizedMcpServer {
            name: key.to_string(),
            config: McpServerConfig::Http { url },
        },
    })
}

fn package_integration(
    key: &str,
    package: &str,
    pinned: &str,
    host: &str,
    env: BTreeMap<String, String>,
    source: &str,
) -> ParsedServer {
    ParsedServer::Integration(ParsedIntegration {
        source_name: key.to_string(),
        spec: IntegrationSpec {
            v: SPEC_VERSION,
            key: key.to_string(),
            transport: Transport::Package,
            url: None,
            package: Some(package.to_string()),
            hosts: vec![host.to_string()],
            secret_env: env.keys().cloned().collect(),
            secret_headers: vec![],
            source: format!("stdio:{source}"),
            notes: vec![],
        },
        secrets: IntegrationSecrets {
            env,
            headers: BTreeMap::new(),
        },
        legacy: NormalizedMcpServer {
            name: key.to_string(),
            config: McpServerConfig::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), pinned.to_string()],
            },
        },
    })
}

/// `@scope/name@1.2.3` → `@scope/name`; `name@latest` → `name`.
fn strip_version(spec: &str) -> &str {
    match spec.rfind('@') {
        Some(index) if index > 0 => &spec[..index],
        _ => spec,
    }
}

/// Only `allowed` env names may appear (`dropped` ones are accepted and
/// discarded); `required` must be present under one of its aliases.
fn catalog_env(
    env: &BTreeMap<String, String>,
    required: (&str, &[&str]),
    dropped: &[&str],
) -> Result<String, &'static str> {
    let (target, aliases) = required;
    if env
        .keys()
        .any(|k| k != target && !aliases.contains(&k.as_str()) && !dropped.contains(&k.as_str()))
    {
        return Err("unsupported_env");
    }
    std::iter::once(target)
        .chain(aliases.iter().copied())
        .find_map(|name| env.get(name).cloned())
        .ok_or("invalid_secret")
}

fn parse_stdio(
    name: &str,
    key: &str,
    obj: &serde_json::Map<String, serde_json::Value>,
    env: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
    legacy: &impl Fn(&str, &str, &[String]) -> bool,
) -> Result<ParsedServer, &'static str> {
    if obj.contains_key("url") || obj.contains_key("headers") || !headers.is_empty() {
        return Err("invalid_config");
    }
    let command = obj
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or("invalid_config")?;
    let args: Vec<String> = match obj.get("args") {
        None => vec![],
        Some(v) => v
            .as_array()
            .ok_or("invalid_config")?
            .iter()
            .map(|a| a.as_str().map(str::to_string).ok_or("invalid_config"))
            .collect::<Result<_, _>>()?,
    };
    if command.contains("${") || args.iter().any(|a| a.contains("${")) {
        return Err("unresolved_placeholder");
    }
    let base = command.rsplit(['/', '\\']).next().unwrap_or(command);
    let runner = RUNNERS.contains(&base);
    let package = runner
        .then(|| {
            args.iter()
                .map(|a| strip_version(a))
                .find(|a| CATALOG.contains(a))
        })
        .flatten();

    match package {
        Some("@modelcontextprotocol/server-github") => {
            let token = catalog_env(&env, ("GITHUB_PERSONAL_ACCESS_TOKEN", &[]), &[])?;
            Ok(bearer_remote(
                key,
                "https://api.githubcopilot.com/mcp/",
                "api.githubcopilot.com",
                &token,
                "stdio:@modelcontextprotocol/server-github",
                GITHUB_NOTE,
            )?)
        }
        Some("@sanity/mcp-server") => {
            let token = catalog_env(
                &env,
                ("SANITY_API_TOKEN", &[]),
                &["SANITY_PROJECT_ID", "SANITY_DATASET", "MCP_USER_ROLE"],
            )?;
            Ok(bearer_remote(
                key,
                "https://mcp.sanity.io",
                "mcp.sanity.io",
                &token,
                "stdio:@sanity/mcp-server",
                SANITY_NOTE,
            )?)
        }
        Some("@hubspot/mcp-server") => {
            let token = catalog_env(
                &env,
                ("PRIVATE_APP_ACCESS_TOKEN", &["HUBSPOT_ACCESS_TOKEN"]),
                &[],
            )?;
            Ok(package_integration(
                key,
                "hubspot",
                "@hubspot/mcp-server@0.4.0",
                "api.hubspot.com",
                BTreeMap::from([("PRIVATE_APP_ACCESS_TOKEN".to_string(), token)]),
                "@hubspot/mcp-server",
            ))
        }
        Some("@suekou/mcp-notion-server") => {
            let token = catalog_env(&env, ("NOTION_API_TOKEN", &[]), &[])?;
            Ok(package_integration(
                key,
                "notion",
                "@suekou/mcp-notion-server@2.0.2",
                "api.notion.com",
                BTreeMap::from([("NOTION_API_TOKEN".to_string(), token)]),
                "@suekou/mcp-notion-server",
            ))
        }
        _ if runner || !env.is_empty() => Err("unsupported_package"),
        _ => {
            if !legacy(name, command, &args) {
                return Err("invalid_config");
            }
            Ok(ParsedServer::Legacy(NormalizedMcpServer {
                name: name.to_string(),
                config: McpServerConfig::Stdio {
                    command: command.to_string(),
                    args,
                },
            }))
        }
    }
}

const CATALOG: &[&str] = &[
    "@modelcontextprotocol/server-github",
    "@sanity/mcp-server",
    "@hubspot/mcp-server",
    "@suekou/mcp-notion-server",
];

fn bearer_remote(
    key: &str,
    url: &str,
    host: &str,
    token: &str,
    source: &str,
    note: &str,
) -> Result<ParsedServer, &'static str> {
    let value = format!("Bearer {token}");
    check_secret_value(&value)?;
    Ok(remote_integration(
        key,
        url.to_string(),
        host.to_string(),
        BTreeMap::from([("Authorization".to_string(), value)]),
        source.to_string(),
        vec![note.to_string()],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets() -> IntegrationSecrets {
        IntegrationSecrets {
            env: BTreeMap::from([("NOTION_API_TOKEN".into(), "synthetic-plain-env-99".into())]),
            headers: BTreeMap::from([("Authorization".into(), "Bearer synthetic-plain-hdr-98".into())]),
        }
    }

    #[test]
    fn vault_round_trips() {
        let key = vault_key(&[7; 32]);
        let sealed = seal(&key, 1, "item-a", &secrets()).unwrap();
        assert!(sealed.starts_with(b"nv1"));
        assert_eq!(open(&key, 1, "item-a", &sealed).unwrap(), secrets());
    }

    #[test]
    fn vault_is_bound_to_org_item_and_key() {
        let key = vault_key(&[7; 32]);
        let sealed = seal(&key, 1, "item-a", &secrets()).unwrap();
        assert_eq!(open(&key, 2, "item-a", &sealed), Err(VaultError::Rejected));
        assert_eq!(open(&key, 1, "item-b", &sealed), Err(VaultError::Rejected));
        assert_eq!(
            open(&vault_key(&[8; 32]), 1, "item-a", &sealed),
            Err(VaultError::Rejected)
        );
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(open(&key, 1, "item-a", &tampered), Err(VaultError::Rejected));
        assert_eq!(open(&key, 1, "item-a", b"nv"), Err(VaultError::Malformed));
        assert_eq!(open(&key, 1, "item-a", b""), Err(VaultError::Malformed));
        assert_eq!(open(&key, 1, "item-a", &sealed[..15]), Err(VaultError::Malformed));
    }

    #[test]
    fn vault_key_is_domain_separated() {
        assert_ne!(vault_key(&[7; 32]), [7; 32]);
        assert_ne!(vault_key(&[7; 32]), vault_key(&[8; 32]));
    }

    #[test]
    fn ciphertext_never_contains_plaintext() {
        let sealed = seal(&vault_key(&[7; 32]), 1, "item-a", &secrets()).unwrap();
        let text = String::from_utf8_lossy(&sealed);
        assert!(!text.contains("synthetic-plain-env-99"));
        assert!(!text.contains("synthetic-plain-hdr-98"));
        assert!(!text.contains("NOTION_API_TOKEN"));
    }

    #[test]
    fn url_rules() {
        assert!(validate_remote_url("https://api.fireflies.ai/mcp").is_some());
        assert!(validate_remote_url("https://api.example.com:443/mcp").is_some());
        for bad in [
            "http://api.example.com/mcp",
            "https://10.0.0.1/mcp",
            "https://169.254.169.254/latest",
            "https://[::1]/mcp",
            "https://localhost/mcp",
            "https://user:pw@api.example.com/mcp",
            "https://api.example.com/mcp?token=1",
            "https://api.example.com/mcp#x",
            "https://api.example.com:8443/mcp",
            "https://printer.local/mcp",
            "https://db.internal/mcp",
            "https://x.home.arpa/",
            "https://abc.onion/",
            "https://intranet/",
            "https://example.123/",
            "not a url",
        ] {
            assert!(validate_remote_url(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn header_and_value_rules() {
        assert!(valid_header_name("Authorization"));
        assert!(valid_header_name("X-Api-Key"));
        for bad in ["Host", "cookie", "Proxy-Authorization", "Sec-Fetch", "Mcp-Session-Id", "-x", "a b", ""] {
            assert!(!valid_header_name(bad), "{bad}");
        }
        assert_eq!(check_secret_value("a\r\nb"), Err("invalid_secret"));
        assert_eq!(check_secret_value("tab\there"), Err("invalid_secret"));
        assert_eq!(check_secret_value("del\u{7f}"), Err("invalid_secret"));
        assert_eq!(check_secret_value("caf\u{e9}"), Err("invalid_secret"));
        assert_eq!(check_secret_value("nul\0"), Err("invalid_secret"));
        assert_eq!(check_secret_value(&"a".repeat(8193)), Err("invalid_secret"));
        assert!(check_secret_value(&"a".repeat(8192)).is_ok());
        assert_eq!(check_secret_value(""), Err("invalid_secret"));
        assert_eq!(check_secret_value("${X}"), Err("unresolved_placeholder"));
        assert!(check_secret_value("Bearer ok").is_ok());
        assert!(valid_key("github"));
        assert!(!valid_key("GitHub"));
        assert!(!valid_key("_x"));
        assert!(!valid_key("abcdefghijklmnopqrstuvwxy"));
    }

    #[test]
    fn version_suffix_is_stripped() {
        assert_eq!(strip_version("@sanity/mcp-server@latest"), "@sanity/mcp-server");
        assert_eq!(strip_version("@hubspot/mcp-server"), "@hubspot/mcp-server");
        assert_eq!(strip_version("pkg@1.0"), "pkg");
    }
}
