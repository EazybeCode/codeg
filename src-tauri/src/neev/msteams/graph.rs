//! Microsoft Graph authority for Teams authorization facts Neev cannot learn
//! from a bot activity: whether a person is an owner, member or guest of a
//! Team, and whether a channel is a standard channel with no guests or
//! members from other tenants.
//!
//! A SEPARATE authority from the bot: its own Entra application credential
//! (`NEEV_TEAMS_GRAPH_*`), its own client-credentials token for the Graph
//! audience, requested from the tenant that installed the app. The Bot
//! Connector token (`bot::Connector`) is never sent to Graph and this token is
//! never sent to the Bot Connector.
//!
//! Only resource-specific-consent (RSC) application permissions are needed,
//! each granted per Team by that Team's owner when the app is installed:
//! - `TeamMember.Read.Group` — `GET /teams/{group}/members?$filter=userId`
//!   (owner / member / guest role and member tenant);
//! - `ChannelSettings.Read.Group` — `GET /teams/{group}/channels/{channel}`
//!   (`membershipType`, `isArchived`, `summary`).
//! No tenant-wide permission is requested. Without the configuration, or when
//! Graph refuses, throttles or answers unexpectedly, every caller fails CLOSED.
//! Response bodies and URLs are never logged or returned.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;

use super::config::{is_guid, GraphConfig};

const GRAPH_SCOPE: &str = "https://graph.microsoft.com/.default";
const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
const TIMEOUT: Duration = Duration::from_secs(15);
const TOKEN_MARGIN: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum GraphError {
    /// No Graph authority is configured: the fact cannot be verified.
    #[error("Graph authority is not configured")]
    Disabled,
    /// Token refused, or Graph answered 401/403 (consent missing or revoked).
    #[error("Graph refused the request")]
    Denied,
    /// The Team or channel does not exist (or is not visible to this app).
    #[error("Graph resource not found")]
    NotFound,
    /// Throttled, a server error, a timeout or an unreadable answer.
    #[error("Graph unavailable")]
    Unavailable,
}

/// A person's standing in a Team, from Graph's conversationMember roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Membership {
    Owner,
    Member,
    /// `roles` contains `guest` (B2B guest of the tenant).
    Guest,
    /// A member whose `tenantId` is not the Team's tenant.
    External,
    /// Not a member of the Team.
    Absent,
}


/// What Graph says about a channel's audience.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChannelAudience {
    pub(crate) membership_type: String,
    pub(crate) archived: bool,
    /// `None` when Graph did not report a summary (unknown).
    pub(crate) guests: Option<u64>,
    pub(crate) external_members: Option<bool>,
}

impl ChannelAudience {
    /// The pilot audience: a standard, live channel with no guests and no
    /// members from other tenants. Anything unknown or different is refused —
    /// Microsoft adds channel kinds over time, so this is an exact allow-list.
    pub(crate) fn pilot_eligible(&self) -> bool {
        self.membership_type == "standard"
            && !self.archived
            && self.guests == Some(0)
            && self.external_members == Some(false)
    }
}

/// Parse `GET /teams/{group}/members?$filter=userId eq '<oid>'`.
///
/// Fail closed on anything not fully understood:
/// - a reply with `@odata.nextLink` is incomplete and is refused outright (a
///   later page could contradict a match; nextLink is never followed with a
///   bearer token);
/// - every role must be a known string (`owner`, `guest`); an unknown or
///   non-string role is refused even next to `owner`;
/// - the member's `tenantId` must be PRESENT, a GUID, and the Team's tenant
///   to classify as internal; a missing, null, malformed or other tenant is
///   never an owner or member.
pub(crate) fn membership_from(value: &Value, team_tenant: &str, object_id: &str) -> Result<Membership, GraphError> {
    if value.get("@odata.nextLink").is_some() {
        return Err(GraphError::Unavailable);
    }
    let members = value.get("value").and_then(Value::as_array).ok_or(GraphError::Unavailable)?;
    let mut found: Option<Membership> = None;
    for member in members {
        let user = member.get("userId").and_then(Value::as_str).unwrap_or_default();
        if !user.eq_ignore_ascii_case(object_id) {
            continue;
        }
        let mut owner = false;
        let mut guest = false;
        for role in member.get("roles").and_then(Value::as_array).ok_or(GraphError::Unavailable)? {
            match role.as_str().map(str::to_ascii_lowercase).as_deref() {
                Some("owner") => owner = true,
                Some("guest") => guest = true,
                // A role this code does not know is not proof of anything.
                _ => return Err(GraphError::Unavailable),
            }
        }
        let tenant = member
            .get("tenantId")
            .and_then(Value::as_str)
            .filter(|tenant| is_guid(tenant));
        let standing = if guest {
            Membership::Guest
        } else {
            match tenant {
                Some(tenant) if tenant.eq_ignore_ascii_case(team_tenant) => {
                    if owner {
                        Membership::Owner
                    } else {
                        Membership::Member
                    }
                }
                // Another, missing or unreadable tenancy is never internal.
                _ => Membership::External,
            }
        };
        match found {
            Some(previous) if previous != standing => return Err(GraphError::Unavailable),
            _ => found = Some(standing),
        }
    }
    Ok(found.unwrap_or(Membership::Absent))
}

/// Parse `GET /teams/{group}/channels/{channel}` (with `summary` selected).
pub(crate) fn audience_from(channel: &Value, channel_id: &str, summary: &Value) -> Result<ChannelAudience, GraphError> {
    let id = channel.get("id").and_then(Value::as_str).ok_or(GraphError::Unavailable)?;
    if id != channel_id {
        return Err(GraphError::Unavailable);
    }
    // The summary read must describe the same channel when it names one.
    if summary.get("id").is_some_and(|other| other.as_str() != Some(channel_id)) {
        return Err(GraphError::Unavailable);
    }
    let summary = summary.get("summary").filter(|summary| summary.is_object());
    Ok(ChannelAudience {
        membership_type: channel
            .get("membershipType")
            .and_then(Value::as_str)
            .ok_or(GraphError::Unavailable)?
            .to_owned(),
        archived: channel.get("isArchived").and_then(Value::as_bool).unwrap_or(true),
        guests: summary.and_then(|summary| summary.get("guestsCount")).and_then(Value::as_u64),
        external_members: summary
            .and_then(|summary| summary.get("hasMembersFromOtherTenants"))
            .and_then(Value::as_bool),
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

fn segment(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

/// The production Graph client. Tokens are cached per tenant.
pub(crate) struct GraphAuthority {
    client: reqwest::Client,
    client_id: String,
    client_secret: String,
    pub(crate) login_base: String,
    pub(crate) graph_base: String,
    tokens: Mutex<HashMap<String, (String, Instant)>>,
}

impl GraphAuthority {
    pub(crate) fn new(config: &GraphConfig) -> Result<Self, GraphError> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| GraphError::Unavailable)?,
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            login_base: "https://login.microsoftonline.com".to_owned(),
            graph_base: GRAPH_BASE.to_owned(),
            tokens: Mutex::new(HashMap::new()),
        })
    }

    /// A Graph-audience app token for the installing tenant (cached). Its
    /// effective permissions are exactly what that tenant's admin granted.
    pub(crate) async fn token(&self, tenant_id: &str) -> Result<String, GraphError> {
        if !is_guid(tenant_id) {
            return Err(GraphError::Denied);
        }
        let mut tokens = self.tokens.lock().await;
        if let Some((token, valid_until)) = tokens.get(tenant_id) {
            if Instant::now() < *valid_until {
                return Ok(token.clone());
            }
        }
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "client_credentials")
            .append_pair("client_id", &self.client_id)
            .append_pair("client_secret", &self.client_secret)
            .append_pair("scope", GRAPH_SCOPE)
            .finish();
        let response = self
            .client
            .post(format!("{}/{tenant_id}/oauth2/v2.0/token", self.login_base))
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|_| GraphError::Unavailable)?;
        let status = response.status();
        if status.is_client_error() {
            return Err(GraphError::Denied);
        }
        if !status.is_success() {
            return Err(GraphError::Unavailable);
        }
        let parsed: TokenResponse = response.json().await.map_err(|_| GraphError::Unavailable)?;
        if parsed.access_token.is_empty() {
            return Err(GraphError::Unavailable);
        }
        let lifetime = Duration::from_secs(parsed.expires_in).saturating_sub(TOKEN_MARGIN);
        tokens.insert(tenant_id.to_owned(), (parsed.access_token.clone(), Instant::now() + lifetime));
        Ok(parsed.access_token)
    }

    async fn get(&self, tenant_id: &str, path: &str, query: &[(&str, &str)]) -> Result<Value, GraphError> {
        let token = self.token(tenant_id).await?;
        let response = self
            .client
            .get(format!("{}{path}", self.graph_base))
            .query(query)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| GraphError::Unavailable)?;
        match response.status().as_u16() {
            200 => response.json().await.map_err(|_| GraphError::Unavailable),
            401 => {
                // A rotated secret or revoked consent: ask Entra again next time.
                self.tokens.lock().await.remove(tenant_id);
                Err(GraphError::Denied)
            }
            403 => Err(GraphError::Denied),
            404 => Err(GraphError::NotFound),
            _ => Err(GraphError::Unavailable),
        }
    }

    /// A person's standing in a Team (`group_id` is the Team's Entra group id).
    pub(crate) async fn membership(&self, tenant_id: &str, group_id: &str, object_id: &str) -> Result<Membership, GraphError> {
        if !is_guid(group_id) || !is_guid(object_id) {
            return Err(GraphError::NotFound);
        }
        let filter = format!("(microsoft.graph.aadUserConversationMember/userId eq '{object_id}')");
        let value = self
            .get(tenant_id, &format!("/teams/{}/members", segment(group_id)), &[("$filter", &filter)])
            .await?;
        membership_from(&value, tenant_id, object_id)
    }

    /// A channel's audience facts.
    pub(crate) async fn channel_audience(&self, tenant_id: &str, group_id: &str, channel_id: &str) -> Result<ChannelAudience, GraphError> {
        if !is_guid(group_id) || channel_id.is_empty() || channel_id.len() > 256 {
            return Err(GraphError::NotFound);
        }
        let path = format!("/teams/{}/channels/{}", segment(group_id), segment(channel_id));
        let channel = self
            .get(tenant_id, &path, &[("$select", "id,membershipType,isArchived")])
            .await?;
        let summary = self.get(tenant_id, &path, &[("$select", "id,summary")]).await?;
        audience_from(&channel, channel_id, &summary)
    }
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
