use super::{database_error, AuthError};
use sea_orm::sea_query::{
    Alias, ColumnDef, Expr, ForeignKey, ForeignKeyAction, Index, IndexCreateStatement, Table,
    TableCreateStatement,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction};

pub(super) const TABLE_NAMES: &[&str] = &[
    "neev_resources",
    "neev_messages",
    "neev_document_revisions",
    "neev_content_usage",
    "neev_content_leases",
];
pub(super) const SCHEMA_VERSION: i64 = 3;

fn table(
    name: &str,
    columns: &[(&str, &str)],
    primary: &[&str],
    checks: &[&str],
    unique: &[&[&str]],
) -> TableCreateStatement {
    let mut table = Table::create();
    table.table(Alias::new(name));
    for (name, kind) in columns {
        let mut column = ColumnDef::new(Alias::new(*name));
        match kind.as_bytes()[0] {
            b'i' => {
                column.integer();
            }
            b'l' => {
                column.big_integer();
            }
            b'b' => {
                column.boolean();
            }
            _ => {
                column.text();
            }
        }
        if !kind.ends_with('?') {
            column.not_null();
        }
        table.col(&mut column);
    }
    if !primary.is_empty() {
        let mut key = Index::create();
        for column in primary {
            key.col(Alias::new(*column));
        }
        table.primary_key(&mut key);
    }
    for check in checks {
        table.check(Expr::cust(*check));
    }
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
    from_table: &str,
    from_columns: &[&str],
    to_table: &str,
    to_columns: &[&str],
) {
    let mut key = ForeignKey::create();
    key.from_tbl(Alias::new(from_table));
    for column in from_columns {
        key.from_col(Alias::new(*column));
    }
    key.to_tbl(Alias::new(to_table));
    for column in to_columns {
        key.to_col(Alias::new(*column));
    }
    table.foreign_key(key.on_delete(ForeignKeyAction::Restrict));
}

pub(super) fn tables() -> Vec<TableCreateStatement> {
    let mut resources = table(
        "neev_resources",
        &[
            ("org_id", "i"),
            ("id", "t"),
            ("kind", "t"),
            ("node_id", "i"),
            ("creator_id", "i"),
            ("title", "t"),
            ("visibility", "t"),
            ("origin_chat_id", "t?"),
            ("origin_kind", "t?"),
            ("auth_revision", "l"),
            ("content_revision", "l"),
            ("archived", "b"),
            ("created_at", "l"),
            ("updated_at", "l"),
        ],
        &["org_id", "id"],
        &[
            "kind IN ('chat','document')",
            "visibility IN ('private','organization')",
            "auth_revision>=1",
            "content_revision>=0",
            "created_at>=0",
            "updated_at>=0",
            "(origin_chat_id IS NULL AND origin_kind IS NULL) OR (origin_chat_id IS NOT NULL AND kind='document' AND origin_kind='chat')",
        ],
        &[&["org_id", "id", "kind"]],
    );
    foreign(
        &mut resources,
        "neev_resources",
        &["org_id", "node_id"],
        "neev_nodes",
        &["org_id", "id"],
    );
    foreign(
        &mut resources,
        "neev_resources",
        &["creator_id"],
        "neev_accounts",
        &["id"],
    );
    foreign(
        &mut resources,
        "neev_resources",
        &["org_id"],
        "neev_organizations",
        &["id"],
    );
    foreign(
        &mut resources,
        "neev_resources",
        &["org_id", "origin_chat_id", "origin_kind"],
        "neev_resources",
        &["org_id", "id", "kind"],
    );

    let mut messages = table(
        "neev_messages",
        &[
            ("org_id", "i"),
            ("chat_id", "t"),
            ("resource_kind", "t"),
            ("id", "t"),
            ("sequence", "l"),
            ("author_id", "i"),
            ("body", "t"),
            ("byte_length", "l"),
            ("created_at", "l"),
        ],
        &["org_id", "chat_id", "sequence"],
        &[
            "resource_kind='chat'",
            "sequence>=1",
            "byte_length BETWEEN 1 AND 32768",
            "created_at>=0",
        ],
        &[&["org_id", "id"]],
    );
    foreign(
        &mut messages,
        "neev_messages",
        &["org_id", "chat_id", "resource_kind"],
        "neev_resources",
        &["org_id", "id", "kind"],
    );
    foreign(
        &mut messages,
        "neev_messages",
        &["author_id"],
        "neev_accounts",
        &["id"],
    );

    let mut revisions = table(
        "neev_document_revisions",
        &[
            ("org_id", "i"),
            ("document_id", "t"),
            ("resource_kind", "t"),
            ("revision", "l"),
            ("blob_key", "t"),
            ("sha256", "t"),
            ("byte_length", "l"),
            ("author_id", "i"),
            ("created_at", "l"),
            ("parent_revision", "l?"),
            ("restored_from", "l?"),
        ],
        &["org_id", "document_id", "revision"],
        &[
            "resource_kind='document'",
            "revision BETWEEN 1 AND 1000",
            "byte_length BETWEEN 0 AND 1048576",
            "created_at>=0",
        ],
        &[&["org_id", "blob_key"]],
    );
    foreign(
        &mut revisions,
        "neev_document_revisions",
        &["org_id", "document_id", "resource_kind"],
        "neev_resources",
        &["org_id", "id", "kind"],
    );
    foreign(
        &mut revisions,
        "neev_document_revisions",
        &["org_id", "document_id", "parent_revision"],
        "neev_document_revisions",
        &["org_id", "document_id", "revision"],
    );
    foreign(
        &mut revisions,
        "neev_document_revisions",
        &["org_id", "document_id", "restored_from"],
        "neev_document_revisions",
        &["org_id", "document_id", "revision"],
    );
    foreign(
        &mut revisions,
        "neev_document_revisions",
        &["author_id"],
        "neev_accounts",
        &["id"],
    );

    let mut usage = table(
        "neev_content_usage",
        &[
            ("org_id", "i"),
            ("charged_bytes", "l"),
            ("chats", "l"),
            ("documents", "l"),
            ("messages", "l"),
        ],
        &["org_id"],
        &[
            "charged_bytes BETWEEN 0 AND 1073741824",
            "chats BETWEEN 0 AND 10000",
            "documents BETWEEN 0 AND 10000",
            "messages BETWEEN 0 AND 100000",
        ],
        &[],
    );
    foreign(
        &mut usage,
        "neev_content_usage",
        &["org_id"],
        "neev_organizations",
        &["id"],
    );

    let mut leases = table(
        "neev_content_leases",
        &[
            ("org_id", "i"),
            ("resource_id", "t"),
            ("id", "t"),
            ("actor_id", "i"),
            ("request_id", "t"),
            ("body_hash", "t"),
            ("blob_key", "t"),
            ("reserved_bytes", "l"),
            ("expected_content_revision", "l"),
            ("expected_auth_revision", "l"),
            ("expires_at", "l"),
        ],
        &["org_id", "resource_id"],
        &[
            "reserved_bytes BETWEEN 0 AND 1048576",
            "expected_content_revision>=0",
            "expected_auth_revision>=1",
            "expires_at>=0",
        ],
        &[&["org_id", "id"]],
    );
    foreign(
        &mut leases,
        "neev_content_leases",
        &["org_id", "resource_id"],
        "neev_resources",
        &["org_id", "id"],
    );
    foreign(
        &mut leases,
        "neev_content_leases",
        &["actor_id"],
        "neev_accounts",
        &["id"],
    );

    vec![resources, messages, revisions, usage, leases]
}

fn index(name: &str, table: &str, columns: &[&str]) -> IndexCreateStatement {
    let mut index = Index::create();
    index.name(name).table(Alias::new(table));
    for column in columns {
        index.col(Alias::new(*column));
    }
    index.to_owned()
}

pub(super) fn indexes() -> Vec<IndexCreateStatement> {
    vec![
        index(
            "neev_resources_node_kind",
            "neev_resources",
            &["org_id", "node_id", "kind", "archived", "id"],
        ),
        index(
            "neev_resources_origin",
            "neev_resources",
            &["org_id", "origin_chat_id", "id"],
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
