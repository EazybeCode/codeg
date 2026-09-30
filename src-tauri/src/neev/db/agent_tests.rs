use super::*;
use crate::neev::test_support as h;
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait, Value};

const SOURCE_ID: &str = "10000000-0000-4000-8000-000000000001";
const DOCUMENT_ID: &str = "10000000-0000-4000-8000-000000000002";

fn opaque(byte: u8) -> String {
    std::iter::repeat_n(char::from(byte), 32).collect()
}

async fn write(db: &DatabaseConnection, sql: &str, values: Vec<Value>) -> Result<(), AuthError> {
    execute(db, sql, values).await.map(|_| ())
}

#[allow(clippy::too_many_arguments)]
async fn insert_connection(
    db: &DatabaseConnection,
    id: &str,
    org_id: i32,
    user_id: i32,
    provider: &str,
    generation: i64,
    state: &str,
    account_label: Option<&str>,
    checked_at: Option<i64>,
    error_code: Option<&str>,
    storage_handle: Option<&str>,
    created_at: i64,
    updated_at: i64,
) -> Result<(), AuthError> {
    write(
        db,
        "INSERT INTO neev_agent_connections(id,org_id,user_id,provider,generation,state,account_label,checked_at,error_code,storage_handle,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
        vec![
            id.into(),
            org_id.into(),
            user_id.into(),
            provider.into(),
            generation.into(),
            state.into(),
            account_label.map(str::to_owned).into(),
            checked_at.into(),
            error_code.map(str::to_owned).into(),
            storage_handle.map(str::to_owned).into(),
            created_at.into(),
            updated_at.into(),
        ],
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn insert_attempt(
    db: &DatabaseConnection,
    id: &str,
    connection_id: &str,
    generation: i64,
    session_id: &str,
    state: &str,
    created_at: i64,
    expires_at: i64,
    updated_at: i64,
) -> Result<(), AuthError> {
    write(
        db,
        "INSERT INTO neev_agent_attempts(id,connection_id,generation,session_id,state,created_at,expires_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
        vec![
            id.into(),
            connection_id.into(),
            generation.into(),
            session_id.into(),
            state.into(),
            created_at.into(),
            expires_at.into(),
            updated_at.into(),
        ],
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn insert_operation(
    db: &DatabaseConnection,
    id: &str,
    connection_id: &str,
    kind: &str,
    request_key: &str,
    target_attempt_id: Option<&str>,
    target_generation: i64,
    result_generation: i64,
    state: &str,
    lease_owner: Option<&str>,
    lease_expires_at: Option<i64>,
    error_code: Option<&str>,
    created_at: i64,
    expires_at: i64,
    updated_at: i64,
) -> Result<(), AuthError> {
    write(
        db,
        "INSERT INTO neev_agent_operations(id,connection_id,kind,request_key,target_attempt_id,target_generation,result_generation,state,lease_owner,lease_expires_at,error_code,created_at,expires_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
        vec![
            id.into(),
            connection_id.into(),
            kind.into(),
            request_key.into(),
            target_attempt_id.map(str::to_owned).into(),
            target_generation.into(),
            result_generation.into(),
            state.into(),
            lease_owner.map(str::to_owned).into(),
            lease_expires_at.into(),
            error_code.map(str::to_owned).into(),
            created_at.into(),
            expires_at.into(),
            updated_at.into(),
        ],
    )
    .await
}

async fn relation_exists(db: &DatabaseConnection, name: &str, kind: &str) -> bool {
    let (sql, values): (&str, Vec<Value>) = if db.get_database_backend() == DbBackend::Postgres {
        (
            "SELECT 1 AS present FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname=$1 AND c.relkind=$2",
            vec![name.into(), kind.into()],
        )
    } else {
        (
            "SELECT 1 AS present FROM sqlite_master WHERE name=$1 AND type=$2",
            vec![name.into(), kind.into()],
        )
    };
    one(db, sql, values).await.unwrap().is_some()
}

async fn schema_version(db: &DatabaseConnection) -> i64 {
    one(
        db,
        "SELECT version FROM neev_schema_version WHERE id=1",
        vec![],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "version")
    .unwrap()
}

async fn initialize_v2(db: &DatabaseConnection) {
    let backend = db.get_database_backend();
    if backend == DbBackend::Sqlite {
        execute(db, "PRAGMA foreign_keys=ON", vec![]).await.unwrap();
    }
    let tx = db.begin().await.unwrap();
    for table in migration::tables() {
        tx.execute(backend.build(&table)).await.unwrap();
    }
    for index in migration::indexes() {
        tx.execute(backend.build(&index)).await.unwrap();
    }
    execute(
        &tx,
        "INSERT INTO neev_schema_version(id,version) VALUES(1,2)",
        vec![],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn initialize_historical(db: &DatabaseConnection, version: i64) {
    initialize_v2(db).await;
    if version >= 3 {
        let tx = db.begin().await.unwrap();
        let backend = db.get_database_backend();
        for table in content_migration::tables() {
            let mut sql = backend.build(&table).to_string();
            if version == 3 {
                sql = sql.replace(
                    "origin_chat_id IS NOT NULL AND origin_kind IS NOT NULL AND",
                    "origin_chat_id IS NOT NULL AND",
                );
            }
            execute(&tx, &sql, vec![]).await.unwrap();
        }
        for index in content_migration::indexes() {
            tx.execute(backend.build(&index)).await.unwrap();
        }
        if version == 4 {
            content_migration::harden_origins(&tx).await.unwrap();
        }
        execute(
            &tx,
            "UPDATE neev_schema_version SET version=$1 WHERE id=1",
            vec![version.into()],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
}

async fn downgrade_five(db: &DatabaseConnection, version: i64) {
    // attachments15 first: its references point at turns and messages, so it
    // cannot outlive the tables below.
    super::tests::drop_mention_tables(db).await;
    super::tests::drop_attachment_tables(db).await;
    for table in [
        // session11 (turns reference sessions), then org9, then gateway8.
        "neev_agent_turns",
        "neev_agent_sessions",
        "neev_image_blobs",
        "neev_org_join_codes",
        "neev_gateway_revoke_intents",
        "neev_gateway_redemptions",
        "neev_gateway_relying_sessions",
        "neev_gateway_exchanges",
        "neev_gateway_relying_bindings",
        "neev_gateway_revocations",
        "neev_gateway_audience",
    ] {
        execute(db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    for table in agent_status_migration::TABLE_NAMES.iter().rev() {
        execute(db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    execute(db, "DROP TABLE neev_document_formats", vec![])
        .await
        .unwrap();
    for table in agent_migration::TABLE_NAMES.iter().rev() {
        execute(db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    if version <= 3 {
        for table in content_migration::TABLE_NAMES.iter().rev() {
            execute(db, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
    }
    if version == 3 {
        let backend = db.get_database_backend();
        let tx = db.begin().await.unwrap();
        for table in content_migration::tables() {
            let sql = backend.build(&table).to_string().replace(
                "origin_chat_id IS NOT NULL AND origin_kind IS NOT NULL AND",
                "origin_chat_id IS NOT NULL AND",
            );
            execute(&tx, &sql, vec![]).await.unwrap();
        }
        for index in content_migration::indexes() {
            tx.execute(backend.build(&index)).await.unwrap();
        }
        tx.commit().await.unwrap();
    }
    execute(
        db,
        "UPDATE neev_schema_version SET version=$1 WHERE id=1",
        vec![version.into()],
    )
    .await
    .unwrap();
}

async fn seed_preserved_data(db: &DatabaseConnection, with_content: bool) {
    write(db, "INSERT INTO neev_accounts(id,display_name,auth_revision,disabled,created_at) VALUES(101,'Preserved owner',1,false,1000)", vec![]).await.unwrap();
    write(db, "INSERT INTO neev_organizations(id,owner_user_id,name,revision,disabled,created_at) VALUES(201,101,'Preserved org',1,false,1000)", vec![]).await.unwrap();
    write(db, "INSERT INTO neev_memberships(id,org_id,user_id,role,revision,active) VALUES(301,201,101,'admin',1,true)", vec![]).await.unwrap();
    write(db, "INSERT INTO neev_nodes(id,org_id,parent_id,kind,name,revision,created_by) VALUES(401,201,NULL,'project','Preserved project',1,101)", vec![]).await.unwrap();
    if with_content {
        write(db, "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES(201,$1,'chat',401,101,'Private source','private',NULL,NULL,1,0,false,1000,1000)", vec![SOURCE_ID.into()]).await.unwrap();
        write(db, "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES(201,$1,'document',401,101,'Source-bound document','private',$2,'chat',1,1,false,1000,1001)", vec![DOCUMENT_ID.into(), SOURCE_ID.into()]).await.unwrap();
        write(db, "INSERT INTO neev_document_revisions(org_id,document_id,resource_kind,revision,blob_key,sha256,byte_length,author_id,created_at,parent_revision,restored_from) VALUES(201,$1,'document',1,'preserved-blob',$2,17,101,1001,NULL,NULL)", vec![DOCUMENT_ID.into(), "a".repeat(64).into()]).await.unwrap();
    }
}

async fn assert_preserved_data(db: &DatabaseConnection, with_content: bool) {
    assert_eq!(
        schema_version(db).await,
        CURRENT_SCHEMA_VERSION
    );
    assert_eq!(h::count(db, "neev_accounts").await, 1);
    assert_eq!(h::count(db, "neev_organizations").await, 1);
    assert_eq!(h::count(db, "neev_memberships").await, 1);
    assert_eq!(h::count(db, "neev_nodes").await, 1);
    let row = one(db, "SELECT a.display_name,o.name AS org_name,m.role,n.name AS node_name FROM neev_accounts a JOIN neev_organizations o ON o.owner_user_id=a.id JOIN neev_memberships m ON m.org_id=o.id AND m.user_id=a.id JOIN neev_nodes n ON n.org_id=o.id WHERE a.id=101 AND o.id=201 AND m.id=301 AND n.id=401", vec![]).await.unwrap().unwrap();
    assert_eq!(
        row.try_get::<String>("", "display_name").unwrap(),
        "Preserved owner"
    );
    assert_eq!(
        row.try_get::<String>("", "org_name").unwrap(),
        "Preserved org"
    );
    assert_eq!(row.try_get::<String>("", "role").unwrap(), "admin");
    assert_eq!(
        row.try_get::<String>("", "node_name").unwrap(),
        "Preserved project"
    );
    if with_content {
        assert_eq!(h::count(db, "neev_resources").await, 2);
        assert_eq!(h::count(db, "neev_document_revisions").await, 1);
        let row = one(db, "SELECT r.title,r.visibility,r.origin_chat_id,d.blob_key,d.byte_length FROM neev_resources r JOIN neev_document_revisions d ON d.org_id=r.org_id AND d.document_id=r.id WHERE r.org_id=201 AND r.id=$1 AND d.revision=1", vec![DOCUMENT_ID.into()]).await.unwrap().unwrap();
        assert_eq!(
            row.try_get::<String>("", "title").unwrap(),
            "Source-bound document"
        );
        assert_eq!(row.try_get::<String>("", "visibility").unwrap(), "private");
        assert_eq!(
            row.try_get::<String>("", "origin_chat_id").unwrap(),
            SOURCE_ID
        );
        assert_eq!(
            row.try_get::<String>("", "blob_key").unwrap(),
            "preserved-blob"
        );
        assert_eq!(row.try_get::<i64>("", "byte_length").unwrap(), 17);
        assert!(write(
            db,
            "UPDATE neev_resources SET origin_kind=NULL WHERE org_id=201 AND id=$1",
            vec![DOCUMENT_ID.into()]
        )
        .await
        .is_err());
    }
    for table in agent_migration::TABLE_NAMES {
        assert!(
            relation_exists(
                db,
                table,
                if db.get_database_backend() == DbBackend::Postgres {
                    "r"
                } else {
                    "table"
                }
            )
            .await
        );
        assert_eq!(h::count(db, table).await, 0);
    }
    assert_eq!(initialize(db).await, Ok(()));
}

#[tokio::test]
async fn native_initialization_creates_empty_personal_agent_metadata() {
    let database = h::database().await;
    let rows = database
        .query_all(statement(
            &database,
            "SELECT id FROM neev_agent_connections",
            vec![],
        ))
        .await
        .expect("native personal metadata table exists");
    assert!(rows.is_empty());
    assert_eq!(
        schema_version(&database).await,
        CURRENT_SCHEMA_VERSION
    );
}

async fn connection_contract(db: &DatabaseConnection) {
    let owner = h::native_account(db, "connection-owner", "connection-owner@example.test").await;
    let member = h::native_account(db, "connection-member", "connection-member@example.test").await;
    let outsider = h::native_account(
        db,
        "connection-outsider",
        "connection-outsider@example.test",
    )
    .await;
    let (org, _) = h::organization(db, owner, "admin").await;
    h::add_membership(db, org, member, "member").await;
    let (other_org, _) = h::organization(db, owner, "admin").await;
    let handle = opaque(b'h');
    insert_connection(
        db,
        &opaque(b'1'),
        org,
        owner,
        "claude",
        1,
        "disconnected",
        None,
        None,
        None,
        None,
        0,
        0,
    )
    .await
    .unwrap();
    insert_connection(
        db,
        &opaque(b'2'),
        org,
        owner,
        "codex",
        1,
        "connected",
        Some("Personal 🚀"),
        Some(10),
        None,
        Some(&handle),
        0,
        10,
    )
    .await
    .unwrap();
    insert_connection(
        db,
        &opaque(b'3'),
        other_org,
        owner,
        "claude",
        1,
        "disconnected",
        None,
        None,
        None,
        None,
        0,
        0,
    )
    .await
    .unwrap();
    insert_connection(
        db,
        &opaque(b'4'),
        org,
        member,
        "claude",
        1,
        "connecting",
        None,
        None,
        Some("interrupted"),
        Some(&handle),
        0,
        0,
    )
    .await
    .unwrap();
    assert!(insert_connection(
        db,
        &opaque(b'5'),
        org,
        owner,
        "claude",
        2,
        "disconnected",
        None,
        None,
        None,
        None,
        0,
        0
    )
    .await
    .is_err());
    assert!(insert_connection(
        db,
        &opaque(b'6'),
        org,
        outsider,
        "claude",
        1,
        "disconnected",
        None,
        None,
        None,
        None,
        0,
        0
    )
    .await
    .is_err());
    let before = h::count(db, "neev_agent_connections").await;
    let invalid = [
        insert_connection(
            db,
            "short",
            org,
            member,
            "codex",
            1,
            "disconnected",
            None,
            None,
            None,
            None,
            0,
            0,
        )
        .await,
        insert_connection(
            db,
            &opaque(b'7'),
            0,
            member,
            "codex",
            1,
            "disconnected",
            None,
            None,
            None,
            None,
            0,
            0,
        )
        .await,
        insert_connection(
            db,
            &opaque(b'7'),
            org,
            0,
            "codex",
            1,
            "disconnected",
            None,
            None,
            None,
            None,
            0,
            0,
        )
        .await,
        insert_connection(
            db,
            &opaque(b'7'),
            org,
            member,
            "other",
            1,
            "disconnected",
            None,
            None,
            None,
            None,
            0,
            0,
        )
        .await,
        insert_connection(
            db,
            &opaque(b'7'),
            org,
            member,
            "codex",
            0,
            "disconnected",
            None,
            None,
            None,
            None,
            0,
            0,
        )
        .await,
        insert_connection(
            db,
            &opaque(b'7'),
            org,
            member,
            "codex",
            1,
            "wrong",
            None,
            None,
            None,
            None,
            0,
            0,
        )
        .await,
        insert_connection(
            db,
            &opaque(b'7'),
            org,
            member,
            "codex",
            1,
            "disconnected",
            None,
            None,
            None,
            None,
            -1,
            0,
        )
        .await,
        insert_connection(
            db,
            &opaque(b'7'),
            org,
            member,
            "codex",
            1,
            "disconnected",
            None,
            None,
            None,
            None,
            1,
            0,
        )
        .await,
    ];
    assert!(invalid.iter().all(Result::is_err));
    assert_eq!(h::count(db, "neev_agent_connections").await, before);
    assert!(insert_connection(
        db,
        &opaque(b'8'),
        org,
        member,
        "codex",
        2,
        "connected",
        Some(&"🚀".repeat(161)),
        Some(0),
        None,
        Some(&handle),
        0,
        0
    )
    .await
    .is_err());
    let label160 = "🚀".repeat(160);
    insert_connection(
        db,
        &opaque(b'7'),
        org,
        member,
        "codex",
        1,
        "connected",
        Some(&label160),
        Some(0),
        None,
        Some(&opaque(b'z')),
        0,
        0,
    )
    .await
    .unwrap();
    write(
        db,
        "DELETE FROM neev_agent_connections WHERE id=$1",
        vec![opaque(b'7').into()],
    )
    .await
    .unwrap();
    for (label, checked, error, storage) in [
        (None, Some(0), None, Some(handle.as_str())),
        (Some("label"), None, None, Some(handle.as_str())),
        (Some("label"), Some(0), None, None),
        (
            Some("label"),
            Some(0),
            Some("expired"),
            Some(handle.as_str()),
        ),
    ] {
        assert!(insert_connection(
            db,
            &opaque(b'8'),
            org,
            member,
            "codex",
            2,
            "connected",
            label,
            checked,
            error,
            storage,
            0,
            0
        )
        .await
        .is_err());
    }
    for column in ["account_label", "checked_at", "storage_handle"] {
        assert!(
            write(
                db,
                &format!("UPDATE neev_agent_connections SET {column}=NULL WHERE id=$1"),
                vec![opaque(b'2').into()]
            )
            .await
            .is_err(),
            "connected update cleared {column}"
        );
    }
    assert!(write(
        db,
        "UPDATE neev_agent_connections SET error_code='expired' WHERE id=$1",
        vec![opaque(b'2').into()]
    )
    .await
    .is_err());
    assert!(write(
        db,
        "UPDATE neev_agent_connections SET account_label='label' WHERE id=$1",
        vec![opaque(b'4').into()]
    )
    .await
    .is_err());
    assert!(write(
        db,
        "UPDATE neev_agent_connections SET checked_at=1 WHERE id=$1",
        vec![opaque(b'4').into()]
    )
    .await
    .is_err());
    for state in [
        "connecting",
        "reconnect_required",
        "disconnecting",
        "unavailable",
    ] {
        write(
            db,
            "UPDATE neev_agent_connections SET state=$1 WHERE id=$2",
            vec![state.into(), opaque(b'4').into()],
        )
        .await
        .unwrap();
    }
    for error in [
        "provider_unavailable",
        "reauth_required",
        "unsupported_account",
        "expired",
        "canceled",
        "cleanup_failed",
        "interrupted",
    ] {
        write(
            db,
            "UPDATE neev_agent_connections SET error_code=$1 WHERE id=$2",
            vec![error.into(), opaque(b'4').into()],
        )
        .await
        .unwrap();
    }
    for (label, checked, error, storage) in [
        (Some("label"), None, None, None),
        (None, Some(0), None, None),
        (None, None, Some("expired"), None),
        (None, None, None, Some(handle.as_str())),
    ] {
        assert!(insert_connection(
            db,
            &opaque(b'8'),
            org,
            member,
            "codex",
            2,
            "disconnected",
            label,
            checked,
            error,
            storage,
            0,
            0
        )
        .await
        .is_err());
    }
    assert!(insert_connection(
        db,
        &opaque(b'8'),
        org,
        member,
        "codex",
        2,
        "unavailable",
        Some("label"),
        None,
        None,
        Some(&handle),
        0,
        0
    )
    .await
    .is_err());
    assert!(insert_connection(
        db,
        &opaque(b'8'),
        org,
        member,
        "codex",
        2,
        "unavailable",
        None,
        Some(0),
        None,
        Some(&handle),
        0,
        0
    )
    .await
    .is_err());
    assert!(insert_connection(
        db,
        &opaque(b'8'),
        org,
        member,
        "codex",
        2,
        "disconnected",
        None,
        None,
        Some("expired"),
        None,
        0,
        0
    )
    .await
    .is_err());
    assert!(insert_connection(
        db,
        &opaque(b'8'),
        org,
        member,
        "codex",
        2,
        "connecting",
        None,
        None,
        Some("raw-provider-output"),
        Some(&handle),
        0,
        0
    )
    .await
    .is_err());
    assert!(insert_connection(
        db,
        &opaque(b'8'),
        org,
        member,
        "codex",
        2,
        "connecting",
        None,
        None,
        None,
        Some("short"),
        0,
        0
    )
    .await
    .is_err());
    assert!(insert_connection(
        db,
        &opaque(b'8'),
        org,
        member,
        "codex",
        2,
        "connected",
        Some(""),
        Some(0),
        None,
        Some(&handle),
        0,
        0
    )
    .await
    .is_err());
    assert!(insert_connection(
        db,
        &opaque(b'8'),
        org,
        member,
        "codex",
        2,
        "connected",
        Some("label"),
        Some(-1),
        None,
        Some(&handle),
        0,
        0
    )
    .await
    .is_err());
    insert_connection(
        db,
        &opaque(b'8'),
        org,
        member,
        "codex",
        i64::MAX,
        "disconnected",
        None,
        None,
        None,
        None,
        i64::MAX - 1,
        i64::MAX,
    )
    .await
    .unwrap();
}

async fn attempt_operation_contract(db: &DatabaseConnection) {
    let owner = h::native_account(db, "operation-owner", "operation-owner@example.test").await;
    let (org, _) = h::organization(db, owner, "admin").await;
    let connection1 = opaque(b'a');
    let connection2 = opaque(b'b');
    insert_connection(
        db,
        &connection1,
        org,
        owner,
        "claude",
        1,
        "disconnected",
        None,
        None,
        None,
        None,
        0,
        0,
    )
    .await
    .unwrap();
    insert_connection(
        db,
        &connection2,
        org,
        owner,
        "codex",
        1,
        "disconnected",
        None,
        None,
        None,
        None,
        0,
        0,
    )
    .await
    .unwrap();
    let attempt1 = opaque(b'c');
    insert_attempt(
        db,
        &attempt1,
        &connection1,
        1,
        "exact-session",
        "pending",
        0,
        10,
        0,
    )
    .await
    .unwrap();
    let before = h::count(db, "neev_agent_attempts").await;
    for result in [
        insert_attempt(db, "short", &connection1, 2, "s", "pending", 0, 10, 0).await,
        insert_attempt(db, &opaque(b'd'), "short", 2, "s", "pending", 0, 10, 0).await,
        insert_attempt(db, &opaque(b'd'), &connection1, 0, "s", "pending", 0, 10, 0).await,
        insert_attempt(db, &opaque(b'd'), &connection1, 2, "", "pending", 0, 10, 0).await,
        insert_attempt(
            db,
            &opaque(b'd'),
            &connection1,
            2,
            &"s".repeat(129),
            "pending",
            0,
            10,
            0,
        )
        .await,
        insert_attempt(db, &opaque(b'd'), &connection1, 2, "s", "wrong", 0, 10, 0).await,
        insert_attempt(
            db,
            &opaque(b'd'),
            &connection1,
            2,
            "s",
            "pending",
            -1,
            10,
            0,
        )
        .await,
        insert_attempt(db, &opaque(b'd'), &connection1, 2, "s", "pending", 1, 1, 1).await,
        insert_attempt(db, &opaque(b'd'), &connection1, 2, "s", "pending", 1, 2, 0).await,
        insert_attempt(
            db,
            &opaque(b'd'),
            &opaque(b'z'),
            2,
            "s",
            "pending",
            0,
            10,
            0,
        )
        .await,
        insert_attempt(db, &opaque(b'd'), &connection1, 1, "s", "pending", 0, 10, 0).await,
    ] {
        assert!(result.is_err());
    }
    assert_eq!(h::count(db, "neev_agent_attempts").await, before);
    for (offset, state) in ["completed", "canceled", "expired", "failed"]
        .into_iter()
        .enumerate()
    {
        insert_attempt(
            db,
            &opaque(b'e' + offset as u8),
            &connection1,
            offset as i64 + 2,
            "s",
            state,
            0,
            10,
            0,
        )
        .await
        .unwrap();
    }
    insert_attempt(
        db,
        &opaque(b'i'),
        &connection1,
        i64::MAX,
        &"s".repeat(128),
        "pending",
        i64::MAX - 1,
        i64::MAX,
        i64::MAX,
    )
    .await
    .unwrap();
    let operation1 = opaque(b'j');
    insert_operation(
        db,
        &operation1,
        &connection1,
        "connect",
        "request",
        Some(&attempt1),
        1,
        1,
        "queued",
        None,
        None,
        None,
        0,
        10,
        0,
    )
    .await
    .unwrap();
    insert_operation(
        db,
        &opaque(b'k'),
        &connection1,
        "cancel",
        "request",
        Some(&attempt1),
        1,
        2,
        "leased",
        Some(&opaque(b'l')),
        Some(5),
        Some("canceled"),
        0,
        10,
        0,
    )
    .await
    .unwrap();
    insert_operation(
        db,
        &opaque(b'm'),
        &connection1,
        "disconnect",
        "request",
        None,
        2,
        3,
        "completed",
        None,
        None,
        None,
        0,
        10,
        0,
    )
    .await
    .unwrap();
    let before = h::count(db, "neev_agent_operations").await;
    for result in [
        insert_operation(
            db,
            "short",
            &connection1,
            "disconnect",
            "x",
            None,
            1,
            1,
            "queued",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            "short",
            "disconnect",
            "x",
            None,
            1,
            1,
            "queued",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            None,
            1,
            1,
            "leased",
            Some("short"),
            Some(1),
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            None,
            1,
            1,
            "leased",
            Some(&opaque(b'o')),
            Some(-1),
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "wrong",
            "x",
            None,
            1,
            1,
            "queued",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "",
            None,
            1,
            1,
            "queued",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            &"r".repeat(65),
            None,
            1,
            1,
            "queued",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "connect",
            "x",
            Some("short"),
            1,
            1,
            "queued",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            None,
            0,
            1,
            "queued",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            None,
            2,
            1,
            "queued",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            None,
            1,
            1,
            "wrong",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            None,
            1,
            1,
            "queued",
            Some(&opaque(b'o')),
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            None,
            1,
            1,
            "queued",
            None,
            Some(1),
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            None,
            1,
            1,
            "leased",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            None,
            1,
            1,
            "leased",
            Some(&opaque(b'o')),
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            None,
            1,
            1,
            "leased",
            None,
            Some(1),
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "connect",
            "x",
            None,
            1,
            1,
            "queued",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "cancel",
            "x",
            None,
            1,
            1,
            "queued",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            Some(&attempt1),
            1,
            1,
            "queued",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "connect",
            "x",
            Some(&attempt1),
            1,
            1,
            "queued",
            None,
            None,
            Some("raw"),
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            None,
            1,
            1,
            "queued",
            None,
            None,
            None,
            -1,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            None,
            1,
            1,
            "queued",
            None,
            None,
            None,
            1,
            1,
            1,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "disconnect",
            "x",
            None,
            1,
            1,
            "queued",
            None,
            None,
            None,
            1,
            2,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection2,
            "connect",
            "x",
            Some(&attempt1),
            1,
            1,
            "queued",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
        insert_operation(
            db,
            &opaque(b'n'),
            &connection1,
            "connect",
            "request",
            Some(&attempt1),
            1,
            1,
            "queued",
            None,
            None,
            None,
            0,
            10,
            0,
        )
        .await,
    ] {
        assert!(result.is_err());
    }
    assert_eq!(h::count(db, "neev_agent_operations").await, before);
    for column in ["lease_owner", "lease_expires_at"] {
        assert!(
            write(
                db,
                &format!("UPDATE neev_agent_operations SET {column}=NULL WHERE id=$1"),
                vec![opaque(b'k').into()]
            )
            .await
            .is_err(),
            "leased update cleared {column}"
        );
    }
    assert!(write(
        db,
        "UPDATE neev_agent_operations SET kind='connect' WHERE id=$1",
        vec![opaque(b'm').into()]
    )
    .await
    .is_err());
    assert!(write(
        db,
        "UPDATE neev_agent_operations SET kind='cancel' WHERE id=$1",
        vec![opaque(b'm').into()]
    )
    .await
    .is_err());
    assert!(write(
        db,
        "UPDATE neev_agent_operations SET lease_owner=$1 WHERE id=$2",
        vec![opaque(b'o').into(), operation1.clone().into()]
    )
    .await
    .is_err());
    for error in [
        "provider_unavailable",
        "reauth_required",
        "unsupported_account",
        "expired",
        "canceled",
        "cleanup_failed",
        "interrupted",
    ] {
        write(
            db,
            "UPDATE neev_agent_operations SET error_code=$1 WHERE id=$2",
            vec![error.into(), operation1.clone().into()],
        )
        .await
        .unwrap();
    }
    for state in ["abandoned", "queued", "completed"] {
        write(
            db,
            "UPDATE neev_agent_operations SET state=$1 WHERE id=$2",
            vec![state.into(), opaque(b'm').into()],
        )
        .await
        .unwrap();
    }
    insert_operation(
        db,
        &opaque(b'p'),
        &connection1,
        "disconnect",
        &"r".repeat(64),
        None,
        i64::MAX,
        i64::MAX,
        "abandoned",
        None,
        None,
        None,
        i64::MAX - 1,
        i64::MAX,
        i64::MAX,
    )
    .await
    .unwrap();
    assert!(write(
        db,
        "DELETE FROM neev_agent_attempts WHERE id=$1",
        vec![attempt1.into()]
    )
    .await
    .is_err());
    assert!(write(
        db,
        "DELETE FROM neev_agent_connections WHERE id=$1",
        vec![connection1.into()]
    )
    .await
    .is_err());
}

async fn exact_columns(db: &DatabaseConnection) {
    for (table, expected) in [
        (
            "neev_agent_connections",
            vec![
                "id",
                "org_id",
                "user_id",
                "provider",
                "generation",
                "state",
                "account_label",
                "checked_at",
                "error_code",
                "storage_handle",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "neev_agent_attempts",
            vec![
                "id",
                "connection_id",
                "generation",
                "session_id",
                "state",
                "created_at",
                "expires_at",
                "updated_at",
            ],
        ),
        (
            "neev_agent_operations",
            vec![
                "id",
                "connection_id",
                "kind",
                "request_key",
                "target_attempt_id",
                "target_generation",
                "result_generation",
                "state",
                "lease_owner",
                "lease_expires_at",
                "error_code",
                "created_at",
                "expires_at",
                "updated_at",
            ],
        ),
    ] {
        let (sql, values): (&str, Vec<Value>) = if db.get_database_backend() == DbBackend::Postgres
        {
            (
                    "SELECT column_name AS name FROM information_schema.columns WHERE table_schema=current_schema() AND table_name=$1 ORDER BY ordinal_position",
                    vec![table.into()],
                )
        } else {
            (
                "SELECT name FROM pragma_table_info($1) ORDER BY cid",
                vec![table.into()],
            )
        };
        let actual: Vec<String> = db
            .query_all(statement(db, sql, values))
            .await
            .unwrap()
            .iter()
            .map(|row| row.try_get("", "name").unwrap())
            .collect();
        assert_eq!(actual, expected, "unexpected columns in {table}");
        let (sql, values): (&str, Vec<Value>) = if db.get_database_backend() == DbBackend::Postgres
        {
            (
                    "SELECT column_default FROM information_schema.columns WHERE table_schema=current_schema() AND table_name=$1 ORDER BY ordinal_position",
                    vec![table.into()],
                )
        } else {
            (
                "SELECT dflt_value AS column_default FROM pragma_table_info($1) ORDER BY cid",
                vec![table.into()],
            )
        };
        let defaults = db.query_all(statement(db, sql, values)).await.unwrap();
        assert!(
            defaults.iter().all(|row| row
                .try_get::<Option<String>>("", "column_default")
                .unwrap()
                .is_none()),
            "automatic defaults in {table}"
        );
    }
}

async fn indexes_exist(db: &DatabaseConnection) {
    for name in [
        "idx_neev_agent_attempts_state_expires",
        "idx_neev_agent_operations_state_lease_expires",
        "idx_neev_agent_connections_user_org",
    ] {
        let (sql, values): (&str, Vec<Value>) = if db.get_database_backend() == DbBackend::Postgres
        {
            ("SELECT 1 AS present FROM pg_indexes WHERE schemaname=current_schema() AND indexname=$1", vec![name.into()])
        } else {
            (
                "SELECT 1 AS present FROM sqlite_master WHERE type='index' AND name=$1",
                vec![name.into()],
            )
        };
        assert!(
            one(db, sql, values).await.unwrap().is_some(),
            "missing index {name}"
        );
    }
}

async fn rerun_preserves_agent_rows(db: &DatabaseConnection) {
    let before = (
        h::count(db, "neev_agent_connections").await,
        h::count(db, "neev_agent_attempts").await,
        h::count(db, "neev_agent_operations").await,
    );
    assert_eq!(initialize(db).await, Ok(()));
    assert_eq!(
        (
            h::count(db, "neev_agent_connections").await,
            h::count(db, "neev_agent_attempts").await,
            h::count(db, "neev_agent_operations").await,
        ),
        before
    );
}

#[tokio::test]
async fn sqlite_agent_constraints_and_ownership_are_enforced() {
    let db = h::database().await;
    connection_contract(&db).await;
    let db = h::database().await;
    attempt_operation_contract(&db).await;
    indexes_exist(&db).await;
    exact_columns(&db).await;
    rerun_preserves_agent_rows(&db).await;
}

#[tokio::test]
#[ignore = "requires explicitly isolated NEEV_TEST_DATABASE_URL"]
async fn postgres_agent_constraints_and_ownership_are_enforced() {
    use futures::FutureExt;
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("agent contract owned schema: {schema}");
    let outcome = std::panic::AssertUnwindSafe(async {
        connection_contract(&db).await;
        attempt_operation_contract(&db).await;
        indexes_exist(&db).await;
        exact_columns(&db).await;
        rerun_preserves_agent_rows(&db).await;
    })
    .catch_unwind()
    .await;
    match outcome {
        Ok(()) => h::cleanup_postgres(db, admin, schema).await,
        Err(panic) => {
            eprintln!("retained failed agent contract owned schema: {schema}");
            drop(db);
            drop(admin);
            std::panic::resume_unwind(panic);
        }
    }
}

#[tokio::test]
async fn sqlite_exact_native_versions_upgrade_without_data_loss() {
    for version in [2, 3, 4] {
        let db = h::empty_database().await;
        initialize_historical(&db, version).await;
        seed_preserved_data(&db, version >= 3).await;
        assert_eq!(initialize(&db).await, Ok(()));
        assert_preserved_data(&db, version >= 3).await;
    }
}

#[tokio::test]
#[ignore = "requires explicitly isolated NEEV_TEST_DATABASE_URL"]
async fn postgres_exact_native_versions_upgrade_without_data_loss() {
    use futures::FutureExt;
    for version in [2, 3, 4] {
        let (db, admin, schema) = h::postgres_database().await;
        eprintln!("agent upgrade owned schema: {schema}");
        let outcome = std::panic::AssertUnwindSafe(async {
            downgrade_five(&db, version).await;
            seed_preserved_data(&db, version >= 3).await;
            assert_eq!(initialize(&db).await, Ok(()));
            assert_preserved_data(&db, version >= 3).await;
        })
        .catch_unwind()
        .await;
        match outcome {
            Ok(()) => h::cleanup_postgres(db, admin, schema).await,
            Err(panic) => {
                eprintln!("retained failed agent upgrade owned schema: {schema}");
                drop(db);
                drop(admin);
                std::panic::resume_unwind(panic);
            }
        }
    }
}

#[tokio::test]
async fn initializer_refuses_mismatched_partial_and_non_table_agent_schemas() {
    let db = h::database().await;
    execute(&db, "UPDATE neev_schema_version SET version=4", vec![])
        .await
        .unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(schema_version(&db).await, 4);
    assert_eq!(h::count(&db, "neev_agent_connections").await, 0);

    let db = h::database().await;
    execute(&db, "DROP TABLE neev_agent_operations", vec![])
        .await
        .unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert_eq!(
        schema_version(&db).await,
        CURRENT_SCHEMA_VERSION
    );
    assert!(!relation_exists(&db, "neev_agent_operations", "table").await);

    let db = h::database().await;
    execute(&db, "CREATE TABLE unknown_agent_data(id INTEGER)", vec![])
        .await
        .unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert!(relation_exists(&db, "unknown_agent_data", "table").await);

    let db = h::database().await;
    execute(&db, "DROP TABLE neev_agent_operations", vec![])
        .await
        .unwrap();
    execute(
        &db,
        "CREATE VIEW neev_agent_operations AS SELECT id FROM neev_agent_connections",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(initialize(&db).await, Err(AuthError::Unavailable));
    assert!(relation_exists(&db, "neev_agent_operations", "view").await);
}
