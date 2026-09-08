use reqwest::{header, Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

use super::{
    github::{
        GithubEvidenceError, GithubIdentity, GithubMembershipEvidence,
        GithubOrganizationChoice,
    },
    session::TeamRole,
};

const MAX_BODY_BYTES: usize = 1_048_576;
const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 100;

#[derive(Deserialize)]
struct WireUser {
    id: i64,
    login: String,
    #[serde(rename = "type")]
    account_type: String,
    #[serde(default)]
    name: Option<Value>,
    #[serde(default)]
    avatar_url: Option<Value>,
}

#[derive(Deserialize)]
struct WireMembershipUser {
    id: i64,
    login: String,
    #[serde(rename = "type")]
    account_type: String,
}

#[derive(Deserialize)]
struct WireOrganization {
    id: i64,
    login: String,
    #[serde(default)]
    avatar_url: Option<Value>,
}

#[derive(Deserialize)]
struct WireMembership {
    state: String,
    role: String,
    user: WireMembershipUser,
    organization: WireOrganization,
}

enum Endpoint {
    Identity,
    Listing,
    Individual,
}

pub(super) async fn identity(
    client: &Client,
    origin: &Url,
    access_token: &str,
) -> Result<GithubIdentity, GithubEvidenceError> {
    let response = send(client, endpoint_url(origin, &["user"])?, access_token).await?;
    let body = success_body(response, Endpoint::Identity).await?;
    let wire: WireUser =
        serde_json::from_slice(&body).map_err(|_| GithubEvidenceError::Unavailable)?;
    validate_wire_user(wire)
}

pub(super) async fn active_organizations(
    client: &Client,
    origin: &Url,
    access_token: &str,
    expected_user_id: i64,
) -> Result<Vec<GithubOrganizationChoice>, GithubEvidenceError> {
    let mut organizations = Vec::new();
    let mut seen_org_ids = HashSet::new();
    for page in 1..=MAX_PAGES {
        let mut url = endpoint_url(origin, &["user", "memberships", "orgs"])?;
        url.query_pairs_mut()
            .append_pair("state", "active")
            .append_pair("per_page", "100")
            .append_pair("page", &page.to_string());
        let response = send(client, url, access_token).await?;
        let body = success_body(response, Endpoint::Listing).await?;
        let rows: Vec<WireMembership> =
            serde_json::from_slice(&body).map_err(|_| GithubEvidenceError::Unavailable)?;
        let row_count = rows.len();
        if row_count > PAGE_SIZE {
            return Err(GithubEvidenceError::Unavailable);
        }
        for row in rows {
            match validate_membership(row, expected_user_id)? {
                ValidatedMembership::Active(choice) => {
                    if !seen_org_ids.insert(choice.github_org_id) {
                        return Err(GithubEvidenceError::Unavailable);
                    }
                    organizations.push(choice);
                }
                ValidatedMembership::Pending { org_id } => {
                    if !seen_org_ids.insert(org_id) {
                        return Err(GithubEvidenceError::Unavailable);
                    }
                }
            }
        }
        if row_count < PAGE_SIZE {
            organizations.sort_by_key(|organization| organization.github_org_id);
            return Ok(organizations);
        }
    }
    Err(GithubEvidenceError::Unavailable)
}

pub(super) async fn verify_membership(
    client: &Client,
    origin: &Url,
    access_token: &str,
    expected_user_id: i64,
    expected_org_id: i64,
    organization_login: &str,
) -> Result<GithubMembershipEvidence, GithubEvidenceError> {
    let response = send(
        client,
        endpoint_url(
            origin,
            &["user", "memberships", "orgs", organization_login],
        )?,
        access_token,
    )
    .await?;
    let body = success_body(response, Endpoint::Individual).await?;
    let row: WireMembership =
        serde_json::from_slice(&body).map_err(|_| GithubEvidenceError::Unavailable)?;
    let organization = match validate_membership(row, expected_user_id)? {
        ValidatedMembership::Active(choice) => choice,
        ValidatedMembership::Pending { .. } => return Err(GithubEvidenceError::NotMember),
    };
    if organization.github_org_id != expected_org_id
        || !organization.login.eq_ignore_ascii_case(organization_login)
    {
        return Err(GithubEvidenceError::NotMember);
    }
    Ok(GithubMembershipEvidence {
        github_user_id: expected_user_id,
        organization,
    })
}

async fn send(
    client: &Client,
    url: Url,
    access_token: &str,
) -> Result<reqwest::Response, GithubEvidenceError> {
    client
        .get(url)
        .bearer_auth(access_token)
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|_| GithubEvidenceError::Unavailable)
}

fn endpoint_url(origin: &Url, segments: &[&str]) -> Result<Url, GithubEvidenceError> {
    let mut url = origin.clone();
    url.set_query(None);
    url.set_fragment(None);
    let mut path = url
        .path_segments_mut()
        .map_err(|_| GithubEvidenceError::Unavailable)?;
    path.clear();
    for segment in segments {
        path.push(segment);
    }
    drop(path);
    Ok(url)
}

async fn success_body(
    mut response: reqwest::Response,
    endpoint: Endpoint,
) -> Result<Vec<u8>, GithubEvidenceError> {
    if response.status() != StatusCode::OK {
        return Err(status_error(&response, endpoint));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| GithubEvidenceError::Unavailable)?
    {
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(GithubEvidenceError::Unavailable);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn status_error(response: &reqwest::Response, endpoint: Endpoint) -> GithubEvidenceError {
    let status = response.status();
    if status == StatusCode::UNAUTHORIZED {
        return GithubEvidenceError::ReauthenticationRequired;
    }
    let rate_limited = status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN
            && (response
                .headers()
                .get("x-ratelimit-remaining")
                .is_some_and(|value| value.as_bytes() == b"0")
                || response.headers().contains_key(header::RETRY_AFTER)));
    if rate_limited || status.is_server_error() {
        return GithubEvidenceError::Unavailable;
    }
    if matches!(endpoint, Endpoint::Individual)
        && matches!(status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND)
    {
        return GithubEvidenceError::NotMember;
    }
    GithubEvidenceError::Unavailable
}

fn validate_wire_user(wire: WireUser) -> Result<GithubIdentity, GithubEvidenceError> {
    if wire.id <= 0 || wire.account_type != "User" || !super::github::valid_login(&wire.login) {
        return Err(GithubEvidenceError::Unavailable);
    }
    Ok(GithubIdentity {
        github_user_id: wire.id,
        login: wire.login,
        name: optional_name(wire.name),
        avatar_url: optional_avatar(wire.avatar_url),
    })
}

enum ValidatedMembership {
    Active(GithubOrganizationChoice),
    Pending { org_id: i64 },
}

fn validate_membership(
    wire: WireMembership,
    expected_user_id: i64,
) -> Result<ValidatedMembership, GithubEvidenceError> {
    if wire.user.id <= 0
        || wire.user.account_type != "User"
        || !super::github::valid_login(&wire.user.login)
        || wire.organization.id <= 0
        || !super::github::valid_login(&wire.organization.login)
    {
        return Err(GithubEvidenceError::Unavailable);
    }
    if wire.user.id != expected_user_id {
        return Err(GithubEvidenceError::NotMember);
    }
    let role = match wire.role.as_str() {
        "admin" => TeamRole::Admin,
        "member" => TeamRole::Member,
        _ => return Err(GithubEvidenceError::Unavailable),
    };
    match wire.state.as_str() {
        "pending" => {
            return Ok(ValidatedMembership::Pending {
                org_id: wire.organization.id,
            })
        }
        "active" => {}
        _ => return Err(GithubEvidenceError::Unavailable),
    }
    Ok(ValidatedMembership::Active(GithubOrganizationChoice {
        github_org_id: wire.organization.id,
        login: wire.organization.login,
        avatar_url: optional_avatar(wire.organization.avatar_url),
        role,
    }))
}

fn optional_name(value: Option<Value>) -> Option<String> {
    let name = value?.as_str()?.to_owned();
    (name.chars().count() <= 256 && !name.chars().any(char::is_control)).then_some(name)
}

fn optional_avatar(value: Option<Value>) -> Option<String> {
    let value = value?;
    let raw = value.as_str()?;
    let url = Url::parse(raw).ok()?;
    (url.scheme() == "https"
        && url.host_str() == Some("avatars.githubusercontent.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none())
    .then(|| url.to_string())
}
