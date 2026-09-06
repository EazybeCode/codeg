use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::Extension,
    http::{header, HeaderMap, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use reqwest::Url;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{
    app_state::AppState,
    db::entities::{org, workspace_repository},
    workspace_catalog::{
        connect_repository, create_draft, create_node_with_repository_identity, list_drafts,
        list_nodes, update_node_with_repository_identity, validate_branch,
        validate_repository_full_name, CatalogError, CreateDraftInput, CreateNodeInput,
        GitHubRepositoryAccess, RepositoryAccess, RepositoryDefault,
        RepositoryIdentityExpectation, Scope, UpdateNodeInput,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum TrustedOrigin {
    Missing,
    Invalid,
    Exact(String),
}

/// Same-origin policy for browser mutations. Tests inject this value directly;
/// production reads the one trusted deployment origin from WORKOS_APP_ORIGIN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserMutationPolicy {
    trusted_origin: TrustedOrigin,
}

impl BrowserMutationPolicy {
    pub fn from_app_origin(value: Option<&str>) -> Self {
        let trusted_origin = match value {
            None => TrustedOrigin::Missing,
            Some(value) => match exact_http_origin(value) {
                Some(origin) => TrustedOrigin::Exact(origin),
                None => TrustedOrigin::Invalid,
            },
        };
        Self { trusted_origin }
    }

    pub fn from_environment() -> Self {
        let value = std::env::var("WORKOS_APP_ORIGIN").ok();
        Self::from_app_origin(value.as_deref())
    }

    pub fn validate(&self, headers: &HeaderMap) -> Result<(), CatalogError> {
        if let Some(origin) = headers.get(header::ORIGIN) {
            let origin = origin
                .to_str()
                .map_err(|_| CatalogError::InvalidInput("invalid origin".into()))?;
            return match &self.trusted_origin {
                TrustedOrigin::Exact(trusted) if origin == trusted => Ok(()),
                _ => Err(CatalogError::InvalidInput("origin not allowed".into())),
            };
        }

        if browser_fetch_metadata_present(headers) {
            return Err(CatalogError::InvalidInput(
                "browser mutation requires an origin".into(),
            ));
        }

        // Explicit exception for authenticated non-browser clients. Callers
        // must resolve the cookie scope before invoking this policy.
        Ok(())
    }
}

fn exact_http_origin(value: &str) -> Option<String> {
    if value.trim() != value {
        return None;
    }
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let origin = url.origin().ascii_serialization();
    (value == origin).then_some(origin)
}

fn browser_fetch_metadata_present(headers: &HeaderMap) -> bool {
    [
        "sec-fetch-site",
        "sec-fetch-mode",
        "sec-fetch-dest",
        "sec-fetch-user",
    ]
    .iter()
    .any(|name| headers.contains_key(*name))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConnectRepositoryInput {
    full_name: String,
    starting_branch: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListDraftsInput {
    node_id: i32,
}

#[derive(Serialize)]
struct CatalogErrorBody {
    error: &'static str,
}

fn content_type_is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
}

fn parse_json<T: DeserializeOwned>(headers: &HeaderMap, body: &[u8]) -> Result<T, CatalogError> {
    if !content_type_is_json(headers) {
        return Err(CatalogError::InvalidInput(
            "application/json content type required".into(),
        ));
    }
    serde_json::from_slice(body)
        .map_err(|_| CatalogError::InvalidInput("invalid JSON request".into()))
}

pub async fn scope_from_headers(
    headers: &HeaderMap,
    db: &DatabaseConnection,
) -> Result<Scope, CatalogError> {
    let user = super::github_auth::current_user_result(headers, db)
        .await?
        .ok_or(CatalogError::Unauthorized)?;
    let organization_exists = org::Entity::find_by_id(user.org_id)
        .one(db)
        .await?
        .is_some();
    if !organization_exists {
        return Err(CatalogError::Unauthorized);
    }
    Ok(Scope {
        org_id: user.org_id,
        user_id: user.id,
    })
}

fn error_contract(error: &CatalogError) -> (StatusCode, &'static str) {
    match error {
        CatalogError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
        CatalogError::ReauthRequired => (StatusCode::UNAUTHORIZED, "reauth_required"),
        CatalogError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        CatalogError::RepositoryUnavailable => {
            (StatusCode::NOT_FOUND, "repository_unavailable")
        }
        CatalogError::InvalidInput(_) => (StatusCode::BAD_REQUEST, "invalid_input"),
        CatalogError::Conflict => (StatusCode::CONFLICT, "conflict"),
        CatalogError::RepositoryRequired => {
            (StatusCode::UNPROCESSABLE_ENTITY, "repository_required")
        }
        CatalogError::UpstreamUnavailable => {
            (StatusCode::BAD_GATEWAY, "upstream_unavailable")
        }
        CatalogError::Database => (StatusCode::INTERNAL_SERVER_ERROR, "database"),
    }
}

pub(crate) fn catalog_error_response(error: CatalogError) -> Response<Body> {
    let (status, code) = error_contract(&error);
    (status, Json(CatalogErrorBody { error: code })).into_response()
}

fn catalog_response<T: Serialize>(result: Result<T, CatalogError>) -> Response<Body> {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => catalog_error_response(error),
    }
}

/// Testable core shared by all catalog HTTP handlers. It owns no credentials
/// or mutable provider state; the database, repository verifier, and browser
/// policy are readonly injected dependencies.
pub(crate) struct WorkspaceCatalogHandler<'a> {
    db: &'a DatabaseConnection,
    repository_access: &'a dyn RepositoryAccess,
    mutation_policy: &'a BrowserMutationPolicy,
}

impl<'a> WorkspaceCatalogHandler<'a> {
    pub(crate) fn new(
        db: &'a DatabaseConnection,
        repository_access: &'a dyn RepositoryAccess,
        mutation_policy: &'a BrowserMutationPolicy,
    ) -> Self {
        Self {
            db,
            repository_access,
            mutation_policy,
        }
    }

    async fn mutation_scope(&self, headers: &HeaderMap) -> Result<Scope, CatalogError> {
        let scope = scope_from_headers(headers, self.db).await?;
        self.mutation_policy.validate(headers)?;
        Ok(scope)
    }

    async fn verify_default(
        &self,
        scope: &Scope,
        default: Option<&RepositoryDefault>,
    ) -> Result<Option<RepositoryIdentityExpectation>, CatalogError> {
        let Some(default) = default else {
            return Ok(None);
        };
        let repository = workspace_repository::Entity::find_by_id(default.repository_id)
            .filter(workspace_repository::Column::OrgId.eq(scope.org_id))
            .one(self.db)
            .await?
            .ok_or(CatalogError::NotFound)?;
        let verified = self
            .repository_access
            .verify(scope, &repository.full_name, &default.starting_branch)
            .await?;
        if verified.github_repository_id != repository.github_repository_id
            || verified.starting_branch != default.starting_branch
        {
            return Err(CatalogError::RepositoryUnavailable);
        }
        if verified.github_repository_id.is_empty()
            || !verified
                .github_repository_id
                .bytes()
                .all(|byte| byte.is_ascii_digit())
        {
            return Err(CatalogError::UpstreamUnavailable);
        }
        validate_repository_full_name(&verified.full_name)
            .map_err(|_| CatalogError::UpstreamUnavailable)?;
        validate_branch(&verified.starting_branch)
            .map_err(|_| CatalogError::UpstreamUnavailable)?;
        Ok(Some(RepositoryIdentityExpectation {
            repository_id: repository.id,
            github_repository_id: repository.github_repository_id,
        }))
    }

    pub(crate) async fn list_workspace_nodes(
        &self,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Response<Body> {
        let result = async {
            let scope = scope_from_headers(headers, self.db).await?;
            let _: EmptyInput = parse_json(headers, body)?;
            list_nodes(self.db, &scope).await
        }
        .await;
        catalog_response(result)
    }

    pub(crate) async fn create_workspace_node(
        &self,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Response<Body> {
        let result = async {
            let scope = self.mutation_scope(headers).await?;
            let input: CreateNodeInput = parse_json(headers, body)?;
            let expected = self
                .verify_default(&scope, input.repository_default.as_ref())
                .await?;
            create_node_with_repository_identity(self.db, &scope, input, expected).await
        }
        .await;
        catalog_response(result)
    }

    pub(crate) async fn update_workspace_node(
        &self,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Response<Body> {
        let result = async {
            let scope = self.mutation_scope(headers).await?;
            let input: UpdateNodeInput = parse_json(headers, body)?;
            let expected = self
                .verify_default(&scope, input.repository_default.as_ref())
                .await?;
            update_node_with_repository_identity(self.db, &scope, input, expected).await
        }
        .await;
        catalog_response(result)
    }

    pub(crate) async fn connect_workspace_repository(
        &self,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Response<Body> {
        let result = async {
            let scope = self.mutation_scope(headers).await?;
            let input: ConnectRepositoryInput = parse_json(headers, body)?;
            connect_repository(
                self.db,
                &scope,
                self.repository_access,
                &input.full_name,
                &input.starting_branch,
            )
            .await
        }
        .await;
        catalog_response(result)
    }

    pub(crate) async fn create_workspace_chat_draft(
        &self,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Response<Body> {
        let result = async {
            let scope = self.mutation_scope(headers).await?;
            let input: CreateDraftInput = parse_json(headers, body)?;
            create_draft(self.db, &scope, self.repository_access, input).await
        }
        .await;
        catalog_response(result)
    }

    pub(crate) async fn list_workspace_chat_drafts(
        &self,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Response<Body> {
        let result = async {
            let scope = scope_from_headers(headers, self.db).await?;
            let input: ListDraftsInput = parse_json(headers, body)?;
            list_drafts(self.db, &scope, input.node_id).await
        }
        .await;
        catalog_response(result)
    }
}

fn production_dependencies(
    state: &AppState,
) -> Result<(GitHubRepositoryAccess, BrowserMutationPolicy), CatalogError> {
    Ok((
        GitHubRepositoryAccess::new(&state.db.conn)?,
        BrowserMutationPolicy::from_environment(),
    ))
}

pub async fn list_workspace_nodes(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let (access, policy) = match production_dependencies(&state) {
        Ok(dependencies) => dependencies,
        Err(error) => return catalog_error_response(error),
    };
    WorkspaceCatalogHandler::new(&state.db.conn, &access, &policy)
        .list_workspace_nodes(&headers, &body)
        .await
}

pub async fn create_workspace_node(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let (access, policy) = match production_dependencies(&state) {
        Ok(dependencies) => dependencies,
        Err(error) => return catalog_error_response(error),
    };
    WorkspaceCatalogHandler::new(&state.db.conn, &access, &policy)
        .create_workspace_node(&headers, &body)
        .await
}

pub async fn update_workspace_node(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let (access, policy) = match production_dependencies(&state) {
        Ok(dependencies) => dependencies,
        Err(error) => return catalog_error_response(error),
    };
    WorkspaceCatalogHandler::new(&state.db.conn, &access, &policy)
        .update_workspace_node(&headers, &body)
        .await
}

pub async fn connect_workspace_repository(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let (access, policy) = match production_dependencies(&state) {
        Ok(dependencies) => dependencies,
        Err(error) => return catalog_error_response(error),
    };
    WorkspaceCatalogHandler::new(&state.db.conn, &access, &policy)
        .connect_workspace_repository(&headers, &body)
        .await
}

pub async fn create_workspace_chat_draft(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let (access, policy) = match production_dependencies(&state) {
        Ok(dependencies) => dependencies,
        Err(error) => return catalog_error_response(error),
    };
    WorkspaceCatalogHandler::new(&state.db.conn, &access, &policy)
        .create_workspace_chat_draft(&headers, &body)
        .await
}

pub async fn list_workspace_chat_drafts(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let (access, policy) = match production_dependencies(&state) {
        Ok(dependencies) => dependencies,
        Err(error) => return catalog_error_response(error),
    };
    WorkspaceCatalogHandler::new(&state.db.conn, &access, &policy)
        .list_workspace_chat_drafts(&headers, &body)
        .await
}
