use super::{
    backup::verify_snapshot,
    documents, resources,
    test_support::{request, Harness},
    types::*,
};
use crate::neev::{db, test_support as h};
use futures::FutureExt;
use sea_orm::DatabaseConnection;

// Catches omission of historical revisions, trusting corrupt bytes, and cross-org lookup.
async fn restored_bytes(database: DatabaseConnection) {
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
    let manifest = verify_snapshot(&h.state.database, &h.state.blobs)
        .await
        .unwrap();
    assert_eq!(manifest.schema_version, 3);
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
    assert!(verify_snapshot(&h.state.database, &h.state.blobs)
        .await
        .is_err());
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
    restored_bytes(h::database().await).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_restored_bytes() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("backup owned schema: {schema}");
    let outcome = std::panic::AssertUnwindSafe(restored_bytes(db.clone()))
        .catch_unwind()
        .await;
    h::cleanup_postgres(db, admin, schema).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
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
    assert_eq!(verify_snapshot(&db, &store).await.unwrap().blobs.len(), 0);
    db::execute(&db, "PRAGMA query_only=OFF", vec![])
        .await
        .unwrap();
    for version in [1_i64, 2, 4] {
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
}
