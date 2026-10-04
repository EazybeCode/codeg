//! Shared, authenticated MCP integrations ("brokered" library MCP items).
//!
//! Types shared by the library (storage, import, admin API, vault) and the
//! runtime (the broker bundle handed to the launcher on stdin). An integration
//! is a `kind='mcp'` library item with three extra files next to its
//! legacy-compatible, credential-free `mcp.json`:
//!
//! * [`SPEC_PATH`] — the non-secret broker spec ([`IntegrationSpec`]);
//! * [`SECRETS_PATH`] — the sealed [`IntegrationSecrets`] (never listed, never
//!   staged, never logged);
//! * [`STATUS_PATH`] — the last admin test ([`IntegrationStatus`]).
//!
//! Agents never receive a secret: their MCP config names only the bridge, and
//! the secrets travel once, on the launcher's stdin, to the broker sidecar
//! (`scripts/operations/neev_mcp_broker.py`), which runs under another uid.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub(crate) const SPEC_PATH: &str = "integration.json";
pub(crate) const SECRETS_PATH: &str = "secrets.enc";
pub(crate) const STATUS_PATH: &str = "status.json";

/// The broker spec version this server writes and accepts.
pub(crate) const SPEC_VERSION: u32 = 1;

/// How the broker reaches the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Transport {
    /// A remote MCP server (Streamable HTTP, or legacy HTTP+SSE by fallback).
    Remote,
    /// A pinned npm package installed in the image (`/opt/neev/mcp`).
    Package,
}

/// Non-secret description of one brokered integration (`integration.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct IntegrationSpec {
    pub(crate) v: u32,
    /// MCP server key the agent sees (`mcp__<key>__<tool>`): `[a-z0-9][a-z0-9_-]{0,23}`,
    /// unique among the org's MCP items.
    pub(crate) key: String,
    pub(crate) transport: Transport,
    /// Remote only: the https URL (no userinfo/query/fragment, port 443).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) url: Option<String>,
    /// Package only: the broker's package id (`hubspot` | `notion`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) package: Option<String>,
    /// Exact hosts the run's slot proxy admits for this integration (1..=4).
    pub(crate) hosts: Vec<String>,
    /// Names (never values) of the sealed env secrets (package only).
    pub(crate) secret_env: Vec<String>,
    /// Names (never values) of the sealed header secrets (remote only).
    pub(crate) secret_headers: Vec<String>,
    /// Where it came from, for the reader: e.g. `stdio:@hubspot/mcp-server` or
    /// `http:https://api.fireflies.ai/mcp`.
    pub(crate) source: String,
    /// Plain-language notes shown to admins and members (conversion caveats).
    pub(crate) notes: Vec<String>,
}

/// The sealed credentials of one integration. `Debug` prints names only.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IntegrationSecrets {
    #[serde(default)]
    pub(crate) env: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) headers: BTreeMap<String, String>,
}

impl std::fmt::Debug for IntegrationSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntegrationSecrets")
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// The last admin test of an integration (`status.json`), shown to members.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IntegrationStatus {
    /// `ready`, `auth_failed`, `forbidden`, `not_found`, `egress_denied`,
    /// `unreachable`, `upstream_error`, `protocol_error`, `session_expired`,
    /// `busy`, `package_unavailable`, `broker_unavailable`, `vault_unavailable`,
    /// `runtime_unavailable`.
    pub(crate) state: String,
    /// Unix seconds, as a decimal string.
    pub(crate) checked_at: String,
    pub(crate) tool_count: Option<u32>,
    /// Non-secret detail, e.g. `HTTP 401`.
    pub(crate) detail: Option<String>,
    /// The item revision the test ran against (a stale result is never shown
    /// as current).
    pub(crate) revision: String,
}

/// One enabled brokered integration as the runtime receives it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct BrokeredIntegration {
    pub(crate) spec: IntegrationSpec,
    /// `None` when the sealed secrets could not be opened (`vault_unavailable`):
    /// the run proceeds without this integration and says so.
    pub(crate) secrets: Option<IntegrationSecrets>,
}

impl std::fmt::Debug for BrokeredIntegration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokeredIntegration")
            .field("spec", &self.spec)
            .field("secrets", &self.secrets)
            .finish()
    }
}

/// An admin's test of one org's integrations, run through the real runtime path.
#[derive(Debug, Clone)]
pub(crate) struct IntegrationCheckRequest {
    pub(crate) user_id: i32,
    pub(crate) org_id: i32,
    pub(crate) integrations: Vec<BrokeredIntegration>,
}

/// One integration's test outcome (`state` as in [`IntegrationStatus`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IntegrationCheckResult {
    pub(crate) key: String,
    pub(crate) state: String,
    pub(crate) tool_count: Option<u32>,
    pub(crate) http_status: Option<u16>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_debug_never_prints_values() {
        let mut secrets = IntegrationSecrets::default();
        secrets
            .headers
            .insert("Authorization".into(), "Bearer synthetic-value-123".into());
        secrets
            .env
            .insert("NOTION_API_TOKEN".into(), "synthetic-env-456".into());
        let text = format!(
            "{:?}",
            BrokeredIntegration {
                spec: IntegrationSpec {
                    v: SPEC_VERSION,
                    key: "k".into(),
                    transport: Transport::Remote,
                    url: Some("https://api.example.com/mcp".into()),
                    package: None,
                    hosts: vec!["api.example.com".into()],
                    secret_env: vec![],
                    secret_headers: vec!["Authorization".into()],
                    source: "test".into(),
                    notes: vec![],
                },
                secrets: Some(secrets),
            }
        );
        assert!(text.contains("Authorization"));
        assert!(!text.contains("synthetic-value-123"));
        assert!(!text.contains("synthetic-env-456"));
    }

    #[test]
    fn spec_round_trips_and_rejects_unknown_fields() {
        let json = r#"{"v":1,"key":"hubspot","transport":"package","package":"hubspot","hosts":["api.hubspot.com"],"secretEnv":["PRIVATE_APP_ACCESS_TOKEN"],"secretHeaders":[],"source":"stdio:@hubspot/mcp-server","notes":[]}"#;
        let spec: IntegrationSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.transport, Transport::Package);
        assert_eq!(serde_json::to_string(&spec).unwrap(), json);
        assert!(serde_json::from_str::<IntegrationSpec>(
            &json.replace("\"notes\":[]", "\"notes\":[],\"command\":\"sh\"")
        )
        .is_err());
    }
}
