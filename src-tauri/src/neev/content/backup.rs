use super::{rich, types::ContentError, BlobStore};
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

const FAILURE_GUIDANCE: &str = "Native content verification failed. Require --database NAME --schema NAME --confirm-isolated and dedicated verifier environment; inspect the isolated restore privately.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VerificationStage {
    Arguments,
    Environment,
    Reexec,
    Runtime,
    DocumentRoot,
    DatabaseOptions,
    DatabaseConnect,
    Destination,
    SnapshotSchema,
    ReferenceQuery,
    ReferenceExtraction,
    TransactionRollback,
    BlobRead,
    RichDecode,
    DatabaseClose,
    ManifestEncoding,
}

impl VerificationStage {
    fn code(self) -> &'static str {
        match self {
            Self::Arguments => "NEEV_VERIFY_STAGE_ARGUMENTS",
            Self::Environment => "NEEV_VERIFY_STAGE_ENVIRONMENT",
            Self::Reexec => "NEEV_VERIFY_STAGE_REEXEC",
            Self::Runtime => "NEEV_VERIFY_STAGE_RUNTIME",
            Self::DocumentRoot => "NEEV_VERIFY_STAGE_DOCUMENT_ROOT",
            Self::DatabaseOptions => "NEEV_VERIFY_STAGE_DATABASE_OPTIONS",
            Self::DatabaseConnect => "NEEV_VERIFY_STAGE_DATABASE_CONNECT",
            Self::Destination => "NEEV_VERIFY_STAGE_DESTINATION",
            Self::SnapshotSchema => "NEEV_VERIFY_STAGE_SNAPSHOT_SCHEMA",
            Self::ReferenceQuery => "NEEV_VERIFY_STAGE_REFERENCE_QUERY",
            Self::ReferenceExtraction => "NEEV_VERIFY_STAGE_REFERENCE_EXTRACTION",
            Self::TransactionRollback => "NEEV_VERIFY_STAGE_TRANSACTION_ROLLBACK",
            Self::BlobRead => "NEEV_VERIFY_STAGE_BLOB_READ",
            Self::RichDecode => "NEEV_VERIFY_STAGE_RICH_DECODE",
            Self::DatabaseClose => "NEEV_VERIFY_STAGE_DATABASE_CLOSE",
            Self::ManifestEncoding => "NEEV_VERIFY_STAGE_MANIFEST_ENCODING",
        }
    }
}

#[derive(Debug)]
pub(super) struct VerifierFailure {
    stage: VerificationStage,
    error: ContentError,
}

impl VerifierFailure {
    fn new(stage: VerificationStage, error: ContentError) -> Self {
        Self { stage, error }
    }

    pub(super) fn stage_code(&self) -> &'static str {
        self.stage.code()
    }

    pub(super) fn diagnostic(&self) -> String {
        format!("{FAILURE_GUIDANCE} [{}]", self.stage.code())
    }

    pub(super) fn into_content_error(self) -> ContentError {
        self.error
    }
}

fn at_stage<T>(
    result: Result<T, ContentError>,
    stage: VerificationStage,
) -> Result<T, VerifierFailure> {
    result.map_err(|error| VerifierFailure::new(stage, error))
}

pub(crate) async fn verify_snapshot(
    database: &DatabaseConnection,
    store: &BlobStore,
) -> Result<Manifest, ContentError> {
    verify_snapshot_staged(database, store)
        .await
        .map_err(VerifierFailure::into_content_error)
}

pub(super) async fn verify_snapshot_staged(
    database: &DatabaseConnection,
    store: &BlobStore,
) -> Result<Manifest, VerifierFailure> {
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
    .map_err(|_| ContentError::Storage);
    let tx = at_stage(tx, VerificationStage::SnapshotSchema)?;
    // Return the archive's verified historical marker unchanged.
    let schema_version = at_stage(
        db::validate_content_snapshot(&tx)
            .await
            .map_err(ContentError::from),
        VerificationStage::SnapshotSchema,
    )?;
    let sql = if schema_version == 6 {
        "SELECT r.org_id,r.blob_key,r.sha256,r.byte_length,f.format FROM neev_document_revisions r JOIN neev_document_formats f ON f.org_id=r.org_id AND f.document_id=r.document_id AND f.revision=r.revision ORDER BY r.org_id,r.blob_key"
    } else {
        "SELECT org_id,blob_key,sha256,byte_length,'markdown' AS format FROM neev_document_revisions ORDER BY org_id,blob_key"
    };
    let rows = tx
        .query_all(db::statement(&tx, sql, vec![]))
        .await
        .map_err(|_| ContentError::Storage);
    let rows = at_stage(rows, VerificationStage::ReferenceQuery)?;
    let mut blobs = Vec::with_capacity(rows.len());
    let mut rich_blobs = std::collections::HashSet::new();
    for row in rows {
        let reference = Reference {
            org_id: row
                .try_get("", "org_id")
                .map_err(|_| ContentError::Storage)
                .map_err(|error| {
                    VerifierFailure::new(VerificationStage::ReferenceExtraction, error)
                })?,
            key: row
                .try_get("", "blob_key")
                .map_err(|_| ContentError::Storage)
                .map_err(|error| {
                    VerifierFailure::new(VerificationStage::ReferenceExtraction, error)
                })?,
            sha256: row
                .try_get("", "sha256")
                .map_err(|_| ContentError::Storage)
                .map_err(|error| {
                    VerifierFailure::new(VerificationStage::ReferenceExtraction, error)
                })?,
            byte_length: row
                .try_get("", "byte_length")
                .map_err(|_| ContentError::Storage)
                .map_err(|error| {
                    VerifierFailure::new(VerificationStage::ReferenceExtraction, error)
                })?,
        };
        if row
            .try_get::<String>("", "format")
            .map_err(|_| ContentError::Storage)
            .map_err(|error| {
                VerifierFailure::new(VerificationStage::ReferenceExtraction, error)
            })?
            == "blocks"
        {
            rich_blobs.insert((reference.org_id, reference.key.clone()));
        }
        blobs.push(reference);
    }
    at_stage(
        tx.rollback().await.map_err(|_| ContentError::Storage),
        VerificationStage::TransactionRollback,
    )?;
    // Canonical ordering independent of a restored database's collation.
    blobs.sort_by(|a, b| (a.org_id, &a.key).cmp(&(b.org_id, &b.key)));
    for blob in &blobs {
        let bytes = at_stage(
            store
                .read(blob.org_id, &blob.key, blob.byte_length, &blob.sha256)
                .await,
            VerificationStage::BlobRead,
        )?;
        if rich_blobs.contains(&(blob.org_id, blob.key.clone())) {
            at_stage(
                rich::decode(&bytes).map_err(|_| ContentError::Storage),
                VerificationStage::RichDecode,
            )?;
        }
    }
    Ok(Manifest {
        schema_version,
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

pub(super) fn staged_arguments(args: &[String]) -> Result<Arguments, VerifierFailure> {
    at_stage(arguments(args), VerificationStage::Arguments)
}

pub(super) fn isolated_command(
    executable: &std::path::Path,
    args: &[String],
    database_url: &std::ffi::OsStr,
    document_root: &std::ffi::OsStr,
) -> std::process::Command {
    let mut command = std::process::Command::new(executable);
    command
        .env_clear()
        .args(args)
        .env("NEEV_VERIFY_DATABASE_URL", database_url)
        .env("NEEV_VERIFY_DOCUMENT_ROOT", document_root)
        .env("NEEV_VERIFY_ISOLATED", "1");
    command
}

fn isolated_environment() -> Result<(), ContentError> {
    if std::env::var("NEEV_VERIFY_ISOLATED").as_deref() != Ok("1")
        || std::env::vars_os().any(|(name, _)| {
            // CoreFoundation adds this after exec on macOS. Never forward or
            // use its value; no PostgreSQL or general environment exception.
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
    {
        return Err(ContentError::Invalid);
    }
    Ok(())
}

pub(super) fn staged_isolated_environment() -> Result<(), VerifierFailure> {
    at_stage(isolated_environment(), VerificationStage::Environment)
}

pub(super) fn controlled_options(
    raw: &str,
    schema: &str,
) -> Result<sea_orm::sqlx::postgres::PgConnectOptions, ContentError> {
    use sea_orm::sqlx::{
        postgres::{PgConnectOptions, PgSslMode},
        ConnectOptions,
    };
    use std::collections::BTreeMap;
    // new_without_pgpass still reads PG*; this contract must precede its call.
    isolated_environment()?;
    if !identifier(schema) || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(ContentError::Invalid);
    }
    let (scheme, rest) = raw.split_once("://").ok_or(ContentError::Invalid)?;
    if !matches!(scheme, "postgres" | "postgresql") {
        return Err(ContentError::Invalid);
    }
    let (_, path) = rest.split_once('/').ok_or(ContentError::Invalid)?;
    let raw_database = path.split(['?', '#']).next().ok_or(ContentError::Invalid)?;
    let database = urlencoding::decode(raw_database).map_err(|_| ContentError::Invalid)?;
    if !identifier(&database) || raw_database.contains('/') {
        return Err(ContentError::Invalid);
    }
    let parsed = url::Url::parse(raw).map_err(|_| ContentError::Invalid)?;
    if parsed.fragment().is_some()
        || urlencoding::decode(parsed.path().trim_start_matches('/'))
            .map_err(|_| ContentError::Invalid)?
            != database
    {
        return Err(ContentError::Invalid);
    }
    let mut values = BTreeMap::<String, String>::new();
    fn insert(
        values: &mut BTreeMap<String, String>,
        name: &str,
        value: String,
    ) -> Result<(), ContentError> {
        if (value.is_empty() && name != "password")
            || value.contains('\0')
            || values.insert(name.into(), value).is_some()
        {
            return Err(ContentError::Invalid);
        }
        Ok(())
    }
    if let Some(host) = parsed.host_str().filter(|host| !host.is_empty()) {
        insert(
            &mut values,
            "host",
            urlencoding::decode(host.trim_start_matches('[').trim_end_matches(']'))
                .map_err(|_| ContentError::Invalid)?
                .into_owned(),
        )?;
    }
    if let Some(port) = parsed.port() {
        insert(&mut values, "port", port.to_string())?;
    }
    if !parsed.username().is_empty() {
        insert(
            &mut values,
            "user",
            urlencoding::decode(parsed.username())
                .map_err(|_| ContentError::Invalid)?
                .into_owned(),
        )?;
    }
    if let Some(password) = parsed.password() {
        insert(
            &mut values,
            "password",
            urlencoding::decode(password)
                .map_err(|_| ContentError::Invalid)?
                .into_owned(),
        )?;
    }
    for pair in parsed
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|part| !part.is_empty())
    {
        let (key, value) = pair.split_once('=').ok_or(ContentError::Invalid)?;
        let key = urlencoding::decode(key).map_err(|_| ContentError::Invalid)?;
        if !matches!(
            key.as_ref(),
            "host"
                | "port"
                | "user"
                | "password"
                | "sslmode"
                | "sslrootcert"
                | "sslcert"
                | "sslkey"
        ) {
            return Err(ContentError::Invalid);
        }
        insert(
            &mut values,
            &key,
            urlencoding::decode(value)
                .map_err(|_| ContentError::Invalid)?
                .into_owned(),
        )?;
    }
    let host = values.get("host").ok_or(ContentError::Invalid)?;
    let user = values.get("user").ok_or(ContentError::Invalid)?;
    let port: u16 = values
        .get("port")
        .map(|value| value.parse().map_err(|_| ContentError::Invalid))
        .transpose()?
        .unwrap_or(5432);
    if port == 0
        || host.contains(',')
        || values.contains_key("sslcert") != values.contains_key("sslkey")
    {
        return Err(ContentError::Invalid);
    }
    let ssl = values
        .get("sslmode")
        .map(|value| {
            value
                .parse::<PgSslMode>()
                .map_err(|_| ContentError::Invalid)
        })
        .transpose()?
        .unwrap_or(PgSslMode::Prefer);
    // SQLx 0.8 treats Allow as Disable (its fallback is unimplemented).
    if matches!(ssl, PgSslMode::Allow) {
        return Err(ContentError::Invalid);
    }
    // Public constructor, public setters, no URL parser and no apply_pgpass path.
    let mut options = PgConnectOptions::new_without_pgpass()
        .host(host)
        .port(port)
        .username(user)
        .database(&database)
        .ssl_mode(ssl)
        .application_name("neev-content-verify")
        .options([("search_path", schema)])
        .disable_statement_logging();
    if host.starts_with('/') {
        options = options.socket(host);
    }
    if let Some(password) = values.get("password") {
        options = options.password(password);
    }
    if let Some(path) = values.get("sslrootcert") {
        options = options.ssl_root_cert(path);
    }
    if let Some(path) = values.get("sslcert") {
        options = options.ssl_client_cert(path);
    }
    if let Some(path) = values.get("sslkey") {
        options = options.ssl_client_key(path);
    }
    Ok(options)
}

pub(super) fn staged_controlled_options(
    raw: &str,
    schema: &str,
) -> Result<sea_orm::sqlx::postgres::PgConnectOptions, VerifierFailure> {
    at_stage(
        controlled_options(raw, schema),
        VerificationStage::DatabaseOptions,
    )
}

pub(super) fn staged_document_root(
    root: &std::path::Path,
) -> Result<BlobStore, VerifierFailure> {
    at_stage(BlobStore::open(root), VerificationStage::DocumentRoot)
}

async fn run(args: Arguments) -> Result<String, VerifierFailure> {
    use sha2::{Digest, Sha256};
    staged_isolated_environment()?;
    let database_url = at_stage(
        std::env::var("NEEV_VERIFY_DATABASE_URL").map_err(|_| ContentError::Invalid),
        VerificationStage::Environment,
    )?;
    let document_root = at_stage(
        std::env::var_os("NEEV_VERIFY_DOCUMENT_ROOT").ok_or(ContentError::Invalid),
        VerificationStage::Environment,
    )?;
    let options = staged_controlled_options(&database_url, &args.schema)?;
    if options.get_database() != Some(args.database.as_str()) {
        return Err(VerifierFailure::new(
            VerificationStage::DatabaseOptions,
            ContentError::Invalid,
        ));
    }
    let store = staged_document_root(std::path::Path::new(&document_root))?;
    let pool = sea_orm::sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(15))
        .connect_with(options)
        .await
        .map_err(|_| ContentError::Storage);
    let pool = at_stage(pool, VerificationStage::DatabaseConnect)?;
    let database = sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(pool);
    let result = async {
        at_stage(
            verify_destination(&database, &args.database, &args.schema, true).await,
            VerificationStage::Destination,
        )?;
        verify_snapshot_staged(&database, &store).await
    }
    .await;
    at_stage(
        database.close().await.map_err(|_| ContentError::Storage),
        VerificationStage::DatabaseClose,
    )?;
    let manifest = result?;
    let encoded = at_stage(
        serde_json::to_vec(&manifest).map_err(|_| ContentError::Storage),
        VerificationStage::ManifestEncoding,
    )?;
    Ok(serde_json::json!({
        "schema_version": manifest.schema_version,
        "referenced_blobs": manifest.blobs.len(),
        "referenced_bytes": manifest.blobs.iter().map(|blob| blob.byte_length as u64).sum::<u64>(),
        "manifest_sha256": format!("{:x}", Sha256::digest(&encoded)),
    })
    .to_string())
}

pub(crate) fn run_content_verifier() -> std::process::ExitCode {
    let result: Result<String, VerifierFailure> = (|| {
        let raw_args = std::env::args().skip(1).collect::<Vec<_>>();
        let args = staged_arguments(&raw_args)?;
        if std::env::var("NEEV_VERIFY_ISOLATED").as_deref() != Ok("1") {
            let url = at_stage(
                std::env::var_os("NEEV_VERIFY_DATABASE_URL").ok_or(ContentError::Invalid),
                VerificationStage::Environment,
            )?;
            let root = at_stage(
                std::env::var_os("NEEV_VERIFY_DOCUMENT_ROOT").ok_or(ContentError::Invalid),
                VerificationStage::Environment,
            )?;
            let executable = at_stage(
                std::env::current_exe().map_err(|_| ContentError::Storage),
                VerificationStage::Reexec,
            )?;
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                // exec replaces this process: no waiting parent or orphan child.
                let _ = isolated_command(&executable, &raw_args, &url, &root).exec();
            }
            return Err(VerifierFailure::new(
                VerificationStage::Reexec,
                ContentError::Storage,
            ));
        }
        staged_isolated_environment()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| ContentError::Storage);
        let runtime = at_stage(runtime, VerificationStage::Runtime)?;
        runtime.block_on(run(args))
    })();
    match result {
        Ok(summary) => {
            println!("{summary}");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            // Never print connection, SQL, path, or content diagnostics.
            eprintln!("{}", error.diagnostic());
            std::process::ExitCode::FAILURE
        }
    }
}
