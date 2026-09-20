//! Schema-11 migration tests: the additive upgrade preserves every schema-9 row,
//! the fresh install and the upgrade path converge on the same inventory, and
//! [`session_migration::validate`] rejects corruption the declared constraints
//! cannot catch.
//!
//! Corruption that the real (constrained) table would refuse is asserted against
//! an unconstrained replica — a hand-built table with the same column names and
//! no CHECKs — which is exactly what a restored dump or an edited SQLite file
//! can look like.

use super::*;
use crate::neev::test_support as h;
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait, Value};

const SESSION: &str = "11111111-1111-4111-8111-111111111111";
const TURN: &str = "22222222-2222-4222-8222-222222222222";

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

async fn table_exists(db: &DatabaseConnection, name: &str) -> bool {
    one(
        db,
        "SELECT name FROM pragma_table_list WHERE schema='main' AND name=$1",
        vec![name.into()],
    )
    .await
    .unwrap()
    .is_some()
}

/// Take a fresh (v11) database back to the exact schema-9 shape: drop the two
/// session tables and rewind the marker. The additive message columns stay —
/// which is precisely the case `upgrade`'s column guard has to survive.
async fn downgrade_to_nine(db: &DatabaseConnection) {
    for table in [
        "neev_agent_turns",
        "neev_agent_sessions",
        "neev_image_blobs",
    ] {
        execute(db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    execute(db, "UPDATE neev_schema_version SET version=9", vec![])
        .await
        .unwrap();
}

/// A member + org + project + chat, and one human message in that chat.
/// Returns `(org, user, chat_id)`.
async fn seed(db: &DatabaseConnection) -> (i32, i32, String) {
    let user = h::native_account(db, "session-owner", "owner@example.test").await;
    let (org, _) = h::organization(db, user, "admin").await;
    let node: i32 = one(
        db,
        "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,NULL,'project','P',1,$2) RETURNING id",
        vec![org.into(), user.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "id")
    .unwrap();
    let chat = "33333333-3333-4333-8333-333333333333";
    execute(
        db,
        "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,'chat',$3,$4,'Chat','private',NULL,NULL,1,1,false,1000,1000)",
        vec![org.into(), chat.into(), node.into(), user.into()],
    )
    .await
    .unwrap();
    execute(
        db,
        "INSERT INTO neev_messages(org_id,chat_id,resource_kind,id,sequence,author_id,body,byte_length,created_at) VALUES($1,$2,'chat',$3,1,$4,'hello',5,1000)",
        vec![
            org.into(),
            chat.into(),
            "44444444-4444-4444-8444-444444444444".into(),
            user.into(),
        ],
    )
    .await
    .unwrap();
    (org, user, chat.to_owned())
}

async fn insert_session(db: &DatabaseConnection, org: i32, user: i32) {
    execute(
        db,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,visibility,kind,bound_chat_id,bound_kind,archived,revision,created_at,updated_at) VALUES($1,$2,$3,'Plan the launch','claude','default','private','chat',NULL,NULL,false,1,1000,1000)",
        vec![org.into(), SESSION.into(), user.into()],
    )
    .await
    .unwrap();
}

async fn insert_turn(db: &DatabaseConnection, org: i32, status: &str) {
    let (error, completed): (Value, Value) = match status {
        "running" => (Value::String(None), Value::BigInt(None)),
        "completed" => (Value::String(None), 1100_i64.into()),
        _ => ("canceled".into(), 1100_i64.into()),
    };
    execute(
        db,
        "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,prompt,response,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,1,$3,'run-1','ask','partial',$4,$5,'claude','default',1000,1050,$6)",
        vec![
            org.into(),
            SESSION.into(),
            TURN.into(),
            status.into(),
            error,
            completed,
        ],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn fresh_install_creates_session_storage_at_version_eleven() {
    let db = h::database().await;
    assert_eq!(schema_version(&db).await, session_migration::SCHEMA_VERSION);
    assert!(table_exists(&db, "neev_agent_sessions").await);
    assert!(table_exists(&db, "neev_agent_turns").await);
    // The additive message columns exist and default existing rows to human.
    let (org, user, _) = seed(&db).await;
    let row = one(
        &db,
        "SELECT author_kind,agent_label FROM neev_messages WHERE org_id=$1 AND author_id=$2",
        vec![org.into(), user.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.try_get::<String>("", "author_kind").unwrap(), "human");
    assert_eq!(
        row.try_get::<Option<String>>("", "agent_label").unwrap(),
        None
    );
}

#[tokio::test]
async fn nine_to_eleven_upgrade_preserves_every_row() {
    let db = h::database().await;
    let (org, user, chat) = seed(&db).await;
    downgrade_to_nine(&db).await;
    assert_eq!(schema_version(&db).await, 9);

    assert_eq!(initialize(&db).await, Ok(()));

    assert_eq!(schema_version(&db).await, session_migration::SCHEMA_VERSION);
    assert!(table_exists(&db, "neev_agent_sessions").await);
    assert!(table_exists(&db, "neev_agent_turns").await);
    // Pre-existing content survived the additive upgrade untouched.
    assert_eq!(count(&db, "neev_messages").await, 1);
    assert_eq!(count(&db, "neev_resources").await, 1);
    let body: String = one(
        &db,
        "SELECT body FROM neev_messages WHERE org_id=$1 AND chat_id=$2",
        vec![org.into(), chat.clone().into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "body")
    .unwrap();
    assert_eq!(body, "hello");
    // And the new storage is usable immediately afterwards.
    insert_session(&db, org, user).await;
    insert_turn(&db, org, "completed").await;
    assert_eq!(count(&db, "neev_agent_sessions").await, 1);
}

#[tokio::test]
async fn ten_to_eleven_preserves_content_and_image_inventory() {
    let db = h::database().await;
    seed(&db).await;
    for table in ["neev_agent_turns", "neev_agent_sessions"] {
        execute(&db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    execute(&db, "UPDATE neev_schema_version SET version=10", vec![])
        .await
        .unwrap();
    assert_eq!(validate_content_snapshot(&db).await, Ok(10));
    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(validate_content_snapshot(&db).await, Ok(11));
    assert!(table_exists(&db, "neev_image_blobs").await);
    assert_eq!(count(&db, "neev_messages").await, 1);
    assert_eq!(initialize(&db).await, Ok(()));
}

#[tokio::test]
async fn upgrade_is_replayable_when_message_columns_already_exist() {
    // Dropping only the session tables leaves `neev_messages` already carrying
    // `author_kind`/`agent_label`; a bare ADD COLUMN would fail here.
    let db = h::database().await;
    seed(&db).await;
    downgrade_to_nine(&db).await;
    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(schema_version(&db).await, session_migration::SCHEMA_VERSION);
    // Idempotent: initializing an already-current database validates in place.
    assert_eq!(initialize(&db).await, Ok(()));
    assert_eq!(count(&db, "neev_messages").await, 1);
}

#[tokio::test]
async fn constrained_tables_refuse_out_of_domain_rows() {
    let db = h::database().await;
    let (org, user, chat) = seed(&db).await;
    insert_session(&db, org, user).await;
    // A non-private session cannot be stored at all (§5.3 is a schema fact).
    assert!(execute(
        &db,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,visibility,kind,bound_chat_id,bound_kind,archived,revision,created_at,updated_at) VALUES($1,'55555555-5555-4555-8555-555555555555',$2,'T','claude','default','organization','chat',NULL,NULL,false,1,1000,1000)",
        vec![org.into(), user.into()],
    )
    .await
    .is_err());
    // An owner with no membership in the org is refused by the composite FK.
    let outsider = h::native_account(&db, "outsider", "outsider@example.test").await;
    assert!(execute(
        &db,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,visibility,kind,bound_chat_id,bound_kind,archived,revision,created_at,updated_at) VALUES($1,'66666666-6666-4666-8666-666666666666',$2,'T','claude','default','private','chat',NULL,NULL,false,1,1000,1000)",
        vec![org.into(), outsider.into()],
    )
    .await
    .is_err());
    // A live turn may not carry a completion or an error.
    assert!(execute(
        &db,
        "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,prompt,response,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,9,'77777777-7777-4777-8777-777777777777','run-9','ask','','running','provider','claude','default',1000,1000,1100)",
        vec![org.into(), SESSION.into()],
    )
    .await
    .is_err());
    // A failed turn MUST carry both.
    assert!(execute(
        &db,
        "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,prompt,response,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,9,'77777777-7777-4777-8777-777777777777','run-9','ask','','failed',NULL,'claude','default',1000,1000,1100)",
        vec![org.into(), SESSION.into()],
    )
    .await
    .is_err());
    // A bound chat from another org cannot be referenced (composite FK carries
    // the tenant), so a session can never point at a foreign chat.
    let stranger = h::native_account(&db, "stranger", "stranger@example.test").await;
    let (other_org, _) = h::organization(&db, stranger, "admin").await;
    assert!(execute(
        &db,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,visibility,kind,bound_chat_id,bound_kind,archived,revision,created_at,updated_at) VALUES($1,'88888888-8888-4888-8888-888888888888',$2,'T','claude','default','private','chat',$3,'chat',false,1,1000,1000)",
        vec![other_org.into(), stranger.into(), chat.into()],
    )
    .await
    .is_err());
}

/// Build an unconstrained replica of a table (same columns, no CHECK/FK), swap
/// it in, and run the validator against the corrupted row.
async fn with_unconstrained<F>(
    table: &str,
    columns: &str,
    insert: &str,
    values: Vec<Value>,
    seed_first: F,
) where
    F: for<'a> FnOnce(
        &'a DatabaseConnection,
        i32,
        i32,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>>,
{
    let db = h::database().await;
    let (org, user, _) = seed(&db).await;
    seed_first(&db, org, user).await;
    execute(&db, "PRAGMA foreign_keys=OFF", vec![])
        .await
        .unwrap();
    let tx = db.begin().await.unwrap();
    execute(
        &tx,
        &format!("CREATE TEMP TABLE replica_{table} AS SELECT * FROM {table}"),
        vec![],
    )
    .await
    .unwrap();
    execute(&tx, &format!("DROP TABLE {table}"), vec![])
        .await
        .unwrap();
    execute(&tx, &format!("CREATE TABLE {table}({columns})"), vec![])
        .await
        .unwrap();
    execute(
        &tx,
        &format!("INSERT INTO {table} SELECT * FROM replica_{table}"),
        vec![],
    )
    .await
    .unwrap();
    execute(&tx, &format!("DROP TABLE replica_{table}"), vec![])
        .await
        .unwrap();
    // The corruption a constrained table would have refused.
    execute(&tx, insert, values).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        session_migration::validate(&db).await,
        Err(AuthError::Unavailable),
        "validator accepted corrupted {table}"
    );
}

const SESSION_COLS: &str = "org_id INTEGER NOT NULL,id TEXT NOT NULL,owner_id INTEGER NOT NULL,title TEXT NOT NULL,provider TEXT NOT NULL,model TEXT NOT NULL,visibility TEXT NOT NULL,kind TEXT NOT NULL,bound_chat_id TEXT,bound_kind TEXT,archived BOOLEAN NOT NULL,revision BIGINT NOT NULL,created_at BIGINT NOT NULL,updated_at BIGINT NOT NULL";
const TURN_COLS: &str = "org_id INTEGER NOT NULL,session_id TEXT NOT NULL,sequence BIGINT NOT NULL,id TEXT NOT NULL,run_id TEXT NOT NULL,request_id TEXT,prompt TEXT NOT NULL,response TEXT NOT NULL,status TEXT NOT NULL,error_code TEXT,provider TEXT NOT NULL,model TEXT NOT NULL,created_at BIGINT NOT NULL,updated_at BIGINT NOT NULL,completed_at BIGINT";

fn nothing<'a>(
    _db: &'a DatabaseConnection,
    _org: i32,
    _user: i32,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
    Box::pin(async {})
}

fn with_session<'a>(
    db: &'a DatabaseConnection,
    org: i32,
    user: i32,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
    Box::pin(async move { insert_session(db, org, user).await })
}

#[tokio::test]
async fn validator_rejects_a_session_that_escaped_the_private_default() {
    with_unconstrained(
        "neev_agent_sessions",
        SESSION_COLS,
        "INSERT INTO neev_agent_sessions VALUES(1,'99999999-9999-4999-8999-999999999999',1,'T','claude','default','organization','chat',NULL,NULL,0,1,1000,1000)",
        vec![],
        nothing,
    )
    .await;
}

#[tokio::test]
async fn validator_rejects_a_session_with_an_unknown_provider() {
    with_unconstrained(
        "neev_agent_sessions",
        SESSION_COLS,
        "INSERT INTO neev_agent_sessions VALUES(1,'99999999-9999-4999-8999-999999999999',1,'T','gpt','default','private','chat',NULL,NULL,0,1,1000,1000)",
        vec![],
        nothing,
    )
    .await;
}

#[tokio::test]
async fn validator_rejects_a_session_whose_owner_lost_its_membership() {
    // org 4242 has no memberships at all: the tenant boundary is re-proved by
    // join, not inherited from a (dropped) foreign key.
    with_unconstrained(
        "neev_agent_sessions",
        SESSION_COLS,
        "INSERT INTO neev_agent_sessions VALUES(4242,'99999999-9999-4999-8999-999999999999',1,'T','claude','default','private','chat',NULL,NULL,0,1,1000,1000)",
        vec![],
        nothing,
    )
    .await;
}

#[tokio::test]
async fn validator_rejects_a_session_id_of_the_wrong_storage_class() {
    with_unconstrained(
        "neev_agent_sessions",
        SESSION_COLS,
        "INSERT INTO neev_agent_sessions VALUES(1,777,1,'T','claude','default','private','chat',NULL,NULL,0,1,1000,1000)",
        vec![],
        nothing,
    )
    .await;
}

#[tokio::test]
async fn validator_rejects_a_half_bound_session() {
    with_unconstrained(
        "neev_agent_sessions",
        SESSION_COLS,
        "INSERT INTO neev_agent_sessions VALUES(1,'99999999-9999-4999-8999-999999999999',1,'T','claude','default','private','chat','33333333-3333-4333-8333-333333333333',NULL,0,1,1000,1000)",
        vec![],
        nothing,
    )
    .await;
}

#[tokio::test]
async fn validator_rejects_a_turn_stuck_running_with_a_completion() {
    with_unconstrained(
        "neev_agent_turns",
        TURN_COLS,
        "INSERT INTO neev_agent_turns VALUES(1,'11111111-1111-4111-8111-111111111111',5,'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','run-5',NULL,'ask','','running',NULL,'claude','default',1000,1000,1100)",
        vec![],
        with_session,
    )
    .await;
}

#[tokio::test]
async fn validator_rejects_a_turn_with_an_unknown_error_code() {
    with_unconstrained(
        "neev_agent_turns",
        TURN_COLS,
        "INSERT INTO neev_agent_turns VALUES(1,'11111111-1111-4111-8111-111111111111',5,'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','run-5',NULL,'ask','','failed','meltdown','claude','default',1000,1000,1100)",
        vec![],
        with_session,
    )
    .await;
}

#[tokio::test]
async fn validator_rejects_a_turn_whose_session_is_gone() {
    with_unconstrained(
        "neev_agent_turns",
        TURN_COLS,
        "INSERT INTO neev_agent_turns VALUES(1,'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',5,'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','run-5',NULL,'ask','','completed',NULL,'claude','default',1000,1000,1100)",
        vec![],
        with_session,
    )
    .await;
}

#[tokio::test]
async fn validator_rejects_a_turn_contradicting_its_session_provider() {
    with_unconstrained(
        "neev_agent_turns",
        TURN_COLS,
        "INSERT INTO neev_agent_turns VALUES(1,'11111111-1111-4111-8111-111111111111',5,'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','run-5',NULL,'ask','','completed',NULL,'codex','default',1000,1000,1100)",
        vec![],
        with_session,
    )
    .await;
}

#[tokio::test]
async fn validator_rejects_a_duplicate_run_id() {
    with_unconstrained(
        "neev_agent_turns",
        TURN_COLS,
        "INSERT INTO neev_agent_turns VALUES(1,'11111111-1111-4111-8111-111111111111',5,'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','run-1',NULL,'ask','','completed',NULL,'claude','default',1000,1000,1100),(1,'11111111-1111-4111-8111-111111111111',6,'cccccccc-cccc-4ccc-8ccc-cccccccccccc','run-1',NULL,'ask','','completed',NULL,'claude','default',1000,1000,1100)",
        vec![],
        with_session,
    )
    .await;
}

#[tokio::test]
async fn validator_rejects_an_agent_message_without_a_provider_label() {
    // The pairing SQLite cannot express as a table CHECK is still proved.
    let db = h::database().await;
    let (org, _, chat) = seed(&db).await;
    execute(
        &db,
        "UPDATE neev_messages SET author_kind='agent' WHERE org_id=$1 AND chat_id=$2",
        vec![org.into(), chat.into()],
    )
    .await
    .unwrap();
    assert_eq!(
        session_migration::validate(&db).await,
        Err(AuthError::Unavailable)
    );
}

#[tokio::test]
async fn validator_rejects_a_human_message_carrying_a_provider_label() {
    let db = h::database().await;
    let (org, _, chat) = seed(&db).await;
    execute(
        &db,
        "UPDATE neev_messages SET agent_label='claude' WHERE org_id=$1 AND chat_id=$2",
        vec![org.into(), chat.into()],
    )
    .await
    .unwrap();
    assert_eq!(
        session_migration::validate(&db).await,
        Err(AuthError::Unavailable)
    );
}

#[tokio::test]
async fn validator_accepts_a_labelled_agent_message_and_real_rows() {
    let db = h::database().await;
    let (org, user, chat) = seed(&db).await;
    insert_session(&db, org, user).await;
    insert_turn(&db, org, "completed").await;
    execute(
        &db,
        "INSERT INTO neev_messages(org_id,chat_id,resource_kind,id,sequence,author_id,body,byte_length,created_at,author_kind,agent_label) VALUES($1,$2,'chat',$3,2,$4,'agent reply',11,1100,'agent','claude')",
        vec![
            org.into(),
            chat.into(),
            "55555555-5555-4555-8555-555555555555".into(),
            user.into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(session_migration::validate(&db).await, Ok(()));
}

#[tokio::test]
async fn validator_walks_past_the_keyset_page_boundary() {
    // More rows than one page, with the corruption on the LAST page only.
    let db = h::database().await;
    let (org, user, _) = seed(&db).await;
    insert_session(&db, org, user).await;
    for index in 1..=300_i64 {
        let id = format!("{index:08}-0000-4000-8000-000000000000");
        execute(
            &db,
            "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,prompt,response,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,$3,$4,$5,'ask','','completed',NULL,'claude','default',1000,1000,1100)",
            vec![
                org.into(),
                SESSION.into(),
                index.into(),
                id.into(),
                format!("run-{index}").into(),
            ],
        )
        .await
        .unwrap();
    }
    assert_eq!(session_migration::validate(&db).await, Ok(()));
    // Corrupt a row that only the last page reaches, with corruption the REAL
    // constrained table accepts: `provider IN (...)` is a per-row CHECK, so a
    // turn contradicting its session's provider is storable and only the
    // validator's join catches it.
    execute(
        &db,
        "UPDATE neev_agent_turns SET provider='codex' WHERE id='00000299-0000-4000-8000-000000000000'",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(
        session_migration::validate(&db).await,
        Err(AuthError::Unavailable)
    );
}
