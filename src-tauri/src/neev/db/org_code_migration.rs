//! Additive per-organization join-code storage (schema9). One reusable,
//! admin-managed code per org: unguessable, server-validated, encrypted at rest
//! for admin re-display, rotate/disable. No runtime beyond CRUD + membership join
//! (see `org/join_code.rs`). Additive only — never edits the v2..v8 baseline.
//!
//! `validate` is the startup/snapshot counterpart every other additive migration
//! ships: it re-derives the declared shape from the persisted rows instead of
//! trusting that the DDL that created them is still in force. An imported dump,
//! a hand-edited row or a partially restored backup carries no CHECK/UNIQUE/FK
//! history, so the same invariants are re-proved here — storage type, column
//! domain, key uniqueness, references and the tenant boundary — and anything
//! that fails refuses the whole database with `AuthError::Unavailable` rather
//! than admitting a half-trusted admission secret.
//!
//! `code_enc` is deliberately validated as a *container shape only*. Without the
//! encryption key any 40-byte blob is indistinguishable from a real AES-256-GCM
//! message, so this module never decrypts and never treats well-formed bytes as
//! authentic; it only rejects shapes `encrypt_code` could not have produced.
use super::{database_error, one, statement, AuthError};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sea_orm::sea_query::{
    Alias, ColumnDef, Expr, ForeignKey, ForeignKeyAction, Index, Table, TableCreateStatement,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, QueryResult, Value};

pub(super) const SCHEMA_VERSION: i64 = 9;
pub(super) const TABLE_NAMES: &[&str] = &["neev_org_join_codes"];

// Shared native domain bounds (identical to the gateway8 validator).
const NATIVE_MAX: i64 = 2_147_483_647;
const BIGINT_MAX: i64 = 9_223_372_036_854_775_807;
const TIME_MAX: i64 = 253_402_300_799;
// Rows are keyset-scanned a page at a time; the whole table is never buffered.
const PAGE: usize = 256;

// `code_enc` container: a 12-byte random nonce prefix followed by the AES-256-GCM
// ciphertext of the normalized code plus its 16-byte tag, encoded base64url
// without padding (`org/join_code.rs::encrypt_code`).
const NONCE: usize = 12;
const TAG: usize = 16;
// The plaintext is the normalized code. `preview`/`join` refuse a normalized code
// shorter than 8 characters, and the alphabet is single-byte ASCII, so a stored
// container outside this range could never admit anyone even if it decrypted.
const MIN_CODE: usize = 8;
const MAX_CODE: usize = 64;
const MIN_ENC_BYTES: usize = NONCE + TAG + MIN_CODE;
const MAX_ENC_BYTES: usize = NONCE + TAG + MAX_CODE;
// Mirrors the declared `length(code_enc) BETWEEN 1 AND 512` check so an imported
// snapshot that lost its CHECK constraints is still held to it.
const ENC_MAX_CHARS: usize = 512;
// Mirrors `length(code_hash)=64` (lowercase hex SHA-256).
const HASH_CHARS: usize = 64;

fn tables() -> Vec<TableCreateStatement> {
    let mut table = Table::create();
    table.table(Alias::new("neev_org_join_codes"));
    for (name, kind) in [
        ("org_id", "i"),
        ("code_hash", "t"),
        ("code_enc", "t"),
        ("disabled", "b"),
        ("created_by", "i"),
        ("created_at", "l"),
        ("revision", "l"),
    ] {
        let mut column = ColumnDef::new(Alias::new(name));
        match kind {
            "i" => {
                column.integer();
            }
            "l" => {
                column.big_integer();
            }
            "b" => {
                column.boolean();
            }
            _ => {
                column.text();
            }
        }
        column.not_null();
        table.col(&mut column);
    }
    let mut primary = Index::create();
    primary.col(Alias::new("org_id"));
    table.primary_key(&mut primary);
    for check in [
        "org_id>0",
        "length(code_hash)=64",
        "length(code_enc) BETWEEN 1 AND 512",
        "created_by>0",
        "created_at>=0",
        "revision>=1",
    ] {
        table.check(Expr::cust(check));
    }
    let mut unique = Index::create();
    unique.unique().col(Alias::new("code_hash"));
    table.index(&mut unique);
    let mut fk_org = ForeignKey::create();
    fk_org
        .from_tbl(Alias::new("neev_org_join_codes"))
        .from_col(Alias::new("org_id"))
        .to_tbl(Alias::new("neev_organizations"))
        .to_col(Alias::new("id"));
    table.foreign_key(fk_org.on_delete(ForeignKeyAction::Restrict));
    let mut fk_by = ForeignKey::create();
    fk_by
        .from_tbl(Alias::new("neev_org_join_codes"))
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
    // Same discipline as gateway8/status7: publish nothing that does not already
    // satisfy its own validator.
    validate(tx).await
}

// ---- Private scalar predicates (shape only; they mint no authority) ----

/// Lowercase hex SHA-256, exactly as `format!("{:x}", Sha256::digest(..))` emits.
/// Uppercase or mixed case would hash-mismatch every lookup, so it is corruption.
fn canonical_hash(value: &str) -> bool {
    value.len() == HASH_CHARS
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Structural check of the AES-256-GCM container. Never decrypts: without the
/// key, authentic and forged bytes are indistinguishable, so this only proves the
/// value is canonical unpadded base64url whose plaintext length could have come
/// from a normalized join code.
fn canonical_container(value: &str) -> bool {
    if value.is_empty() || value.len() > ENC_MAX_CHARS {
        return false;
    }
    URL_SAFE_NO_PAD.decode(value).is_ok_and(|bytes| {
        (MIN_ENC_BYTES..=MAX_ENC_BYTES).contains(&bytes.len())
            // Reject padded, whitespace-carrying or trailing-bit variants: only
            // the exact encoding `encrypt_code` writes round-trips.
            && URL_SAFE_NO_PAD.encode(&bytes) == value
    })
}

// ---- Bounded page fetch (never accumulates the whole table) ----

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

// SQLite reports a storage class per value; PostgreSQL enforces it per column.
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

/// Native id columns are INTEGER: i32 on PostgreSQL, dynamically typed on SQLite.
fn typed_native_id(row: &QueryResult, col: &str, sqlite: bool) -> Result<i64, AuthError> {
    if sqlite {
        typed_i64(row, col, true)
    } else {
        row.try_get::<i32>("", col)
            .map(i64::from)
            .map_err(database_error)
    }
}

/// SQLite has no boolean storage class: the value must be integer 0 or 1, which
/// is the column's entire domain.
fn typed_bool(row: &QueryResult, col: &str, sqlite: bool) -> Result<bool, AuthError> {
    if sqlite {
        return match typed_i64(row, col, true)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(AuthError::Unavailable),
        };
    }
    row.try_get::<bool>("", col).map_err(database_error)
}

// ---- Validator ----

/// Re-prove the schema9 contract against what is actually stored. Any violation
/// refuses the database; nothing here mutates a row.
pub(super) async fn validate<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    let sqlite = match db.get_database_backend() {
        DbBackend::Sqlite => true,
        DbBackend::Postgres => false,
        _ => return Err(AuthError::Unavailable),
    };
    // Phase 1: per-row storage class and column domain, page by page.
    validate_rows(db, sqlite).await?;
    // Phase 2: independent uniqueness of the declared primary and unique keys.
    validate_keys(db).await?;
    // Phase 3: references and the tenant boundary.
    validate_references(db).await?;
    Ok(())
}

async fn validate_rows<C: ConnectionTrait>(db: &C, sqlite: bool) -> Result<(), AuthError> {
    let select = type_cols(
        sqlite,
        &[
            "org_id",
            "code_hash",
            "code_enc",
            "disabled",
            "created_by",
            "created_at",
            "revision",
        ],
    );
    let mut cursor: Option<i32> = None;
    loop {
        let rows = match cursor {
            None => {
                fetch_page(
                    db,
                    &format!(
                        "SELECT {select} FROM neev_org_join_codes ORDER BY org_id LIMIT {PAGE}"
                    ),
                    vec![],
                )
                .await?
            }
            Some(key) => {
                fetch_page(
                    db,
                    &format!("SELECT {select} FROM neev_org_join_codes WHERE org_id > $1 ORDER BY org_id LIMIT {PAGE}"),
                    vec![key.into()],
                )
                .await?
            }
        };
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let org_id = typed_native_id(row, "org_id", sqlite)?;
            let code_hash = typed_str(row, "code_hash", sqlite)?;
            let code_enc = typed_str(row, "code_enc", sqlite)?;
            let created_by = typed_native_id(row, "created_by", sqlite)?;
            let created_at = typed_i64(row, "created_at", sqlite)?;
            let revision = typed_i64(row, "revision", sqlite)?;
            // `disabled` carries no cross-column invariant: the storage class and
            // the 0/1 domain enforced inside `typed_bool` are its whole contract.
            typed_bool(row, "disabled", sqlite)?;
            if !(1..=NATIVE_MAX).contains(&org_id)
                || !(1..=NATIVE_MAX).contains(&created_by)
                || !canonical_hash(&code_hash)
                || !canonical_container(&code_enc)
                || !(0..=TIME_MAX).contains(&created_at)
                || !(1..=BIGINT_MAX).contains(&revision)
            {
                return Err(AuthError::Unavailable);
            }
        }
        // Safe narrowing: every `org_id` on this page was just bounded to i32.
        cursor = Some(typed_native_id(rows.last().unwrap(), "org_id", sqlite)? as i32);
        if rows.len() < PAGE {
            break;
        }
    }
    Ok(())
}

async fn validate_keys<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    for query in [
        // One code per organization (declared primary key).
        "SELECT 1 AS invalid FROM neev_org_join_codes GROUP BY org_id HAVING COUNT(*)<>1 LIMIT 1",
        // A code must resolve to exactly one organization: a duplicated hash would
        // let one pasted code admit a caller to either tenant.
        "SELECT 1 AS invalid FROM neev_org_join_codes GROUP BY code_hash HAVING COUNT(*)<>1 LIMIT 1",
        // Distinct organizations must not share a ciphertext either; identical
        // containers imply a copied row rather than an independent rotation.
        "SELECT 1 AS invalid FROM neev_org_join_codes GROUP BY code_enc HAVING COUNT(*)<>1 LIMIT 1",
    ] {
        if any(db, query).await? {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}

async fn validate_references<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    for query in [
        // Every code belongs to a real organization (declared foreign key).
        "SELECT 1 AS invalid FROM neev_org_join_codes j LEFT JOIN neev_organizations o ON o.id=j.org_id WHERE o.id IS NULL LIMIT 1",
        // ...and was published by a real account (declared foreign key).
        "SELECT 1 AS invalid FROM neev_org_join_codes j LEFT JOIN neev_accounts a ON a.id=j.created_by WHERE a.id IS NULL LIMIT 1",
        // Tenant boundary, which no declared constraint expresses: `rotate` runs
        // only under an org-scoped admin session, so the publisher always held a
        // membership in that same organization. Memberships are deactivated, never
        // deleted, so the row survives removal and demotion — a code attributed to
        // an account that was never in the org is an injected row.
        "SELECT 1 AS invalid FROM neev_org_join_codes j LEFT JOIN neev_memberships m ON m.org_id=j.org_id AND m.user_id=j.created_by WHERE m.id IS NULL LIMIT 1",
    ] {
        if any(db, query).await? {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neev::test_support as h;
    // Explicit rather than relying on the glob re-exporting the parent's `as _`.
    use base64::Engine as _;
    use sea_orm::DatabaseConnection;
    use std::sync::Arc;

    const NOW: i64 = 1_700_000_000;
    // 64 lowercase hex characters.
    fn hash(seed: char) -> String {
        format!("{seed}{}", "0123456789abcdef".repeat(4))[..64].to_owned()
    }
    // A container of exactly the shape `encrypt_code` writes: 12-byte nonce +
    // 12-byte plaintext + 16-byte tag, canonical base64url without padding.
    fn container(seed: u8) -> String {
        URL_SAFE_NO_PAD.encode([seed; NONCE + 12 + TAG])
    }

    /// Fresh v9 DB plus one organization, its owner and a second account that is
    /// deliberately not a member of that organization.
    async fn fixture() -> (DatabaseConnection, i32, i32, i32) {
        let db = h::database().await;
        let owner = h::native_account(&db, "code-owner", "owner@example.test").await;
        let (org, _) = h::organization(&db, owner, "admin").await;
        let outsider = h::native_account(&db, "code-outsider", "outsider@example.test").await;
        (db, org, owner, outsider)
    }

    /// Replace the real table with a same-typed replica carrying no CHECK, no
    /// primary key, no unique index and no foreign keys — an imported dump or a
    /// hand-edited snapshot, i.e. exactly the state the validator exists for.
    async fn unconstrained(db: &DatabaseConnection) {
        h::execute(db, "DROP TABLE neev_org_join_codes", vec![]).await;
        h::execute(
            db,
            "CREATE TABLE neev_org_join_codes (org_id INTEGER, code_hash TEXT, code_enc TEXT, disabled BOOLEAN, created_by INTEGER, created_at BIGINT, revision BIGINT)",
            vec![],
        )
        .await;
    }

    async fn insert(db: &DatabaseConnection, org: i32, by: i32, seed: u8) {
        h::execute(
            db,
            "INSERT INTO neev_org_join_codes(org_id,code_hash,code_enc,disabled,created_by,created_at,revision) VALUES($1,$2,$3,false,$4,$5,1)",
            vec![
                org.into(),
                hash(char::from(b'a' + seed % 6)).into(),
                container(seed).into(),
                by.into(),
                NOW.into(),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn fresh_schema_validates_and_an_empty_table_is_valid() {
        let (db, _, _, _) = fixture().await;
        assert_eq!(h::count(&db, "neev_org_join_codes").await, 0);
        assert_eq!(validate(&db).await, Ok(()));
    }

    /// The strongest anchor: whatever the live service writes must satisfy the
    /// validator, so the container bounds can never drift from `encrypt_code`.
    #[tokio::test]
    async fn a_rotated_code_written_by_the_service_validates() {
        let db = h::database().await;
        let owner = h::native_account(&db, "rotator", "rotator@example.test").await;
        let (org, _) = h::organization(&db, owner, "admin").await;
        let admin = h::scoped_context(&db, owner, org, NOW).await;
        let state = crate::neev::startup::ServerState::new(
            db,
            h::preview_config(),
            Arc::new(h::TestClock::new(NOW)),
        )
        .unwrap();
        let first = crate::neev::org::join_code::rotate(&state, &admin)
            .await
            .unwrap();
        assert_eq!(validate(&state.database).await, Ok(()));
        // Rotation rewrites hash, ciphertext and revision in place.
        let second = crate::neev::org::join_code::rotate(&state, &admin)
            .await
            .unwrap();
        assert_ne!(second.code, first.code);
        assert_eq!(validate(&state.database).await, Ok(()));
        crate::neev::org::join_code::set_enabled(&state, &admin, false)
            .await
            .unwrap();
        assert_eq!(validate(&state.database).await, Ok(()));
    }

    #[tokio::test]
    async fn a_well_formed_row_in_an_unconstrained_table_still_validates() {
        // Control: proves the corruption tests below fail for their stated reason
        // and not because the replica table is itself rejected.
        let (db, org, owner, _) = fixture().await;
        unconstrained(&db).await;
        insert(&db, org, owner, 0).await;
        assert_eq!(validate(&db).await, Ok(()));
    }

    // ---- Corruption the declared schema cannot catch (real constrained table) ----

    #[tokio::test]
    async fn uppercase_or_non_hex_code_hash_is_refused() {
        // Each of these satisfies the declared `length(code_hash)=64` check and is
        // therefore insertable into the real table; only the validator rejects it.
        for bad in [
            "A".repeat(64),
            "0123456789ABCDEF".repeat(4),
            "z".repeat(64),
            " ".repeat(64),
        ] {
            let (db, org, owner, _) = fixture().await;
            h::execute(
                &db,
                "INSERT INTO neev_org_join_codes(org_id,code_hash,code_enc,disabled,created_by,created_at,revision) VALUES($1,$2,$3,false,$4,$5,1)",
                vec![org.into(), bad.clone().into(), container(1).into(), owner.into(), NOW.into()],
            )
            .await;
            assert_eq!(
                validate(&db).await,
                Err(AuthError::Unavailable),
                "code_hash {bad:?} must be refused"
            );
        }
    }

    #[tokio::test]
    async fn a_code_enc_that_is_not_a_canonical_container_is_refused() {
        let canonical = container(3);
        let raw = URL_SAFE_NO_PAD.decode(&canonical).unwrap();
        let padded = base64::engine::general_purpose::URL_SAFE.encode(&raw);
        assert!(padded.ends_with('='));
        // Same payload, standard alphabet: `+`/`/` are not base64url.
        let standard_alphabet = base64::engine::general_purpose::STANDARD_NO_PAD.encode([255u8; 40]);
        // Same payload, non-canonical trailing bits in the final character.
        let mut trailing = canonical.clone();
        let last = trailing.pop().unwrap();
        trailing.push(if last == 'B' { 'C' } else { 'B' });
        // Same payload, trailing whitespace from a sloppy dump.
        let whitespace = format!("{canonical}\n");
        for bad in [
            String::from("not base64 at all!!"),
            padded,
            standard_alphabet,
            trailing,
            whitespace,
            canonical[..canonical.len() - 1].to_owned(), // truncated payload
            URL_SAFE_NO_PAD.encode([0u8; NONCE]), // nonce only: no tag, no ciphertext
            URL_SAFE_NO_PAD.encode([0u8; NONCE + TAG + MIN_CODE - 1]), // one byte short
            URL_SAFE_NO_PAD.encode([0u8; NONCE + TAG + MAX_CODE + 1]), // one byte long
            String::new(),
        ] {
            let (db, org, owner, _) = fixture().await;
            // Unconstrained so the empty case is insertable too (the declared
            // `length(code_enc) BETWEEN 1 AND 512` would otherwise block it, and
            // the validator must enforce that bound independently).
            unconstrained(&db).await;
            h::execute(
                &db,
                "INSERT INTO neev_org_join_codes(org_id,code_hash,code_enc,disabled,created_by,created_at,revision) VALUES($1,$2,$3,false,$4,$5,1)",
                vec![org.into(), hash('a').into(), bad.clone().into(), owner.into(), NOW.into()],
            )
            .await;
            assert_eq!(
                validate(&db).await,
                Err(AuthError::Unavailable),
                "code_enc {bad:?} must be refused"
            );
        }
    }

    #[tokio::test]
    async fn a_code_attributed_to_a_non_member_account_is_refused() {
        // Every declared constraint passes: real org, real account, valid hash and
        // container. Only the tenant boundary rejects it.
        let (db, org, _, outsider) = fixture().await;
        insert(&db, org, outsider, 0).await;
        assert_eq!(validate(&db).await, Err(AuthError::Unavailable));
    }

    #[tokio::test]
    async fn a_code_published_by_a_removed_admin_still_validates() {
        // Removal deactivates the membership; it does not orphan the code the
        // admin published, and re-validating must not turn that into an outage.
        let (db, org, owner, _) = fixture().await;
        insert(&db, org, owner, 0).await;
        h::execute(
            &db,
            "UPDATE neev_memberships SET active=false,role='member' WHERE org_id=$1 AND user_id=$2",
            vec![org.into(), owner.into()],
        )
        .await;
        assert_eq!(validate(&db).await, Ok(()));
    }

    // ---- Corruption a restored snapshot can carry (unconstrained replica) ----

    #[tokio::test]
    async fn two_organizations_sharing_one_code_are_refused() {
        let (db, org, owner, _) = fixture().await;
        let second = h::native_account(&db, "second-owner", "second@example.test").await;
        let (other, _) = h::organization(&db, second, "admin").await;
        unconstrained(&db).await;
        // Same hash on two tenants: one pasted code would resolve to either org.
        for (id, by) in [(org, owner), (other, second)] {
            h::execute(
                &db,
                "INSERT INTO neev_org_join_codes(org_id,code_hash,code_enc,disabled,created_by,created_at,revision) VALUES($1,$2,$3,false,$4,$5,1)",
                vec![id.into(), hash('a').into(), container(id as u8).into(), by.into(), NOW.into()],
            )
            .await;
        }
        assert_eq!(validate(&db).await, Err(AuthError::Unavailable));
    }

    #[tokio::test]
    async fn two_organizations_sharing_one_ciphertext_are_refused() {
        let (db, org, owner, _) = fixture().await;
        let second = h::native_account(&db, "second-owner", "second@example.test").await;
        let (other, _) = h::organization(&db, second, "admin").await;
        unconstrained(&db).await;
        for (id, by, seed) in [(org, owner, 1u8), (other, second, 2u8)] {
            h::execute(
                &db,
                "INSERT INTO neev_org_join_codes(org_id,code_hash,code_enc,disabled,created_by,created_at,revision) VALUES($1,$2,$3,false,$4,$5,1)",
                vec![id.into(), hash(char::from(b'a' + seed)).into(), container(9).into(), by.into(), NOW.into()],
            )
            .await;
        }
        assert_eq!(validate(&db).await, Err(AuthError::Unavailable));
    }

    #[tokio::test]
    async fn duplicate_rows_for_one_organization_are_refused() {
        let (db, org, owner, _) = fixture().await;
        unconstrained(&db).await;
        insert(&db, org, owner, 1).await;
        insert(&db, org, owner, 2).await;
        assert_eq!(validate(&db).await, Err(AuthError::Unavailable));
    }

    #[tokio::test]
    async fn dangling_organization_or_publisher_references_are_refused() {
        for (org_shift, user_shift) in [(9_000, 0), (0, 9_000)] {
            let (db, org, owner, _) = fixture().await;
            unconstrained(&db).await;
            insert(&db, org + org_shift, owner + user_shift, 0).await;
            assert_eq!(
                validate(&db).await,
                Err(AuthError::Unavailable),
                "dangling reference (org+{org_shift}, user+{user_shift}) must be refused"
            );
        }
    }

    #[tokio::test]
    async fn out_of_domain_scalars_are_refused() {
        for (column, value) in [
            ("org_id", "0"),
            ("org_id", "-1"),
            ("org_id", "2147483648"),
            ("created_by", "0"),
            ("created_by", "-4"),
            ("created_at", "-1"),
            ("created_at", "253402300800"),
            ("revision", "0"),
            ("revision", "-7"),
            ("disabled", "2"),
            ("disabled", "-1"),
        ] {
            let (db, org, owner, _) = fixture().await;
            unconstrained(&db).await;
            insert(&db, org, owner, 0).await;
            h::execute(
                &db,
                &format!("UPDATE neev_org_join_codes SET {column}={value}"),
                vec![],
            )
            .await;
            assert_eq!(
                validate(&db).await,
                Err(AuthError::Unavailable),
                "{column}={value} must be refused"
            );
        }
    }

    #[tokio::test]
    async fn wrong_sqlite_storage_classes_are_refused() {
        // A dump that reloaded a column under the wrong storage class still
        // satisfies SQLite's loosely-typed CHECKs, but it is not the schema this
        // code was written against. (Values SQLite would silently coerce back to
        // the declared affinity — `created_at='1000'`, `code_hash=9` — are not
        // corruption and belong to the domain test above.)
        for (column, value) in [
            ("disabled", "'false'"),
            ("created_at", "'not-a-time'"),
            ("revision", "'one'"),
            ("org_id", "'first'"),
            ("code_hash", "X'61'"),
            ("code_enc", "X'61'"),
        ] {
            let (db, org, owner, _) = fixture().await;
            unconstrained(&db).await;
            insert(&db, org, owner, 0).await;
            h::execute(
                &db,
                &format!("UPDATE neev_org_join_codes SET {column}={value}"),
                vec![],
            )
            .await;
            assert_eq!(
                validate(&db).await,
                Err(AuthError::Unavailable),
                "{column}={value} must be refused"
            );
        }
    }

    #[tokio::test]
    async fn null_columns_are_refused() {
        for column in [
            "org_id",
            "code_hash",
            "code_enc",
            "disabled",
            "created_by",
            "created_at",
            "revision",
        ] {
            let (db, org, owner, _) = fixture().await;
            unconstrained(&db).await;
            insert(&db, org, owner, 0).await;
            h::execute(
                &db,
                &format!("UPDATE neev_org_join_codes SET {column}=NULL"),
                vec![],
            )
            .await;
            assert_eq!(
                validate(&db).await,
                Err(AuthError::Unavailable),
                "NULL {column} must be refused"
            );
        }
    }

    #[tokio::test]
    async fn a_full_page_of_valid_rows_validates_across_the_keyset_boundary() {
        // Exercises the paging loop: more rows than one page, all distinct keys.
        let (db, _, owner, _) = fixture().await;
        unconstrained(&db).await;
        let rows = PAGE + 5;
        for index in 0..rows {
            let org_id = i32::try_from(index).unwrap() + 1;
            // Distinct hash and distinct ciphertext per row: the uniqueness phase
            // must pass, so the two low bytes carry the whole index (a single byte
            // would collide at index 256 and make this test lie).
            let mut bytes = [0u8; NONCE + 12 + TAG];
            bytes[0] = index as u8;
            bytes[1] = (index >> 8) as u8;
            h::execute(
                &db,
                "INSERT INTO neev_org_join_codes(org_id,code_hash,code_enc,disabled,created_by,created_at,revision) VALUES($1,$2,$3,false,$4,$5,1)",
                vec![
                    org_id.into(),
                    format!("{:064x}", index + 1).into(),
                    URL_SAFE_NO_PAD.encode(bytes).into(),
                    owner.into(),
                    NOW.into(),
                ],
            )
            .await;
        }
        // References are the only thing these invented ids cannot satisfy, so the
        // row scan is asserted directly.
        assert_eq!(validate_rows(&db, true).await, Ok(()));
        assert_eq!(validate_keys(&db).await, Ok(()));
        assert_eq!(h::count(&db, "neev_org_join_codes").await, rows as i64);
        // One corrupt row past the first page must still be found.
        h::execute(
            &db,
            "UPDATE neev_org_join_codes SET revision=0 WHERE org_id=$1",
            vec![i32::try_from(rows).unwrap().into()],
        )
        .await;
        assert_eq!(
            validate_rows(&db, true).await,
            Err(AuthError::Unavailable),
            "corruption beyond the first page must still be refused"
        );
    }
}
