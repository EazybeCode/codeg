use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Response, StatusCode},
    middleware,
    routing::post,
    Router,
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectOptions, ConnectionTrait, Database,
    DatabaseConnection, DbBackend, EntityTrait, PaginatorTrait, QueryFilter, Set, Statement,
};
use sea_orm_migration::MigratorTrait;
use serde_json::{json, Value};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use uuid::Uuid;

use super::workspace_catalog::{BrowserMutationPolicy, WorkspaceCatalogHandler};
use crate::{
    db::{
        entities::{conversation, folder, org, session, user, workspace_chat_draft},
        migration::Migrator,
    },
    web::auth,
    workspace_catalog::{CatalogError, RepositoryAccess, Scope, VerifiedRepository},
};

const ADMIN_URL: &str = "postgres://air1@localhost:5432/postgres";
const TEST_TOKEN: &str = "catalog-postgres-test-token";

type TestResult<T> = Result<T, String>;

#[derive(Clone)]
struct FixtureRepositoryAccess {
    repositories: Arc<HashMap<String, (&'static str, &'static str)>>,
}

#[async_trait]
impl RepositoryAccess for FixtureRepositoryAccess {
    async fn verify(
        &self,
        _scope: &Scope,
        full_name: &str,
        branch: &str,
    ) -> Result<VerifiedRepository, CatalogError> {
        let Some((github_repository_id, canonical_name)) = self.repositories.get(full_name) else {
            return Err(CatalogError::RepositoryUnavailable);
        };
        if branch != "main" {
            return Err(CatalogError::RepositoryUnavailable);
        }
        Ok(VerifiedRepository {
            github_repository_id: (*github_repository_id).into(),
            full_name: (*canonical_name).into(),
            starting_branch: branch.into(),
        })
    }
}

#[derive(Clone)]
struct CatalogHttpState {
    db: DatabaseConnection,
    access: FixtureRepositoryAccess,
    policy: BrowserMutationPolicy,
}

impl CatalogHttpState {
    fn handler(&self) -> WorkspaceCatalogHandler<'_> {
        WorkspaceCatalogHandler::new(&self.db, &self.access, &self.policy)
    }
}

async fn list_nodes(
    State(state): State<Arc<CatalogHttpState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    state.handler().list_workspace_nodes(&headers, &body).await
}

async fn create_node(
    State(state): State<Arc<CatalogHttpState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    state.handler().create_workspace_node(&headers, &body).await
}

async fn update_node(
    State(state): State<Arc<CatalogHttpState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    state.handler().update_workspace_node(&headers, &body).await
}

async fn connect_repository(
    State(state): State<Arc<CatalogHttpState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    state
        .handler()
        .connect_workspace_repository(&headers, &body)
        .await
}

async fn create_draft(
    State(state): State<Arc<CatalogHttpState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    state
        .handler()
        .create_workspace_chat_draft(&headers, &body)
        .await
}

async fn list_drafts(
    State(state): State<Arc<CatalogHttpState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    state
        .handler()
        .list_workspace_chat_drafts(&headers, &body)
        .await
}

struct TestServer {
    base_url: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), std::io::Error>>,
}

impl TestServer {
    async fn start(db: DatabaseConnection) -> TestResult<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|error| format!("bind isolated HTTP server: {error}"))?;
        let address = listener
            .local_addr()
            .map_err(|error| format!("read isolated server address: {error}"))?;
        let base_url = format!("http://{address}");
        let state = Arc::new(CatalogHttpState {
            db,
            access: FixtureRepositoryAccess {
                repositories: Arc::new(HashMap::from([
                    ("acme/primary".into(), ("1001", "acme/primary")),
                    ("acme/override".into(), ("2002", "acme/override")),
                ])),
            },
            policy: BrowserMutationPolicy::from_app_origin(Some(&base_url)),
        });
        let protected = Router::new()
            .route("/api/list_workspace_nodes", post(list_nodes))
            .route("/api/create_workspace_node", post(create_node))
            .route("/api/update_workspace_node", post(update_node))
            .route(
                "/api/connect_workspace_repository",
                post(connect_repository),
            )
            .route("/api/create_workspace_chat_draft", post(create_draft))
            .route("/api/list_workspace_chat_drafts", post(list_drafts))
            .with_state(state)
            .layer(middleware::from_fn(move |request, next| async move {
                auth::require_token(request, next, TEST_TOKEN.into()).await
            }));
        let (shutdown, signal) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, protected)
                .with_graceful_shutdown(async move {
                    let _ = signal.await;
                })
                .await
        });
        Ok(Self {
            base_url,
            shutdown: Some(shutdown),
            task,
        })
    }

    async fn stop(mut self) -> TestResult<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task
            .await
            .map_err(|error| format!("join isolated HTTP server: {error}"))?
            .map_err(|error| format!("stop isolated HTTP server: {error}"))
    }
}

#[derive(Clone)]
struct ApiClient {
    client: reqwest::Client,
    base_url: String,
}

impl ApiClient {
    async fn post(&self, endpoint: &str, session_id: &str, body: Value) -> TestResult<ApiResponse> {
        let response = self
            .client
            .post(format!("{}/api/{endpoint}", self.base_url))
            .bearer_auth(TEST_TOKEN)
            .header("origin", &self.base_url)
            .header("cookie", format!("workos_session={session_id}"))
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("POST {endpoint}: {error}"))?;
        let status = response.status();
        let body = response
            .json()
            .await
            .map_err(|error| format!("decode {endpoint} response: {error}"))?;
        Ok(ApiResponse { status, body })
    }
}

struct ApiResponse {
    status: StatusCode,
    body: Value,
}

fn require(condition: bool, message: impl Into<String>) -> TestResult<()> {
    condition.then_some(()).ok_or_else(|| message.into())
}

fn require_status(response: &ApiResponse, expected: StatusCode, operation: &str) -> TestResult<()> {
    require(
        response.status == expected,
        format!(
            "{operation}: expected {expected}, got {} with {}",
            response.status, response.body
        ),
    )
}

fn json_i32(value: &Value, field: &str) -> TestResult<i32> {
    value[field]
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| format!("missing integer field {field} in {value}"))
}

fn validate_database_name(name: &str) -> TestResult<()> {
    require(
        name.starts_with("workos_catalog_test_")
            && name.len() == "workos_catalog_test_".len() + 32
            && name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
        format!("refusing unsafe disposable database name: {name}"),
    )
}

async fn connect(url: &str, max_connections: u32) -> TestResult<DatabaseConnection> {
    let mut options = ConnectOptions::new(url);
    options
        .max_connections(max_connections)
        .min_connections(1)
        .sqlx_logging(false);
    Database::connect(options)
        .await
        .map_err(|error| format!("connect to {url}: {error}"))
}

async fn seed_identity_rows(db: &DatabaseConnection) -> TestResult<()> {
    let now = Utc::now();
    for (id, github_org_id, login) in [(7001, 700_100, "org-a"), (7002, 700_200, "org-b")] {
        org::ActiveModel {
            id: Set(id),
            github_org_id: Set(github_org_id),
            login: Set(login.into()),
            name: Set(None),
            avatar_url: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .map_err(|error| format!("seed {login}: {error}"))?;
    }
    for (id, github_user_id, login, org_id) in [
        (7101, 710_100, "alice", 7001),
        (7102, 710_200, "bob", 7001),
        (7201, 720_100, "mallory", 7002),
    ] {
        user::ActiveModel {
            id: Set(id),
            github_user_id: Set(github_user_id),
            login: Set(login.into()),
            name: Set(None),
            email: Set(None),
            avatar_url: Set(None),
            org_id: Set(org_id),
            role: Set("member".into()),
            gh_token_enc: Set(None),
            gh_refresh_token_enc: Set(None),
            gh_token_expires_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .map_err(|error| format!("seed {login}: {error}"))?;
    }
    for (id, user_id) in [
        ("alice-session", 7101),
        ("bob-session", 7102),
        ("mallory-session", 7201),
    ] {
        session::ActiveModel {
            id: Set(id.into()),
            user_id: Set(user_id),
            created_at: Set(now),
            expires_at: Set(now + chrono::Duration::hours(1)),
        }
        .insert(db)
        .await
        .map_err(|error| format!("seed {id}: {error}"))?;
    }
    Ok(())
}

async fn exercise_catalog(db: &DatabaseConnection, api: &ApiClient) -> TestResult<()> {
    let unauthorized = api
        .client
        .post(format!("{}/api/list_workspace_nodes", api.base_url))
        .header("content-type", "application/json")
        .header("cookie", "workos_session=alice-session")
        .body("{}")
        .send()
        .await
        .map_err(|error| format!("unauthenticated request: {error}"))?;
    require(
        unauthorized.status() == StatusCode::UNAUTHORIZED,
        format!("token middleware returned {} without bearer", unauthorized.status()),
    )?;

    let project = api
        .post(
            "create_workspace_node",
            "alice-session",
            json!({
                "parentId": null,
                "kind": "project",
                "name": "Catalog project",
                "repositoryDefault": null
            }),
        )
        .await?;
    require_status(&project, StatusCode::OK, "create project")?;
    require(project.body["repository_default"].is_null(), "project unexpectedly owns a repository")?;
    let project_id = json_i32(&project.body, "id")?;

    let other_project = api
        .post(
            "create_workspace_node",
            "mallory-session",
            json!({
                "parentId": null,
                "kind": "project",
                "name": "Other tenant",
                "repositoryDefault": null
            }),
        )
        .await?;
    require_status(&other_project, StatusCode::OK, "create other tenant project")?;
    let other_project_id = json_i32(&other_project.body, "id")?;

    let primary = api
        .post(
            "connect_workspace_repository",
            "alice-session",
            json!({"fullName": "acme/primary", "startingBranch": "main"}),
        )
        .await?;
    require_status(&primary, StatusCode::OK, "connect primary repository")?;
    let primary_id = json_i32(&primary.body, "id")?;
    let override_repo = api
        .post(
            "connect_workspace_repository",
            "alice-session",
            json!({"fullName": "acme/override", "startingBranch": "main"}),
        )
        .await?;
    require_status(&override_repo, StatusCode::OK, "connect override repository")?;
    let override_id = json_i32(&override_repo.body, "id")?;

    let working_directory = api
        .post(
            "create_workspace_node",
            "alice-session",
            json!({
                "parentId": project_id,
                "kind": "working_directory",
                "name": "Primary checkout intent",
                "repositoryDefault": {"repository_id": primary_id, "starting_branch": "main"}
            }),
        )
        .await?;
    require_status(&working_directory, StatusCode::OK, "create working directory")?;
    let working_directory_id = json_i32(&working_directory.body, "id")?;

    let folder = api
        .post(
            "create_workspace_node",
            "alice-session",
            json!({
                "parentId": working_directory_id,
                "kind": "folder",
                "name": "Nested logical folder",
                "repositoryDefault": null
            }),
        )
        .await?;
    require_status(&folder, StatusCode::OK, "create nested folder")?;
    let folder_id = json_i32(&folder.body, "id")?;

    let stale = api
        .post(
            "update_workspace_node",
            "alice-session",
            json!({
                "nodeId": working_directory_id,
                "expectedRevision": 41,
                "parentId": project_id,
                "name": "Stale overwrite",
                "repositoryDefault": {"repository_id": primary_id, "starting_branch": "main"}
            }),
        )
        .await?;
    require_status(&stale, StatusCode::CONFLICT, "reject stale update")?;

    let cross_tenant = api
        .post(
            "update_workspace_node",
            "alice-session",
            json!({
                "nodeId": other_project_id,
                "expectedRevision": 0,
                "parentId": null,
                "name": "Cross-tenant overwrite",
                "repositoryDefault": null
            }),
        )
        .await?;
    require_status(&cross_tenant, StatusCode::NOT_FOUND, "reject cross-tenant update")?;

    let request_id = Uuid::new_v4().to_string();
    let inherited_input = json!({
        "nodeId": folder_id,
        "repositoryOverride": null,
        "title": "Concurrent inherited draft",
        "requestId": request_id
    });
    let mut requests = Vec::new();
    for _ in 0..6 {
        requests.push(api.post(
            "create_workspace_chat_draft",
            "alice-session",
            inherited_input.clone(),
        ));
    }
    let responses = futures_util::future::join_all(requests).await;
    let mut inherited_draft_id = None;
    for response in responses {
        let response = response?;
        require_status(&response, StatusCode::OK, "concurrent inherited retry")?;
        require(
            response.body["repository_id"] == primary_id,
            format!("inherited draft selected wrong repository: {}", response.body),
        )?;
        let id = json_i32(&response.body, "id")?;
        require(
            inherited_draft_id.is_none() || inherited_draft_id == Some(id),
            "equal retry created more than one draft id",
        )?;
        inherited_draft_id = Some(id);
    }
    let inherited_draft_id = inherited_draft_id.ok_or("no concurrent draft response")?;

    let conflict = api
        .post(
            "create_workspace_chat_draft",
            "alice-session",
            json!({
                "nodeId": folder_id,
                "repositoryOverride": null,
                "title": "Changed body",
                "requestId": request_id
            }),
        )
        .await?;
    require_status(&conflict, StatusCode::CONFLICT, "reject changed retry body")?;

    let update_default = api
        .post(
            "update_workspace_node",
            "alice-session",
            json!({
                "nodeId": working_directory_id,
                "expectedRevision": 0,
                "parentId": project_id,
                "name": "Primary checkout intent",
                "repositoryDefault": {"repository_id": override_id, "starting_branch": "main"}
            }),
        )
        .await?;
    require_status(&update_default, StatusCode::OK, "change inherited default")?;
    let retried = api
        .post(
            "create_workspace_chat_draft",
            "alice-session",
            inherited_input,
        )
        .await?;
    require_status(&retried, StatusCode::OK, "retry after parent default change")?;
    require(
        json_i32(&retried.body, "id")? == inherited_draft_id
            && retried.body["repository_id"] == primary_id,
        format!("retry did not preserve the original repository snapshot: {}", retried.body),
    )?;

    let override_draft = api
        .post(
            "create_workspace_chat_draft",
            "alice-session",
            json!({
                "nodeId": folder_id,
                "repositoryOverride": {"repository_id": primary_id, "starting_branch": "main"},
                "title": "Explicit override",
                "requestId": Uuid::new_v4().to_string()
            }),
        )
        .await?;
    require_status(&override_draft, StatusCode::OK, "create overridden draft")?;
    require(
        override_draft.body["repository_id"] == primary_id,
        format!("explicit override was not captured: {}", override_draft.body),
    )?;

    let bob_draft = api
        .post(
            "create_workspace_chat_draft",
            "bob-session",
            json!({
                "nodeId": folder_id,
                "repositoryOverride": {"repository_id": override_id, "starting_branch": "main"},
                "title": "Bob private draft",
                "requestId": Uuid::new_v4().to_string()
            }),
        )
        .await?;
    require_status(&bob_draft, StatusCode::OK, "create second-member draft")?;
    let alice_drafts = api
        .post(
            "list_workspace_chat_drafts",
            "alice-session",
            json!({"nodeId": folder_id}),
        )
        .await?;
    require_status(&alice_drafts, StatusCode::OK, "list creator-private drafts")?;
    let drafts = alice_drafts
        .body
        .as_array()
        .ok_or_else(|| format!("draft list was not an array: {}", alice_drafts.body))?;
    require(
        drafts.len() == 2 && drafts.iter().all(|draft| draft["creator_user_id"] == 7101),
        format!("draft privacy filter leaked rows: {}", alice_drafts.body),
    )?;

    let nodes = api
        .post("list_workspace_nodes", "alice-session", json!({}))
        .await?;
    require_status(&nodes, StatusCode::OK, "list scoped nodes")?;
    require(
        nodes.body.as_array().is_some_and(|nodes| {
            nodes.len() == 3 && nodes.iter().all(|node| node["org_id"] == 7001)
        }),
        format!("node listing crossed tenant boundary: {}", nodes.body),
    )?;

    require(
        workspace_chat_draft::Entity::find()
            .filter(workspace_chat_draft::Column::RequestId.eq(request_id))
            .count(db)
            .await
            .map_err(|error| format!("count retried drafts: {error}"))?
            == 1,
        "concurrent retries persisted more than one row",
    )?;
    require(
        folder::Entity::find()
            .count(db)
            .await
            .map_err(|error| format!("count legacy folders: {error}"))?
            == 0,
        "catalog operations created a physical folder row",
    )?;
    require(
        conversation::Entity::find()
            .count(db)
            .await
            .map_err(|error| format!("count legacy conversations: {error}"))?
            == 0,
        "catalog operations created a legacy conversation row",
    )?;
    require(
        conversation::Entity::find_by_id(inherited_draft_id)
            .one(db)
            .await
            .map_err(|error| format!("look up draft id in legacy conversations: {error}"))?
            .is_none(),
        "a catalog draft id resolved to a launchable legacy conversation",
    )?;
    Ok(())
}

async fn run_http_scenario(db: &DatabaseConnection) -> TestResult<()> {
    seed_identity_rows(db).await?;
    let server = TestServer::start(db.clone()).await?;
    let api = ApiClient {
        client: reqwest::Client::new(),
        base_url: server.base_url.clone(),
    };
    let result = exercise_catalog(db, &api).await;
    let shutdown_result = server.stop().await;
    result.and(shutdown_result)
}

/// Explicitly gated because it creates a real local Postgres database. Run with:
/// `cargo test ... workspace_catalog_postgres_http_integration -- --ignored --test-threads=1`
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires local Postgres role air1 with CREATEDB on localhost:5432"]
async fn workspace_catalog_postgres_http_integration() {
    let database_name = format!("workos_catalog_test_{}", Uuid::new_v4().simple());
    validate_database_name(&database_name).expect("generated database name must be safe");
    let admin = connect(ADMIN_URL, 1)
        .await
        .expect("connect explicit local Postgres admin database");
    admin
        .execute(Statement::from_string(
            DbBackend::Postgres,
            format!("CREATE DATABASE \"{database_name}\""),
        ))
        .await
        .expect("create generated catalog test database");
    let database_url = format!("postgres://air1@localhost:5432/{database_name}");
    let db = match connect(&database_url, 8).await {
        Ok(db) => db,
        Err(error) => {
            admin.close().await.expect("close admin pool");
            panic!("{error}; preserved database {database_name}");
        }
    };
    let migration_result = Migrator::up(&db, None)
        .await
        .map_err(|error| format!("run migrations: {error}"));
    let outcome = match migration_result {
        Ok(()) => run_http_scenario(&db).await,
        Err(error) => Err(error),
    };
    db.close().await.expect("close disposable database pool");

    if let Err(error) = outcome {
        admin.close().await.expect("close admin pool after failure");
        panic!("{error}; preserved database {database_name}");
    }

    validate_database_name(&database_name).expect("cleanup database name must remain exact and safe");
    admin
        .execute(Statement::from_string(
            DbBackend::Postgres,
            format!("DROP DATABASE \"{database_name}\""),
        ))
        .await
        .expect("drop successful catalog test database");
    admin.close().await.expect("close admin pool");
    eprintln!("verified and removed disposable database {database_name}");
}
