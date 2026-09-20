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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neev::test_support as h;
    use sea_orm::DatabaseConnection;

    const DOC: &str = "11111111-1111-4111-8111-111111111111";
    const KEY: &str = "22222222-2222-4222-8222-222222222222";
    fn hex64() -> String {
        "a".repeat(64)
    }

    // Fresh v10 DB with one org and one private document images can bind to.
    async fn fixture() -> (DatabaseConnection, i32, i32) {
        let db = h::database().await;
        let owner = h::native_account(&db, "img-owner", "img@example.test").await;
        let (org, _) = h::organization(&db, owner, "admin").await;
        h::execute(
            &db,
            "INSERT INTO neev_nodes(id,org_id,parent_id,kind,name,revision,created_by) VALUES(1,$1,NULL,'project','P',1,$2)",
            vec![org.into(), owner.into()],
        )
        .await;
        h::execute(
            &db,
            "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,'document',1,$3,'D','private',NULL,NULL,1,0,false,1000,1000)",
            vec![org.into(), DOC.into(), owner.into()],
        )
        .await;
        (db, org, owner)
    }

    // Replace the real table with a same-typed replica carrying no CHECK/PK/FK —
    // exactly the imported-dump / hand-edited state the validator exists for.
    async fn unconstrained(db: &DatabaseConnection) {
        h::execute(db, "DROP TABLE neev_image_blobs", vec![]).await;
        h::execute(
            db,
            "CREATE TABLE neev_image_blobs (org_id INTEGER, key TEXT, document_id TEXT, content_type TEXT, byte_length BIGINT, sha256 TEXT, created_by INTEGER, created_at BIGINT)",
            vec![],
        )
        .await;
    }

    #[tokio::test]
    async fn fresh_and_empty_table_validate() {
        let (db, _, _) = fixture().await;
        assert_eq!(h::count(&db, "neev_image_blobs").await, 0);
        assert_eq!(validate(&db).await, Ok(()));
    }

    #[tokio::test]
    async fn a_well_formed_bound_image_validates() {
        let (db, org, owner) = fixture().await;
        h::execute(
            &db,
            "INSERT INTO neev_image_blobs(org_id,key,document_id,content_type,byte_length,sha256,created_by,created_at) VALUES($1,$2,$3,'image/png',10,$4,$5,1000)",
            vec![org.into(), KEY.into(), DOC.into(), hex64().into(), owner.into()],
        )
        .await;
        assert_eq!(validate(&db).await, Ok(()));
    }

    #[tokio::test]
    async fn corrupt_rows_refuse_the_database() {
        // (content_type, byte_length, sha256, key, document_id, created_by) — each
        // case is valid except one poisoned column (or a dangling reference).
        let cases: &[(&str, i64, String, &str, &str, i64)] = &[
            ("image/svg+xml", 10, hex64(), KEY, DOC, 1), // disallowed type
            ("image/png", 0, hex64(), KEY, DOC, 1),      // zero length
            ("image/png", MAX_IMAGE_BYTES + 1, hex64(), KEY, DOC, 1), // too big
            ("image/png", 10, "z".repeat(64), KEY, DOC, 1), // non-hex sha
            ("image/png", 10, hex64(), "not-a-uuid", DOC, 1), // bad key
            (
                "image/png",
                10,
                hex64(),
                KEY,
                "33333333-3333-4333-8333-333333333333",
                1,
            ), // dangling document_id
            ("image/png", 10, hex64(), KEY, DOC, 999999), // dangling created_by
        ];
        for (content_type, byte_length, sha, key, doc, by) in cases {
            let (db, org, owner) = fixture().await;
            unconstrained(&db).await;
            let created_by = if *by == 1 { owner } else { *by as i32 };
            h::execute(
                &db,
                "INSERT INTO neev_image_blobs(org_id,key,document_id,content_type,byte_length,sha256,created_by,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,1000)",
                vec![
                    org.into(),
                    (*key).into(),
                    (*doc).into(),
                    (*content_type).into(),
                    (*byte_length).into(),
                    sha.clone().into(),
                    created_by.into(),
                ],
            )
            .await;
            assert_eq!(
                validate(&db).await,
                Err(AuthError::Unavailable),
                "content_type={content_type} len={byte_length} key={key} doc={doc} by={by}"
            );
        }
    }

    #[tokio::test]
    async fn duplicate_primary_key_is_refused() {
        let (db, org, owner) = fixture().await;
        unconstrained(&db).await;
        for _ in 0..2 {
            h::execute(
                &db,
                "INSERT INTO neev_image_blobs(org_id,key,document_id,content_type,byte_length,sha256,created_by,created_at) VALUES($1,$2,$3,'image/png',10,$4,$5,1000)",
                vec![org.into(), KEY.into(), DOC.into(), hex64().into(), owner.into()],
            )
            .await;
        }
        assert_eq!(validate(&db).await, Err(AuthError::Unavailable));
    }
}
