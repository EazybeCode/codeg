use super::*;
use crate::neev::test_support as h;
use sea_orm::{ConnectionTrait, Statement};

#[tokio::test]
async fn fresh_schema_is_idempotent_and_does_not_need_github() {
    let db = h::empty_database().await;
    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(h::count(&db, "neev_accounts").await, 0);
    assert_eq!(h::count(&db, "neev_schema_version").await, 1);
    assert!(db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            "SELECT * FROM users"
        ))
        .await
        .is_err());
}

#[tokio::test]
async fn refuses_legacy_database_without_writing_native_tables() {
    let db = h::empty_database().await;
    h::execute(&db, "CREATE TABLE users (id INTEGER PRIMARY KEY)", vec![]).await;
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert!(db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            "SELECT * FROM neev_accounts"
        ))
        .await
        .is_err());
}

#[tokio::test]
async fn refuses_unknown_schema_version() {
    let db = h::database().await;
    h::execute(&db, "UPDATE neev_schema_version SET version=99", vec![]).await;
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
}

#[tokio::test]
async fn fresh_schema_is_version_eleven_with_separate_branding_revision() {
    let db = h::database().await;
    let version = db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            "SELECT version FROM neev_schema_version WHERE id=1".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "version")
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);

    let user = h::native_account(&db, "schema-owner", "schema@example.test").await;
    let row = db
        .query_one(Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO neev_organizations(owner_user_id,name,revision,disabled,created_at) VALUES($1,'Schema org',1,false,1000) RETURNING branding_revision",
            vec![user.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<i64>("", "branding_revision").unwrap(), 1);
}

#[tokio::test]
async fn version_one_native_schema_is_refused_without_migration() {
    let db = h::database().await;
    h::execute(&db, "UPDATE neev_schema_version SET version=1", vec![]).await;
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
}

#[tokio::test]
async fn membership_requires_real_account_and_org() {
    let db = h::database().await;
    let result = db.execute(Statement::from_string(db.get_database_backend(), "INSERT INTO neev_memberships(org_id,user_id,role,revision,active) VALUES(999,999,'admin',1,true)")).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn scoped_session_requires_all_revisions_not_sql_null() {
    let db = h::database().await;
    let user = h::native_account(&db, "owner", "support@neev.so").await;
    h::execute(&db,"INSERT INTO neev_organizations(id,owner_user_id,name,revision,disabled,created_at) VALUES(1,$1,'Demo',1,false,1000)",vec![user.into()]).await;
    h::execute(&db,"INSERT INTO neev_memberships(id,org_id,user_id,role,revision,active) VALUES(1,1,$1,'admin',1,true)",vec![user.into()]).await;
    let result=db.execute(Statement::from_sql_and_values(db.get_database_backend(),"INSERT INTO neev_sessions(id,token_hash,csrf_hash,user_id,account_revision,org_id,member_id,member_revision,org_revision,authenticated_at,expires_at) VALUES('session',$1,$2,$3,1,1,1,NULL,NULL,1000,2000)",vec!["1".repeat(64).into(),"2".repeat(64).into(),user.into()])).await;
    assert!(
        result.is_err(),
        "partially scoped session must violate schema constraint"
    );
}

#[tokio::test]
#[ignore = "requires explicitly isolated NEEV_TEST_DATABASE_URL"]
async fn postgres_schema_reinitialization_and_foreign_keys() {
    let (db, admin, schema) = h::postgres_database().await;
    assert_eq!(initialize(&db).await, Ok(()));
    let result=db.execute(Statement::from_string(db.get_database_backend(),"INSERT INTO neev_memberships(org_id,user_id,role,revision,active) VALUES(999,999,'admin',1,true)")).await;
    assert!(result.is_err());
    h::cleanup_postgres(db, admin, schema).await;
}

/// Strip schema-15 attachment storage, for a fixture rebuilding an older shape.
///
/// Every historical fixture in this directory starts from a FRESH database and
/// peels layers off until it has the inventory of the version under test. Since
/// schema 15 that fresh database carries `neev_attachments` and
/// `neev_attachment_refs`, and a fixture that leaves them behind is not the
/// historical schema it claims to be: `initialize`'s inventory gate refuses the
/// unexpected tables outright, so the migration the test exists to exercise
/// never runs and the failure says nothing about the path under test.
///
/// Order is not incidental. `neev_attachment_refs` carries foreign keys to
/// `neev_attachments`, `neev_messages` and `neev_agent_turns`, so the references
/// go first and the attachments they point at second. Dropping the other way
/// round — or dropping the session and message tables before either — leaves a
/// referencing table pointing at a parent that is gone.
/// Take schema 16 off a fresh database: the mentions a message carries.
///
/// Separate from the attachment tables because the two versions are separate —
/// a fixture that wants to BE schema 15 drops this and keeps those.
/// Remove what schemas 18 (library), 19 (steer) and 21 (folder sharing
/// defaults) added, so a fixture that rebuilds an OLDER inventory really has
/// that inventory. Sharing defaults and steer first (they reference nodes and
/// turns), then library files before items.
pub(super) async fn drop_tables_after_seventeen(db: &DatabaseConnection) {
    for table in [
        "neev_node_sharing_defaults",
        "neev_steer_inputs",
        "neev_library_files",
        "neev_library_items",
    ] {
        execute(db, &format!("DROP TABLE IF EXISTS {table}"), vec![])
            .await
            .unwrap();
    }
}

pub(super) async fn drop_mention_tables(db: &DatabaseConnection) {
    drop_tables_after_seventeen(db).await;
    execute(db, "DROP TABLE neev_message_mentions", vec![])
        .await
        .unwrap();
}

pub(super) async fn drop_attachment_tables(db: &DatabaseConnection) {
    for table in ["neev_attachment_refs", "neev_attachments"] {
        execute(db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
}
