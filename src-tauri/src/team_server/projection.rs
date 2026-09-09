use sea_orm::{DatabaseConnection, EntityTrait};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    db::entities::{org, user},
    team_access::{
        core::TeamAuthError,
        onboarding::OnboardingChoices,
        principal::AuthContext,
        session::TeamRole,
    },
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GithubIdentityDto {
    github_user_id: String,
    login: String,
    name: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GithubOrganizationDto {
    github_org_id: String,
    login: String,
    avatar_url: Option<String>,
    role: &'static str,
}

#[derive(Serialize)]
pub(super) struct OnboardingDto {
    identity: GithubIdentityDto,
    organizations: Vec<GithubOrganizationDto>,
    csrf: String,
}

impl OnboardingDto {
    pub(super) fn new(choices: OnboardingChoices, csrf: &str) -> Self {
        Self {
            identity: GithubIdentityDto {
                github_user_id: choices.identity.github_user_id.to_string(),
                login: choices.identity.login,
                name: choices.identity.name,
                avatar_url: choices.identity.avatar_url,
            },
            organizations: choices
                .organizations
                .into_iter()
                .map(|organization| GithubOrganizationDto {
                    github_org_id: organization.github_org_id.to_string(),
                    login: organization.login,
                    avatar_url: organization.avatar_url,
                    role: role_name(organization.role),
                })
                .collect(),
            csrf: csrf.to_owned(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UserDto {
    id: i32,
    login: String,
    name: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OrganizationDto {
    id: i32,
    login: String,
    name: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Serialize)]
pub(super) struct SessionDto {
    user: UserDto,
    organization: OrganizationDto,
    role: &'static str,
    scope: String,
    csrf: String,
}

impl SessionDto {
    pub(super) async fn load(
        db: &DatabaseConnection,
        context: &AuthContext,
        csrf: &str,
    ) -> Result<Self, TeamAuthError> {
        let account = user::Entity::find_by_id(context.user_id)
            .one(db)
            .await
            .map_err(|_| TeamAuthError::Unavailable)?
            .ok_or(TeamAuthError::Unauthorized)?;
        let organization = org::Entity::find_by_id(context.org_id)
            .one(db)
            .await
            .map_err(|_| TeamAuthError::Unavailable)?
            .ok_or(TeamAuthError::Unauthorized)?;
        if account.id != context.user_id || organization.id != context.org_id {
            return Err(TeamAuthError::Unauthorized);
        }
        Ok(Self {
            user: UserDto {
                id: account.id,
                login: account.login,
                name: account.name,
                avatar_url: account.avatar_url,
            },
            organization: OrganizationDto {
                id: organization.id,
                login: organization.login,
                name: organization.name,
                avatar_url: organization.avatar_url,
            },
            role: role_name(context.role),
            scope: format!("{:x}", Sha256::digest(context.session_id.as_bytes())),
            csrf: csrf.to_owned(),
        })
    }
}

pub(super) fn parse_github_id(value: &str) -> Option<i64> {
    if value.is_empty()
        || value.len() > 19
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse::<i64>().ok().filter(|value| *value > 0)
}

fn role_name(role: TeamRole) -> &'static str {
    match role {
        TeamRole::Admin => "admin",
        TeamRole::Member => "member",
    }
}

#[cfg(test)]
mod tests {
    use super::parse_github_id;

    #[test]
    fn github_ids_use_canonical_checked_positive_i64_decimal() {
        assert_eq!(
            parse_github_id("9007199254740992"),
            Some(9_007_199_254_740_992)
        );
        assert_eq!(parse_github_id("9223372036854775807"), Some(i64::MAX));
        for invalid in ["", "0", "01", "+1", "-1", "1.0", "9223372036854775808"] {
            assert_eq!(parse_github_id(invalid), None, "accepted {invalid}");
        }
    }
}
