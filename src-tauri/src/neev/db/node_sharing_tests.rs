//! Schema 21: folder sharing defaults. Fresh install, 20 -> 21 preserving every
//! row, idempotent restart, refusal of partial or tampered shapes, and the
//! guarded 21 -> 20 downgrade that never discards a configured default.

use super::*;
use crate::neev::test_support as h;
use sea_orm::DatabaseConnection;

const DOC: &str = "11111111-1111-4111-8111-111111111111";
const CHAT: &str = "22222222-2222-4222-8222-222222222222";

async fn version(db: &DatabaseConnection) -> i64 {
    one(db, "SELECT version FROM neev_schema_version WHERE id=1", vec![])
        .await
        .unwrap()
        .unwrap()
        .try_get("", "version")
        .unwrap()
}

async fn has_table(db: &DatabaseConnection) -> bool {
    let sql = match db.get_database_backend() {
        DbBackend::Postgres => "SELECT 1 AS x FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname='neev_node_sharing_defaults'",
        _ => "SELECT 1 AS x FROM sqlite_master WHERE type='table' AND name='neev_node_sharing_defaults'",
    };
    one(db, sql, vec![]).await.unwrap().is_some()
}

/// An org with a project, a nested folder and a private + a shared resource.
async fn seed(db: &DatabaseConnection) -> (i32, i32, i32, i32) {
    let user = h::native_account(db, "sharing-owner", "sharing@example.test").await;
    let (org, _) = h::organization(db, user, "admin").await;
    let project: i32 = one(db, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,NULL,'project','Shared project',3,$2) RETURNING id", vec![org.into(), user.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    let folder: i32 = one(db, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,$2,'folder','Nested',5,$3) RETURNING id", vec![org.into(), project.into(), user.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    for (id, kind, node, visibility) in [
        (DOC, "document", folder, "private"),
        (CHAT, "chat", project, "organization"),
    ] {
        execute(db, "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,$3,$4,$5,'Kept',$6,NULL,NULL,4,0,false,1000,1000)", vec![org.into(), id.into(), kind.into(), node.into(), user.into(), visibility.into()]).await.unwrap();
    }
    (org, user, project, folder)
}

/// Every node and resource row as text, so "unchanged" means byte-identical.
async fn rows(db: &DatabaseConnection) -> Vec<String> {
    let mut out = Vec::new();
    for sql in [
        "SELECT CAST(id AS text) || '|' || CAST(org_id AS text) || '|' || coalesce(CAST(parent_id AS text),'-') || '|' || kind || '|' || name || '|' || CAST(revision AS text) || '|' || CAST(created_by AS text) AS line FROM neev_nodes ORDER BY id",
        "SELECT CAST(org_id AS text) || '|' || id || '|' || kind || '|' || CAST(node_id AS text) || '|' || CAST(creator_id AS text) || '|' || title || '|' || visibility || '|' || coalesce(origin_chat_id,'-') || '|' || CAST(auth_revision AS text) || '|' || CAST(content_revision AS text) || '|' || CASE WHEN archived THEN 't' ELSE 'f' END || '|' || CAST(created_at AS text) || '|' || CAST(updated_at AS text) AS line FROM neev_resources ORDER BY id",
    ] {
        for row in db
            .query_all(statement(db, sql, vec![]))
            .await
            .unwrap()
        {
            out.push(row.try_get::<String>("", "line").unwrap());
        }
    }
    out
}

async fn catalog(db: &DatabaseConnection) -> Vec<String> {
    let sql = match db.get_database_backend() {
        DbBackend::Postgres => "SELECT c.relname || ':' || c.relkind::text AS item FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() UNION ALL SELECT t.relname || ':' || k.conname || ':' || pg_get_constraintdef(k.oid) AS item FROM pg_constraint k JOIN pg_class t ON t.oid=k.conrelid JOIN pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname=current_schema() ORDER BY item",
        _ => "SELECT type || ':' || name || ':' || tbl_name || ':' || coalesce(sql,'') AS item FROM sqlite_master ORDER BY name",
    };
    db.query_all(statement(db, sql, vec![]))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<String>("", "item").unwrap())
        .collect()
}

/// A fresh install is schema 24; these tests are about the 21 layer, so take
/// the (empty) schema-24 sharing, schema-23 trash and schema-22 knowledge
/// layers off first.
async fn at_twenty_one(db: &DatabaseConnection) {
    assert_eq!(downgrade_msteams(db).await, Ok(msteams_migration::Downgrade::Done));
    assert_eq!(downgrade_member_sharing(db).await, Ok(member_sharing_migration::Downgrade::Done));
    assert_eq!(downgrade_node_trash(db).await, Ok(node_trash_migration::Downgrade::Done));
    assert_eq!(
        downgrade_knowledge(db).await,
        Ok(knowledge_migration::Downgrade::Downgraded)
    );
    assert_eq!(version(db).await, node_sharing_migration::SCHEMA_VERSION);
}

async fn set_default(db: &DatabaseConnection, org: i32, node: i32, user: i32, value: &str) -> Result<(), AuthError> {
    execute(db, "INSERT INTO neev_node_sharing_defaults(org_id,node_id,default_visibility,set_by,updated_at) VALUES($1,$2,$3,$4,1000)", vec![org.into(), node.into(), value.into(), user.into()]).await.map(|_| ())
}

async fn fresh_is_twenty_one_and_constrained(db: &DatabaseConnection) {
    at_twenty_one(db).await;
    assert_eq!(version(db).await, node_sharing_migration::SCHEMA_VERSION);
    assert_eq!(node_sharing_migration::validate(db).await, Ok(()));
    let (org, user, project, folder) = seed(db).await;
    set_default(db, org, project, user, "organization").await.unwrap();
    set_default(db, org, folder, user, "private").await.unwrap();
    // One explicit default per node.
    assert!(set_default(db, org, project, user, "private").await.is_err());
    // Only the two audiences; no "public", no case games, no empty value.
    let other = h::native_account(db, "sharing-other", "other@example.test").await;
    let (other_org, _) = h::organization(db, other, "admin").await;
    let foreign: i32 = one(db, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,NULL,'project','Theirs',1,$2) RETURNING id", vec![other_org.into(), other.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    for bad in ["public", "Organization", ""] {
        assert!(set_default(db, other_org, foreign, other, bad).await.is_err(), "{bad}");
    }
    // The node FK carries the org: a valid node id cannot be claimed by another tenant.
    assert!(set_default(db, other_org, project, other, "organization").await.is_err());
    // Deleting a node removes its default with it.
    execute(db, "DELETE FROM neev_resources WHERE node_id=$1", vec![folder.into()]).await.unwrap();
    execute(db, "DELETE FROM neev_nodes WHERE id=$1", vec![folder.into()]).await.unwrap();
    assert_eq!(h::count(db, "neev_node_sharing_defaults").await, 1);
    assert_eq!(validate_content_snapshot(db).await, Ok(node_sharing_migration::SCHEMA_VERSION));
}

/// 20 -> 21 changes no existing row; restart at 21 is a no-op.
async fn twenty_upgrades_preserving_rows(db: &DatabaseConnection) {
    at_twenty_one(db).await;
    seed(db).await;
    assert_eq!(downgrade_node_sharing(db).await, Ok(node_sharing_migration::Downgrade::Done));
    assert_eq!(version(db).await, 20);
    assert!(!has_table(db).await);
    assert_eq!(validate_content_snapshot(db).await, Ok(20));
    let before = rows(db).await;
    assert_eq!(before.len(), 4);

    initialize(db).await.unwrap();
    assert_eq!(version(db).await, CURRENT_SCHEMA_VERSION);
    assert!(has_table(db).await);
    assert_eq!(h::count(db, "neev_node_sharing_defaults").await, 0);
    assert_eq!(rows(db).await, before, "the migration rewrote an existing row");

    let catalog_once = catalog(db).await;
    initialize(db).await.unwrap();
    assert_eq!(version(db).await, CURRENT_SCHEMA_VERSION);
    assert_eq!(rows(db).await, before);
    assert_eq!(catalog(db).await, catalog_once, "restart changed the catalog");
}

/// The downgrade restores exactly the catalog a schema-20 install has: the
/// fresh-21 catalog minus the one table (constraint names included), and the
/// same on every round trip.
async fn downgrade_restores_the_exact_catalog(db: &DatabaseConnection) {
    at_twenty_one(db).await;
    let fresh = catalog(db).await;
    let expected: Vec<String> = fresh
        .iter()
        .filter(|item| !item.contains("neev_node_sharing_defaults"))
        .cloned()
        .collect();
    assert!(expected.len() < fresh.len());
    seed(db).await;
    let before = rows(db).await;
    assert_eq!(downgrade_node_sharing(db).await, Ok(node_sharing_migration::Downgrade::Done));
    assert_eq!(catalog(db).await, expected);
    assert_eq!(rows(db).await, before);
    // Guarded on the marker: a second run changes nothing.
    assert_eq!(
        downgrade_node_sharing(db).await,
        Ok(node_sharing_migration::Downgrade::NotAtTwentyOne(20))
    );
    // The downgraded database is a valid schema 20 (the whole 20 arm runs) and
    // re-migrates; a second round trip lands on the same catalog.
    initialize(db).await.unwrap();
    // Re-migration goes all the way to 23; step back to compare with the 21 catalog.
    at_twenty_one(db).await;
    assert_eq!(catalog(db).await, fresh);
    assert_eq!(downgrade_node_sharing(db).await, Ok(node_sharing_migration::Downgrade::Done));
    assert_eq!(catalog(db).await, expected);
    assert_eq!(rows(db).await, before);
    initialize(db).await.unwrap();
    assert_eq!(version(db).await, CURRENT_SCHEMA_VERSION);
}

/// A configured default is a user choice: the downgrade refuses and changes nothing.
async fn downgrade_refuses_over_configured_defaults(db: &DatabaseConnection) {
    at_twenty_one(db).await;
    let (org, user, project, folder) = seed(db).await;
    set_default(db, org, project, user, "organization").await.unwrap();
    set_default(db, org, folder, user, "private").await.unwrap();
    let before = rows(db).await;
    let catalog_before = catalog(db).await;
    assert_eq!(
        downgrade_node_sharing(db).await,
        Ok(node_sharing_migration::Downgrade::DefaultsExist { rows: 2 })
    );
    assert_eq!(version(db).await, node_sharing_migration::SCHEMA_VERSION);
    assert_eq!(h::count(db, "neev_node_sharing_defaults").await, 2);
    assert_eq!(rows(db).await, before);
    assert_eq!(catalog(db).await, catalog_before);
    // Still a healthy 21.
    initialize(db).await.unwrap();
    assert_eq!(h::count(db, "neev_node_sharing_defaults").await, 2);
}

#[tokio::test]
async fn sqlite_fresh_schema_is_twenty_one_and_constrained() {
    let db = h::database().await;
    fresh_is_twenty_one_and_constrained(&db).await;
}

#[tokio::test]
async fn sqlite_twenty_upgrades_to_twenty_one_preserving_rows() {
    let db = h::database().await;
    twenty_upgrades_preserving_rows(&db).await;
}

#[tokio::test]
async fn sqlite_downgrade_restores_the_exact_schema_twenty_catalog() {
    let db = h::database().await;
    downgrade_restores_the_exact_catalog(&db).await;
    let integrity: String = one(&db, "PRAGMA integrity_check", vec![])
        .await
        .unwrap()
        .unwrap()
        .try_get("", "integrity_check")
        .unwrap();
    assert_eq!(integrity, "ok");
    assert!(one(&db, "PRAGMA foreign_key_check", vec![]).await.unwrap().is_none());
}

#[tokio::test]
async fn sqlite_downgrade_refuses_while_defaults_are_configured() {
    let db = h::database().await;
    downgrade_refuses_over_configured_defaults(&db).await;
}

#[tokio::test]
async fn sqlite_partial_or_tampered_schema_twenty_one_is_refused_without_change() {
    // Marker 20 with the 21 table already present: unknown combination.
    let db = h::database().await;
    seed(&db).await;
    execute(&db, "UPDATE neev_schema_version SET version=20 WHERE id=1", vec![]).await.unwrap();
    let before = rows(&db).await;
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(version(&db).await, 20);
    assert_eq!(rows(&db).await, before);

    // Marker 21 without the table.
    let db = h::database().await;
    at_twenty_one(&db).await;
    execute(&db, "DROP TABLE neev_node_sharing_defaults", vec![]).await.unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(validate_content_snapshot(&db).await, Err(AuthError::Unavailable));
    assert_eq!(downgrade_node_sharing(&db).await, Err(AuthError::Unavailable));
    assert_eq!(version(&db).await, 21);

    // Marker 21 with a look-alike table that lost its audience CHECK.
    let db = h::database().await;
    at_twenty_one(&db).await;
    execute(&db, "DROP TABLE neev_node_sharing_defaults", vec![]).await.unwrap();
    execute(&db, "CREATE TABLE neev_node_sharing_defaults (org_id integer NOT NULL, node_id integer NOT NULL, default_visibility text NOT NULL, set_by integer NOT NULL, updated_at bigint NOT NULL, PRIMARY KEY (org_id, node_id))", vec![]).await.unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(validate_content_snapshot(&db).await, Err(AuthError::Unavailable));
    // The downgrade validates before it drops: it refuses an unknown shape too.
    assert_eq!(downgrade_node_sharing(&db).await, Err(AuthError::Unavailable));
    assert!(has_table(&db).await);

    // A default smuggled onto a working directory is refused by the validators.
    let db = h::database().await;
    let (org, user, project, _) = seed(&db).await;
    let directory: i32 = one(&db, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,$2,'working_directory','wd',1,$3) RETURNING id", vec![org.into(), project.into(), user.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    set_default(&db, org, directory, user, "organization").await.unwrap();
    assert_eq!(validate_content_snapshot(&db).await, Err(AuthError::Unavailable));
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_fresh_schema_is_twenty_one_and_constrained() {
    let (db, admin, schema) = h::postgres_database().await;
    fresh_is_twenty_one_and_constrained(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_twenty_upgrades_to_twenty_one_preserving_rows() {
    let (db, admin, schema) = h::postgres_database().await;
    twenty_upgrades_preserving_rows(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_downgrade_restores_the_exact_schema_twenty_catalog() {
    let (db, admin, schema) = h::postgres_database().await;
    downgrade_restores_the_exact_catalog(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_downgrade_refuses_while_defaults_are_configured() {
    let (db, admin, schema) = h::postgres_database().await;
    downgrade_refuses_over_configured_defaults(&db).await;
    h::cleanup_postgres(db, admin, schema).await;
}
