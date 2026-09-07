use std::collections::HashMap;

use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};

use super::workspace_catalog::{
    catalog_error_response, BrowserMutationPolicy, WorkspaceCatalogHandler,
};
use crate::{
    db::{
        entities::{org, session, user, workspace_chat_draft},
        test_helpers::fresh_in_memory_db,
        AppDatabase,
    },
    workspace_catalog::{
        create_node, CatalogError, CreateNodeInput, RepositoryAccess, Scope, VerifiedRepository,
        WorkspaceNodeKind,
    },
};

struct FixtureRepositoryAccess {
    by_user: HashMap<(i32, String, String), Result<VerifiedRepository, CatalogError>>,
}

struct RepositoryIdentityChangingAccess {
    db: sea_orm::DatabaseConnection,
    repository_id: i32,
}

#[async_trait]
impl RepositoryAccess for RepositoryIdentityChangingAccess {
    async fn verify(
        &self,
        _scope: &Scope,
        _full_name: &str,
        branch: &str,
    ) -> Result<VerifiedRepository, CatalogError> {
        let repository = crate::db::entities::workspace_repository::Entity::find_by_id(
            self.repository_id,
        )
        .one(&self.db)
        .await
        .unwrap()
        .unwrap();
        let mut active: crate::db::entities::workspace_repository::ActiveModel = repository.into();
        active.github_repository_id = Set("999".into());
        active.update(&self.db).await.unwrap();
        Ok(verified("123", "acme/docs", branch))
    }
}

#[async_trait]
impl RepositoryAccess for FixtureRepositoryAccess {
    async fn verify(
        &self,
        scope: &Scope,
        full_name: &str,
        branch: &str,
    ) -> Result<VerifiedRepository, CatalogError> {
        self.by_user
            .get(&(scope.user_id, full_name.into(), branch.into()))
            .cloned()
            .unwrap_or(Err(CatalogError::RepositoryUnavailable))
    }
}

fn denied_access() -> FixtureRepositoryAccess {
    FixtureRepositoryAccess {
        by_user: HashMap::new(),
    }
}

fn verified(github_repository_id: &str, full_name: &str, branch: &str) -> VerifiedRepository {
    VerifiedRepository {
        github_repository_id: github_repository_id.into(),
        full_name: full_name.into(),
        starting_branch: branch.into(),
    }
}

async fn seed_org(db: &AppDatabase, id: i32) {
    let now = Utc::now();
    org::ActiveModel {
        id: Set(id),
        github_org_id: Set(i64::from(id) * 100),
        login: Set(format!("org-{id}")),
        name: Set(None),
        avatar_url: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .unwrap();
}

async fn seed_user(db: &AppDatabase, id: i32, org_id: i32, login: &str) {
    let now = Utc::now();
    user::ActiveModel {
        id: Set(id),
        github_user_id: Set(i64::from(id) * 1000),
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
    .insert(&db.conn)
    .await
    .unwrap();
}

async fn seed_session(db: &AppDatabase, id: &str, user_id: i32, expired: bool) {
    let now = Utc::now();
    session::ActiveModel {
        id: Set(id.into()),
        user_id: Set(user_id),
        created_at: Set(now),
        expires_at: Set(if expired {
            now - chrono::Duration::minutes(1)
        } else {
            now + chrono::Duration::hours(1)
        }),
    }
    .insert(&db.conn)
    .await
    .unwrap();
}

fn json_headers(session_id: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer eazybe-dev"),
    );
    if let Some(session_id) = session_id {
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("workos_session={session_id}")).unwrap(),
        );
    }
    headers
}

async fn body_json(response: Response<Body>) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn project_input(name: &str) -> CreateNodeInput {
    CreateNodeInput {
        parent_id: None,
        kind: WorkspaceNodeKind::Project,
        name: name.into(),
        repository_default: None,
    }
}

#[tokio::test]
async fn catalog_repository_metadata_is_cookie_scoped_and_credential_free() {
    use crate::db::entities::workspace_repository;
    let db = fresh_in_memory_db().await;
    for id in [1, 2] {
        seed_org(&db, id).await;
        seed_user(&db, id, id, &format!("user-{id}")).await;
        seed_session(&db, &format!("session-{id}"), id, false).await;
        workspace_repository::ActiveModel {
            id: Set(id), org_id: Set(id), github_repository_id: Set(format!("{id}00")),
            full_name: Set(format!("org-{id}/docs")), created_at: Set(Utc::now()), updated_at: Set(Utc::now()),
        }.insert(&db.conn).await.unwrap();
    }
    let access = denied_access();
    let policy = BrowserMutationPolicy::from_app_origin(None);
    let handler = WorkspaceCatalogHandler::new(&db.conn, &access, &policy);
    let (status, value) = body_json(handler.list_workspace_repositories(&json_headers(Some("session-1")), b"{}").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, serde_json::json!([{ "id": 1, "org_id": 1, "github_repository_id": "100", "full_name": "org-1/docs" }]));
    assert_eq!(handler.list_workspace_repositories(&json_headers(None), b"{}").await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(handler.list_workspace_repositories(&json_headers(Some("session-1")), br#"{"orgId":2}"#).await.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn catalog_handlers_require_a_live_user_cookie_even_with_the_shared_bearer() {
    let db = fresh_in_memory_db().await;
    seed_org(&db, 1).await;
    seed_user(&db, 1, 1, "alice").await;
    seed_session(&db, "expired", 1, true).await;
    let access = denied_access();
    let policy = BrowserMutationPolicy::from_app_origin(Some("https://workos.example"));
    let handler = WorkspaceCatalogHandler::new(&db.conn, &access, &policy);

    for headers in [json_headers(None), json_headers(Some("expired"))] {
        let (status, body) = body_json(handler.list_workspace_nodes(&headers, b"{}").await).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body, serde_json::json!({ "error": "unauthorized" }));
    }
}

#[tokio::test]
async fn catalog_session_database_failure_is_not_misreported_as_unauthorized() {
    let db = fresh_in_memory_db().await;
    let access = denied_access();
    let policy = BrowserMutationPolicy::from_app_origin(None);
    let handler = WorkspaceCatalogHandler::new(&db.conn, &access, &policy);
    let headers = json_headers(Some("session-on-closed-database"));
    db.conn.clone().close().await.unwrap();

    assert!(super::github_auth::current_user_result(&headers, &db.conn)
        .await
        .is_err());
    let (status, body) = body_json(handler.list_workspace_nodes(&headers, b"{}").await).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, serde_json::json!({ "error": "database" }));
}

#[tokio::test]
async fn catalog_handler_derives_the_valid_cookie_scope_and_never_lists_another_org() {
    let db = fresh_in_memory_db().await;
    seed_org(&db, 1).await;
    seed_org(&db, 2).await;
    seed_user(&db, 1, 1, "alice").await;
    seed_user(&db, 2, 2, "mallory").await;
    seed_session(&db, "alice-session", 1, false).await;
    create_node(&db.conn, &Scope { org_id: 1, user_id: 1 }, project_input("Alice"))
        .await
        .unwrap();
    create_node(&db.conn, &Scope { org_id: 2, user_id: 2 }, project_input("Mallory"))
        .await
        .unwrap();
    let access = denied_access();
    let policy = BrowserMutationPolicy::from_app_origin(Some("https://workos.example"));
    let handler = WorkspaceCatalogHandler::new(&db.conn, &access, &policy);

    let (status, body) = body_json(
        handler
            .list_workspace_nodes(&json_headers(Some("alice-session")), b"{}")
            .await,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["name"], "Alice");
    assert_eq!(body[0]["org_id"], 1);
}

#[tokio::test]
async fn catalog_handler_rejects_cross_org_node_ids() {
    let db = fresh_in_memory_db().await;
    seed_org(&db, 1).await;
    seed_org(&db, 2).await;
    seed_user(&db, 1, 1, "alice").await;
    seed_user(&db, 2, 2, "mallory").await;
    seed_session(&db, "alice-session", 1, false).await;
    let other_node = create_node(
        &db.conn,
        &Scope { org_id: 2, user_id: 2 },
        project_input("Other"),
    )
    .await
    .unwrap();
    let access = denied_access();
    let policy = BrowserMutationPolicy::from_app_origin(Some("https://workos.example"));
    let handler = WorkspaceCatalogHandler::new(&db.conn, &access, &policy);
    let mut headers = json_headers(Some("alice-session"));
    headers.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://workos.example"),
    );
    let body = serde_json::json!({
        "nodeId": other_node.id,
        "expectedRevision": 0,
        "parentId": null,
        "name": "Stolen",
        "repositoryDefault": null
    });

    let (status, body) = body_json(
        handler
            .update_workspace_node(&headers, body.to_string().as_bytes())
            .await,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, serde_json::json!({ "error": "not_found" }));
}

#[tokio::test]
async fn another_members_existing_connection_does_not_authorize_an_inaccessible_default() {
    let db = fresh_in_memory_db().await;
    seed_org(&db, 1).await;
    seed_user(&db, 1, 1, "alice").await;
    seed_user(&db, 2, 1, "bob").await;
    seed_session(&db, "alice-session", 1, false).await;
    seed_session(&db, "bob-session", 2, false).await;
    let access = FixtureRepositoryAccess {
        by_user: HashMap::from([(
            (1, "acme/docs".into(), "main".into()),
            Ok(verified("123", "acme/docs", "main")),
        )]),
    };
    let policy = BrowserMutationPolicy::from_app_origin(Some("https://workos.example"));
    let handler = WorkspaceCatalogHandler::new(&db.conn, &access, &policy);
    let mut alice_headers = json_headers(Some("alice-session"));
    alice_headers.insert(header::ORIGIN, HeaderValue::from_static("https://workos.example"));
    let connect = serde_json::json!({ "fullName": "acme/docs", "startingBranch": "main" });
    let (status, repository) = body_json(
        handler
            .connect_workspace_repository(&alice_headers, connect.to_string().as_bytes())
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let mut bob_headers = json_headers(Some("bob-session"));
    bob_headers.insert(header::ORIGIN, HeaderValue::from_static("https://workos.example"));
    let create = serde_json::json!({
        "parentId": null,
        "kind": "working_directory",
        "name": "Private docs",
        "repositoryDefault": {
            "repository_id": repository["id"],
            "starting_branch": "main"
        }
    });
    let (status, body) = body_json(
        handler
            .create_workspace_node(&bob_headers, create.to_string().as_bytes())
            .await,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, serde_json::json!({ "error": "repository_unavailable" }));

    let project = create_node(
        &db.conn,
        &Scope { org_id: 1, user_id: 2 },
        project_input("Bob's project"),
    )
    .await
    .unwrap();
    let update = serde_json::json!({
        "nodeId": project.id,
        "expectedRevision": 0,
        "parentId": null,
        "name": "Bob's project",
        "repositoryDefault": {
            "repository_id": repository["id"],
            "starting_branch": "main"
        }
    });
    let (status, body) = body_json(
        handler
            .update_workspace_node(&bob_headers, update.to_string().as_bytes())
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, serde_json::json!({ "error": "repository_unavailable" }));
}

#[tokio::test]
async fn node_default_rejects_a_changed_github_repository_id() {
    let db = fresh_in_memory_db().await;
    seed_org(&db, 1).await;
    seed_user(&db, 1, 1, "alice").await;
    seed_session(&db, "alice-session", 1, false).await;
    let access = FixtureRepositoryAccess {
        by_user: HashMap::from([(
            (1, "acme/docs".into(), "main".into()),
            Ok(verified("123", "acme/docs", "main")),
        )]),
    };
    let policy = BrowserMutationPolicy::from_app_origin(Some("https://workos.example"));
    let handler = WorkspaceCatalogHandler::new(&db.conn, &access, &policy);
    let mut headers = json_headers(Some("alice-session"));
    headers.insert(header::ORIGIN, HeaderValue::from_static("https://workos.example"));
    let connect = serde_json::json!({ "fullName": "acme/docs", "startingBranch": "main" });
    let (_, repository) = body_json(
        handler
            .connect_workspace_repository(&headers, connect.to_string().as_bytes())
            .await,
    )
    .await;
    let changed_access = FixtureRepositoryAccess {
        by_user: HashMap::from([(
            (1, "acme/docs".into(), "main".into()),
            Ok(verified("999", "acme/docs", "main")),
        )]),
    };
    let changed_handler = WorkspaceCatalogHandler::new(&db.conn, &changed_access, &policy);
    let create = serde_json::json!({
        "parentId": null,
        "kind": "working_directory",
        "name": "Docs",
        "repositoryDefault": {
            "repository_id": repository["id"],
            "starting_branch": "main"
        }
    });

    let (status, body) = body_json(
        changed_handler
            .create_workspace_node(&headers, create.to_string().as_bytes())
            .await,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, serde_json::json!({ "error": "repository_unavailable" }));
}

#[tokio::test]
async fn node_default_revalidates_the_stored_repository_identity_after_external_verification() {
    let db = fresh_in_memory_db().await;
    seed_org(&db, 1).await;
    seed_user(&db, 1, 1, "alice").await;
    seed_session(&db, "alice-session", 1, false).await;
    let initial_access = FixtureRepositoryAccess {
        by_user: HashMap::from([(
            (1, "acme/docs".into(), "main".into()),
            Ok(verified("123", "acme/docs", "main")),
        )]),
    };
    let policy = BrowserMutationPolicy::from_app_origin(Some("https://workos.example"));
    let initial_handler = WorkspaceCatalogHandler::new(&db.conn, &initial_access, &policy);
    let mut headers = json_headers(Some("alice-session"));
    headers.insert(header::ORIGIN, HeaderValue::from_static("https://workos.example"));
    let (_, repository) = body_json(
        initial_handler
            .connect_workspace_repository(
                &headers,
                serde_json::json!({ "fullName": "acme/docs", "startingBranch": "main" })
                    .to_string()
                    .as_bytes(),
            )
            .await,
    )
    .await;
    let repository_id = repository["id"].as_i64().unwrap() as i32;
    let changing_access = RepositoryIdentityChangingAccess {
        db: db.conn.clone(),
        repository_id,
    };
    let handler = WorkspaceCatalogHandler::new(&db.conn, &changing_access, &policy);
    let parent = create_node(
        &db.conn,
        &Scope { org_id: 1, user_id: 1 },
        project_input("Workspace"),
    )
    .await
    .unwrap();
    let create = serde_json::json!({
        "parentId": parent.id,
        "kind": "working_directory",
        "name": "Docs",
        "repositoryDefault": {
            "repository_id": repository_id,
            "starting_branch": "main"
        }
    });

    let (status, body) = body_json(
        handler
            .create_workspace_node(&headers, create.to_string().as_bytes())
            .await,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, serde_json::json!({ "error": "repository_unavailable" }));
}

#[tokio::test]
async fn draft_lists_exclude_another_member_and_reject_cross_org_nodes() {
    let db = fresh_in_memory_db().await;
    seed_org(&db, 1).await;
    seed_org(&db, 2).await;
    seed_user(&db, 1, 1, "alice").await;
    seed_user(&db, 2, 1, "bob").await;
    seed_user(&db, 3, 2, "mallory").await;
    seed_session(&db, "alice-session", 1, false).await;
    let alice_scope = Scope { org_id: 1, user_id: 1 };
    let mallory_scope = Scope { org_id: 2, user_id: 3 };
    let node = create_node(&db.conn, &alice_scope, project_input("Shared"))
        .await
        .unwrap();
    let other_node = create_node(&db.conn, &mallory_scope, project_input("Other"))
        .await
        .unwrap();
    let now = Utc::now();
    for (id, creator_user_id, title) in [(1, 1, "Mine"), (2, 2, "Bob's")] {
        workspace_chat_draft::ActiveModel {
            id: Set(id),
            org_id: Set(1),
            node_id: Set(node.id),
            creator_user_id: Set(creator_user_id),
            repository_id: Set(1),
            github_repository_id: Set("123".into()),
            full_name: Set("acme/docs".into()),
            starting_branch: Set("main".into()),
            title: Set(Some(title.into())),
            request_id: Set(format!("request-{id}")),
            request_fingerprint: Set(format!("fingerprint-{id}")),
            execution_state: Set("unprovisioned".into()),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .unwrap();
    }
    let access = denied_access();
    let policy = BrowserMutationPolicy::from_app_origin(None);
    let handler = WorkspaceCatalogHandler::new(&db.conn, &access, &policy);
    let headers = json_headers(Some("alice-session"));

    let (status, body) = body_json(
        handler
            .list_workspace_chat_drafts(
                &headers,
                serde_json::json!({ "nodeId": node.id }).to_string().as_bytes(),
            )
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["title"], "Mine");

    let (status, body) = body_json(
        handler
            .list_workspace_chat_drafts(
                &headers,
                serde_json::json!({ "nodeId": other_node.id })
                    .to_string()
                    .as_bytes(),
            )
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, serde_json::json!({ "error": "not_found" }));
}

#[tokio::test]
async fn create_draft_handler_uses_cookie_scope_and_stable_repository_required_error() {
    let db = fresh_in_memory_db().await;
    seed_org(&db, 1).await;
    seed_user(&db, 1, 1, "alice").await;
    seed_session(&db, "alice-session", 1, false).await;
    let node = create_node(
        &db.conn,
        &Scope {
            org_id: 1,
            user_id: 1,
        },
        project_input("Unbound"),
    )
    .await
    .unwrap();
    let access = denied_access();
    let policy = BrowserMutationPolicy::from_app_origin(None);
    let handler = WorkspaceCatalogHandler::new(&db.conn, &access, &policy);
    let input = serde_json::json!({
        "nodeId": node.id,
        "repositoryOverride": null,
        "title": null,
        "requestId": "0c58d910-3e1a-43c9-9462-785335828829"
    });

    let (status, body) = body_json(
        handler
            .create_workspace_chat_draft(
                &json_headers(Some("alice-session")),
                input.to_string().as_bytes(),
            )
            .await,
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body, serde_json::json!({ "error": "repository_required" }));
}

#[tokio::test]
async fn browser_mutations_require_same_origin_but_allow_authenticated_non_browsers() {
    let db = fresh_in_memory_db().await;
    seed_org(&db, 1).await;
    seed_user(&db, 1, 1, "alice").await;
    seed_session(&db, "alice-session", 1, false).await;
    let access = denied_access();
    let trusted_policy =
        BrowserMutationPolicy::from_app_origin(Some("https://workos.example"));
    let handler = WorkspaceCatalogHandler::new(&db.conn, &access, &trusted_policy);
    let input = serde_json::json!({
        "parentId": null,
        "kind": "project",
        "name": "Marketing",
        "repositoryDefault": null
    });

    let mut forged = json_headers(Some("alice-session"));
    forged.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://evil.example"),
    );
    let (status, _) = body_json(
        handler
            .create_workspace_node(&forged, input.to_string().as_bytes())
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let mut missing = json_headers(Some("alice-session"));
    missing.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
    let (status, _) = body_json(
        handler
            .create_workspace_node(&missing, input.to_string().as_bytes())
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let unset_policy = BrowserMutationPolicy::from_app_origin(None);
    let unset_handler = WorkspaceCatalogHandler::new(&db.conn, &access, &unset_policy);
    let mut browser = json_headers(Some("alice-session"));
    browser.insert(header::ORIGIN, HeaderValue::from_static("https://workos.example"));
    let (status, _) = body_json(
        unset_handler
            .create_workspace_node(&browser, input.to_string().as_bytes())
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let malformed_policy =
        BrowserMutationPolicy::from_app_origin(Some("https://workos.example/path"));
    let malformed_handler = WorkspaceCatalogHandler::new(&db.conn, &access, &malformed_policy);
    let (status, _) = body_json(
        malformed_handler
            .create_workspace_node(&browser, input.to_string().as_bytes())
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let mut forwarded_host = forged.clone();
    forwarded_host.insert(
        "x-forwarded-host",
        HeaderValue::from_static("workos.example"),
    );
    let (status, _) = body_json(
        handler
            .create_workspace_node(&forwarded_host, input.to_string().as_bytes())
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = body_json(
        handler
            .create_workspace_node(
                &json_headers(Some("alice-session")),
                input.to_string().as_bytes(),
            )
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Marketing");
}

#[tokio::test]
async fn catalog_endpoints_require_json_and_reject_unknown_fields() {
    let db = fresh_in_memory_db().await;
    seed_org(&db, 1).await;
    seed_user(&db, 1, 1, "alice").await;
    seed_session(&db, "alice-session", 1, false).await;
    let access = denied_access();
    let policy = BrowserMutationPolicy::from_app_origin(None);
    let handler = WorkspaceCatalogHandler::new(&db.conn, &access, &policy);

    let mut no_content_type = json_headers(Some("alice-session"));
    no_content_type.remove(header::CONTENT_TYPE);
    let (status, body) =
        body_json(handler.list_workspace_nodes(&no_content_type, b"{}").await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, serde_json::json!({ "error": "invalid_input" }));

    let (status, body) = body_json(
        handler
            .list_workspace_nodes(&json_headers(Some("alice-session")), br#"{"extra":true}"#)
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, serde_json::json!({ "error": "invalid_input" }));

    let (status, body) = body_json(
        handler
            .list_workspace_nodes(&json_headers(Some("alice-session")), b"{")
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, serde_json::json!({ "error": "invalid_input" }));
}

#[tokio::test]
async fn catalog_errors_have_stable_codes_and_statuses_without_internal_messages() {
    for (error, expected_status, expected_code) in [
        (CatalogError::Unauthorized, StatusCode::UNAUTHORIZED, "unauthorized"),
        (CatalogError::ReauthRequired, StatusCode::UNAUTHORIZED, "reauth_required"),
        (CatalogError::NotFound, StatusCode::NOT_FOUND, "not_found"),
        (
            CatalogError::RepositoryUnavailable,
            StatusCode::NOT_FOUND,
            "repository_unavailable",
        ),
        (
            CatalogError::InvalidInput("/secret/path must not leak".into()),
            StatusCode::BAD_REQUEST,
            "invalid_input",
        ),
        (CatalogError::Conflict, StatusCode::CONFLICT, "conflict"),
        (
            CatalogError::RepositoryRequired,
            StatusCode::UNPROCESSABLE_ENTITY,
            "repository_required",
        ),
        (
            CatalogError::UpstreamUnavailable,
            StatusCode::BAD_GATEWAY,
            "upstream_unavailable",
        ),
        (
            CatalogError::Database,
            StatusCode::INTERNAL_SERVER_ERROR,
            "database",
        ),
    ] {
        let (status, body) = body_json(catalog_error_response(error)).await;
        assert_eq!(status, expected_status);
        assert_eq!(body, serde_json::json!({ "error": expected_code }));
    }
}
