use std::time::Duration;

use async_trait::async_trait;
use reqwest::{redirect::Policy, StatusCode};
use url::Url;

use crate::neev::{auth::email::canonical_email, error::AuthError};

use super::{MailMessage, MailSender};

const ENDPOINT: &str = "https://api.resend.com/emails";
const APPROVED_FROM: &str = "verification@neev.so";

pub(crate) struct Resend {
    client: reqwest::Client,
    api_key: String,
    from: String,
    endpoint: Url,
}

impl Resend {
    pub(crate) fn new(api_key: String, from: String) -> Result<Self, AuthError> {
        Self::with_endpoint(
            api_key,
            from,
            Url::parse(ENDPOINT).map_err(|_| AuthError::Unavailable)?,
        )
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        api_key: String,
        from: String,
        endpoint: Url,
    ) -> Result<Self, AuthError> {
        if endpoint.scheme() != "http"
            || !endpoint
                .host_str()
                .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback())
        {
            return Err(AuthError::Invalid);
        }
        Self::with_endpoint(api_key, from, endpoint)
    }

    fn with_endpoint(api_key: String, from: String, endpoint: Url) -> Result<Self, AuthError> {
        if api_key.trim().is_empty()
            || api_key.len() > 512
            || api_key.chars().any(char::is_control)
            || from != APPROVED_FROM
        {
            return Err(AuthError::Invalid);
        }
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| AuthError::Unavailable)?;
        Ok(Self {
            client,
            api_key,
            from: format!("Neev <{from}>"),
            endpoint,
        })
    }
}

#[async_trait]
impl MailSender for Resend {
    async fn send(&self, message: MailMessage) -> Result<(), AuthError> {
        if canonical_email(&message.recipient).as_deref() != Some(message.recipient.as_str())
            || message.subject.is_empty()
            || message.subject.len() > 200
            || message.subject.chars().any(char::is_control)
            || message.text.is_empty()
            || message.text.len() > 32 * 1024
            || message
                .text
                .chars()
                .any(|character| character.is_control() && character != '\n' && character != '\t')
        {
            return Err(AuthError::Invalid);
        }
        let response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "from": self.from,
                "to": message.recipient,
                "subject": message.subject,
                "text": message.text,
            }))
            .send()
            .await
            .map_err(|_| AuthError::Unavailable)?;
        if response.status().is_success() {
            Ok(())
        } else if response.status() == StatusCode::TOO_MANY_REQUESTS {
            Err(AuthError::Limited)
        } else {
            Err(AuthError::Unavailable)
        }
    }
}
