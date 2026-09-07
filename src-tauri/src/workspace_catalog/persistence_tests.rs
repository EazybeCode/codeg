use super::{
    create_node, list_nodes, update_node, CatalogError, CreateNodeInput, RepositoryDefault, Scope,
    UpdateNodeInput, WorkspaceNodeKind,
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ConnectionTrait, EntityTrait, PaginatorTrait, Set,
};
use sea_orm_migration::MigratorTrait;

use crate::db::{
    entities::{conversation, folder, org, user, workspace_repository},
    migration::Migrator,
    test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder},
    AppDatabase,
};
use crate::models::agent::AgentType;

async fn seed_scope(db: &AppDatabase, scope: &Scope, github_org_id: i64) {
    let now = Utc::now();
    org::ActiveModel {
        id: Set(scope.org_id),
        github_org_id: Set(github_org_id),
        login: Set(format!("org-{}", scope.org_id)),
        name: Set(None),
        avatar_url: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .expect("seed organization");
    user::ActiveModel {
        id: Set(scope.user_id),
        github_user_id: Set(github_org_id * 10),
        login: Set(format!("user-{}", scope.user_id)),
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
    .expect("seed user");
}

async fn seed_two_scopes(db: &AppDatabase) -> (Scope, Scope) {
    let first = Scope {
        org_id: 1,
        user_id: 1,
    };
    let second = Scope {
        org_id: 2,
        user_id: 2,
    };
    seed_scope(db, &first, 100).await;
    seed_scope(db, &second, 200).await;
    (first, second)
}

async fn seed_repository(db: &AppDatabase, org_id: i32, github_id: &str) -> i32 {
    let now = Utc::now();
    workspace_repository::ActiveModel {
        id: NotSet,
        org_id: Set(org_id),
        github_repository_id: Set(github_id.into()),
        full_name: Set(format!("org-{org_id}/repo-{github_id}")),
        verified_names: Set(serde_json::to_string(&vec![format!("org-{org_id}/repo-{github_id}")]).unwrap()),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .expect("seed repository")
    .id
}

#[tokio::test]
async fn workspace_catalog_verified_name_history_survives_renames_and_backfills() {
    use sea_orm_migration::SchemaManager;
    let db = fresh_in_memory_db().await;
    let manager = SchemaManager::new(&db.conn);
    assert!(manager.has_column("workspace_repository", "verified_names").await.unwrap(), "trusted repository name history must be persisted");
    let (scope, _) = seed_two_scopes(&db).await;
    use sea_orm_migration::MigrationTrait;
    use crate::db::migration::m20260907_000004_workspace_repository_names::Migration as NamesMigration;
    NamesMigration.down(&manager).await.unwrap();
    // Simulate an existing pre-fix catalog row before the additive migration.
    db.conn.execute_unprepared("INSERT INTO workspace_repository (org_id, github_repository_id, full_name, created_at, updated_at) VALUES (1, '123', 'acme/old', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)").await.unwrap();
    NamesMigration.up(&manager).await.unwrap();
    let access = super::draft_tests::FakeRepositoryAccess::new(&[
        ("acme/old", "main", "123", "acme/old"),
        ("acme/new", "main", "123", "acme/new"),
        ("acme/latest", "main", "123", "acme/latest"),
        ("foreign/repo", "main", "999", "foreign/repo"),
    ]);
    let first = super::connect_repository(&db.conn, &scope, &access, "acme/old", "main").await.unwrap();
    for name in ["acme/new", "acme/latest"] {
        assert_eq!(super::connect_repository(&db.conn, &scope, &access, name, "main").await.unwrap().id, first.id);
    }
    let row = db.conn.query_one(sea_orm::Statement::from_sql_and_values(db.conn.get_database_backend(), "SELECT verified_names FROM workspace_repository WHERE id = $1", [first.id.into()])).await.unwrap().unwrap();
    let names: String = row.try_get("", "verified_names").unwrap();
    assert_eq!(serde_json::from_str::<Vec<String>>(&names).unwrap(), ["acme/old", "acme/new", "acme/latest"]);
    let foreign = super::connect_repository(&db.conn, &scope, &access, "foreign/repo", "main").await.unwrap();
    assert_ne!(foreign.id, first.id);
    assert!(NamesMigration.down(&manager).await.is_err(), "downgrade cannot discard trusted identity history");
    db.conn.execute_unprepared("UPDATE workspace_repository SET verified_names = 'not-json' WHERE github_repository_id = '123'").await.unwrap();
    assert!(super::connect_repository(&db.conn, &scope, &access, "acme/latest", "main").await.is_err());
}

fn create_input(
    parent_id: Option<i32>,
    kind: WorkspaceNodeKind,
    name: &str,
    repository_default: Option<RepositoryDefault>,
) -> CreateNodeInput {
    CreateNodeInput {
        parent_id,
        kind,
        name: name.into(),
        repository_default,
    }
}

#[tokio::test]
async fn catalog_project_without_filesystem() {
    let db = fresh_in_memory_db().await;
    let (scope, other_scope) = seed_two_scopes(&db).await;
    let input = CreateNodeInput {
        parent_id: None,
        kind: WorkspaceNodeKind::Project,
        name: "Marketing".into(),
        repository_default: None,
    };
    let before = folder::Entity::find().count(&db.conn).await.unwrap();
    let created = create_node(&db.conn, &scope, input).await.unwrap();
    assert_eq!(created.name, "Marketing");
    assert_eq!(
        folder::Entity::find().count(&db.conn).await.unwrap(),
        before
    );
    assert_eq!(
        list_nodes(&db.conn, &other_scope).await.unwrap().len(),
        0
    );
}

#[tokio::test]
async fn workspace_catalog_scopes_listing_and_requires_org_membership() {
    let db = fresh_in_memory_db().await;
    let (first, second) = seed_two_scopes(&db).await;
    create_node(
        &db.conn,
        &first,
        create_input(None, WorkspaceNodeKind::Project, "First", None),
    )
    .await
    .unwrap();
    create_node(
        &db.conn,
        &second,
        create_input(None, WorkspaceNodeKind::Project, "Second", None),
    )
    .await
    .unwrap();

    assert_eq!(list_nodes(&db.conn, &first).await.unwrap()[0].name, "First");
    assert_eq!(list_nodes(&db.conn, &second).await.unwrap()[0].name, "Second");
    assert_eq!(
        list_nodes(
            &db.conn,
            &Scope {
                org_id: first.org_id,
                user_id: second.user_id,
            },
        )
        .await,
        Err(CatalogError::Unauthorized)
    );
}

#[tokio::test]
async fn workspace_catalog_create_trims_label_and_validates_root_rules() {
    let db = fresh_in_memory_db().await;
    let (scope, _) = seed_two_scopes(&db).await;
    let padded = format!("  {}  ", "界".repeat(200));
    let created = create_node(
        &db.conn,
        &scope,
        create_input(None, WorkspaceNodeKind::Project, &padded, None),
    )
    .await
    .unwrap();
    assert_eq!(created.name, "界".repeat(200));

    for input in [
        create_input(
            Some(created.id),
            WorkspaceNodeKind::Project,
            "nested project",
            None,
        ),
        create_input(None, WorkspaceNodeKind::Folder, "root folder", None),
        create_input(
            None,
            WorkspaceNodeKind::WorkingDirectory,
            "root checkout",
            Some(RepositoryDefault {
                repository_id: 1,
                starting_branch: "main".into(),
            }),
        ),
    ] {
        assert!(matches!(
            create_node(&db.conn, &scope, input).await,
            Err(CatalogError::InvalidInput(_))
        ));
    }
}

#[tokio::test]
async fn workspace_catalog_rejects_cross_tenant_parent_and_repository_ids() {
    let db = fresh_in_memory_db().await;
    let (first, second) = seed_two_scopes(&db).await;
    let first_root = create_node(
        &db.conn,
        &first,
        create_input(None, WorkspaceNodeKind::Project, "First", None),
    )
    .await
    .unwrap();
    let second_root = create_node(
        &db.conn,
        &second,
        create_input(None, WorkspaceNodeKind::Project, "Second", None),
    )
    .await
    .unwrap();
    let foreign_repository_id = seed_repository(&db, second.org_id, "2000").await;

    assert_eq!(
        create_node(
            &db.conn,
            &first,
            create_input(
                Some(second_root.id),
                WorkspaceNodeKind::Folder,
                "Foreign parent",
                None,
            ),
        )
        .await,
        Err(CatalogError::NotFound)
    );
    assert_eq!(
        create_node(
            &db.conn,
            &first,
            create_input(
                Some(first_root.id),
                WorkspaceNodeKind::WorkingDirectory,
                "Foreign repository",
                Some(RepositoryDefault {
                    repository_id: foreign_repository_id,
                    starting_branch: "main".into(),
                }),
            ),
        )
        .await,
        Err(CatalogError::NotFound)
    );
}

#[tokio::test]
async fn workspace_catalog_rejects_self_parent_and_descendant_move() {
    let db = fresh_in_memory_db().await;
    let (scope, _) = seed_two_scopes(&db).await;
    let root = create_node(
        &db.conn,
        &scope,
        create_input(None, WorkspaceNodeKind::Project, "Root", None),
    )
    .await
    .unwrap();
    let parent = create_node(
        &db.conn,
        &scope,
        create_input(
            Some(root.id),
            WorkspaceNodeKind::Folder,
            "Parent",
            None,
        ),
    )
    .await
    .unwrap();
    let child = create_node(
        &db.conn,
        &scope,
        create_input(
            Some(parent.id),
            WorkspaceNodeKind::Folder,
            "Child",
            None,
        ),
    )
    .await
    .unwrap();
    for input in [
        UpdateNodeInput {
            node_id: parent.id,
            expected_revision: parent.revision,
            parent_id: Some(parent.id),
            name: parent.name.clone(),
            repository_default: None,
        },
        UpdateNodeInput {
            node_id: parent.id,
            expected_revision: parent.revision,
            parent_id: Some(child.id),
            name: parent.name.clone(),
            repository_default: None,
        },
    ] {
        assert!(matches!(
            update_node(&db.conn, &scope, input).await,
            Err(CatalogError::InvalidInput(_))
        ));
    }
}

#[tokio::test]
async fn workspace_catalog_allows_logical_folder_below_working_directory_without_filesystem() {
    let db = fresh_in_memory_db().await;
    let (scope, _) = seed_two_scopes(&db).await;
    let repository_id = seed_repository(&db, scope.org_id, "1000").await;
    let root = create_node(
        &db.conn,
        &scope,
        create_input(None, WorkspaceNodeKind::Project, "Root", None),
    )
    .await
    .unwrap();
    let checkout = create_node(
        &db.conn,
        &scope,
        create_input(
            Some(root.id),
            WorkspaceNodeKind::WorkingDirectory,
            "Checkout",
            Some(RepositoryDefault {
                repository_id,
                starting_branch: "main".into(),
            }),
        ),
    )
    .await
    .unwrap();
    let before = folder::Entity::find().count(&db.conn).await.unwrap();

    let logical_folder = create_node(
        &db.conn,
        &scope,
        create_input(
            Some(checkout.id),
            WorkspaceNodeKind::Folder,
            "Nested context",
            None,
        ),
    )
    .await
    .unwrap();

    assert_eq!(logical_folder.parent_id, Some(checkout.id));
    assert_eq!(logical_folder.repository_default, None);
    assert_eq!(folder::Entity::find().count(&db.conn).await.unwrap(), before);
}

#[tokio::test]
async fn workspace_catalog_update_uses_revision_and_preserves_kind() {
    let db = fresh_in_memory_db().await;
    let (scope, _) = seed_two_scopes(&db).await;
    let created = create_node(
        &db.conn,
        &scope,
        create_input(None, WorkspaceNodeKind::Project, "Before", None),
    )
    .await
    .unwrap();
    let updated = update_node(
        &db.conn,
        &scope,
        UpdateNodeInput {
            node_id: created.id,
            expected_revision: 0,
            parent_id: None,
            name: "  After  ".into(),
            repository_default: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(updated.name, "After");
    assert_eq!(updated.kind, WorkspaceNodeKind::Project);
    assert_eq!(updated.revision, 1);

    assert_eq!(
        update_node(
            &db.conn,
            &scope,
            UpdateNodeInput {
                node_id: created.id,
                expected_revision: 0,
                parent_id: None,
                name: "Stale overwrite".into(),
                repository_default: None,
            },
        )
        .await,
        Err(CatalogError::Conflict)
    );
    assert_eq!(list_nodes(&db.conn, &scope).await.unwrap()[0], updated);
}

#[test]
fn workspace_catalog_node_inputs_use_camel_case_and_reject_unknown_fields() {
    let create: CreateNodeInput = serde_json::from_value(serde_json::json!({
        "parentId": null,
        "kind": "project",
        "name": "Catalog",
        "repositoryDefault": null
    }))
    .unwrap();
    assert_eq!(create.name, "Catalog");
    assert!(serde_json::from_value::<UpdateNodeInput>(serde_json::json!({
        "nodeId": 1,
        "expectedRevision": 0,
        "parentId": null,
        "name": "Catalog",
        "repositoryDefault": null,
        "extra": true
    }))
    .is_err());
}

#[tokio::test]
async fn workspace_catalog_migration_rerun_preserves_legacy_rows_and_schema() {
    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/workspace-catalog-legacy").await;
    let conversation_id = seed_conversation(&db, folder_id, AgentType::ClaudeCode).await;

    Migrator::up(&db.conn, None).await.expect("migration rerun");

    assert!(folder::Entity::find_by_id(folder_id)
        .one(&db.conn)
        .await
        .unwrap()
        .is_some());
    assert!(conversation::Entity::find_by_id(conversation_id)
        .one(&db.conn)
        .await
        .unwrap()
        .is_some());
    let draft_columns = db
        .conn
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Sqlite,
            "PRAGMA table_info(workspace_chat_draft)",
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get::<String>("", "name").unwrap())
        .collect::<Vec<_>>();
    assert!(draft_columns.contains(&"request_fingerprint".to_string()));

    let indexes = db
        .conn
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name LIKE 'workspace_%'",
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get::<String>("", "name").unwrap())
        .collect::<Vec<_>>();
    for expected in [
        "idx_workspace_node_org_id",
        "idx_workspace_node_org_parent",
        "idx_workspace_repository_org_id",
        "idx_workspace_chat_draft_org_id",
        "idx_workspace_chat_draft_org_node",
    ] {
        assert!(indexes.iter().any(|actual| actual == expected));
    }
}

#[tokio::test]
async fn workspace_catalog_migration_down_refuses_data_then_drops_empty_tables() {
    let db = fresh_in_memory_db().await;
    // Exercise this migration explicitly; later additive migrations are independent.
    let migration = Migrator::migrations().into_iter()
        .find(|migration| migration.name() == "m20260907_000001_workspace_catalog")
        .unwrap();
    let manager = sea_orm_migration::SchemaManager::new(&db.conn);
    let (scope, _) = seed_two_scopes(&db).await;
    let created = create_node(
        &db.conn,
        &scope,
        create_input(None, WorkspaceNodeKind::Project, "Keep", None),
    )
    .await
    .unwrap();

    assert!(migration.down(&manager).await.is_err());
    assert_eq!(
        list_nodes(&db.conn, &scope).await.unwrap(),
        vec![created.clone()]
    );

    crate::db::entities::workspace_node::Entity::delete_by_id(created.id)
        .exec(&db.conn)
        .await
        .unwrap();
    migration.down(&manager)
        .await
        .expect("drop empty catalog tables");
    let remaining = db
        .conn
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('workspace_repository', 'workspace_node', 'workspace_chat_draft')",
        ))
        .await
        .unwrap();
    assert!(remaining.is_empty());
}
