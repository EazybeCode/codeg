use sea_orm::{ConnectionTrait, Database, EntityTrait, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

use super::{m20260910_000002_team_catalog_admission::Migration, Migrator};

#[tokio::test]
async fn team_catalog_admission_is_empty_on_upgrade_and_down_refuses_data() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let position = Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == "m20260910_000002_team_catalog_admission")
        .unwrap();
    Migrator::up(&db, Some(position as u32)).await.unwrap();
    db.execute(Statement::from_string(
        db.get_database_backend(),
        "INSERT INTO workspace_repository (id, org_id, github_repository_id, full_name, verified_names, created_at, updated_at) VALUES (91, 1, '9001', 'legacy/repository', '[\"legacy/repository\"]', '2026-09-10T00:00:00Z', '2026-09-10T00:00:00Z')",
    )).await.unwrap();
    Migrator::up(&db, None).await.unwrap();
    let manager = SchemaManager::new(&db);
    Migration.up(&manager).await.unwrap();
    let count = db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            "SELECT COUNT(*) AS count FROM team_catalog_admission",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "count")
        .unwrap();
    assert_eq!(count, 0);
    assert!(db.query_one(Statement::from_string(
        db.get_database_backend(),
        "SELECT id FROM workspace_repository WHERE id=91",
    )).await.unwrap().is_some());

    db.execute(Statement::from_string(
        db.get_database_backend(),
        "INSERT INTO team_catalog_admission (resource_kind, resource_id, org_id, creator_user_id, created_at) VALUES ('repository', 1, 1, 1, '2026-09-10T00:00:00Z')",
    ))
    .await
    .unwrap();
    assert!(Migration.down(&manager).await.is_err());
}

#[tokio::test]
async fn team_catalog_admission_constraints_and_empty_down_are_enforced() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    Migration.up(&manager).await.unwrap();
    for values in [
        "('invented', 1, 1, 1, '2026-09-10T00:00:00Z')",
        "('node', 0, 1, 1, '2026-09-10T00:00:00Z')",
        "('chat', 1, 0, 1, '2026-09-10T00:00:00Z')",
        "('chat', 1, 1, 0, '2026-09-10T00:00:00Z')",
    ] {
        let result = db
            .execute(Statement::from_string(
                db.get_database_backend(),
                format!("INSERT INTO team_catalog_admission (resource_kind, resource_id, org_id, creator_user_id, created_at) VALUES {values}"),
            ))
            .await;
        assert!(result.is_err(), "accepted {values}");
    }
    Migration.down(&manager).await.unwrap();
    assert!(!manager.has_table("team_catalog_admission").await.unwrap());
}

#[tokio::test]
#[ignore = "requires local disposable PostgreSQL"]
async fn team_catalog_admission_postgres_rerun_constraints_and_down_refusal() {
    use crate::team_access::test_auth::{create_postgres_principal_fixture, drop_postgres_fixture};
    use crate::db::entities::team_catalog_admission;

    let (database_name, admin, fixture) = create_postgres_principal_fixture(Vec::new())
        .await
        .expect("create disposable PostgreSQL team catalog fixture");
    let outcome: Result<(), String> = async {
        fixture.db.execute(Statement::from_string(
            fixture.db.get_database_backend(),
            "DROP TABLE team_catalog_admission",
        )).await.map_err(|_| "remove admission table to simulate upgrade".to_owned())?;
        fixture.db.execute(Statement::from_string(
            fixture.db.get_database_backend(),
            "DELETE FROM seaql_migrations WHERE version='m20260910_000002_team_catalog_admission'",
        )).await.map_err(|_| "rewind admission migration marker".to_owned())?;
        fixture.db.execute(Statement::from_string(
            fixture.db.get_database_backend(),
            "INSERT INTO workspace_repository (org_id, github_repository_id, full_name, verified_names, created_at, updated_at) VALUES (201, '7777', 'legacy/preserved', '[\"legacy/preserved\"]', '2026-09-10T00:00:00Z', '2026-09-10T00:00:00Z')",
        )).await.map_err(|_| "seed PostgreSQL pre-upgrade catalog".to_owned())?;
        Migrator::up(&fixture.db, None).await.map_err(|_| "upgrade PostgreSQL catalog".to_owned())?;
        let preserved = fixture.db.query_one(Statement::from_string(
            fixture.db.get_database_backend(),
            "SELECT id FROM workspace_repository WHERE github_repository_id='7777'",
        )).await.map_err(|_| "query preserved PostgreSQL catalog".to_owned())?.is_some();
        if !preserved { return Err("PostgreSQL upgrade changed legacy catalog data".into()); }
        let manager = SchemaManager::new(&fixture.db);
        Migration.up(&manager).await.map_err(|_| "rerun migration".to_owned())?;
        let invalid = fixture.db.execute(Statement::from_string(
            fixture.db.get_database_backend(),
            "INSERT INTO team_catalog_admission (resource_kind, resource_id, org_id, creator_user_id, created_at) VALUES ('invalid', 1, 1, 1, '2026-09-10T00:00:00Z')",
        )).await;
        if invalid.is_ok() { return Err("PostgreSQL accepted invalid resource kind".into()); }
        fixture.db.execute(Statement::from_string(
            fixture.db.get_database_backend(),
            "INSERT INTO team_catalog_admission (resource_kind, resource_id, org_id, creator_user_id, created_at) VALUES ('repository', 1, 201, 301, '2026-09-10T00:00:00Z')",
        )).await.map_err(|_| "insert PostgreSQL admission".to_owned())?;
        if Migration.down(&manager).await.is_ok() { return Err("PostgreSQL down accepted live admission".into()); }
        team_catalog_admission::Entity::delete_many().exec(&fixture.db).await.map_err(|_| "clear admission for cleanup".to_owned())?;
        Ok(())
    }.await;
    if let Err(error) = outcome {
        panic!("{error}; preserved database {database_name}");
    }
    drop_postgres_fixture(&database_name, admin, fixture.db)
        .await
        .expect("drop verified PostgreSQL team catalog fixture");
}
