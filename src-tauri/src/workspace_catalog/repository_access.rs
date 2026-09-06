use async_trait::async_trait;
use reqwest::{redirect, Client, Response, StatusCode, Url};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde::Deserialize;
use std::time::Duration;

use crate::db::entities::user;
use crate::web::handlers::github_auth::{ensure_github_token, GithubTokenError};

use super::{validate_branch, CatalogError, Scope};

const GITHUB_API_ORIGIN: &str = "https://api.github.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRepository {
    pub github_repository_id: String,
    pub full_name: String,
    pub starting_branch: String,
}

#[async_trait]
pub trait RepositoryAccess: Send + Sync {
    async fn verify(
        &self,
        scope: &Scope,
        full_name: &str,
        branch: &str,
    ) -> Result<VerifiedRepository, CatalogError>;
}

enum TokenSource {
    Database(DatabaseConnection),
    #[cfg(test)]
    TestFixture,
}

pub struct GitHubRepositoryAccess {
    client: Client,
    origin: Url,
    token_source: TokenSource,
}

impl GitHubRepositoryAccess {
    pub fn new(db: &DatabaseConnection) -> Result<Self, CatalogError> {
        Ok(Self {
            client: build_client()?,
            origin: Url::parse(GITHUB_API_ORIGIN).map_err(|_| CatalogError::UpstreamUnavailable)?,
            token_source: TokenSource::Database(db.clone()),
        })
    }

    #[cfg(test)]
    fn new_for_test(origin: &str) -> Result<Self, CatalogError> {
        Ok(Self {
            client: build_client()?,
            origin: Url::parse(origin)
                .map_err(|_| CatalogError::InvalidInput("invalid test origin".into()))?,
            token_source: TokenSource::TestFixture,
        })
    }

    pub(crate) async fn token(&self, scope: &Scope) -> Result<String, CatalogError> {
        match &self.token_source {
            TokenSource::Database(db) => {
                let scoped_user = user::Entity::find_by_id(scope.user_id)
                    .filter(user::Column::OrgId.eq(scope.org_id))
                    .one(db)
                    .await?
                    .ok_or(CatalogError::ReauthRequired)?;
                ensure_github_token(db, &scoped_user)
                    .await
                    .map_err(|error| match error {
                        GithubTokenError::ReauthRequired(_) => CatalogError::ReauthRequired,
                        GithubTokenError::Network(_) => CatalogError::UpstreamUnavailable,
                    })
            }
            #[cfg(test)]
            TokenSource::TestFixture => Ok("catalog-test-token".into()),
        }
    }

    fn endpoint(&self, segments: &[&str]) -> Result<Url, CatalogError> {
        let mut url = self.origin.clone();
        let mut path = url
            .path_segments_mut()
            .map_err(|_| CatalogError::UpstreamUnavailable)?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
        drop(path);
        Ok(url)
    }

    async fn get(&self, token: &str, url: Url) -> Result<Response, CatalogError> {
        self.client
            .get(url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|_| CatalogError::UpstreamUnavailable)
    }

    /// Metadata-only authorization also works after a starting branch is deleted.
    pub(crate) async fn verified_identity(&self, scope: &Scope, full_name: &str) -> Result<(String, String), CatalogError> {
        let token = self.token(scope).await?;
        let metadata = self.metadata(&token, full_name).await?;
        Ok((metadata.id.to_string(), metadata.full_name))
    }

    async fn metadata(&self, token: &str, full_name: &str) -> Result<GitHubRepositoryResponse, CatalogError> {
        let (owner, repository) = validate_repository_full_name(full_name)?;
        let response = self.get(token, self.endpoint(&["repos", owner, repository])?).await?;
        if let Some(error) = response_error(&response) { return Err(error); }
        let metadata: GitHubRepositoryResponse = response.json().await.map_err(|_| CatalogError::UpstreamUnavailable)?;
        validate_repository_full_name(&metadata.full_name).map_err(|_| CatalogError::UpstreamUnavailable)?;
        if metadata.id == 0 { return Err(CatalogError::UpstreamUnavailable); }
        Ok(metadata)
    }

    async fn branch(&self, token: &str, full_name: &str, branch: &str) -> Result<GitHubBranchResponse, CatalogError> {
        let (owner, repository) = validate_repository_full_name(full_name)?;
        validate_branch(branch)?;
        let response = self.get(token, self.endpoint(&["repos", owner, repository, "branches", branch])?).await?;
        if let Some(error) = response_error(&response) { return Err(error); }
        let found: GitHubBranchResponse = response.json().await.map_err(|_| CatalogError::UpstreamUnavailable)?;
        if found.name != branch { return Err(CatalogError::RepositoryUnavailable); }
        Ok(found)
    }

    pub(crate) async fn resolve_commit(&self, scope: &Scope, full_name: &str, branch: &str) -> Result<(String, String, String), CatalogError> {
        validate_repository_full_name(full_name)?;
        validate_branch(branch)?;
        let token = self.token(scope).await?;
        let metadata = self.metadata(&token, full_name).await?;
        let found = self.branch(&token, &metadata.full_name, branch).await?;
        let sha = found.commit.ok_or(CatalogError::UpstreamUnavailable)?.sha;
        if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(CatalogError::UpstreamUnavailable);
        }
        Ok((metadata.id.to_string(), metadata.full_name, sha.to_ascii_lowercase()))
    }
}

fn build_client() -> Result<Client, CatalogError> {
    Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(redirect::Policy::none())
        .user_agent("EazyWorkOS")
        .build()
        .map_err(|_| CatalogError::UpstreamUnavailable)
}

pub(crate) fn validate_repository_full_name(
    full_name: &str,
) -> Result<(&str, &str), CatalogError> {
    let mut components = full_name.split('/');
    let owner = components.next().unwrap_or_default();
    let repository = components.next().unwrap_or_default();
    if components.next().is_some()
        || !valid_repository_component(owner)
        || !valid_repository_component(repository)
    {
        return Err(CatalogError::InvalidInput(
            "repository must be an owner/name pair".into(),
        ));
    }
    Ok((owner, repository))
}

fn valid_repository_component(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn response_error(response: &Response) -> Option<CatalogError> {
    let status = response.status();
    if status.is_success() {
        return None;
    }
    let error = match status {
        StatusCode::UNAUTHORIZED => CatalogError::ReauthRequired,
        StatusCode::NOT_FOUND => CatalogError::RepositoryUnavailable,
        StatusCode::FORBIDDEN if is_rate_limited(response) => CatalogError::UpstreamUnavailable,
        StatusCode::FORBIDDEN => CatalogError::RepositoryUnavailable,
        StatusCode::TOO_MANY_REQUESTS => CatalogError::UpstreamUnavailable,
        status if status.is_server_error() => CatalogError::UpstreamUnavailable,
        status if status.is_redirection() => CatalogError::RepositoryUnavailable,
        _ => CatalogError::RepositoryUnavailable,
    };
    Some(error)
}

fn is_rate_limited(response: &Response) -> bool {
    response
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        == Some("0")
        || response.headers().contains_key("retry-after")
}

#[derive(Deserialize)]
struct GitHubRepositoryResponse {
    id: u64,
    full_name: String,
}

#[derive(Deserialize)]
struct GitHubBranchResponse {
    name: String,
    commit: Option<GitHubCommit>,
}
#[derive(Deserialize)]
struct GitHubCommit { sha: String }

#[async_trait]
impl RepositoryAccess for GitHubRepositoryAccess {
    async fn verify(
        &self,
        scope: &Scope,
        full_name: &str,
        branch: &str,
    ) -> Result<VerifiedRepository, CatalogError> {
        validate_repository_full_name(full_name)?;
        validate_branch(branch)?;
        let token = self.token(scope).await?;
        let metadata = self.metadata(&token, full_name).await?;
        let found_branch = self.branch(&token, &metadata.full_name, branch).await?;

        Ok(VerifiedRepository {
            github_repository_id: metadata.id.to_string(),
            full_name: metadata.full_name,
            starting_branch: found_branch.name,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{Response, StatusCode, Uri};
    use axum::routing::any;
    use axum::Router;

    use super::*;

    #[derive(Clone)]
    struct FixtureResponse {
        status: StatusCode,
        headers: Vec<(&'static str, &'static str)>,
        body: &'static str,
    }

    type Fixtures = Arc<HashMap<String, FixtureResponse>>;

    async fn fixture_response(
        State((fixtures, requests)): State<(Fixtures, Arc<Mutex<Vec<String>>>)>,
        uri: Uri,
    ) -> Response<Body> {
        let path = uri.path().to_string();
        requests.lock().unwrap().push(path.clone());
        let fixture = fixtures.get(&path).cloned().unwrap_or(FixtureResponse {
            status: StatusCode::NOT_FOUND,
            headers: vec![],
            body: r#"{"message":"not found"}"#,
        });
        let mut response = Response::builder().status(fixture.status);
        for (name, value) in fixture.headers {
            response = response.header(name, value);
        }
        response.body(Body::from(fixture.body)).unwrap()
    }

    async fn start_server(
        fixtures: HashMap<String, FixtureResponse>,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = (Arc::new(fixtures), Arc::clone(&requests));
        let app = Router::new()
            .fallback(any(fixture_response))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), requests)
    }

    fn ok(body: &'static str) -> FixtureResponse {
        FixtureResponse {
            status: StatusCode::OK,
            headers: vec![],
            body,
        }
    }

    fn scope() -> Scope {
        Scope {
            org_id: 1,
            user_id: 1,
        }
    }

    #[tokio::test]
    async fn catalog_repository_encodes_slash_branch_as_one_path_segment() {
        let fixtures = HashMap::from([
            (
                "/repos/input-owner/input-repo".into(),
                ok(r#"{"id":123,"full_name":"Canonical/Handbook"}"#),
            ),
            (
                "/repos/Canonical/Handbook/branches/feature%2Fdocs".into(),
                ok(r#"{"name":"feature/docs","commit":{"sha":"abc123"}}"#),
            ),
        ]);
        let (origin, requests) = start_server(fixtures).await;
        let access = GitHubRepositoryAccess::new_for_test(&origin).unwrap();

        let verified = access
            .verify(&scope(), "input-owner/input-repo", "feature/docs")
            .await
            .unwrap();

        assert_eq!(verified.github_repository_id, "123");
        assert_eq!(verified.full_name, "Canonical/Handbook");
        assert_eq!(verified.starting_branch, "feature/docs");
        assert_eq!(
            *requests.lock().unwrap(),
            vec![
                "/repos/input-owner/input-repo",
                "/repos/Canonical/Handbook/branches/feature%2Fdocs"
            ]
        );
    }

    #[tokio::test]
    async fn catalog_repository_rejects_malicious_names_before_request() {
        let (origin, requests) = start_server(HashMap::new()).await;
        let access = GitHubRepositoryAccess::new_for_test(&origin).unwrap();

        for name in [
            "acme/docs/extra",
            "../docs",
            "acme/..",
            "acme/%2e%2e",
            "acme/docs?admin=true",
            "acme docs/repo",
            "/repo",
        ] {
            assert!(matches!(
                access.verify(&scope(), name, "main").await,
                Err(CatalogError::InvalidInput(_))
            ));
        }
        assert!(requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn catalog_repository_maps_http_failures_without_following_redirects() {
        let mut fixtures = HashMap::new();
        for (owner, response) in [
            (
                "unauthorized",
                FixtureResponse {
                    status: StatusCode::UNAUTHORIZED,
                    headers: vec![],
                    body: "{}",
                },
            ),
            (
                "missing",
                FixtureResponse {
                    status: StatusCode::NOT_FOUND,
                    headers: vec![],
                    body: "{}",
                },
            ),
            (
                "denied",
                FixtureResponse {
                    status: StatusCode::FORBIDDEN,
                    headers: vec![],
                    body: "{}",
                },
            ),
            (
                "limited",
                FixtureResponse {
                    status: StatusCode::FORBIDDEN,
                    headers: vec![("x-ratelimit-remaining", "0")],
                    body: "{}",
                },
            ),
            (
                "secondary",
                FixtureResponse {
                    status: StatusCode::FORBIDDEN,
                    headers: vec![("retry-after", "60")],
                    body: "{}",
                },
            ),
            (
                "busy",
                FixtureResponse {
                    status: StatusCode::TOO_MANY_REQUESTS,
                    headers: vec![],
                    body: "{}",
                },
            ),
            (
                "broken",
                FixtureResponse {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    headers: vec![],
                    body: "{}",
                },
            ),
            (
                "redirect",
                FixtureResponse {
                    status: StatusCode::FOUND,
                    headers: vec![("location", "/repos/followed/repo")],
                    body: "{}",
                },
            ),
            (
                "followed",
                ok(r#"{"id":999,"full_name":"followed/repo"}"#),
            ),
        ] {
            fixtures.insert(format!("/repos/{owner}/repo"), response);
        }
        let (origin, requests) = start_server(fixtures).await;
        let access = GitHubRepositoryAccess::new_for_test(&origin).unwrap();

        for (owner, expected) in [
            ("unauthorized", CatalogError::ReauthRequired),
            ("missing", CatalogError::RepositoryUnavailable),
            ("denied", CatalogError::RepositoryUnavailable),
            ("limited", CatalogError::UpstreamUnavailable),
            ("secondary", CatalogError::UpstreamUnavailable),
            ("busy", CatalogError::UpstreamUnavailable),
            ("broken", CatalogError::UpstreamUnavailable),
            ("redirect", CatalogError::RepositoryUnavailable),
        ] {
            assert_eq!(
                access.verify(&scope(), &format!("{owner}/repo"), "main").await,
                Err(expected),
                "wrong mapping for {owner}"
            );
        }
        assert!(!requests
            .lock()
            .unwrap()
            .iter()
            .any(|path| path == "/repos/followed/repo"));
    }

    #[tokio::test]
    async fn catalog_repository_missing_branch_is_unavailable() {
        let fixtures = HashMap::from([
            (
                "/repos/acme/empty".into(),
                ok(r#"{"id":123,"full_name":"acme/empty"}"#),
            ),
            (
                "/repos/acme/empty/branches/main".into(),
                FixtureResponse {
                    status: StatusCode::NOT_FOUND,
                    headers: vec![],
                    body: "{}",
                },
            ),
        ]);
        let (origin, _) = start_server(fixtures).await;
        let access = GitHubRepositoryAccess::new_for_test(&origin).unwrap();

        assert_eq!(
            access.verify(&scope(), "acme/empty", "main").await,
            Err(CatalogError::RepositoryUnavailable)
        );
    }

    #[tokio::test]
    async fn workspace_execution_metadata_access_survives_deleted_branch_but_resolve_requires_full_commit() {
        let fixtures = HashMap::from([
            ("/repos/acme/docs".into(), ok(r#"{"id":123,"full_name":"acme/docs"}"#)),
            ("/repos/acme/docs/branches/main".into(), ok(r#"{"name":"main","commit":{"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#)),
            ("/repos/acme/docs/branches/bad".into(), ok(r#"{"name":"bad","commit":{"sha":"abc"}}"#)),
        ]);
        let (origin, requests) = start_server(fixtures).await;
        let access = GitHubRepositoryAccess::new_for_test(&origin).unwrap();
        assert_eq!(access.verified_identity(&scope(), "acme/docs").await.unwrap(), ("123".into(), "acme/docs".into()));
        assert_eq!(*requests.lock().unwrap(), vec!["/repos/acme/docs"]);
        assert_eq!(access.resolve_commit(&scope(), "acme/docs", "deleted").await.unwrap_err(), CatalogError::RepositoryUnavailable);
        assert_eq!(access.resolve_commit(&scope(), "acme/docs", "bad").await.unwrap_err(), CatalogError::UpstreamUnavailable);
        assert_eq!(access.resolve_commit(&scope(), "acme/docs", "main").await.unwrap().2, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert!(access.verified_identity(&scope(), "acme/docs").await.is_ok());
    }
}
