use std::time::Duration;

use reqwest::Url;

use super::session::TeamRole;

const GITHUB_ORIGIN: &str = "https://api.github.com";
const PRODUCTION_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum GithubEvidenceError {
    #[error("GitHub reauthentication required")]
    ReauthenticationRequired,
    #[error("GitHub organization membership not found")]
    NotMember,
    #[error("GitHub membership evidence unavailable")]
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GithubIdentity {
    pub github_user_id: i64,
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GithubOrganizationChoice {
    pub github_org_id: i64,
    pub login: String,
    pub avatar_url: Option<String>,
    pub role: TeamRole,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GithubMembershipEvidence {
    pub github_user_id: i64,
    pub organization: GithubOrganizationChoice,
}

#[async_trait::async_trait]
pub(crate) trait GithubMembershipSource: Send + Sync {
    async fn identity(
        &self,
        access_token: &str,
    ) -> Result<GithubIdentity, GithubEvidenceError>;

    async fn active_organizations(
        &self,
        access_token: &str,
        expected_user_id: i64,
    ) -> Result<Vec<GithubOrganizationChoice>, GithubEvidenceError>;

    async fn verify_membership(
        &self,
        access_token: &str,
        expected_user_id: i64,
        expected_org_id: i64,
        organization_login: &str,
    ) -> Result<GithubMembershipEvidence, GithubEvidenceError>;
}

pub(crate) struct GithubMembershipClient {
    client: reqwest::Client,
    origin: Url,
    operation_timeout: Duration,
}

impl GithubMembershipClient {
    pub(crate) fn new() -> Result<Self, GithubEvidenceError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(PRODUCTION_TIMEOUT)
            .user_agent("EazyWorkOS")
            .build()
            .map_err(|_| GithubEvidenceError::Unavailable)?;
        let origin = Url::parse(GITHUB_ORIGIN).map_err(|_| GithubEvidenceError::Unavailable)?;
        Ok(Self {
            client,
            origin,
            operation_timeout: PRODUCTION_TIMEOUT,
        })
    }

    #[cfg(test)]
    pub(super) fn new_for_test(
        origin: &str,
        timeout: Duration,
    ) -> Result<Self, GithubEvidenceError> {
        let origin = Url::parse(origin).map_err(|_| GithubEvidenceError::Unavailable)?;
        if origin.scheme() != "http"
            || !origin
                .host_str()
                .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                .is_some_and(|address| address.is_loopback())
        {
            return Err(GithubEvidenceError::Unavailable);
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .user_agent("EazyWorkOS")
            .no_proxy()
            .build()
            .map_err(|_| GithubEvidenceError::Unavailable)?;
        Ok(Self {
            client,
            origin,
            operation_timeout: timeout,
        })
    }
}

#[async_trait::async_trait]
impl GithubMembershipSource for GithubMembershipClient {
    async fn identity(
        &self,
        access_token: &str,
    ) -> Result<GithubIdentity, GithubEvidenceError> {
        validate_token(access_token)?;
        tokio::time::timeout(
            self.operation_timeout,
            super::github_http::identity(&self.client, &self.origin, access_token),
        )
        .await
        .map_err(|_| GithubEvidenceError::Unavailable)?
    }

    async fn active_organizations(
        &self,
        access_token: &str,
        expected_user_id: i64,
    ) -> Result<Vec<GithubOrganizationChoice>, GithubEvidenceError> {
        validate_token(access_token)?;
        if expected_user_id <= 0 {
            return Err(GithubEvidenceError::NotMember);
        }
        tokio::time::timeout(
            self.operation_timeout,
            super::github_http::active_organizations(
                &self.client,
                &self.origin,
                access_token,
                expected_user_id,
            ),
        )
        .await
        .map_err(|_| GithubEvidenceError::Unavailable)?
    }

    async fn verify_membership(
        &self,
        access_token: &str,
        expected_user_id: i64,
        expected_org_id: i64,
        organization_login: &str,
    ) -> Result<GithubMembershipEvidence, GithubEvidenceError> {
        validate_token(access_token)?;
        if expected_user_id <= 0
            || expected_org_id <= 0
            || !valid_login(organization_login)
        {
            return Err(GithubEvidenceError::NotMember);
        }
        tokio::time::timeout(
            self.operation_timeout,
            super::github_http::verify_membership(
                &self.client,
                &self.origin,
                access_token,
                expected_user_id,
                expected_org_id,
                organization_login,
            ),
        )
        .await
        .map_err(|_| GithubEvidenceError::Unavailable)?
    }
}

fn validate_token(access_token: &str) -> Result<(), GithubEvidenceError> {
    if access_token.trim().is_empty()
        || access_token
            .bytes()
            .any(|byte| matches!(byte, b'\r' | b'\n'))
    {
        Err(GithubEvidenceError::ReauthenticationRequired)
    } else {
        Ok(())
    }
}

pub(super) fn valid_login(login: &str) -> bool {
    let bytes = login.as_bytes();
    (1..=39).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        && !bytes.windows(2).any(|pair| pair == b"--")
}
