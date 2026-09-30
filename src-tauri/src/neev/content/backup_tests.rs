use super::{
    backup::verify_snapshot,
    documents, resources,
    test_support::{request, Harness},
    types::*,
};
use crate::neev::{db, test_support as h};
use futures::FutureExt;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, TransactionTrait};
use sha2::Digest as _;

fn assert_bounded_failure(
    failure: super::backup::VerifierFailure,
    expected_code: &str,
    canaries: &[&str],
) -> ContentError {
    assert_eq!(failure.stage_code(), expected_code);
    let diagnostic = failure.diagnostic();
    assert!(diagnostic.starts_with("Native content verification failed. Require --database NAME --schema NAME --confirm-isolated and dedicated verifier environment; inspect the isolated restore privately."));
    assert_eq!(diagnostic.matches("NEEV_VERIFY_STAGE_").count(), 1);
    for canary in canaries {
        assert!(
            !diagnostic.contains(canary),
            "diagnostic reflected synthetic input"
        );
    }
    failure.into_content_error()
}

const AGENT_TABLES: [&str; 3] = [
    "neev_agent_connections",
    "neev_agent_attempts",
    "neev_agent_operations",
];
const STATUS_TABLES: [&str; 2] = ["neev_agent_status_owner", "neev_agent_status_checks"];
// Declared parent-first; dropped in reverse so turns go before their sessions.
const SESSION_TABLES: [&str; 2] = ["neev_agent_sessions", "neev_agent_turns"];
// FK-safe drop order: children before their gateway parents (section 5).
const GATEWAY_TABLES: [&str; 7] = [
    "neev_gateway_revoke_intents",
    "neev_gateway_redemptions",
    "neev_gateway_relying_sessions",
    "neev_gateway_exchanges",
    "neev_gateway_relying_bindings",
    "neev_gateway_revocations",
    "neev_gateway_audience",
];
const CONTENT_TABLES: [&str; 5] = [
    "neev_resources",
    "neev_messages",
    "neev_document_revisions",
    "neev_content_usage",
    "neev_content_leases",
];
const CONTENT_INDEXES: [&str; 2] = ["neev_resources_node_kind", "neev_resources_origin"];
const HISTORICAL_ORIGIN: &str = "(origin_chat_id IS NULL AND origin_kind IS NULL) OR (origin_chat_id IS NOT NULL AND kind='document' AND origin_kind='chat')";

// A correct hash alone must not make an invalid rich envelope restorable.
async fn rich_snapshot_contract(database: DatabaseConnection, schema_version: i64) {
    let h = Harness::new(database).await;
    let doc = resources::create(
        &h.state,
        &h.creator,
        h.input(ResourceKind::Document, "Mixed archive"),
    )
    .await
    .unwrap();
    documents::save(
        &h.state,
        &h.creator,
        &doc.id,
        SaveInput {
            request_id: request(),
            expected_content_revision: 0,
            expected_auth_revision: 1,
            markdown: "{not a rich envelope}".into(),
        },
    )
    .await
    .unwrap();
    documents::save_rich(
        &h.state,
        &h.creator,
        &doc.id,
        RichSaveInput {
            request_id: request(),
            expected_content_revision: 1,
            expected_auth_revision: 1,
            document: super::rich_tests::document(),
        },
    )
    .await
    .unwrap();
    downgrade_snapshot(&h.state.database, schema_version).await;
    let manifest = verify_snapshot(&h.state.database, &h.state.blobs)
        .await
        .unwrap();
    assert_eq!(
        (manifest.schema_version, manifest.blobs.len()),
        (schema_version, 2)
    );
    let corrupt = h
        .state
        .blobs
        .put(
            h.creator.org_id,
            &uuid::Uuid::new_v4().to_string(),
            b"{\"version\":1,\"blocks\":[]}",
        )
        .await
        .unwrap();
    db::execute(&h.state.database,"UPDATE neev_document_revisions SET blob_key=$1,sha256=$2,byte_length=$3 WHERE org_id=$4 AND document_id=$5 AND revision=2",vec![corrupt.key.into(),corrupt.sha256.into(),corrupt.byte_length.into(),h.creator.org_id.into(),doc.id.clone().into()]).await.unwrap();
    assert_eq!(
        verify_snapshot(&h.state.database, &h.state.blobs).await,
        Err(ContentError::Storage)
    );
    assert_eq!(
        assert_bounded_failure(
            super::backup::verify_snapshot_staged(&h.state.database, &h.state.blobs)
                .await
                .unwrap_err(),
            "NEEV_VERIFY_STAGE_RICH_DECODE",
            &["{not a rich envelope}", "Mixed archive"],
        ),
        ContentError::Storage
    );
    assert_eq!(
        documents::restore(&h.state, &h.creator, &doc.id, 2, &request(), 2, 1).await,
        Err(ContentError::Storage)
    );
    assert_eq!(
        documents::read(&h.state, &h.creator, &doc.id, Some(1))
            .await
            .unwrap()
            .1,
        b"{not a rich envelope}"
    );
}

#[tokio::test]
async fn sqlite_rich_backup_validates_body_format() {
    rich_snapshot_contract(h::database().await, 6).await;
}

#[tokio::test]
async fn schema_seven_blocks_verification_uses_format_metadata() {
    rich_snapshot_contract(h::database().await, 7).await;
}
#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_rich_backup_validates_body_format() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("rich backup owned schema: {schema}");
    rich_snapshot_contract(db.clone(), 6).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_schema_seven_blocks_verification_uses_format_metadata() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("schema7 rich backup owned schema: {schema}");
    rich_snapshot_contract(db.clone(), 7).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
async fn sqlite_gateway8_rich_format_six_seven_eight() {
    for schema_version in [6, 7, 8] {
        rich_snapshot_contract(h::database().await, schema_version).await;
    }
}

#[tokio::test]
async fn sqlite_schema_nine_through_eleven_decode_rich_bodies_and_catch_corruption() {
    // Regression: a v9/v10 snapshot must decode block documents (not read them as
    // markdown) and reject a corrupt rich body at the rich-decode stage.
    for schema_version in [9, 10, 11, 12, 13, 14, 15, 16, 17] {
        rich_snapshot_contract(h::database().await, schema_version).await;
    }
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_gateway8_rich_format_six_seven_eight() {
    for schema_version in [6, 7, 8] {
        let (db, admin, schema) = h::postgres_database().await;
        eprintln!("gateway8 rich format owned schema: {schema}");
        rich_snapshot_contract(db.clone(), schema_version).await;
        h::cleanup_postgres(db, admin, schema).await;
    }
}

async fn reconstruct_sqlite_v3(tx: &sea_orm::DatabaseTransaction) {
    let mut tables = Vec::new();
    for name in CONTENT_TABLES {
        let sql = db::one(
            tx,
            "SELECT sql FROM sqlite_master WHERE type='table' AND name=$1",
            vec![name.into()],
        )
        .await
        .unwrap()
        .unwrap()
        .try_get::<String>("", "sql")
        .unwrap();
        tables.push((name, sql));
    }
    let mut indexes = Vec::new();
    for name in CONTENT_INDEXES {
        indexes.push(
            db::one(
                tx,
                "SELECT sql FROM sqlite_master WHERE type='index' AND name=$1",
                vec![name.into()],
            )
            .await
            .unwrap()
            .unwrap()
            .try_get::<String>("", "sql")
            .unwrap(),
        );
    }
    for name in CONTENT_TABLES.iter().rev() {
        db::execute(
            tx,
            &format!("CREATE TEMP TABLE backup_fixture_{name} AS SELECT * FROM {name}"),
            vec![],
        )
        .await
        .unwrap();
        db::execute(tx, &format!("DROP TABLE {name}"), vec![])
            .await
            .unwrap();
    }
    for (name, sql) in tables {
        let sql = if name == "neev_resources" {
            let historical = sql.replace(
                "origin_chat_id IS NOT NULL AND origin_kind IS NOT NULL AND",
                "origin_chat_id IS NOT NULL AND",
            );
            assert_ne!(historical, sql, "SQLite v3 origin CHECK was not weakened");
            historical
        } else {
            sql
        };
        db::execute(tx, &sql, vec![]).await.unwrap();
    }
    for sql in indexes {
        db::execute(tx, &sql, vec![]).await.unwrap();
    }
    for name in CONTENT_TABLES {
        db::execute(
            tx,
            &format!("INSERT INTO {name} SELECT * FROM backup_fixture_{name}"),
            vec![],
        )
        .await
        .unwrap();
        db::execute(tx, &format!("DROP TABLE backup_fixture_{name}"), vec![])
            .await
            .unwrap();
    }
}

async fn reconstruct_postgres_v3(tx: &sea_orm::DatabaseTransaction) {
    let constraints = tx
        .query_all(db::statement(
            tx,
            "SELECT c.conname FROM pg_constraint c JOIN pg_class t ON t.oid=c.conrelid JOIN pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname=current_schema() AND t.relname='neev_resources' AND c.contype='c' AND pg_get_constraintdef(c.oid) LIKE '%origin_chat_id%'",
            vec![],
        ))
        .await
        .unwrap();
    assert!(
        constraints.len() >= 2,
        "fresh native5 must contain creation and hardening origin CHECKs"
    );
    for row in constraints {
        let name = row.try_get::<String>("", "conname").unwrap();
        assert!(
            !name.is_empty()
                && name.len() <= 63
                && name
                    .bytes()
                    .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric()),
            "unexpected owned constraint identifier"
        );
        db::execute(
            tx,
            &format!("ALTER TABLE neev_resources DROP CONSTRAINT \"{name}\""),
            vec![],
        )
        .await
        .unwrap();
    }
    db::execute(
        tx,
        &format!("ALTER TABLE neev_resources ADD CONSTRAINT neev_resources_valid_origin_v3 CHECK ({HISTORICAL_ORIGIN})"),
        vec![],
    )
    .await
    .unwrap();
}

async fn downgrade_snapshot(database: &DatabaseConnection, schema_version: i64) {
    if schema_version == db::CURRENT_SCHEMA_VERSION {
        return;
    }
    // Schemas 18 (library) and 19 (steer) are newer than every inventory this
    // fixture rebuilds; 20 only widened a CHECK, which older markers tolerate.
    for table in ["neev_steer_inputs", "neev_library_files", "neev_library_items"] {
        db::execute(database, &format!("DROP TABLE IF EXISTS {table}"), vec![])
            .await
            .unwrap();
    }
    if schema_version == 17 {
        db::execute(database, "UPDATE neev_schema_version SET version=17", vec![])
            .await
            .unwrap();
        return;
    }
    db::execute(
        database,
        "DROP INDEX neev_attachments_chat_archived",
        vec![],
    )
    .await
    .unwrap();
    db::execute(
        database,
        "DROP INDEX neev_attachments_session_archived",
        vec![],
    )
    .await
    .unwrap();
    db::execute(
        database,
        "ALTER TABLE neev_attachments DROP COLUMN archived",
        vec![],
    )
    .await
    .unwrap();
    if schema_version == 16 {
        db::execute(
            database,
            "UPDATE neev_schema_version SET version=16",
            vec![],
        )
        .await
        .unwrap();
        return;
    }
    // 16 adds the mentions a message carries.
    db::execute(database, "DROP TABLE neev_message_mentions", vec![])
        .await
        .unwrap();
    if schema_version == 15 {
        db::execute(
            database,
            "UPDATE neev_schema_version SET version=15",
            vec![],
        )
        .await
        .unwrap();
        return;
    }
    // 15 adds two TABLES; a snapshot of an older version does not have them at
    // all, and `validate_content_snapshot` compares the inventory exactly.
    // References go before the attachments they point at.
    for table in ["neev_attachment_refs", "neev_attachments"] {
        db::execute(database, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    if schema_version == 14 {
        db::execute(
            database,
            "UPDATE neev_schema_version SET version=14",
            vec![],
        )
        .await
        .unwrap();
        return;
    }
    // 14 is two additive COLUMNS on the sessions table; a snapshot of an older
    // version sheds them and keeps the table.
    for column in ["fast", "effort"] {
        db::execute(
            database,
            &format!("ALTER TABLE neev_agent_sessions DROP COLUMN {column}"),
            vec![],
        )
        .await
        .unwrap();
    }
    if schema_version == 13 {
        db::execute(
            database,
            "UPDATE neev_schema_version SET version=13",
            vec![],
        )
        .await
        .unwrap();
        return;
    }
    // 13 and 12 are additive COLUMNS on the session tables; a snapshot of an
    // older version drops them and keeps the tables, which only go below 11.
    db::execute(
        database,
        "ALTER TABLE neev_agent_turns DROP COLUMN activity",
        vec![],
    )
    .await
    .unwrap();
    if schema_version < 12 {
        db::execute(
            database,
            "ALTER TABLE neev_agent_sessions DROP COLUMN node_id",
            vec![],
        )
        .await
        .unwrap();
    }
    if schema_version >= 11 {
        db::execute(
            database,
            "UPDATE neev_schema_version SET version=$1",
            vec![schema_version.into()],
        )
        .await
        .unwrap();
        return;
    }
    let sqlite_rebuild =
        schema_version == 3 && database.get_database_backend() == DbBackend::Sqlite;
    if sqlite_rebuild {
        db::execute(database, "PRAGMA foreign_keys=OFF", vec![])
            .await
            .unwrap();
    }
    let tx = database.begin().await.unwrap();
    // v11 added the agent-session tables on top of a fresh install; every lower
    // target must remove them (turns first — they reference sessions).
    for table in SESSION_TABLES.iter().rev() {
        db::execute(&tx, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    if schema_version < 10 {
        db::execute(&tx, "DROP TABLE neev_image_blobs", vec![])
            .await
            .unwrap();
    }
    if schema_version < 9 {
        // v9 added the join-code table; lower targets must remove it too.
        db::execute(&tx, "DROP TABLE neev_org_join_codes", vec![])
            .await
            .unwrap();
    }
    if schema_version < 8 {
        for table in GATEWAY_TABLES {
            db::execute(&tx, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
    }
    if schema_version < 7 {
        for table in STATUS_TABLES.iter().rev() {
            db::execute(&tx, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
    }
    if schema_version < 6 {
        db::execute(&tx, "DROP TABLE neev_document_formats", vec![])
            .await
            .unwrap();
    }
    if schema_version < 5 {
        for table in AGENT_TABLES.iter().rev() {
            db::execute(&tx, &format!("DROP TABLE {table}"), vec![])
                .await
                .unwrap();
        }
    }
    if schema_version == 3 {
        match database.get_database_backend() {
            DbBackend::Sqlite => reconstruct_sqlite_v3(&tx).await,
            DbBackend::Postgres => reconstruct_postgres_v3(&tx).await,
            _ => panic!("unsupported test database"),
        }
    }
    db::execute(
        &tx,
        "UPDATE neev_schema_version SET version=$1",
        vec![schema_version.into()],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    if sqlite_rebuild {
        db::execute(database, "PRAGMA foreign_keys=ON", vec![])
            .await
            .unwrap();
        assert!(database
            .query_all(db::statement(database, "PRAGMA foreign_key_check", vec![]))
            .await
            .unwrap()
            .is_empty());
    }
}

// Catches omission of historical revisions, trusting corrupt bytes, and cross-org lookup.
async fn restored_bytes(database: DatabaseConnection, schema_version: i64) {
    let h = Harness::new(database).await;
    let doc = resources::create(
        &h.state,
        &h.creator,
        h.input(ResourceKind::Document, "Secret title"),
    )
    .await
    .unwrap();
    for (head, text) in [(0, "old secret"), (1, "new secret")] {
        documents::save(
            &h.state,
            &h.creator,
            &doc.id,
            SaveInput {
                request_id: request(),
                expected_content_revision: head,
                expected_auth_revision: 1,
                markdown: text.into(),
            },
        )
        .await
        .unwrap();
    }
    // Populate through the current service, then reconstruct the actual archive
    // inventory. Historical verification never calls a schema6 service writer.
    downgrade_snapshot(&h.state.database, schema_version).await;
    let historical_null_shape = db::execute(
        &h.state.database,
        "UPDATE neev_resources SET origin_chat_id='missing-source',origin_kind=NULL WHERE org_id=$1 AND id=$2",
        vec![h.creator.org_id.into(), doc.id.clone().into()],
    ).await;
    if schema_version == 3 {
        assert!(
            historical_null_shape.is_ok(),
            "schema3 retains its historical SQL-NULL origin behavior"
        );
        db::execute(&h.state.database,"UPDATE neev_resources SET origin_chat_id=NULL,origin_kind=NULL WHERE org_id=$1 AND id=$2",vec![h.creator.org_id.into(),doc.id.clone().into()]).await.unwrap();
    } else {
        assert!(historical_null_shape.is_err());
    }
    let manifest = verify_snapshot(&h.state.database, &h.state.blobs)
        .await
        .unwrap();
    assert_eq!(manifest.schema_version, schema_version);
    assert_eq!(manifest.blobs.len(), 2);
    assert_eq!(
        manifest.blobs.iter().map(|b| b.byte_length).sum::<i64>(),
        20
    );
    assert_eq!(
        verify_snapshot(&h.state.database, &h.state.blobs)
            .await
            .unwrap(),
        manifest
    );
    let encoded = serde_json::to_string(&manifest).unwrap();
    for secret in ["secret", "example.test", "Secret title"] {
        assert!(!encoded.contains(secret));
    }
    h.state
        .blobs
        .put(h.creator.org_id, &request(), b"orphan")
        .await
        .unwrap();
    let org = h
        .state
        .config
        .document_root
        .join(h.creator.org_id.to_string());
    std::fs::write(org.join("incomplete.tmp"), b"unfinished").unwrap();
    assert_eq!(
        verify_snapshot(&h.state.database, &h.state.blobs)
            .await
            .unwrap(),
        manifest
    );
    let key = &manifest.blobs[0].key;
    let path = org.join(format!("{key}.md"));
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"bad secret").unwrap();
    assert_eq!(
        assert_bounded_failure(
            super::backup::verify_snapshot_staged(&h.state.database, &h.state.blobs)
                .await
                .unwrap_err(),
            "NEEV_VERIFY_STAGE_BLOB_READ",
            &["bad secret", "Secret title", path.to_str().unwrap()],
        ),
        ContentError::Storage
    );
    assert_eq!(
        verify_snapshot(&h.state.database, &h.state.blobs).await,
        Err(ContentError::Storage)
    );
    std::fs::write(&path, &bytes).unwrap();
    h.state
        .blobs
        .put(h.foreign.org_id, key, &bytes)
        .await
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(verify_snapshot(&h.state.database, &h.state.blobs)
        .await
        .is_err());
}

#[tokio::test]
async fn sqlite_restored_bytes() {
    for version in [3, 4, 5, 6, 7, 8] {
        restored_bytes(h::database().await, version).await;
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_restored_bytes() {
    for version in [3, 4, 5, 6, 7, 8] {
        let (db, admin, schema) = h::postgres_database().await;
        eprintln!("backup owned schema: {schema}");
        let outcome = std::panic::AssertUnwindSafe(restored_bytes(db.clone(), version))
            .catch_unwind()
            .await;
        match outcome {
            Ok(()) => h::cleanup_postgres(db, admin, schema).await,
            Err(panic) => {
                eprintln!("retained failed backup owned schema: {schema}");
                drop(db);
                drop(admin);
                std::panic::resume_unwind(panic);
            }
        }
    }
}

// Catches a free-form isolation label being accepted while connected elsewhere.
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_destination_must_match_connection() {
    use super::backup::verify_destination;
    let (db, admin, schema) = h::postgres_database().await;
    let isolated = format!("neev_restore_{}", uuid::Uuid::new_v4().simple());
    eprintln!("backup destination owned schemas: {schema}, {isolated}");
    db::execute(&admin, &format!("CREATE SCHEMA {isolated}"), vec![])
        .await
        .unwrap();
    let mut options =
        sea_orm::ConnectOptions::new(std::env::var("NEEV_TEST_DATABASE_URL").unwrap());
    options
        .max_connections(1)
        .sqlx_logging(false)
        .set_schema_search_path(&isolated);
    let restored = sea_orm::Database::connect(options).await.unwrap();
    let outcome = std::panic::AssertUnwindSafe(async {
        assert!(
            verify_destination(&db, "neev_identity_test", &isolated, true)
                .await
                .is_err()
        );
        assert!(
            verify_destination(&restored, "wrong_database", &isolated, true)
                .await
                .is_err()
        );
        assert!(
            verify_destination(&restored, "neev_identity_test", &isolated, false)
                .await
                .is_err()
        );
        assert!(verify_destination(&db, "neev_identity_test", &schema, true)
            .await
            .is_err());
        assert!(
            verify_destination(&restored, "neev_identity_test", &isolated, true)
                .await
                .is_ok()
        );
    })
    .catch_unwind()
    .await;
    restored.close().await.unwrap();
    db::execute(&admin, &format!("DROP SCHEMA {isolated}"), vec![])
        .await
        .unwrap();
    h::cleanup_postgres(db, admin, schema).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

// A verifier must neither initialize an empty DB nor migrate/repair a snapshot.
#[tokio::test]
async fn snapshot_schema_is_exact_and_read_only() {
    let cfg = h::preview_config();
    let store = super::BlobStore::open(&cfg.document_root).unwrap();
    let empty = h::empty_database().await;
    assert_eq!(
        assert_bounded_failure(
            super::backup::verify_snapshot_staged(&empty, &store)
                .await
                .unwrap_err(),
            "NEEV_VERIFY_STAGE_SNAPSHOT_SCHEMA",
            &[cfg.document_root.to_str().unwrap()],
        ),
        ContentError::Auth(crate::neev::error::AuthError::Unavailable)
    );
    assert!(verify_snapshot(&empty, &store).await.is_err());
    assert!(db::one(
        &empty,
        "SELECT name FROM sqlite_master WHERE type='table'",
        vec![]
    )
    .await
    .unwrap()
    .is_none());
    let db = h::database().await;
    db::execute(&db, "PRAGMA query_only=ON", vec![])
        .await
        .unwrap();
    let manifest = verify_snapshot(&db, &store).await.unwrap();
    assert_eq!(manifest.schema_version, db::CURRENT_SCHEMA_VERSION);
    assert_eq!(manifest.blobs.len(), 0);
    db::execute(&db, "PRAGMA query_only=OFF", vec![])
        .await
        .unwrap();
    let partial = h::database().await;
    db::execute(&partial, "DROP TABLE neev_agent_operations", vec![])
        .await
        .unwrap();
    assert!(
        verify_snapshot(&partial, &store).await.is_err(),
        "schema5 missing one agent table must be refused"
    );
    for version in [3_i64, 4] {
        db::execute(
            &db,
            "UPDATE neev_schema_version SET version=$1",
            vec![version.into()],
        )
        .await
        .unwrap();
        assert!(
            verify_snapshot(&db, &store).await.is_err(),
            "historical marker {version} must reject agent tables"
        );
    }
    db::execute(&db, "UPDATE neev_schema_version SET version=5", vec![])
        .await
        .unwrap();
    // This fixture rewinds the current schema to a historical inventory.
    // Mentions first (they point at messages), then attachment children before
    // dropping their session parents.
    db::execute(&db, "DROP TABLE neev_message_mentions", vec![])
        .await
        .unwrap();
    db::execute(&db, "DROP TABLE neev_attachment_refs", vec![])
        .await
        .unwrap();
    db::execute(&db, "DROP TABLE neev_attachments", vec![])
        .await
        .unwrap();
    db::execute(&db, "DROP TABLE neev_image_blobs", vec![])
        .await
        .unwrap();
    for table in SESSION_TABLES.iter().rev() {
        db::execute(&db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    db::execute(&db, "DROP TABLE neev_org_join_codes", vec![])
        .await
        .unwrap();
    for table in GATEWAY_TABLES {
        db::execute(&db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    for table in STATUS_TABLES.iter().rev() {
        db::execute(&db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    db::execute(&db, "DROP TABLE neev_document_formats", vec![])
        .await
        .unwrap();
    for table in AGENT_TABLES.iter().rev() {
        db::execute(&db, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    for version in [3_i64, 4] {
        db::execute(
            &db,
            "UPDATE neev_schema_version SET version=$1",
            vec![version.into()],
        )
        .await
        .unwrap();
        db::execute(&db, "PRAGMA query_only=ON", vec![])
            .await
            .unwrap();
        assert_eq!(
            verify_snapshot(&db, &store).await.unwrap().schema_version,
            version
        );
        db::execute(&db, "PRAGMA query_only=OFF", vec![])
            .await
            .unwrap();
        assert_eq!(
            db::one(&db, "SELECT version FROM neev_schema_version", vec![])
                .await
                .unwrap()
                .unwrap()
                .try_get::<i64>("", "version")
                .unwrap(),
            version
        );
    }
    for version in [1_i64, 2, 5, 6] {
        db::execute(
            &db,
            "UPDATE neev_schema_version SET version=$1",
            vec![version.into()],
        )
        .await
        .unwrap();
        assert!(verify_snapshot(&db, &store).await.is_err());
        assert_eq!(
            db::one(&db, "SELECT version FROM neev_schema_version", vec![])
                .await
                .unwrap()
                .unwrap()
                .try_get::<i64>("", "version")
                .unwrap(),
            version
        );
    }
    db::execute(&db, "UPDATE neev_schema_version SET version=3", vec![])
        .await
        .unwrap();
    db::execute(&db, "CREATE TABLE legacy_data(id INTEGER)", vec![])
        .await
        .unwrap();
    assert!(verify_snapshot(&db, &store).await.is_err());
    db::execute(&db, "DROP TABLE legacy_data", vec![])
        .await
        .unwrap();
    db::execute(&db, "DROP TABLE neev_content_leases", vec![])
        .await
        .unwrap();
    assert!(verify_snapshot(&db, &store).await.is_err());

    let missing_agent = h::database().await;
    db::execute(&missing_agent, "DROP TABLE neev_agent_operations", vec![])
        .await
        .unwrap();
    assert!(
        verify_snapshot(&missing_agent, &store).await.is_err(),
        "schema5 must reject a missing agent table"
    );
}

#[test]
fn verifier_arguments_require_explicit_isolation_and_confirmation() {
    use super::backup::arguments;
    let valid = [
        "--database",
        "neev_restore_0123456789abcdef0123456789abcdef",
        "--schema",
        "public",
        "--confirm-isolated",
    ]
    .map(String::from)
    .to_vec();
    assert!(arguments(&valid).is_ok());
    assert!(arguments(&valid[..4]).is_err());
    assert!(arguments(&[]).is_err());
    for (index, replacement) in [
        (1, "production"),
        (3, "public,other"),
        (3, "missing*"),
        (0, "--url"),
        (1, "neev_restore_label"),
    ] {
        let mut bad = valid.clone();
        bad[index] = replacement.into();
        assert!(arguments(&bad).is_err());
    }
    let mut duplicate = valid.clone();
    duplicate.extend(["--schema".into(), "public".into()]);
    assert!(arguments(&duplicate).is_err());

    let argument_canary = "cli-secret.example.test/account-42";
    let failure = super::backup::staged_arguments(
        &["--database", argument_canary, "--confirm-isolated"].map(String::from),
    )
    .unwrap_err();
    assert_eq!(
        assert_bounded_failure(failure, "NEEV_VERIFY_STAGE_ARGUMENTS", &[argument_canary]),
        ContentError::Invalid
    );

    let root_canary = "relative/private-document-root";
    let failure = match super::backup::staged_document_root(std::path::Path::new(root_canary)) {
        Ok(_) => panic!("relative document root must fail closed"),
        Err(failure) => failure,
    };
    assert_eq!(
        assert_bounded_failure(failure, "NEEV_VERIFY_STAGE_DOCUMENT_ROOT", &[root_canary]),
        ContentError::Invalid
    );
}

// A valid native schema can still contain a value that cannot be extracted as
// the verifier's declared reference type when SQLite constraints were bypassed.
#[tokio::test]
async fn snapshot_reference_extraction_has_a_bounded_stage() {
    let h = Harness::new(h::database().await).await;
    let title_canary = "reference-secret.example.test/account-84";
    let doc = resources::create(
        &h.state,
        &h.creator,
        h.input(ResourceKind::Document, title_canary),
    )
    .await
    .unwrap();
    documents::save(
        &h.state,
        &h.creator,
        &doc.id,
        SaveInput {
            request_id: request(),
            expected_content_revision: 0,
            expected_auth_revision: 1,
            markdown: "typed extraction canary".into(),
        },
    )
    .await
    .unwrap();
    db::execute(
        &h.state.database,
        "PRAGMA ignore_check_constraints=ON",
        vec![],
    )
    .await
    .unwrap();
    db::execute(
        &h.state.database,
        "UPDATE neev_document_revisions SET byte_length=X'0102' WHERE org_id=$1 AND document_id=$2",
        vec![h.creator.org_id.into(), doc.id.into()],
    )
    .await
    .unwrap();
    let failure = super::backup::verify_snapshot_staged(&h.state.database, &h.state.blobs)
        .await
        .unwrap_err();
    assert_eq!(
        assert_bounded_failure(
            failure,
            "NEEV_VERIFY_STAGE_REFERENCE_EXTRACTION",
            &[title_canary, "typed extraction canary"],
        ),
        ContentError::Storage
    );
    assert_eq!(
        verify_snapshot(&h.state.database, &h.state.blobs).await,
        Err(ContentError::Storage)
    );
}

// A same-name empty view must not hide missing revision storage as zero references.
async fn revision_view_is_refused(database: DatabaseConnection) {
    let cfg = h::preview_config();
    let store = super::BlobStore::open(&cfg.document_root).unwrap();
    // Use the exact schema5 inventory to isolate the original relation-kind
    // regression from schema6's new restrictive revision foreign key.
    downgrade_snapshot(&database, 5).await;
    db::execute(&database, "DROP TABLE neev_document_revisions", vec![])
        .await
        .unwrap();
    db::execute(&database, "CREATE VIEW neev_document_revisions AS SELECT 1 AS org_id,'' AS blob_key,'' AS sha256,0 AS byte_length WHERE false", vec![]).await.unwrap();
    if database.get_database_backend() == sea_orm::DbBackend::Sqlite {
        db::execute(&database, "PRAGMA query_only=ON", vec![])
            .await
            .unwrap();
    }
    assert!(
        verify_snapshot(&database, &store).await.is_err(),
        "same-name view must not be accepted as native revision storage"
    );
    let query = if database.get_database_backend() == sea_orm::DbBackend::Postgres {
        "SELECT COUNT(*) AS n FROM pg_views WHERE schemaname=current_schema() AND viewname='neev_document_revisions'"
    } else {
        "SELECT COUNT(*) AS n FROM sqlite_master WHERE type='view' AND name='neev_document_revisions'"
    };
    assert_eq!(
        db::one(&database, query, vec![])
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "n")
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn sqlite_revision_view_is_refused_without_writes() {
    revision_view_is_refused(h::database().await).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_revision_view_is_refused_without_writes() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("backup relation-kind owned schema: {schema}");
    let outcome = std::panic::AssertUnwindSafe(revision_view_is_refused(db.clone()))
        .catch_unwind()
        .await;
    finish_postgres_fixture(db, admin, schema, outcome).await;
}

async fn finish_postgres_fixture(
    db: DatabaseConnection,
    admin: DatabaseConnection,
    schema: String,
    outcome: Result<(), Box<dyn std::any::Any + Send>>,
) {
    if let Err(panic) = outcome {
        eprintln!("retained failed backup fixture: {schema}");
        drop(db);
        drop(admin);
        std::panic::resume_unwind(panic);
    }
    h::cleanup_postgres(db, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_backup_failure_retains_fixture_before_rethrow() {
    let (db, admin, schema) = h::postgres_database().await;
    let retained = db.clone();
    let inspect = admin.clone();
    let name = schema.clone();
    let outcome = std::panic::AssertUnwindSafe(finish_postgres_fixture(
        db,
        admin,
        schema,
        Err(Box::new("intentional retention probe")),
    ))
    .catch_unwind()
    .await;
    assert!(outcome.is_err());
    assert!(
        db::one(
            &inspect,
            "SELECT 1 AS present FROM pg_namespace WHERE nspname=$1",
            vec![name.clone().into()]
        )
        .await
        .unwrap()
        .is_some(),
        "failure evidence must exist before rethrow"
    );
    // This regression passed: its intentional probe is disposable. A real test
    // failure above unwinds before reaching this successful-test cleanup.
    h::cleanup_postgres(retained, inspect, name).await;
}

fn bounded_child(mut command: std::process::Command) -> std::process::Output {
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().unwrap();
    let start = std::time::Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(10) {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("bounded verifier regression child timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.stdout.len() + output.stderr.len() < 16000);
    output
}

// Only child environments are changed. Both pgpass locations are synthetic owned files.
#[test]
fn verifier_connection_ignores_ambient_settings_and_password_files() {
    use std::os::unix::fs::PermissionsExt;
    let cfg = h::preview_config();
    for name in [".pgpass", "custom.pgpass"] {
        let path = cfg.document_root.join(name);
        std::fs::write(&path, "*:*:*:*:synthetic-file-password\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    for mode in ["ambient", "custom", "default"] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "neev::content::backup_tests::ambient_connection_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]);
        command
            .env_clear()
            .env("HOME", &cfg.document_root)
            .env("NEEV_VERIFY_DOCUMENT_ROOT", &cfg.document_root)
            .env("NEEV_VERIFY_ISOLATED", "1")
            .env(
                "NEEV_VERIFY_DATABASE_URL",
                "postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1",
            )
            .env("PGHOST", "unintended.invalid")
            .env("PGHOSTADDR", "192.0.2.1")
            .env("PGUSER", "unintended-user")
            .env("PGDATABASE", "unintended-db")
            .env("PGPORT", "1")
            .env("PGOPTIONS", "-c search_path=wrong")
            .env("PGSSLMODE", "disable")
            .env("PGSSLROOTCERT", "/unintended-ca")
            .env("PGSSLCERT", "/unintended-cert")
            .env("PGSSLKEY", "/unintended-key")
            .env("PGAPPNAME", "unintended-app");
        if mode == "ambient" {
            command.env("PGPASSWORD", "synthetic-env-password");
        }
        command.env(
            "PGPASSFILE",
            if mode == "custom" {
                cfg.document_root.join("custom.pgpass")
            } else {
                std::path::PathBuf::from("/dev/null")
            },
        );
        let output = bounded_child(command);
        assert!(output.status.success(), "ambient PostgreSQL settings or password files reached verifier connection options ({mode})");
    }
}

#[test]
#[ignore = "spawned with synthetic environment by verifier regression"]
fn ambient_connection_child() {
    let Ok(url) = std::env::var("NEEV_VERIFY_DATABASE_URL") else {
        return;
    };
    let root = std::env::var_os("NEEV_VERIFY_DOCUMENT_ROOT").unwrap();
    let failure = super::backup::staged_isolated_environment().unwrap_err();
    assert_eq!(
        assert_bounded_failure(
            failure,
            "NEEV_VERIFY_STAGE_ENVIRONMENT",
            &["synthetic-env-password", "unintended.invalid"],
        ),
        ContentError::Invalid
    );
    assert!(
        super::backup::controlled_options(&url, "public").is_err(),
        "an internal marker alone must not bypass the environment contract"
    );
    let args = [
        "--exact",
        "neev::content::backup_tests::controlled_connection_child",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]
    .map(String::from);
    let command = super::backup::isolated_command(
        &std::env::current_exe().unwrap(),
        &args,
        url.as_ref(),
        &root,
    );
    assert!(
        bounded_child(command).status.success(),
        "controlled connection child failed its isolated-settings assertions"
    );
}

#[test]
#[ignore = "spawned with synthetic environment by verifier regression"]
fn controlled_connection_child() {
    use sea_orm::sqlx::ConnectOptions;
    let Ok(url) = std::env::var("NEEV_VERIFY_DATABASE_URL") else {
        return;
    };
    let unexpected = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| {
            if cfg!(target_os = "macos") && name == "__CF_USER_TEXT_ENCODING" {
                return false;
            }
            !matches!(
                name.to_str(),
                Some(
                    "NEEV_VERIFY_DATABASE_URL"
                        | "NEEV_VERIFY_DOCUMENT_ROOT"
                        | "NEEV_VERIFY_ISOLATED"
                )
            )
        })
        .collect::<Vec<_>>();
    assert!(
        unexpected.is_empty(),
        "unexpected environment key names: {unexpected:?}"
    );
    let options = super::backup::controlled_options(&url, "public").unwrap();
    assert!(
        options.to_url_lossy().password().is_none(),
        "connection acquired an ambient/file password"
    );
    assert!(
        options.get_host() == "/tmp/neev-pg.aTlLNW",
        "connection retained ambient host"
    );
    assert!(options.get_username() == "air1");
    assert!(options.get_database() == Some("neev_identity_test"));
    assert_eq!(options.get_port(), 55439);
    assert!(matches!(
        options.get_ssl_mode(),
        sea_orm::sqlx::postgres::PgSslMode::Prefer
    ));
    assert!(options.get_options() == Some("-c search_path=public"));
    let rendered = options.to_url_lossy();
    assert!(!rendered
        .query_pairs()
        .any(|(key, _)| matches!(key.as_ref(), "sslrootcert" | "sslcert" | "sslkey")));
    let explicit = super::backup::controlled_options("postgresql://test@localhost/db?password=literal+plus&sslmode=verify-full&sslrootcert=/synthetic/ca&sslcert=/synthetic/cert&sslkey=/synthetic/key", "public").unwrap();
    assert!(matches!(
        explicit.get_ssl_mode(),
        sea_orm::sqlx::postgres::PgSslMode::VerifyFull
    ));
    assert!(
        urlencoding::decode(explicit.to_url_lossy().password().unwrap()).unwrap() == "literal+plus"
    );
    let rendered = explicit.to_url_lossy();
    for (name, expected) in [
        // SQLx's lossy URL displays CertificateInput::File with this prefix.
        ("sslrootcert", "file: /synthetic/ca"),
        ("sslcert", "file: /synthetic/cert"),
        ("sslkey", "file: /synthetic/key"),
    ] {
        assert!(rendered
            .query_pairs()
            .any(|(key, value)| key == name && value == expected));
    }
    for bad in [
        "postgresql:///db?user=test",
        "postgresql://localhost/db",
        "postgresql://test@localhost/db?host=other",
        "postgresql://test@localhost/db?port=1&port=2",
        "postgresql://test@localhost/first/../db",
        "postgresql://test@localhost/db?options=-csearch_path=other",
        "postgresql://test@localhost/db?sslmode=wrong",
        "postgresql://test@localhost/db?sslmode=allow",
        "postgresql://test@localhost/db?sslcert=/synthetic/only-cert",
        "postgresql://test@localhost/db?channel_binding=require",
        "postgresql://test@localhost/db#fragment",
    ] {
        assert!(
            super::backup::controlled_options(bad, "public").is_err(),
            "ambiguous or unsupported connection must fail closed"
        );
    }
    let options_canary =
        "postgresql://test:options-secret@localhost/db?options=/private/canary.sql";
    let failure = super::backup::staged_controlled_options(options_canary, "public").unwrap_err();
    assert_eq!(
        assert_bounded_failure(
            failure,
            "NEEV_VERIFY_STAGE_DATABASE_OPTIONS",
            &["options-secret", "/private/canary.sql", options_canary],
        ),
        ContentError::Invalid
    );
}

/// Where a tenant's attachment blob actually lives, so a test can lose or
/// damage one the way a bad archive does.
fn attachment_path(h: &Harness, org_id: i32, key: &str) -> std::path::PathBuf {
    h.state
        .config
        .document_root
        .join(org_id.to_string())
        .join(format!("{key}.att"))
}

fn attachment_png() -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&[0u8; 56]);
    bytes
}

fn reference_for<'a>(
    manifest: &'a super::backup::Manifest,
    key: &str,
) -> Option<&'a super::backup::Reference> {
    manifest.blobs.iter().find(|blob| blob.key == key)
}

/// A snapshot has to account for the FILES a conversation carries, not just the
/// rows that name them.
///
/// Before this, `verify_snapshot` checked document bodies (schema >= 3) and
/// document images (schema >= 10) but walked straight past `neev_attachments`.
/// An archive whose `.att` blobs were never copied — or were copied short —
/// verified as healthy, and the loss surfaced when someone opened the
/// conversation and found an attachment that no longer had any bytes behind it.
async fn attachment_snapshot_contract(database: DatabaseConnection) {
    let h = Harness::new(database).await;
    let chat = h.chat().await;
    let baseline = verify_snapshot(&h.state.database, &h.state.blobs)
        .await
        .unwrap();
    assert_eq!(baseline.schema_version, db::CURRENT_SCHEMA_VERSION);

    let png = attachment_png();
    let notes = b"attachment acceptance notes\n".to_vec();
    let image = super::attachment::store(
        &h.state,
        &h.creator,
        super::attachment::Owner::Chat(&chat.id),
        "one.png",
        &png,
    )
    .await
    .unwrap();
    let text = super::attachment::store(
        &h.state,
        &h.creator,
        super::attachment::Owner::Chat(&chat.id),
        "two.txt",
        &notes,
    )
    .await
    .unwrap();

    // The manifest is what an export and a restore are compared on, so both
    // files have to be IN it, with the length and digest the row claims.
    let manifest = verify_snapshot(&h.state.database, &h.state.blobs)
        .await
        .unwrap();
    assert_eq!(manifest.blobs.len(), baseline.blobs.len() + 2);
    let listed =
        reference_for(&manifest, &image.key).expect("the image attachment is accounted for");
    assert_eq!(
        (listed.org_id, listed.byte_length),
        (h.creator.org_id, png.len() as i64)
    );
    assert_eq!(
        listed.sha256,
        format!("{:x}", sha2::Sha256::digest(&png)),
        "the manifest must carry the real digest, not a placeholder"
    );
    assert!(reference_for(&manifest, &text.key).is_some());

    // A file the archive never copied. The row survives and still looks
    // perfectly healthy on its own; only reading the bytes finds it.
    let lost = attachment_path(&h, h.creator.org_id, &image.key);
    std::fs::remove_file(&lost).unwrap();
    assert_eq!(
        verify_snapshot(&h.state.database, &h.state.blobs).await,
        Err(ContentError::NotFound),
        "a missing attachment blob must not verify as healthy"
    );
    assert_eq!(
        assert_bounded_failure(
            super::backup::verify_snapshot_staged(&h.state.database, &h.state.blobs)
                .await
                .unwrap_err(),
            "NEEV_VERIFY_STAGE_BLOB_READ",
            &["one.png", "two.txt", "attachment acceptance notes"],
        ),
        ContentError::NotFound
    );
    std::fs::write(&lost, &png).unwrap();
    assert!(verify_snapshot(&h.state.database, &h.state.blobs)
        .await
        .is_ok());

    // A file the archive copied SHORT. It reads back fine; only the length and
    // the digest say what happened.
    let truncated = attachment_path(&h, h.creator.org_id, &text.key);
    std::fs::write(&truncated, &notes[..4]).unwrap();
    assert_eq!(
        verify_snapshot(&h.state.database, &h.state.blobs).await,
        Err(ContentError::Storage),
        "a truncated attachment blob must not verify as healthy"
    );
    assert_eq!(
        assert_bounded_failure(
            super::backup::verify_snapshot_staged(&h.state.database, &h.state.blobs)
                .await
                .unwrap_err(),
            "NEEV_VERIFY_STAGE_BLOB_READ",
            &["one.png", "two.txt", "attachment acceptance notes"],
        ),
        ContentError::Storage
    );
    std::fs::write(&truncated, &notes).unwrap();
    assert!(verify_snapshot(&h.state.database, &h.state.blobs)
        .await
        .is_ok());

    // Same bytes, same length, a digest that no longer describes them: the
    // check is against what the row CLAIMS, which is the difference between
    // verifying the files and counting them.
    db::execute(
        &h.state.database,
        "UPDATE neev_attachments SET sha256=$1 WHERE org_id=$2 AND key=$3",
        vec![
            "0".repeat(64).into(),
            h.creator.org_id.into(),
            text.key.clone().into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        verify_snapshot(&h.state.database, &h.state.blobs).await,
        Err(ContentError::Storage),
        "bytes that no longer match the recorded digest must not verify"
    );
    db::execute(
        &h.state.database,
        "UPDATE neev_attachments SET sha256=$1,byte_length=$2 WHERE org_id=$3 AND key=$4",
        vec![
            format!("{:x}", sha2::Sha256::digest(&notes)).into(),
            (notes.len() as i64 - 3).into(),
            h.creator.org_id.into(),
            text.key.clone().into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        verify_snapshot(&h.state.database, &h.state.blobs).await,
        Err(ContentError::Storage),
        "a length that no longer matches the bytes must not verify"
    );
}

/// The paged read covers every tenant, and the keyset predicate excludes
/// nothing at either end.
///
/// The references are read a page at a time, cursored on `(org_id, key)`. This
/// fixture is smaller than one page on purpose — 500 fsynced uploads would buy
/// a slow test rather than a better one — so what it pins down is the predicate
/// itself: the lowest-ordered attachment is not skipped by the opening cursor,
/// and an attachment belonging to a DIFFERENT organization is still reached and
/// still verified. A predicate that only advanced the key, or only matched the
/// cursor's own org, would drop one of these two rows and call the snapshot
/// healthy.
async fn attachment_paging_contract(database: DatabaseConnection) {
    let h = Harness::new(database).await;
    let mine = h.chat().await;
    let theirs = resources::create(
        &h.state,
        &h.foreign,
        ResourceInput {
            node_id: h.foreign_node,
            kind: ResourceKind::Chat,
            title: "Another tenant".into(),
            origin_chat_id: None,
            request_id: request(),
        },
    )
    .await
    .unwrap();
    let png = attachment_png();
    let first = super::attachment::store(
        &h.state,
        &h.creator,
        super::attachment::Owner::Chat(&mine.id),
        "mine.png",
        &png,
    )
    .await
    .unwrap();
    let second = super::attachment::store(
        &h.state,
        &h.foreign,
        super::attachment::Owner::Chat(&theirs.id),
        "theirs.png",
        &png,
    )
    .await
    .unwrap();
    let manifest = verify_snapshot(&h.state.database, &h.state.blobs)
        .await
        .unwrap();
    assert!(reference_for(&manifest, &first.key).is_some());
    assert!(reference_for(&manifest, &second.key).is_some());
    assert_ne!(h.creator.org_id, h.foreign.org_id);

    // Losing the SECOND tenant's blob must still fail: it is only reachable
    // once the cursor has crossed the organization boundary.
    std::fs::remove_file(attachment_path(&h, h.foreign.org_id, &second.key)).unwrap();
    assert_eq!(
        verify_snapshot(&h.state.database, &h.state.blobs).await,
        Err(ContentError::NotFound)
    );
}

#[tokio::test]
async fn sqlite_snapshot_verifies_attachment_bytes_not_just_rows() {
    attachment_snapshot_contract(h::database().await).await;
}

#[tokio::test]
async fn sqlite_snapshot_walks_every_tenants_attachments() {
    attachment_paging_contract(h::database().await).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_snapshot_verifies_attachment_bytes_not_just_rows() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("attachment snapshot owned schema: {schema}");
    attachment_snapshot_contract(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
    let (db, admin, schema) = h::postgres_database().await;
    attachment_paging_contract(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
}
