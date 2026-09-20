//! Additive per-organization join-code storage (schema9). One reusable,
//! admin-managed code per org: unguessable, server-validated, encrypted at rest
//! for admin re-display, rotate/disable. No runtime beyond CRUD + membership join
//! (see `org/join_code.rs`). Additive only — never edits the v2..v8 baseline.
use super::{database_error, AuthError};
use sea_orm::sea_query::{
    Alias, ColumnDef, Expr, ForeignKey, ForeignKeyAction, Index, Table, TableCreateStatement,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction};

pub(super) const SCHEMA_VERSION: i64 = 9;
pub(super) const TABLE_NAMES: &[&str] = &["neev_org_join_codes"];

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
    Ok(())
}
