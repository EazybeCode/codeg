//! Teams configuration: off unless explicitly enabled AND complete.
//!
//! `NEEV_TEAMS_ENABLED` absent or `false` means no Teams route exists, whatever
//! else is set. `true` requires the single-tenant bot credential; a partial set
//! is a startup error rather than a silently absent integration. Which tenants,
//! Teams, channels and people Neev answers is stored in the database (schema 25,
//! `bindings`) and managed by Neev admins — the old mapping file is retired, and
//! setting `NEEV_TEAMS_MAPPING_FILE` is refused so a stale file cannot look
//! like it still grants or narrows anything.

use std::collections::HashMap;

/// Teams' Bot Connector service URL hosts, unless the operator narrows or
/// extends them (sovereign clouds use other hosts).
pub(crate) const DEFAULT_SERVICE_URL_HOSTS: &[&str] = &["smba.trafficmanager.net"];

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TeamsConfigError {
    #[error("NEEV_TEAMS_ENABLED must be exactly true or false")]
    Flag,
    #[error("Teams is enabled but {0} is missing or invalid")]
    Key(&'static str),
    #[error("NEEV_TEAMS_MAPPING_FILE is retired: Teams bindings are managed in Neev (remove the variable)")]
    MappingRetired,
}

#[derive(Clone)]
pub(crate) struct TeamsConfig {
    /// The Azure Bot's Microsoft App ID (Entra application client id).
    pub(crate) bot_app_id: String,
    /// The Entra tenant that owns the single-tenant bot registration.
    pub(crate) bot_tenant_id: String,
    pub(crate) bot_client_secret: String,
    /// Lowercase hostnames a Bot Connector `serviceUrl` may name.
    pub(crate) service_url_hosts: Vec<String>,
    /// `api://…` Application ID URI of the tab SSO scope; `None` disables the
    /// SSO status endpoint (the tab then shows its signed-out launcher).
    pub(crate) tab_app_id_uri: Option<String>,
    /// The separate Graph authority (`graph`) that verifies Team ownership,
    /// membership, guests and channel audience. `None` = not configured: every
    /// channel request and every Team/channel binding fails closed.
    pub(crate) graph: Option<GraphConfig>,
}

/// Credentials of the Entra application whose resource-specific consent the
/// Team owner granted (`TeamMember.Read.Group`, `ChannelSettings.Read.Group`).
/// Deliberately separate variables from the bot's: the operator decides — and
/// the ops guide records — which registration holds Graph consent.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GraphConfig {
    pub(crate) client_id: String,
    pub(crate) client_secret: String,
    /// `NEEV_TEAMS_GRAPH_FILES=true`: the operator states that the tenant admin
    /// APPROVED the file permissions for this app (read: `File.Read.Group` RSC
    /// for channel `filesFolder` discovery; write: a `Sites.Selected` write
    /// grant on the pilot Team's site). Off by default: generated documents
    /// then stay in Neev and the thread says so. Graph still enforces the
    /// actual grants; a refusal fails the publication, never widens it.
    pub(crate) files: bool,
}

impl std::fmt::Debug for GraphConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("files", &self.files)
            .finish()
    }
}

impl std::fmt::Debug for TeamsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamsConfig")
            .field("bot_app_id", &self.bot_app_id)
            .field("bot_tenant_id", &self.bot_tenant_id)
            .field("bot_client_secret", &"<redacted>")
            .field("service_url_hosts", &self.service_url_hosts)
            .field("tab_app_id_uri", &self.tab_app_id_uri)
            .field("graph", &self.graph)
            .finish()
    }
}

/// A Microsoft-style GUID: 8-4-4-4-12 hex, any case, no braces.
pub(crate) fn is_guid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn key<'a>(
    values: &'a HashMap<String, String>,
    name: &'static str,
) -> Result<&'a str, TeamsConfigError> {
    values
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty() && !value.chars().any(char::is_control))
        .ok_or(TeamsConfigError::Key(name))
}

pub(crate) fn teams(
    values: &HashMap<String, String>,
) -> Result<Option<TeamsConfig>, TeamsConfigError> {
    match values.get("NEEV_TEAMS_ENABLED").map(String::as_str) {
        None | Some("false") => return Ok(None),
        Some("true") => {}
        Some(_) => return Err(TeamsConfigError::Flag),
    }
    let bot_app_id = key(values, "NEEV_TEAMS_BOT_APP_ID")?.to_ascii_lowercase();
    if !is_guid(&bot_app_id) {
        return Err(TeamsConfigError::Key("NEEV_TEAMS_BOT_APP_ID"));
    }
    let bot_tenant_id = key(values, "NEEV_TEAMS_BOT_TENANT_ID")?.to_ascii_lowercase();
    if !is_guid(&bot_tenant_id) {
        return Err(TeamsConfigError::Key("NEEV_TEAMS_BOT_TENANT_ID"));
    }
    let bot_client_secret = key(values, "NEEV_TEAMS_BOT_CLIENT_SECRET")?.to_owned();
    if bot_client_secret.len() > 512 || !bot_client_secret.chars().all(|c| c.is_ascii_graphic()) {
        return Err(TeamsConfigError::Key("NEEV_TEAMS_BOT_CLIENT_SECRET"));
    }
    let service_url_hosts = match values.get("NEEV_TEAMS_SERVICE_URL_HOSTS") {
        None => DEFAULT_SERVICE_URL_HOSTS
            .iter()
            .map(|host| (*host).to_owned())
            .collect(),
        Some(list) => {
            let hosts: Vec<String> = list
                .split(',')
                .map(str::trim)
                .filter(|host| !host.is_empty())
                .map(str::to_owned)
                .collect();
            let valid = |host: &String| {
                host.len() <= 253
                    && host.contains('.')
                    && host.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-'
                    })
                    && !host.starts_with('.')
                    && !host.ends_with('.')
            };
            if hosts.is_empty() || !hosts.iter().all(valid) {
                return Err(TeamsConfigError::Key("NEEV_TEAMS_SERVICE_URL_HOSTS"));
            }
            hosts
        }
    };
    let tab_app_id_uri = match values.get("NEEV_TEAMS_TAB_APP_ID_URI") {
        None => None,
        Some(uri) => {
            // The SSO scope must be THIS bot's registration: `api://<host>/<app id>`
            // or `api://<host>/botid-<app id>` (Teams' documented shapes).
            let valid = uri.starts_with("api://")
                && !uri.chars().any(|c| c.is_whitespace() || c.is_control())
                && (uri.ends_with(&format!("/{bot_app_id}"))
                    || uri.ends_with(&format!("/botid-{bot_app_id}")));
            if !valid || uri.len() > 256 {
                return Err(TeamsConfigError::Key("NEEV_TEAMS_TAB_APP_ID_URI"));
            }
            Some(uri.clone())
        }
    };
    let graph = match values.get("NEEV_TEAMS_GRAPH_ENABLED").map(String::as_str) {
        None | Some("false") => None,
        Some("true") => {
            let client_id = key(values, "NEEV_TEAMS_GRAPH_CLIENT_ID")?.to_ascii_lowercase();
            if !is_guid(&client_id) {
                return Err(TeamsConfigError::Key("NEEV_TEAMS_GRAPH_CLIENT_ID"));
            }
            let client_secret = key(values, "NEEV_TEAMS_GRAPH_CLIENT_SECRET")?.to_owned();
            if client_secret.len() > 512 || !client_secret.chars().all(|c| c.is_ascii_graphic()) {
                return Err(TeamsConfigError::Key("NEEV_TEAMS_GRAPH_CLIENT_SECRET"));
            }
            let files = match values.get("NEEV_TEAMS_GRAPH_FILES").map(String::as_str) {
                None | Some("false") => false,
                Some("true") => true,
                Some(_) => return Err(TeamsConfigError::Key("NEEV_TEAMS_GRAPH_FILES")),
            };
            Some(GraphConfig {
                client_id,
                client_secret,
                files,
            })
        }
        Some(_) => return Err(TeamsConfigError::Key("NEEV_TEAMS_GRAPH_ENABLED")),
    };
    // File publication without a Graph authority is a configuration error,
    // not a silently ignored flag.
    if graph.is_none() && values.get("NEEV_TEAMS_GRAPH_FILES").is_some_and(|value| value == "true") {
        return Err(TeamsConfigError::Key("NEEV_TEAMS_GRAPH_FILES"));
    }
    if values.contains_key("NEEV_TEAMS_MAPPING_FILE") {
        return Err(TeamsConfigError::MappingRetired);
    }
    Ok(Some(TeamsConfig {
        bot_app_id,
        bot_tenant_id,
        bot_client_secret,
        service_url_hosts,
        tab_app_id_uri,
        graph,
    }))
}
