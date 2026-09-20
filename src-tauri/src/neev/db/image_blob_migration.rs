//! Additive per-document image metadata (schema10). Every uploaded image is bound
//! to the document it belongs to so the serve path can enforce that document's
//! CURRENT ACL on every read — a same-org member who is not authorized for a
//! private document must not read its images even if they guess the key. Additive
//! only: never edits the v2..v9 baseline.
//!
//! `validate` re-proves the declared shape against what is actually stored (an
//! imported dump / hand-edited row / partial restore carries no CHECK/UNIQUE/FK
//! history): storage class, column domain, primary-key uniqueness, and referential
//! integrity to the owning resource and account. Any violation refuses the whole
//! database with `AuthError::Unavailable` rather than serving a half-trusted blob.
use super::{database_error, one, statement, AuthError};
use sea_orm::sea_query::{
    Alias, ColumnDef, Expr, ForeignKey, ForeignKeyAction, Index, Table, TableCreateStatement,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, QueryResult, Value};

pub(super) const SCHEMA_VERSION: i64 = 10;
pub(super) const TABLE_NAMES: &[&str] = &["neev_image_blobs"];

// Native/bigint/time domain bounds, identical to the gateway8/org9 validators.
const NATIVE_MAX: i64 = 2_147_483_647;
const TIME_MAX: i64 = 253_402_300_799;
// Mirrors the storage cap in content::blob (MAX_IMAGE_BYTES = 5 MiB).
const MAX_IMAGE_BYTES: i64 = 5 * 1024 * 1024;
const UUID_CHARS: usize = 36;
const HASH_CHARS: usize = 64;
const PAGE: usize = 500;
// The only content types the upload sniffts and the serve path re-classifies.
const CONTENT_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

fn tables() -> Vec<TableCreateStatement> {
    let mut table = Table::create();
    table.table(Alias::new("neev_image_blobs"));
    for (name, kind) in [
        ("org_id", "i"),
        ("key", "t"),
        ("document_id", "t"),
        ("content_type", "t"),
        ("byte_length", "l"),
        ("sha256", "t"),
        ("created_by", "i"),
        ("created_at", "l"),
    ] {
        let mut column = ColumnDef::new(Alias::new(name));
        match kind {
            "i" => {
                column.integer();
            }
            "l" => {
                column.big_integer();
            }
            _ => {
                column.text();
            }
        }
        column.not_null();
        table.col(&mut column);
    }
    let mut primary = Index::create();
    primary.col(Alias::new("org_id")).col(Alias::new("key"));
    table.primary_key(&mut primary);
    let types = CONTENT_TYPES
        .iter()
        .map(|value| format!("'{value}'"))
        .collect::<Vec<_>>()
        .join(",");
    for check in [
        "org_id>0".to_owned(),
        format!("length(key)={UUID_CHARS}"),
        format!("length(document_id)={UUID_CHARS}"),
        format!("content_type IN ({types})"),
        format!("byte_length BETWEEN 1 AND {MAX_IMAGE_BYTES}"),
        format!("length(sha256)={HASH_CHARS}"),
        "created_by>0".to_owned(),
        "created_at>=0".to_owned(),
    ] {
        table.check(Expr::cust(&check));
    }
    // The image inherits the owning document's audience; the composite FK keeps it
    // in the same tenant and removes the row when the document is hard-deleted.
    let mut fk_document = ForeignKey::create();
    fk_document
        .from_tbl(Alias::new("neev_image_blobs"))
        .from_col(Alias::new("org_id"))
        .from_col(Alias::new("document_id"))
        .to_tbl(Alias::new("neev_resources"))
        .to_col(Alias::new("org_id"))
        .to_col(Alias::new("id"));
    table.foreign_key(fk_document.on_delete(ForeignKeyAction::Cascade));
    let mut fk_by = ForeignKey::create();
    fk_by
        .from_tbl(Alias::new("neev_image_blobs"))
        .from_col(Alias::new("created_by"))
        .to_tbl(Alias::new("neev_accounts"))
        .to_col(Alias::new("id"));
    table.foreign_key(fk_by.on_delete(ForeignKeyAction::Restrict));
    vec![table]
}

pub(super) async fn upgrade(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    let backend = tx.get_database_backend();
    for table in tables() {
        tx.execute(backend.build(&table))
            .await
            .map_err(database_error)?;
    }
    // Same discipline as gateway8/org9: publish nothing that fails its validator.
    validate(tx).await
}

// ---- Shape predicates (duplicated deliberately; this module owns its own copies
// so the reviewer-owned org9 validator and this one never share mutable state). ----

fn canonical_hash(value: &str) -> bool {
    value.len() == HASH_CHARS
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|parsed| parsed.hyphenated().to_string() == value)
}

async fn fetch_page<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    args: Vec<Value>,
) -> Result<Vec<QueryResult>, AuthError> {
    let rows = db
        .query_all(statement(db, sql, args))
        .await
        .map_err(database_error)?;
    if rows.len() > PAGE {
        return Err(AuthError::Unavailable);
    }
    Ok(rows)
}

async fn any<C: ConnectionTrait>(db: &C, sql: &str) -> Result<bool, AuthError> {
    Ok(one(db, sql, vec![]).await?.is_some())
}

fn type_cols(sqlite: bool, cols: &[&str]) -> String {
    cols.iter()
        .map(|col| {
            if sqlite {
                format!("{col},typeof({col}) AS {col}__t")
            } else {
                (*col).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn storage_class(row: &QueryResult, col: &str) -> Result<String, AuthError> {
    row.try_get::<String>("", &format!("{col}__t"))
        .map_err(database_error)
}

fn typed_i64(row: &QueryResult, col: &str, sqlite: bool) -> Result<i64, AuthError> {
    if sqlite && storage_class(row, col)? != "integer" {
        return Err(AuthError::Unavailable);
    }
    row.try_get::<i64>("", col).map_err(database_error)
}

fn typed_str(row: &QueryResult, col: &str, sqlite: bool) -> Result<String, AuthError> {
    if sqlite && storage_class(row, col)? != "text" {
        return Err(AuthError::Unavailable);
    }
    row.try_get::<String>("", col).map_err(database_error)
}

fn typed_native_id(row: &QueryResult, col: &str, sqlite: bool) -> Result<i64, AuthError> {
    if sqlite {
        typed_i64(row, col, true)
    } else {
        row.try_get::<i32>("", col)
            .map(i64::from)
            .map_err(database_error)
    }
}

// ---- Validator ----

/// Re-prove the schema10 contract against stored rows. Any violation refuses the
/// database; nothing here mutates a row.
pub(super) async fn validate<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    let sqlite = match db.get_database_backend() {
        DbBackend::Sqlite => true,
        DbBackend::Postgres => false,
        _ => return Err(AuthError::Unavailable),
    };
    validate_rows(db, sqlite).await?;
    // Independent primary-key uniqueness (do not trust a possibly-dropped PK index).
    if any(
        db,
        "SELECT 1 FROM neev_image_blobs GROUP BY org_id,key HAVING COUNT(*)>1 LIMIT 1",
    )
    .await?
    {
        return Err(AuthError::Unavailable);
    }
    // Referential integrity: every image is bound to a live resource in its own
    // tenant, and to a real author account.
    if any(
        db,
        "SELECT 1 FROM neev_image_blobs i LEFT JOIN neev_resources r ON r.org_id=i.org_id AND r.id=i.document_id WHERE r.id IS NULL LIMIT 1",
    )
    .await?
        || any(
            db,
            "SELECT 1 FROM neev_image_blobs i LEFT JOIN neev_accounts a ON a.id=i.created_by WHERE a.id IS NULL LIMIT 1",
        )
        .await?
    {
        return Err(AuthError::Unavailable);
    }
    Ok(())
}

async fn validate_rows<C: ConnectionTrait>(db: &C, sqlite: bool) -> Result<(), AuthError> {
    let cols = type_cols(
        sqlite,
        &[
            "org_id",
            "key",
            "document_id",
            "content_type",
            "byte_length",
            "sha256",
            "created_by",
            "created_at",
        ],
    );
    let mut after = String::new();
    loop {
        let sql = format!(
            "SELECT {cols} FROM neev_image_blobs WHERE key>$1 ORDER BY key LIMIT {PAGE}"
        );
        let rows = fetch_page(db, &sql, vec![after.clone().into()]).await?;
        let Some(last) = rows.last() else { break };
        after = typed_str(last, "key", sqlite)?;
        for row in &rows {
            let org_id = typed_native_id(row, "org_id", sqlite)?;
            let key = typed_str(row, "key", sqlite)?;
            let document_id = typed_str(row, "document_id", sqlite)?;
            let content_type = typed_str(row, "content_type", sqlite)?;
            let byte_length = typed_i64(row, "byte_length", sqlite)?;
            let sha256 = typed_str(row, "sha256", sqlite)?;
            let created_by = typed_native_id(row, "created_by", sqlite)?;
            let created_at = typed_i64(row, "created_at", sqlite)?;
            if !(1..=NATIVE_MAX).contains(&org_id)
                || !canonical_uuid(&key)
                || !canonical_uuid(&document_id)
                || !CONTENT_TYPES.contains(&content_type.as_str())
                || !(1..=MAX_IMAGE_BYTES).contains(&byte_length)
                || !canonical_hash(&sha256)
                || !(1..=NATIVE_MAX).contains(&created_by)
                || !(0..=TIME_MAX).contains(&created_at)
            {
                return Err(AuthError::Unavailable);
            }
        }
        if rows.len() < PAGE {
            break;
        }
    }
    Ok(())
}
