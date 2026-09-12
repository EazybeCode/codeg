use super::{database_error, AuthError};
use sea_orm::sea_query::{
    Alias, ColumnDef, Expr, ForeignKey, ForeignKeyAction, Index, IndexCreateStatement, Table,
    TableCreateStatement,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction};

pub(super) const TABLE_NAMES: &[&str] = &[
    "neev_agent_connections",
    "neev_agent_attempts",
    "neev_agent_operations",
];
pub(super) const SCHEMA_VERSION: i64 = 5;

const ERROR_CODE: &str = "error_code IS NULL OR error_code IN ('provider_unavailable','reauth_required','unsupported_account','expired','canceled','cleanup_failed','interrupted')";

fn column(name: &str, kind: char, nullable: bool) -> ColumnDef {
    let mut column = ColumnDef::new(Alias::new(name));
    match kind {
        'i' => {
            column.integer();
        }
        'l' => {
            column.big_integer();
        }
        _ => {
            column.text();
        }
    }
    if !nullable {
        column.not_null();
    }
    column
}

fn checked(mut column: ColumnDef, name: &str, expression: &str) -> ColumnDef {
    column
        .extra(format!(r#"CONSTRAINT "{name}""#))
        .check(Expr::cust(expression));
    column
}

fn table(
    name: &str,
    columns: Vec<ColumnDef>,
    primary: &[&str],
    unique: &[&[&str]],
) -> TableCreateStatement {
    let mut table = Table::create();
    table.table(Alias::new(name));
    for column in columns {
        table.col(column);
    }
    let mut key = Index::create();
    for column in primary {
        key.col(Alias::new(*column));
    }
    table.primary_key(&mut key);
    for columns in unique {
        let mut key = Index::create();
        key.unique();
        for column in *columns {
            key.col(Alias::new(*column));
        }
        table.index(&mut key);
    }
    table
}

fn foreign(
    table: &mut TableCreateStatement,
    name: &str,
    from_table: &str,
    from_columns: &[&str],
    to_table: &str,
    to_columns: &[&str],
) {
    let mut key = ForeignKey::create();
    key.name(name).from_tbl(Alias::new(from_table));
    for column in from_columns {
        key.from_col(Alias::new(*column));
    }
    key.to_tbl(Alias::new(to_table));
    for column in to_columns {
        key.to_col(Alias::new(*column));
    }
    table.foreign_key(
        key.on_delete(ForeignKeyAction::Restrict)
            .on_update(ForeignKeyAction::Restrict),
    );
}

fn tables() -> Vec<TableCreateStatement> {
    let mut connections = table(
        "neev_agent_connections",
        vec![
            checked(
                column("id", 't', false),
                "ck_neev_agent_connections_id_length",
                "length(id)=32",
            ),
            checked(
                column("org_id", 'i', false),
                "ck_neev_agent_connections_org_positive",
                "org_id>0",
            ),
            checked(
                column("user_id", 'i', false),
                "ck_neev_agent_connections_user_positive",
                "user_id>0",
            ),
            checked(
                column("provider", 't', false),
                "ck_neev_agent_connections_provider",
                "provider IN ('claude','codex')",
            ),
            checked(
                column("generation", 'l', false),
                "ck_neev_agent_connections_generation",
                "generation>0",
            ),
            checked(
                column("state", 't', false),
                "ck_neev_agent_connections_state",
                "state IN ('disconnected','connecting','connected','reconnect_required','disconnecting','unavailable')",
            ),
            checked(
                column("account_label", 't', true),
                "ck_neev_agent_connections_account_label",
                "account_label IS NULL OR length(account_label) BETWEEN 1 AND 160",
            ),
            checked(
                column("checked_at", 'l', true),
                "ck_neev_agent_connections_checked_at",
                "checked_at IS NULL OR checked_at>=0",
            ),
            checked(
                column("error_code", 't', true),
                "ck_neev_agent_connections_error_code",
                ERROR_CODE,
            ),
            checked(
                column("storage_handle", 't', true),
                "ck_neev_agent_connections_storage_handle",
                "storage_handle IS NULL OR length(storage_handle)=32",
            ),
            checked(
                checked(
                    column("created_at", 'l', false),
                    "ck_neev_agent_connections_created_at",
                    "created_at>=0",
                ),
                "ck_neev_agent_connections_state_shape",
                "(state='connected' AND account_label IS NOT NULL AND checked_at IS NOT NULL AND storage_handle IS NOT NULL AND error_code IS NULL) OR (state='disconnected' AND account_label IS NULL AND checked_at IS NULL AND storage_handle IS NULL AND error_code IS NULL) OR (state IN ('connecting','reconnect_required','disconnecting','unavailable') AND account_label IS NULL AND checked_at IS NULL)",
            ),
            checked(
                column("updated_at", 'l', false),
                "ck_neev_agent_connections_updated_at",
                "updated_at>=created_at",
            ),
        ],
        &["id"],
        &[&["org_id", "user_id", "provider"]],
    );
    foreign(
        &mut connections,
        "fk_neev_agent_connections_membership",
        "neev_agent_connections",
        &["org_id", "user_id"],
        "neev_memberships",
        &["org_id", "user_id"],
    );

    let mut attempts = table(
        "neev_agent_attempts",
        vec![
            checked(
                column("id", 't', false),
                "ck_neev_agent_attempts_id_length",
                "length(id)=32",
            ),
            checked(
                column("connection_id", 't', false),
                "ck_neev_agent_attempts_connection_id_length",
                "length(connection_id)=32",
            ),
            checked(
                column("generation", 'l', false),
                "ck_neev_agent_attempts_generation",
                "generation>0",
            ),
            checked(
                column("session_id", 't', false),
                "ck_neev_agent_attempts_session_id",
                "length(session_id) BETWEEN 1 AND 128",
            ),
            checked(
                column("state", 't', false),
                "ck_neev_agent_attempts_state",
                "state IN ('pending','completed','canceled','expired','failed')",
            ),
            checked(
                column("created_at", 'l', false),
                "ck_neev_agent_attempts_created_at",
                "created_at>=0",
            ),
            checked(
                column("expires_at", 'l', false),
                "ck_neev_agent_attempts_expires_at",
                "expires_at>created_at",
            ),
            checked(
                column("updated_at", 'l', false),
                "ck_neev_agent_attempts_updated_at",
                "updated_at>=created_at",
            ),
        ],
        &["id"],
        &[&["connection_id", "generation"], &["connection_id", "id"]],
    );
    foreign(
        &mut attempts,
        "fk_neev_agent_attempts_connection",
        "neev_agent_attempts",
        &["connection_id"],
        "neev_agent_connections",
        &["id"],
    );

    let mut operations = table(
        "neev_agent_operations",
        vec![
            checked(
                column("id", 't', false),
                "ck_neev_agent_operations_id_length",
                "length(id)=32",
            ),
            checked(
                column("connection_id", 't', false),
                "ck_neev_agent_operations_connection_id_length",
                "length(connection_id)=32",
            ),
            checked(
                column("kind", 't', false),
                "ck_neev_agent_operations_kind",
                "kind IN ('connect','cancel','disconnect')",
            ),
            checked(
                column("request_key", 't', false),
                "ck_neev_agent_operations_request_key",
                "length(request_key) BETWEEN 1 AND 64",
            ),
            checked(
                column("target_attempt_id", 't', true),
                "ck_neev_agent_operations_target_attempt_id",
                "target_attempt_id IS NULL OR length(target_attempt_id)=32",
            ),
            checked(
                column("target_generation", 'l', false),
                "ck_neev_agent_operations_target_generation",
                "target_generation>0",
            ),
            checked(
                column("result_generation", 'l', false),
                "ck_neev_agent_operations_result_generation",
                "result_generation>=target_generation",
            ),
            checked(
                column("state", 't', false),
                "ck_neev_agent_operations_state",
                "state IN ('queued','leased','completed','abandoned')",
            ),
            checked(
                column("lease_owner", 't', true),
                "ck_neev_agent_operations_lease_owner",
                "lease_owner IS NULL OR length(lease_owner)=32",
            ),
            checked(
                column("lease_expires_at", 'l', true),
                "ck_neev_agent_operations_lease_expires_at",
                "lease_expires_at IS NULL OR lease_expires_at>=0",
            ),
            checked(
                column("error_code", 't', true),
                "ck_neev_agent_operations_error_code",
                ERROR_CODE,
            ),
            checked(
                checked(
                    checked(
                        column("created_at", 'l', false),
                        "ck_neev_agent_operations_created_at",
                        "created_at>=0",
                    ),
                    "ck_neev_agent_operations_lease_shape",
                    "(state='leased' AND lease_owner IS NOT NULL AND lease_expires_at IS NOT NULL) OR (state IN ('queued','completed','abandoned') AND lease_owner IS NULL AND lease_expires_at IS NULL)",
                ),
                "ck_neev_agent_operations_target_shape",
                "(kind IN ('connect','cancel') AND target_attempt_id IS NOT NULL) OR (kind='disconnect' AND target_attempt_id IS NULL)",
            ),
            checked(
                column("expires_at", 'l', false),
                "ck_neev_agent_operations_expires_at",
                "expires_at>created_at",
            ),
            checked(
                column("updated_at", 'l', false),
                "ck_neev_agent_operations_updated_at",
                "updated_at>=created_at",
            ),
        ],
        &["id"],
        &[&["connection_id", "kind", "request_key"]],
    );
    foreign(
        &mut operations,
        "fk_neev_agent_operations_connection",
        "neev_agent_operations",
        &["connection_id"],
        "neev_agent_connections",
        &["id"],
    );
    foreign(
        &mut operations,
        "fk_neev_agent_operations_attempt",
        "neev_agent_operations",
        &["connection_id", "target_attempt_id"],
        "neev_agent_attempts",
        &["connection_id", "id"],
    );

    vec![connections, attempts, operations]
}

fn index(name: &str, table: &str, columns: &[&str]) -> IndexCreateStatement {
    let mut index = Index::create();
    index.name(name).table(Alias::new(table));
    for column in columns {
        index.col(Alias::new(*column));
    }
    index.to_owned()
}

fn indexes() -> Vec<IndexCreateStatement> {
    vec![
        index(
            "idx_neev_agent_attempts_state_expires",
            "neev_agent_attempts",
            &["state", "expires_at"],
        ),
        index(
            "idx_neev_agent_operations_state_lease_expires",
            "neev_agent_operations",
            &["state", "lease_expires_at"],
        ),
        index(
            "idx_neev_agent_connections_user_org",
            "neev_agent_connections",
            &["user_id", "org_id"],
        ),
    ]
}

pub(super) async fn upgrade(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    let backend = tx.get_database_backend();
    for table in tables() {
        tx.execute(backend.build(&table))
            .await
            .map_err(database_error)?;
    }
    for index in indexes() {
        tx.execute(backend.build(&index))
            .await
            .map_err(database_error)?;
    }
    Ok(())
}
