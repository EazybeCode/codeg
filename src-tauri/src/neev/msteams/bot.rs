//! The bot's transport half, which does not touch Neev storage: the recipient
//! check and the Bot Connector client. Replies are not HTTP responses in the
//! Bot Framework; they are POSTs back to the activity's `serviceUrl`, signed
//! with a client-credentials token for the single-tenant bot registration.
//! (Where a message may go and who sent it is `access`, from the database.)

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;

use super::activity::Envelope;
use super::codes::Refusal;
use super::config::TeamsConfig;

/// The Bot Framework id of our bot in Teams is `28:<app id>`.
pub(crate) fn check_recipient(envelope: &Envelope, bot_app_id: &str) -> Result<(), Refusal> {
    let addressed = envelope
        .bot_id
        .strip_prefix("28:")
        .unwrap_or(&envelope.bot_id)
        .eq_ignore_ascii_case(bot_app_id);
    if addressed {
        Ok(())
    } else {
        Err(Refusal::WrongBot)
    }
}

/// A `serviceUrl` we will send a bot token to: HTTPS, no credentials, and a
/// host the operator allows. The connector token already binds `serviceurl`,
/// so this is defence in depth against a mis-scoped registration.
pub(crate) fn service_url_allowed(service_url: &str, hosts: &[String]) -> bool {
    let Ok(parsed) = url::Url::parse(service_url) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.host_str().is_some_and(|host| {
            hosts
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(host))
        })
}

/// `{serviceUrl}/v3/conversations/{id}/activities/{replyTo}`.
pub(crate) fn reply_url(service_url: &str, conversation_id: &str, reply_to_id: &str) -> String {
    format!(
        "{}/v3/conversations/{}/activities/{}",
        service_url.trim_end_matches('/'),
        urlencoding::encode(conversation_id),
        urlencoding::encode(reply_to_id),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ConnectorError {
    #[error("bot token unavailable")]
    Token,
    #[error("serviceUrl not allowed")]
    ServiceUrl,
    /// A definite refusal (4xx other than 429): retrying cannot help.
    #[error("Bot Connector refused the send ({0})")]
    Refused(u16),
    /// 429 or 5xx: nothing was posted; retry after the given delay.
    #[error("Bot Connector asked to retry ({status})")]
    Retry {
        status: u16,
        retry_after_secs: Option<u64>,
    },
    /// The connection could not be made: nothing was posted.
    #[error("Bot Connector unreachable")]
    Unreachable,
    /// The request may have been posted (timeout or broken connection after
    /// sending): delivery is at-least-once from here.
    #[error("Bot Connector outcome unknown")]
    Uncertain,
    /// A success answer Neev could not read.
    #[error("Bot Connector answer unreadable")]
    Invalid,
}

/// Seconds from a numeric `Retry-After` header, capped at an hour.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| secs.min(3600))
}

fn transport_error(error: &reqwest::Error) -> ConnectorError {
    if error.is_connect() || error.is_builder() {
        ConnectorError::Unreachable
    } else {
        ConnectorError::Uncertain
    }
}

/// `{serviceUrl}/v3/teams/{teamId}`.
pub(crate) fn team_url(service_url: &str, team_id: &str) -> String {
    format!(
        "{}/v3/teams/{}",
        service_url.trim_end_matches('/'),
        urlencoding::encode(team_id),
    )
}

const CONNECTOR_SCOPE: &str = "https://api.botframework.com/.default";
const SEND_TIMEOUT: Duration = Duration::from_secs(15);
/// Refresh a cached token this long before Entra says it expires.
const TOKEN_MARGIN: Duration = Duration::from_secs(5 * 60);

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

/// Outbound half of the bot: client-credentials token + send.
pub(crate) struct Connector {
    client: reqwest::Client,
    pub(crate) token_url: String,
    app_id: String,
    client_secret: String,
    pub(crate) service_url_hosts: Vec<String>,
    token: Mutex<Option<(String, Instant)>>,
    /// Plain-HTTP loopback service URLs are accepted (tests only).
    #[cfg(test)]
    pub(crate) allow_loopback: bool,
}

impl Connector {
    pub(crate) fn new(config: &TeamsConfig) -> Result<Self, ConnectorError> {
        let client = reqwest::Client::builder()
            .timeout(SEND_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ConnectorError::Unreachable)?;
        Ok(Self {
            client,
            // Single-tenant bot: tokens come from the registration's own tenant.
            token_url: format!(
                "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
                config.bot_tenant_id
            ),
            app_id: config.bot_app_id.clone(),
            client_secret: config.bot_client_secret.clone(),
            service_url_hosts: config.service_url_hosts.clone(),
            token: Mutex::new(None),
            #[cfg(test)]
            allow_loopback: false,
        })
    }

    async fn token(&self) -> Result<String, ConnectorError> {
        let mut cached = self.token.lock().await;
        if let Some((token, valid_until)) = cached.as_ref() {
            if Instant::now() < *valid_until {
                return Ok(token.clone());
            }
        }
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "client_credentials")
            .append_pair("client_id", &self.app_id)
            .append_pair("client_secret", &self.client_secret)
            .append_pair("scope", CONNECTOR_SCOPE)
            .finish();
        let response = self
            .client
            .post(&self.token_url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await
            .map_err(|_| ConnectorError::Token)?;
        if !response.status().is_success() {
            return Err(ConnectorError::Token);
        }
        let parsed: TokenResponse = response.json().await.map_err(|_| ConnectorError::Token)?;
        if parsed.access_token.is_empty() {
            return Err(ConnectorError::Token);
        }
        let lifetime = Duration::from_secs(parsed.expires_in).saturating_sub(TOKEN_MARGIN);
        *cached = Some((parsed.access_token.clone(), Instant::now() + lifetime));
        Ok(parsed.access_token)
    }

    fn allowed(&self, service_url: &str) -> bool {
        #[cfg(test)]
        if self.allow_loopback && service_url.starts_with("http://127.0.0.1:") {
            return true;
        }
        service_url_allowed(service_url, &self.service_url_hosts)
    }

    /// Reply in the conversation (and thread) of the activity `reply_to_id`.
    /// Returns the id Teams gave the posted message, when it reports one.
    pub(crate) async fn reply(
        &self,
        service_url: &str,
        conversation_id: &str,
        reply_to_id: &str,
        activity: &Value,
    ) -> Result<Option<String>, ConnectorError> {
        if !self.allowed(service_url) {
            return Err(ConnectorError::ServiceUrl);
        }
        let token = self.token().await?;
        let response = self
            .client
            .post(reply_url(service_url, conversation_id, reply_to_id))
            .bearer_auth(token)
            .json(activity)
            .send()
            .await
            .map_err(|error| transport_error(&error))?;
        let status = response.status();
        if status.is_success() {
            // `ResourceResponse { id }`; an empty body is still a delivery.
            let body = response.bytes().await.map_err(|_| ConnectorError::Uncertain)?;
            let id = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_owned))
                .filter(|id| !id.is_empty() && id.len() <= 256);
            return Ok(id);
        }
        Err(self.failure(status, response.headers()).await)
    }

    async fn failure(&self, status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap) -> ConnectorError {
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // A revoked or rotated secret: drop the cached token so the
            // next send asks Entra again instead of failing until expiry.
            *self.token.lock().await = None;
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            ConnectorError::Retry {
                status: status.as_u16(),
                retry_after_secs: retry_after(headers),
            }
        } else {
            ConnectorError::Refused(status.as_u16())
        }
    }

    /// The Entra group id (`aadGroupId`) of a Team the bot is installed in.
    pub(crate) async fn team_group_id(&self, service_url: &str, team_id: &str) -> Result<String, ConnectorError> {
        if !self.allowed(service_url) {
            return Err(ConnectorError::ServiceUrl);
        }
        let token = self.token().await?;
        let response = self
            .client
            .get(team_url(service_url, team_id))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|error| transport_error(&error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(self.failure(status, response.headers()).await);
        }
        let value: Value = response.json().await.map_err(|_| ConnectorError::Invalid)?;
        if value.get("id").and_then(Value::as_str) != Some(team_id) {
            return Err(ConnectorError::Invalid);
        }
        value
            .get("aadGroupId")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
            .filter(|group| super::config::is_guid(group))
            .ok_or(ConnectorError::Invalid)
    }
}
