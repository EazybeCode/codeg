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
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,visibility,kind,bound_chat_id,bound_kind,archived,revision,created_at,updated_at) VALUES($1,$2,$3,'S','claude','default','private','chat',NULL,NULL,0,1,0,0)",
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
        attachment_migration::SCHEMA_VERSION
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
        attachment_migration::SCHEMA_VERSION
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
