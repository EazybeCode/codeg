use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};

use super::{
    connect_repository, CatalogError, GitHubRepositoryAccess, RepositoryAccess, Scope,
    VerifiedRepository,
};
use crate::db::{
    entities::{org, user, workspace_repository},
    test_helpers::fresh_in_memory_db,
    AppDatabase,
};

struct FakeRepositoryAccess {
    repositories: HashMap<(String, String), VerifiedRepository>,
}

impl FakeRepositoryAccess {
    fn new(entries: &[(&str, &str, &str, &str)]) -> Self {
        let repositories = entries
            .iter()
            .map(|(full_name, branch, github_id, canonical_name)| {
                (
                    ((*full_name).into(), (*branch).into()),
                    VerifiedRepository {
                        github_repository_id: (*github_id).into(),
                        full_name: (*canonical_name).into(),
                        starting_branch: (*branch).into(),
                    },
                )
            })
            .collect();
        Self { repositories }
    }
}

#[async_trait]
impl RepositoryAccess for FakeRepositoryAccess {
    async fn verify(
        &self,
        _scope: &Scope,
        full_name: &str,
        branch: &str,
    ) -> Result<VerifiedRepository, CatalogError> {
        self.repositories
            .get(&(full_name.into(), branch.into()))
            .cloned()
            .ok_or(CatalogError::RepositoryUnavailable)
    }
}

struct UpstreamFailure;

#[async_trait]
impl RepositoryAccess for UpstreamFailure {
    async fn verify(
        &self,
        _scope: &Scope,
        _full_name: &str,
        _branch: &str,
    ) -> Result<VerifiedRepository, CatalogError> {
        Err(CatalogError::UpstreamUnavailable)
    }
}

async fn seed_scope(db: &AppDatabase, scope: &Scope) {
    let now = Utc::now();
    org::ActiveModel {
        id: Set(scope.org_id),
        github_org_id: Set(100),
        login: Set("acme".into()),
        name: Set(None),
        avatar_url: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .unwrap();
    user::ActiveModel {
        id: Set(scope.user_id),
        github_user_id: Set(101),
        login: Set("alice".into()),
        name: Set(None),
        email: Set(None),
        avatar_url: Set(None),
        org_id: Set(scope.org_id),
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

async fn setup() -> (AppDatabase, Scope) {
    let db = fresh_in_memory_db().await;
    let scope = Scope {
        org_id: 1,
        user_id: 1,
    };
    seed_scope(&db, &scope).await;
    (db, scope)
}

#[tokio::test]
async fn catalog_repository_same_basename_different_owners_are_distinct() {
    let (db, scope) = setup().await;
    let access = FakeRepositoryAccess::new(&[
        ("acme/docs", "main", "123", "acme/docs"),
        ("other/docs", "main", "456", "other/docs"),
    ]);

    let first = connect_repository(&db.conn, &scope, &access, "acme/docs", "main")
        .await
        .unwrap();
    let second = connect_repository(&db.conn, &scope, &access, "other/docs", "main")
        .await
        .unwrap();

    assert_ne!(first.id, second.id);
    assert_eq!(first.github_repository_id, "123");
    assert_eq!(second.github_repository_id, "456");
    assert_eq!(
        workspace_repository::Entity::find()
            .filter(workspace_repository::Column::OrgId.eq(scope.org_id))
            .count(&db.conn)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn catalog_repository_repeat_verified_id_renames_without_replacing_catalog_id() {
    let (db, scope) = setup().await;
    let access = FakeRepositoryAccess::new(&[
        ("acme/docs", "main", "123", "acme/docs"),
        ("acme/handbook", "main", "123", "acme/handbook"),
    ]);

    let first = connect_repository(&db.conn, &scope, &access, "acme/docs", "main")
        .await
        .unwrap();
    let renamed = connect_repository(&db.conn, &scope, &access, "acme/handbook", "main")
        .await
        .unwrap();

    assert_eq!(first.id, renamed.id);
    assert_eq!(renamed.github_repository_id, "123");
    assert_eq!(renamed.full_name, "acme/handbook");
    assert_eq!(
        workspace_repository::Entity::find()
            .filter(workspace_repository::Column::OrgId.eq(scope.org_id))
            .count(&db.conn)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn catalog_repository_unavailable_verification_persists_nothing() {
    let (db, scope) = setup().await;
    let access = FakeRepositoryAccess::new(&[]);

    assert_eq!(
        connect_repository(&db.conn, &scope, &access, "acme/missing", "main").await,
        Err(CatalogError::RepositoryUnavailable)
    );
    assert_eq!(
        workspace_repository::Entity::find().count(&db.conn).await.unwrap(),
        0
    );
}

#[tokio::test]
async fn catalog_repository_upstream_failure_persists_nothing() {
    let (db, scope) = setup().await;

    assert_eq!(
        connect_repository(&db.conn, &scope, &UpstreamFailure, "acme/docs", "main").await,
        Err(CatalogError::UpstreamUnavailable)
    );
    assert_eq!(
        workspace_repository::Entity::find().count(&db.conn).await.unwrap(),
        0
    );
}

#[tokio::test]
async fn catalog_repository_missing_stored_token_requires_reauthentication() {
    let (db, scope) = setup().await;
    let access = GitHubRepositoryAccess::new(&db.conn).unwrap();

    assert_eq!(
        access.verify(&scope, "acme/docs", "main").await,
        Err(CatalogError::ReauthRequired)
    );
}
