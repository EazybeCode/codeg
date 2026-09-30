//! Schema-15 migration tests.
//!
//! The one that matters most is [`reopening_a_migrated_database_succeeds`].
//! Schema 15 is the first version since 11 to add TABLES, so it is the first
//! whose inventory differs from the set the early guard in [`super::migrate`]
//! enumerates. That guard runs BEFORE the version match, so a database that had
//! migrated perfectly was refused the next time the server opened it: the
//! process came up once, migrated, served, and then never started again.
//!
//! Every existing migration test opened a database once. None of them could
//! have caught it, which is why these open twice and assert the rows survive.

use super::*;
use crate::neev::test_support as h;
use sea_orm::{ConnectionTrait, DatabaseConnection, Value};

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
    one(db, &format!("SELECT COUNT(*) AS n FROM {table}"), vec![])
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap()
}

async fn execute(db: &DatabaseConnection, sql: &str, args: Vec<Value>) {
    db.execute(statement(db, sql, args)).await.unwrap();
}

/// A real tenant, chat, message and agent session.
///
/// Built through `OrgHarness` and the content APIs rather than hand-written
/// INSERTs. The first version of this seeded rows by hand and immediately hit a
/// column that does not exist; every constraint it did not know about was
/// another failure waiting. The harness already owns the shape.
///
/// Returns (org_id, chat_id, message_id, session_id).
async fn seed(harness: &h::OrgHarness) -> (i32, String, String, String) {
    let actor = harness.owner_context().clone();
    let state = harness.state();
    let node = crate::neev::catalog::nodes::create(
        state,
        &actor,
        crate::neev::types::NodeInput {
            parent_id: None,
            kind: crate::neev::types::NodeKind::Project,
            name: "Project".into(),
        },
    )
    .await
    .unwrap();
    let chat = crate::neev::content::resources::create(
        state,
        &actor,
        crate::neev::content::types::ResourceInput {
            node_id: node.id,
            kind: crate::neev::content::types::ResourceKind::Chat,
            title: "Chat".into(),
            origin_chat_id: None,
            request_id: uuid::Uuid::new_v4().hyphenated().to_string(),
        },
    )
    .await
    .unwrap();
    let message = crate::neev::content::messages::append(
        state,
        &actor,
        &chat.id,
        &uuid::Uuid::new_v4().hyphenated().to_string(),
        chat.auth_revision,
        "hi",
    )
    .await
    .unwrap();
    // The session and its turn are written directly: this shape is proven by
    // the content-side attachment tests, and `sessions::create` validates
    // against a provider catalogue that has nothing to do with storage.
    let session = uuid::Uuid::new_v4().hyphenated().to_string();
    execute(
        &state.database,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,visibility,kind,bound_chat_id,bound_kind,archived,revision,created_at,updated_at) VALUES($1,$2,$3,'S','claude','default','private','chat',NULL,NULL,FALSE,1,0,0)",
        vec![
            actor.org_id.into(),
            session.clone().into(),
            actor.identity.user_id.into(),
        ],
    )
    .await;
    execute(
        &state.database,
        "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,request_id,prompt,response,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,1,$3,$4,NULL,'p','','running',NULL,'claude','default',0,0,NULL)",
        vec![
            actor.org_id.into(),
            session.clone().into(),
            uuid::Uuid::new_v4().hyphenated().to_string().into(),
            uuid::Uuid::new_v4().hyphenated().to_string().into(),
        ],
    )
    .await;
    (actor.org_id, chat.id, message.id, session)
}

/// One attachment on a chat message and one on an agent turn — both owners and
/// both reference shapes, so the validator is exercised on real rows.
async fn attach(
    db: &DatabaseConnection,
    org_id: i32,
    chat_id: &str,
    message_id: &str,
    session_id: &str,
) -> (String, String) {
    let hash = "a".repeat(64);
    let on_chat = uuid::Uuid::new_v4().hyphenated().to_string();
    let on_session = uuid::Uuid::new_v4().hyphenated().to_string();
    execute(
        db,
        "INSERT INTO neev_attachments(org_id,key,chat_id,session_id,file_name,content_type,byte_length,sha256,created_by,created_at) VALUES($1,$2,$3,NULL,'shot.png','image/png',64,$4,1,0)",
        vec![
            org_id.into(),
            on_chat.clone().into(),
            chat_id.to_owned().into(),
            hash.clone().into(),
        ],
    )
    .await;
    execute(
        db,
        "INSERT INTO neev_attachments(org_id,key,chat_id,session_id,file_name,content_type,byte_length,sha256,created_by,created_at) VALUES($1,$2,NULL,$3,'brief.txt','text/plain',20,$4,1,0)",
        vec![
            org_id.into(),
            on_session.clone().into(),
            session_id.to_owned().into(),
            hash.into(),
        ],
    )
    .await;
    execute(
        db,
        "INSERT INTO neev_attachment_refs(org_id,attachment_key,message_id,turn_session_id,turn_sequence,position) VALUES($1,$2,$3,NULL,NULL,0)",
        vec![
            org_id.into(),
            on_chat.clone().into(),
            message_id.to_owned().into(),
        ],
    )
    .await;
    execute(
        db,
        "INSERT INTO neev_attachment_refs(org_id,attachment_key,message_id,turn_session_id,turn_sequence,position) VALUES($1,$2,NULL,$3,1,0)",
        vec![
            org_id.into(),
            on_session.clone().into(),
            session_id.to_owned().into(),
        ],
    )
    .await;
    (on_chat, on_session)
}

#[tokio::test]
async fn fresh_install_creates_attachment_storage_at_version_fifteen() {
    let db = h::database().await;
    assert_eq!(
        schema_version(&db).await,
        CURRENT_SCHEMA_VERSION
    );
    assert_eq!(count(&db, "neev_attachments").await, 0);
    assert_eq!(count(&db, "neev_attachment_refs").await, 0);
}

/// The restart blocker, as a test.
///
/// A migrated database with REAL attachment rows must open again. It did not:
/// the early inventory guard enumerated v2..v11 and refused anything else
/// before the version match could recognise schema 15, so the second
/// `initialize` returned `Unavailable` and the server refused to start.
#[tokio::test]
async fn reopening_a_migrated_database_succeeds() {
    let harness = h::OrgHarness::new().await;
    let (org_id, chat_id, message_id, session_id) = seed(&harness).await;
    let db = harness.state().database.clone();
    let (on_chat, _) = attach(&db, org_id, &chat_id, &message_id, &session_id).await;

    // Re-open, exactly as a restart does.
    assert_eq!(
        initialize(&db).await,
        Ok(()),
        "a migrated schema-15 database must open again"
    );
    assert_eq!(
        schema_version(&db).await,
        CURRENT_SCHEMA_VERSION
    );
    // And again, because a restart is not a one-off event.
    assert_eq!(initialize(&db).await, Ok(()));

    // Every row survived both re-opens: nothing was reset or re-created.
    assert_eq!(count(&db, "neev_attachments").await, 2);
    assert_eq!(count(&db, "neev_attachment_refs").await, 2);
    let surviving: String = one(
        &db,
        "SELECT file_name FROM neev_attachments WHERE key=$1",
        vec![on_chat.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "file_name")
    .unwrap();
    assert_eq!(surviving, "shot.png");
}

/// The validator accepts the rows the application actually writes — both owner
/// kinds and both reference shapes — rather than only an empty table.
#[tokio::test]
async fn the_validator_accepts_real_persisted_attachments() {
    let harness = h::OrgHarness::new().await;
    let (org_id, chat_id, message_id, session_id) = seed(&harness).await;
    let db = harness.state().database.clone();
    attach(&db, org_id, &chat_id, &message_id, &session_id).await;
    assert_eq!(attachment_migration::validate(&db).await, Ok(()));
}

/// And still refuses a reference that would widen an attachment's audience: a
/// chat attachment hung on a message belonging to a different chat.
#[tokio::test]
async fn the_validator_refuses_a_reference_across_conversations() {
    let harness = h::OrgHarness::new().await;
    let (org_id, chat_id, message_id, session_id) = seed(&harness).await;
    let db = harness.state().database.clone();
    let (on_chat, _) = attach(&db, org_id, &chat_id, &message_id, &session_id).await;

    // A second chat, with its own message, in the same tenant.
    let (_, other_chat, other_message, _) = seed(&harness).await;
    assert_ne!(other_chat, chat_id);
    execute(
        &db,
        "INSERT INTO neev_attachment_refs(org_id,attachment_key,message_id,turn_session_id,turn_sequence,position) VALUES($1,$2,$3,NULL,NULL,1)",
        vec![org_id.into(), on_chat.into(), other_message.into()],
    )
    .await;
    assert_eq!(
        attachment_migration::validate(&db).await,
        Err(AuthError::Unavailable),
        "an attachment must not be reachable through another conversation"
    );
}

// ---------------------------------------------------------------------------
// The per-attachment byte ceiling, removed (`attachment_byte_cap_migration`)
// ---------------------------------------------------------------------------

/// The size of the real file that could not be attached before this changed: a
/// generated HTML report. Over BOTH retired caps (10 MiB per file, 25 MiB per
/// turn), so any reintroduction of either fails these tests.
const REAL_HTML_BYTES: i64 = 44_778_046;

/// The table's stored DDL, which is where SQLite keeps a baked CHECK.
async fn attachments_ddl(db: &DatabaseConnection) -> String {
    one(
        db,
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='neev_attachments'",
        vec![],
    )
    .await
    .unwrap()
    .expect("the attachment table exists")
    .try_get::<String>("", "sql")
    .unwrap()
}

/// Insert one attachment of `byte_length`, returning whether the database took it.
///
/// The point is the CHECK, so nothing else about the row varies: a failure here is
/// the byte constraint and not a missing owner or a malformed hash.
async fn try_insert_sized(
    db: &DatabaseConnection,
    org_id: i32,
    chat_id: &str,
    byte_length: i64,
) -> bool {
    db.execute(statement(
        db,
        "INSERT INTO neev_attachments(org_id,key,chat_id,session_id,file_name,content_type,byte_length,sha256,created_by,created_at) VALUES($1,$2,$3,NULL,'report.html','text/plain',$4,$5,1,0)",
        vec![
            org_id.into(),
            uuid::Uuid::new_v4().hyphenated().to_string().into(),
            chat_id.to_owned().into(),
            byte_length.into(),
            "b".repeat(64).into(),
        ],
    ))
    .await
    .is_ok()
}

/// Put schema 15's ranged CHECK back, so the forward migration has something to
/// undo. This is the inverse of `relax_sqlite`, and the table is still empty here,
/// so no rows are copied and the child ref table has nothing pointing at it.
///
/// Sound because the test connection pool is one SQLite connection
/// (`test_support::database`), so these pragmas are the same session the rebuild
/// runs on.
async fn restore_the_old_byte_ceiling(db: &DatabaseConnection) {
    let ddl = attachments_ddl(db).await;
    assert!(
        ddl.contains("byte_length>=1"),
        "expected the relaxed bound to rewrite: {ddl}"
    );
    let old = ddl
        .replacen("neev_attachments", "neev_attachments_old_bound", 1)
        .replace("byte_length>=1", "byte_length BETWEEN 1 AND 10485760");
    let indexes = db
        .query_all(statement(
            db,
            "SELECT sql FROM sqlite_master WHERE type='index' AND tbl_name='neev_attachments' AND sql IS NOT NULL",
            vec![],
        ))
        .await
        .unwrap();
    execute(db, "PRAGMA defer_foreign_keys=ON", vec![]).await;
    execute(db, &old, vec![]).await;
    execute(db, "DROP TABLE neev_attachments", vec![]).await;
    execute(
        db,
        "ALTER TABLE neev_attachments_old_bound RENAME TO neev_attachments",
        vec![],
    )
    .await;
    for index in &indexes {
        let sql: String = index.try_get("", "sql").unwrap();
        execute(db, &sql, vec![]).await;
    }
    execute(db, "PRAGMA defer_foreign_keys=OFF", vec![]).await;
    assert!(
        attachments_ddl(db).await.contains("BETWEEN 1 AND 10485760"),
        "the old ceiling must be back before the migration is asked to remove it"
    );
}

/// A fresh install has no byte ceiling at all, and stores the real file.
#[tokio::test]
async fn a_fresh_install_has_no_attachment_byte_ceiling() {
    let harness = h::OrgHarness::new().await;
    let (org_id, chat_id, _, _) = seed(&harness).await;
    let db = harness.state().database.clone();

    let ddl = attachments_ddl(&db).await;
    assert!(
        !ddl.contains("BETWEEN 1 AND 10485760"),
        "a fresh table must not bake the retired per-file cap: {ddl}"
    );
    assert!(
        ddl.contains("byte_length>=1"),
        "the lower bound stays: {ddl}"
    );
    assert_eq!(
        attachment_byte_cap_migration::validate_attachment_byte_cap(&db).await,
        Ok(())
    );

    // Over the old 10 MiB cap, and over the old 25 MiB turn total.
    assert!(
        try_insert_sized(&db, org_id, &chat_id, REAL_HTML_BYTES).await,
        "a 44 MB attachment must be storable"
    );
    // The lower bound is still a bound: zero bytes is not an attachment.
    assert!(
        !try_insert_sized(&db, org_id, &chat_id, 0).await,
        "byte_length=0 must still be refused"
    );
}

/// The forward migration removes schema 15's ceiling, keeps every row, and is
/// idempotent — the restart path runs it on every open.
#[tokio::test]
async fn the_migration_removes_the_old_ceiling_and_keeps_every_row() {
    let harness = h::OrgHarness::new().await;
    let (org_id, chat_id, message_id, session_id) = seed(&harness).await;
    let db = harness.state().database.clone();

    restore_the_old_byte_ceiling(&db).await;
    // Rows written while the ceiling was in force, including references.
    let (on_chat, on_session) = attach(&db, org_id, &chat_id, &message_id, &session_id).await;
    let rows_before = count(&db, "neev_attachments").await;
    let refs_before = count(&db, "neev_attachment_refs").await;
    assert_eq!(rows_before, 2);
    assert_eq!(refs_before, 2);
    // The bug, reproduced: the real file cannot be stored.
    assert!(
        !try_insert_sized(&db, org_id, &chat_id, REAL_HTML_BYTES).await,
        "precondition: the old ceiling refuses the 44 MB file"
    );

    // Re-open, exactly as a restart does. This is what applies the migration.
    assert_eq!(initialize(&db).await, Ok(()));

    assert_eq!(
        attachment_byte_cap_migration::validate_attachment_byte_cap(&db).await,
        Ok(())
    );
    assert!(!attachments_ddl(&db)
        .await
        .contains("BETWEEN 1 AND 10485760"));
    // Nothing was dropped, and the schema marker did not move.
    assert_eq!(count(&db, "neev_attachments").await, rows_before);
    assert_eq!(count(&db, "neev_attachment_refs").await, refs_before);
    assert_eq!(
        schema_version(&db).await,
        CURRENT_SCHEMA_VERSION,
        "initialization advances to the current schema"
    );
    // The rows are still reachable by key, not merely counted.
    for key in [&on_chat, &on_session] {
        assert!(one(
            &db,
            "SELECT key FROM neev_attachments WHERE org_id=$1 AND key=$2",
            vec![org_id.into(), key.clone().into()],
        )
        .await
        .unwrap()
        .is_some());
    }
    // And the integrity validators still pass on those preserved rows.
    assert_eq!(attachment_migration::validate(&db).await, Ok(()));

    // The fix: the real file now stores.
    assert!(
        try_insert_sized(&db, org_id, &chat_id, REAL_HTML_BYTES).await,
        "a 44 MB attachment must be storable after the migration"
    );
    // Idempotent: a second open changes nothing and still validates.
    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(
        attachment_byte_cap_migration::validate_attachment_byte_cap(&db).await,
        Ok(())
    );
    assert_eq!(count(&db, "neev_attachments").await, rows_before + 1);
}

/// The validator is the canary's assertion, so it must actually fail when a
/// ceiling is present — otherwise a database that was never migrated reports
/// healthy.
#[tokio::test]
async fn the_validator_refuses_a_database_that_still_has_a_byte_ceiling() {
    let harness = h::OrgHarness::new().await;
    let db = harness.state().database.clone();
    restore_the_old_byte_ceiling(&db).await;
    assert_eq!(
        attachment_byte_cap_migration::validate_attachment_byte_cap(&db).await,
        Err(AuthError::Unavailable)
    );
}

#[tokio::test]
async fn schema_sixteen_upgrade_preserves_attachment_refs_and_archive_on_reopen() {
    let harness = h::OrgHarness::new().await;
    let (org_id, chat_id, message_id, session_id) = seed(&harness).await;
    let db = harness.state().database.clone();
    let (key, _) = attach(&db, org_id, &chat_id, &message_id, &session_id).await;
    execute(&db, "DROP INDEX neev_attachments_chat_archived", vec![]).await;
    execute(&db, "DROP INDEX neev_attachments_session_archived", vec![]).await;
    execute(
        &db,
        "ALTER TABLE neev_attachments DROP COLUMN archived",
        vec![],
    )
    .await;
    super::tests::drop_tables_after_seventeen(&db).await;
    execute(
        &db,
        "UPDATE neev_schema_version SET version=16 WHERE id=1",
        vec![],
    )
    .await;
    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(schema_version(&db).await, CURRENT_SCHEMA_VERSION);
    assert_eq!(count(&db, "neev_attachment_refs").await, 2);
    execute(
        &db,
        "UPDATE neev_attachments SET archived=TRUE WHERE key=$1",
        vec![key.clone().into()],
    )
    .await;
    assert_eq!(initialize(&db).await, Ok(()));
    let archived: bool = one(
        &db,
        "SELECT archived FROM neev_attachments WHERE key=$1",
        vec![key.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "archived")
    .unwrap();
    assert!(archived);
    assert_eq!(count(&db, "neev_attachment_refs").await, 2);
}

#[tokio::test]
#[ignore = "requires isolated NEEV_TEST_DATABASE_URL"]
async fn postgres_attachment_archive_migration_and_restore_preserve_refs() {
    let (db, admin, schema) = h::postgres_database().await;
    let harness = h::OrgHarness::new_with_gateway_events_on(db.clone()).await;
    let (org_id, chat_id, message_id, session_id) = seed(&harness).await;
    let (key, _) = attach(&db, org_id, &chat_id, &message_id, &session_id).await;
    execute(
        &db,
        "ALTER TABLE neev_attachments DROP COLUMN archived",
        vec![],
    )
    .await;
    execute(
        &db,
        "UPDATE neev_schema_version SET version=16 WHERE id=1",
        vec![],
    )
    .await;
    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(schema_version(&db).await, 17);
    assert_eq!(count(&db, "neev_attachment_refs").await, 2);
    let actor = harness.owner_context().clone();
    crate::neev::content::attachment::set_archived(harness.state(), &actor, &key, true)
        .await
        .unwrap();
    assert_eq!(initialize(&db).await, Ok(()));
    let page = crate::neev::content::attachment::list_archived_for_chat(
        harness.state(),
        &actor,
        &chat_id,
        None,
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 1);
    crate::neev::content::attachment::set_archived(harness.state(), &actor, &key, false)
        .await
        .unwrap();
    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(count(&db, "neev_attachment_refs").await, 2);
    drop(harness);
    h::cleanup_postgres(db, admin, schema).await;
}
