use super::{types::ContentError, BlobStore};
use crate::neev::db;
use sea_orm::{
    AccessMode, ConnectionTrait, DatabaseConnection, DbBackend, IsolationLevel, TransactionTrait,
};

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Manifest {
    pub(crate) schema_version: i64,
    pub(crate) blobs: Vec<Reference>,
}
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Reference {
    pub(crate) org_id: i32,
    pub(crate) key: String,
    pub(crate) sha256: String,
    pub(crate) byte_length: i64,
}

pub(crate) async fn verify_snapshot(
    database: &DatabaseConnection,
    store: &BlobStore,
) -> Result<Manifest, ContentError> {
    // Never use db::begin (SQLite write intent), initialize, or application startup.
    let tx = if database.get_database_backend() == DbBackend::Postgres {
        database
            .begin_with_config(
                Some(IsolationLevel::RepeatableRead),
                Some(AccessMode::ReadOnly),
            )
            .await
    } else {
        database.begin().await
    }
    .map_err(|_| ContentError::Storage)?;
    db::validate_content_snapshot(&tx).await?;
    let rows = tx.query_all(db::statement(&tx,
        "SELECT org_id,blob_key,sha256,byte_length FROM neev_document_revisions ORDER BY org_id,blob_key", vec![]))
        .await.map_err(|_| ContentError::Storage)?;
    let mut blobs = Vec::with_capacity(rows.len());
    for row in rows {
        blobs.push(Reference {
            org_id: row
                .try_get("", "org_id")
                .map_err(|_| ContentError::Storage)?,
            key: row
                .try_get("", "blob_key")
                .map_err(|_| ContentError::Storage)?,
            sha256: row
                .try_get("", "sha256")
                .map_err(|_| ContentError::Storage)?,
            byte_length: row
                .try_get("", "byte_length")
                .map_err(|_| ContentError::Storage)?,
        });
    }
    tx.rollback().await.map_err(|_| ContentError::Storage)?;
    // Canonical ordering independent of a restored database's collation.
    blobs.sort_by(|a, b| (a.org_id, &a.key).cmp(&(b.org_id, &b.key)));
    for blob in &blobs {
        store
            .read(blob.org_id, &blob.key, blob.byte_length, &blob.sha256)
            .await?;
    }
    Ok(Manifest {
        schema_version: 3,
        blobs,
    })
}

fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_lowercase() || (i > 0 && b.is_ascii_digit()))
}

fn isolated(name: &str) -> bool {
    name.strip_prefix("neev_restore_").is_some_and(|uuid| {
        uuid.len() == 32
            && uuid
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub(super) async fn verify_destination(
    database: &DatabaseConnection,
    expected_database: &str,
    expected_schema: &str,
    confirmed: bool,
) -> Result<(), ContentError> {
    if !confirmed
        || !identifier(expected_database)
        || !identifier(expected_schema)
        || !(isolated(expected_database) || isolated(expected_schema))
        || database.get_database_backend() != DbBackend::Postgres
    {
        return Err(ContentError::Invalid);
    }
    let row = db::one(
        database,
        "SELECT current_database() AS database,current_schema() AS schema",
        vec![],
    )
    .await?
    .ok_or(ContentError::Invalid)?;
    if row
        .try_get::<String>("", "database")
        .map_err(|_| ContentError::Invalid)?
        != expected_database
        || row
            .try_get::<String>("", "schema")
            .map_err(|_| ContentError::Invalid)?
            != expected_schema
    {
        return Err(ContentError::Invalid);
    }
    Ok(())
}

#[derive(Debug)]
pub(super) struct Arguments {
    database: String,
    schema: String,
}

pub(super) fn arguments(args: &[String]) -> Result<Arguments, ContentError> {
    let mut database = None;
    let mut schema = None;
    let mut confirmed = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database" if database.is_none() => database = args.next().cloned(),
            "--schema" if schema.is_none() => schema = args.next().cloned(),
            "--confirm-isolated" if !confirmed => confirmed = true,
            _ => return Err(ContentError::Invalid),
        }
    }
    let database = database.ok_or(ContentError::Invalid)?;
    let schema = schema.ok_or(ContentError::Invalid)?;
    if !confirmed
        || !identifier(&database)
        || !identifier(&schema)
        || !(isolated(&database) || isolated(&schema))
    {
        return Err(ContentError::Invalid);
    }
    Ok(Arguments { database, schema })
}

async fn run(args: Arguments) -> Result<String, ContentError> {
    use sea_orm::{ConnectOptions, Database};
    use sha2::{Digest, Sha256};
    let database_url =
        std::env::var("NEEV_VERIFY_DATABASE_URL").map_err(|_| ContentError::Invalid)?;
    let document_root =
        std::env::var_os("NEEV_VERIFY_DOCUMENT_ROOT").ok_or(ContentError::Invalid)?;
    let parsed = url::Url::parse(&database_url).map_err(|_| ContentError::Invalid)?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql")
        || parsed.fragment().is_some()
        || urlencoding::decode(
            parsed
                .path()
                .strip_prefix('/')
                .ok_or(ContentError::Invalid)?,
        )
        .map_err(|_| ContentError::Invalid)?
            != args.database
    {
        return Err(ContentError::Invalid);
    }
    let store = BlobStore::open(std::path::Path::new(&document_root))?;
    let mut options = ConnectOptions::new(database_url);
    options
        .max_connections(1)
        .sqlx_logging(false)
        .connect_timeout(std::time::Duration::from_secs(15))
        .set_schema_search_path(&args.schema);
    let database = Database::connect(options)
        .await
        .map_err(|_| ContentError::Storage)?;
    let result = async {
        verify_destination(&database, &args.database, &args.schema, true).await?;
        verify_snapshot(&database, &store).await
    }
    .await;
    database.close().await.map_err(|_| ContentError::Storage)?;
    let manifest = result?;
    let encoded = serde_json::to_vec(&manifest).map_err(|_| ContentError::Storage)?;
    Ok(serde_json::json!({
        "schema_version": manifest.schema_version,
        "referenced_blobs": manifest.blobs.len(),
        "referenced_bytes": manifest.blobs.iter().map(|blob| blob.byte_length as u64).sum::<u64>(),
        "manifest_sha256": format!("{:x}", Sha256::digest(&encoded)),
    })
    .to_string())
}

pub(crate) fn run_content_verifier() -> std::process::ExitCode {
    let result = (|| {
        let args = arguments(&std::env::args().skip(1).collect::<Vec<_>>())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| ContentError::Storage)?;
        runtime.block_on(run(args))
    })();
    match result {
        Ok(summary) => {
            println!("{summary}");
            std::process::ExitCode::SUCCESS
        }
        Err(_) => {
            // Never print connection, SQL, path, or content diagnostics.
            eprintln!("Native content verification failed. Require --database NAME --schema NAME --confirm-isolated and dedicated verifier environment; inspect the isolated restore privately.");
            std::process::ExitCode::FAILURE
        }
    }
}
