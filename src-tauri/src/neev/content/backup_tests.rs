use super::{
    backup::verify_snapshot,
    documents, resources,
    test_support::{request, Harness},
    types::*,
};
use crate::neev::{db, test_support as h};
use futures::FutureExt;
use sea_orm::{ConnectionTrait, DatabaseConnection};

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
    assert_eq!(manifest.schema_version, 4);
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
    for version in [1_i64, 2, 5] {
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

// A same-name empty view must not hide missing revision storage as zero references.
async fn revision_view_is_refused(database: DatabaseConnection) {
    let cfg = h::preview_config();
    let store = super::BlobStore::open(&cfg.document_root).unwrap();
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
    h::cleanup_postgres(db, admin, schema).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
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
}
