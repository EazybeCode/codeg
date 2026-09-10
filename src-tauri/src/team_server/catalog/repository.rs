use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{redirect, Client, Response, StatusCode, Url};
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use serde::Deserialize;

use super::{
    authority::{admission, admit, begin_authorized, commit_rechecked, REPOSITORY},
    types::{valid_positive_id, ConnectRepositoryInput, RepositoryDto},
};
use crate::{
    db::entities::{team_catalog_admission, user, workspace_repository},
    team_access::{credentials::CredentialService, principal::AuthContext},
    team_server::{handlers::ApiError, state::TeamState},
    workspace_catalog::{validate_branch, validate_repository_full_name},
};

#[allow(dead_code)]
const GITHUB_API_ORIGIN: &str = "https://api.github.com";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const CATALOG_CAP: u64 = 1000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::team_server) struct VerifiedRepository {
    pub(in crate::team_server) github_repository_id: String,
    pub(in crate::team_server) full_name: String,
    pub(in crate::team_server) starting_branch: String,
}

#[async_trait]
pub(in crate::team_server) trait TeamRepositoryAccess: Send + Sync {
    async fn verify(
        &self,
        context: &AuthContext,
        full_name: &str,
        branch: &str,
    ) -> Result<VerifiedRepository, ApiError>;
}

pub(in crate::team_server) struct GithubTeamRepositoryAccess {
    db: sea_orm::DatabaseConnection,
    credentials: Arc<CredentialService>,
    client: Client,
    origin: Url,
}

impl GithubTeamRepositoryAccess {
    #[allow(dead_code)]
    pub(in crate::team_server) fn new(state: &TeamState) -> Result<Self, ApiError> {
        Self::with_origin(state, GITHUB_API_ORIGIN)
    }

    fn with_origin(state: &TeamState, origin: &str) -> Result<Self, ApiError> {
        Ok(Self {
            db: state.db.clone(),
            credentials: state.credentials.clone(),
            client: Client::builder()
                .timeout(Duration::from_secs(15))
                .redirect(redirect::Policy::none())
                .user_agent("Eazybe-WorkOS")
                .build()
                .map_err(|_| ApiError::Unavailable)?,
            origin: Url::parse(origin).map_err(|_| ApiError::Unavailable)?,
        })
    }

    #[cfg(test)]
    pub(super) fn new_for_test(state: &TeamState, origin: &str) -> Result<Self, ApiError> {
        Self::with_origin(state, origin)
    }

    fn endpoint(&self, segments: &[&str]) -> Result<Url, ApiError> {
        let mut url = self.origin.clone();
        {
            let mut path = url.path_segments_mut().map_err(|_| ApiError::Unavailable)?;
            path.clear();
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        token: &str,
        url: Url,
    ) -> Result<T, ApiError> {
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|_| ApiError::Unavailable)?;
        map_response_status(&response)?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(ApiError::Unavailable);
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ApiError::Unavailable)?;
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(ApiError::Unavailable);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| ApiError::Unavailable)
    }
}

#[derive(Deserialize)]
struct RepositoryResponse {
    id: u64,
    full_name: String,
}

#[derive(Deserialize)]
struct BranchResponse {
    name: String,
}

#[async_trait]
impl TeamRepositoryAccess for GithubTeamRepositoryAccess {
    async fn verify(
        &self,
        context: &AuthContext,
        full_name: &str,
        branch: &str,
    ) -> Result<VerifiedRepository, ApiError> {
        let (owner, repository) = validate_names(full_name, branch)?;
        let github_user_id = user::Entity::find_by_id(context.user_id)
            .one(&self.db)
            .await
            .map_err(|_| ApiError::Unavailable)?
            .map(|row| row.github_user_id)
            .filter(|id| *id > 0)
            .ok_or(ApiError::Unavailable)?;
        let grant = self
            .credentials
            .access(context.user_id, github_user_id)
            .await
            .map_err(ApiError::from)?;
        if grant.user_id != context.user_id
            || grant.github_user_id != github_user_id
            || grant.generation != context.credential_generation
        {
            return Err(ApiError::Unauthorized);
        }
        let metadata: RepositoryResponse = self
            .get_json(
                &grant.access_token,
                self.endpoint(&["repos", owner, repository])?,
            )
            .await?;
        if metadata.id == 0 || metadata.id > i64::MAX as u64 {
            return Err(ApiError::Unavailable);
        }
        validate_repository_full_name(&metadata.full_name)
            .map_err(|_| ApiError::Unavailable)?;
        let (canonical_owner, canonical_repository) = validate_repository_full_name(&metadata.full_name)
            .map_err(|_| ApiError::Unavailable)?;
        let found: BranchResponse = self
            .get_json(
                &grant.access_token,
                self.endpoint(&["repos", canonical_owner, canonical_repository, "branches", branch])?,
            )
            .await?;
        if found.name != branch {
            return Err(ApiError::NotFound);
        }
        Ok(VerifiedRepository {
            github_repository_id: metadata.id.to_string(),
            full_name: metadata.full_name,
            starting_branch: found.name,
        })
    }
}

fn map_response_status(response: &Response) -> Result<(), ApiError> {
    match response.status() {
        status if status.is_success() => Ok(()),
        StatusCode::UNAUTHORIZED => Err(ApiError::Unauthorized),
        StatusCode::FORBIDDEN
            if response.headers().contains_key("retry-after")
                || response
                    .headers()
                    .get("x-ratelimit-remaining")
                    .and_then(|value| value.to_str().ok())
                    .map(str::trim)
                    == Some("0") =>
        {
            Err(ApiError::Unavailable)
        }
        StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => Err(ApiError::NotFound),
        StatusCode::TOO_MANY_REQUESTS => Err(ApiError::Unavailable),
        _ => Err(ApiError::Unavailable),
    }
}

fn validate_names<'a>(full_name: &'a str, branch: &str) -> Result<(&'a str, &'a str), ApiError> {
    let parts = validate_repository_full_name(full_name).map_err(|_| ApiError::InvalidRequest)?;
    validate_branch(branch).map_err(|_| ApiError::InvalidRequest)?;
    Ok(parts)
}

fn validate_verified(verified: &VerifiedRepository, branch: &str) -> Result<i64, ApiError> {
    let id = super::types::parse_revision(&verified.github_repository_id)
        .filter(|id| *id > 0)
        .ok_or(ApiError::Unavailable)?;
    validate_repository_full_name(&verified.full_name).map_err(|_| ApiError::Unavailable)?;
    validate_branch(&verified.starting_branch).map_err(|_| ApiError::Unavailable)?;
    if verified.starting_branch != branch {
        return Err(ApiError::NotFound);
    }
    Ok(id)
}

fn project(row: workspace_repository::Model) -> RepositoryDto {
    RepositoryDto {
        id: row.id,
        github_repository_id: row.github_repository_id,
        full_name: row.full_name,
    }
}

pub(in crate::team_server) async fn list_repositories(
    state: &TeamState,
    context: &AuthContext,
) -> Result<Vec<RepositoryDto>, ApiError> {
    let transaction = begin_authorized(state, context).await?;
    let admissions = team_catalog_admission::Entity::find()
        .filter(team_catalog_admission::Column::OrgId.eq(context.org_id))
        .filter(team_catalog_admission::Column::ResourceKind.eq(REPOSITORY))
        .order_by_asc(team_catalog_admission::Column::ResourceId)
        .limit(CATALOG_CAP)
        .all(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?;
    let mut result = Vec::with_capacity(admissions.len());
    for admission in admissions {
        let Some(row) = workspace_repository::Entity::find_by_id(admission.resource_id)
            .one(&transaction)
            .await
            .map_err(|_| ApiError::Unavailable)?
            .filter(|row| row.org_id == context.org_id && valid_positive_id(row.id))
        else {
            continue;
        };
        if validate_verified_names(&row).is_err() {
            continue;
        }
        result.push(project(row));
    }
    commit_rechecked(state, context, transaction).await?;
    Ok(result)
}

pub(in crate::team_server) async fn connect_repository(
    state: &TeamState,
    context: &AuthContext,
    access: &dyn TeamRepositoryAccess,
    input: ConnectRepositoryInput,
) -> Result<RepositoryDto, ApiError> {
    validate_names(&input.full_name, &input.starting_branch)?;
    let initial = begin_authorized(state, context).await?;
    commit_rechecked(state, context, initial).await?;

    let verified = access
        .verify(context, &input.full_name, &input.starting_branch)
        .await?;
    validate_verified(&verified, &input.starting_branch)?;

    let transaction = begin_authorized(state, context).await?;
    let now = state.clock.now();
    let existing = workspace_repository::Entity::find()
        .filter(workspace_repository::Column::OrgId.eq(context.org_id))
        .filter(
            workspace_repository::Column::GithubRepositoryId
                .eq(verified.github_repository_id.as_str()),
        )
        .one(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?;
    let row = if let Some(existing) = existing {
        let mut names = validate_verified_names(&existing)?;
        if !names.contains(&verified.full_name) {
            names.push(verified.full_name.clone());
        }
        let mut active: workspace_repository::ActiveModel = existing.into();
        active.full_name = Set(verified.full_name);
        active.verified_names = Set(serde_json::to_string(&names).map_err(|_| ApiError::Unavailable)?);
        active.updated_at = Set(now);
        active
            .update(&transaction)
            .await
            .map_err(|_| ApiError::Unavailable)?
    } else {
        let admitted_count = team_catalog_admission::Entity::find()
            .filter(team_catalog_admission::Column::OrgId.eq(context.org_id))
            .filter(team_catalog_admission::Column::ResourceKind.eq(REPOSITORY))
            .count(&transaction)
            .await
            .map_err(|_| ApiError::Unavailable)?;
        if admitted_count >= CATALOG_CAP {
            return Err(ApiError::Conflict);
        }
        workspace_repository::ActiveModel {
            id: NotSet,
            org_id: Set(context.org_id),
            github_repository_id: Set(verified.github_repository_id),
            full_name: Set(verified.full_name.clone()),
            verified_names: Set(serde_json::to_string(&vec![verified.full_name]).map_err(|_| ApiError::Unavailable)?),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&transaction)
        .await
        .map_err(|_| ApiError::Unavailable)?
    };
    if admission(&transaction, REPOSITORY, row.id, context.org_id)
        .await?
        .is_none()
    {
        let admitted_count = team_catalog_admission::Entity::find()
            .filter(team_catalog_admission::Column::OrgId.eq(context.org_id))
            .filter(team_catalog_admission::Column::ResourceKind.eq(REPOSITORY))
            .count(&transaction)
            .await
            .map_err(|_| ApiError::Unavailable)?;
        if admitted_count >= CATALOG_CAP {
            return Err(ApiError::Conflict);
        }
        admit(state, &transaction, context, REPOSITORY, row.id).await?;
    }
    commit_rechecked(state, context, transaction).await?;
    Ok(project(row))
}

pub(super) fn validate_verified_names(
    row: &workspace_repository::Model,
) -> Result<Vec<String>, ApiError> {
    let names: Vec<String> =
        serde_json::from_str(&row.verified_names).map_err(|_| ApiError::Unavailable)?;
    if names.is_empty()
        || !names.contains(&row.full_name)
        || names
            .iter()
            .any(|name| validate_repository_full_name(name).is_err())
        || super::types::parse_revision(&row.github_repository_id)
            .filter(|id| *id > 0)
            .is_none()
    {
        return Err(ApiError::Unavailable);
    }
    Ok(names)
}
