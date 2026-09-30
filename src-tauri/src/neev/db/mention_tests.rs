//! Schema-16 migration tests.
//!
//! Schema 16 adds a TABLE, which is the shape that has broken this migrator
//! before: schema 15 did the same and every live database refused to start
//! afterwards, because the guards that decide which additive upgrades still
//! need applying named only the older inventories. The test that matters here
//! is [`a_real_fifteen_upgrades_and_reopens`] — it builds the inventory a
//! deployed schema-15 database actually has, migrates it, and then opens it
//! twice more, because a restart is not a one-off event.

use super::*;
use crate::neev::test_support as h;
use sea_orm::{ConnectionTrait, DatabaseConnection};

async fn schema_version(db: &DatabaseConnection) -> i64 {
    one(
        db,
        "SELECT version FROM neev_schema_version WHERE id=1",
        vec![],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<i64>("", "version")
    .unwrap()
}

async fn count(db: &DatabaseConnection, table: &str) -> i64 {
    one(db, &format!("SELECT COUNT(*) AS c FROM {table}"), vec![])
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "c")
        .unwrap()
}

/// Turn a freshly migrated database back into the one a schema-15 deployment
/// really has: every schema-15 table, no mentions table, marker 15.
///
/// Built by subtraction rather than by hand on purpose — a hand-written
/// fixture would be a guess about what is deployed, and the thing under test is
/// precisely whether the migrator recognises the REAL inventory.
async fn downgrade_to_fifteen(db: &DatabaseConnection) {
    super::tests::drop_tables_after_seventeen(db).await;
    execute(db, "DROP TABLE neev_message_mentions", vec![])
        .await
        .unwrap();
    execute(
        db,
        "UPDATE neev_schema_version SET version=$1 WHERE id=1",
        vec![15i64.into()],
    )
    .await
    .unwrap();
}

async fn fresh_install_is_sixteen(db: DatabaseConnection) {
    assert_eq!(schema_version(&db).await, CURRENT_SCHEMA_VERSION);
    assert_eq!(count(&db, "neev_message_mentions").await, 0);
    // A fresh install validates as what it says it is.
    assert_eq!(
        validate_content_snapshot(&db).await,
        Ok(CURRENT_SCHEMA_VERSION)
    );
}

/// The blocker schema 15 hit, as a test for schema 16.
async fn a_real_fifteen_upgrades_and_reopens(db: DatabaseConnection) {
    let account = h::native_account(&db, "mention-migration", "mention@example.test").await;
    assert!(account > 0);
    let accounts = count(&db, "neev_accounts").await;
    downgrade_to_fifteen(&db).await;
    assert_eq!(schema_version(&db).await, 15);

    // Up.
    assert_eq!(
        initialize(&db).await,
        Ok(()),
        "a real schema-15 database must migrate to 16"
    );
    assert_eq!(schema_version(&db).await, CURRENT_SCHEMA_VERSION);
    assert_eq!(count(&db, "neev_message_mentions").await, 0);

    // And open again. Twice: the schema-15 failure was invisible to every test
    // that opened a database only once.
    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(schema_version(&db).await, CURRENT_SCHEMA_VERSION);
    // Nothing was reset on the way through.
    assert_eq!(count(&db, "neev_accounts").await, accounts);
    assert_eq!(
        validate_content_snapshot(&db).await,
        Ok(CURRENT_SCHEMA_VERSION)
    );
}

/// A marker claiming 16 over a schema-15 inventory is not a schema-16 database,
/// and is refused rather than repaired.
async fn a_lying_marker_is_refused(db: DatabaseConnection) {
    execute(&db, "DROP TABLE neev_message_mentions", vec![])
        .await
        .unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(schema_version(&db).await, CURRENT_SCHEMA_VERSION);
}

#[tokio::test]
async fn sqlite_fresh_install_is_sixteen() {
    fresh_install_is_sixteen(h::database().await).await;
}

#[tokio::test]
async fn sqlite_a_real_fifteen_upgrades_and_reopens() {
    a_real_fifteen_upgrades_and_reopens(h::database().await).await;
}

#[tokio::test]
async fn sqlite_a_lying_marker_is_refused() {
    a_lying_marker_is_refused(h::database().await).await;
}

/// The same three against real PostgreSQL, which is what production runs and
/// the only place the storage-class half of `validate` executes at all.
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_mention_migration() {
    let (db, admin, schema) = h::postgres_database().await;
    fresh_install_is_sixteen(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;

    let (db, admin, schema) = h::postgres_database().await;
    a_real_fifteen_upgrades_and_reopens(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;

    let (db, admin, schema) = h::postgres_database().await;
    a_lying_marker_is_refused(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
}
