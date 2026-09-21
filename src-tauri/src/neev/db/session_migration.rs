//! Schema 11 — server-owned **agent sessions** and their **turn/run history**,
//! plus the additive agent-author marking on `neev_messages`.
//!
//! Follows schema 10 (document images). The initializer validates each older
//! inventory and applies image storage before these session tables.
//!
//! Additive and forward-only, like every native migration: it creates two new
//! tables and adds two columns to `neev_messages` with row-preserving defaults,
//! so a schema-9 database upgrades in place and every existing row survives.
//!
//! What the storage buys (spec §5.1/§5.2/§5.3):
//! - `neev_agent_sessions` is the durable session entity. `visibility` is
//!   CHECK-pinned to `'private'`: an agent session is the owner's private
//!   assistant and widening it would require a *migration*, not a code change,
//!   so a bug cannot silently publish one.
//! - `neev_agent_turns` is the ordered, server-persisted run history. Each turn
//!   carries its `run_id`, the accumulated `response`, and a terminal status, so
//!   a dropped stream can **reattach and replay** instead of re-running, and a
//!   crash leaves a durable `failed/interrupted` row rather than a phantom
//!   "running" one.
//! - `neev_messages.author_kind` distinguishes an agent-authored team message
//!   from a human one. `author_id` stays the (human) invoker so the existing FK,
//!   ACL and quota paths are untouched and the audit trail records *who invoked*
//!   — the agent never impersonates a human because the kind is explicit and
//!   `agent_label` names the provider that produced the text.
//!
//! The `(author_kind='agent') == (agent_label IS NOT NULL)` pairing cannot be a
//! table CHECK (SQLite `ADD COLUMN` takes only column constraints), so it is
//! enforced by the writer and re-proved against persisted rows in [`validate`].

use super::{database_error, AuthError};
use sea_orm::sea_query::{
    Alias, ColumnDef, Expr, ForeignKey, ForeignKeyAction, Index, IndexCreateStatement, Table,
    TableCreateStatement,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, QueryResult};

pub(super) const SCHEMA_VERSION: i64 = 11;
pub(super) const TABLE_NAMES: &[&str] = &["neev_agent_sessions", "neev_agent_turns"];

/// Additive layer 12: the optional pointer from a saved Agent Chat to a node in
/// the workspace tree, so a session appears under the project it belongs to on
/// every device rather than only where it was created.
///
/// This adds a COLUMN, not a table, so the table inventory is identical to 11's
/// and the stored marker is the only thing that tells the two apart — the same
/// situation as `content_migration`'s plain and hardened versions, and handled
/// the same way in [`super::migrate`].
///
/// **No foreign key, deliberately.** `ADD COLUMN` accepts only column
/// constraints, so the composite `(node_id, org_id)` key that carries the tenant
/// cannot be declared on a table that already exists; declaring it on Postgres
/// only would make the two backends diverge the moment node deletion is
/// implemented. Tenancy is instead checked by the writer inside the same
/// transaction and re-proved against persisted rows in [`validate_node_link`],
/// which is the pattern this file already uses for the `author_kind`/
/// `agent_label` pairing.
///
/// The consequence is a contract for whoever adds node deletion: clear the
/// pointers first, in the deleting transaction, via
/// `agents::sessions::clear_node_pointers`. Doing so is what keeps the validator
/// true, and it is also the only behaviour that neither blocks the delete nor
/// reveals that someone else's PRIVATE session was filed under that node.
pub(super) const NODE_LINK_SCHEMA_VERSION: i64 = 12;

/// Terminal run failures a turn may record. `interrupted` is reserved for
/// startup reconciliation (the server died mid-run), which is why a restarted
/// server can never leave a turn stuck in `running`.
pub(crate) const TURN_ERROR_CODES: &[&str] = &[
    "disabled",
    "unauthorized",
    "isolation",
    "provider",
    "unavailable",
    "io",
    "canceled",
    "interrupted",
    "too_large",
    "unsupported",
];

const TURN_ERROR_CODE_CHECK: &str = "error_code IS NULL OR error_code IN ('disabled','unauthorized','isolation','provider','unavailable','io','canceled','interrupted','too_large','unsupported')";
/// A turn is either live (no completion, no error) or terminal (completed with
/// no error, or failed/canceled with one). No other shape can be stored.
const TURN_STATUS_SHAPE: &str = "(status='running' AND completed_at IS NULL AND error_code IS NULL) OR (status='completed' AND completed_at IS NOT NULL AND error_code IS NULL) OR (status IN ('failed','canceled') AND completed_at IS NOT NULL AND error_code IS NOT NULL)";

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

fn tables() -> Vec<TableCreateStatement> {
    let mut sessions = table(
        "neev_agent_sessions",
        &[
            ("org_id", "i"),
            ("id", "t"),
            ("owner_id", "i"),
            ("title", "t"),
            ("provider", "t"),
            ("model", "t"),
            ("visibility", "t"),
            ("kind", "t"),
            ("bound_chat_id", "t?"),
            ("bound_kind", "t?"),
            ("archived", "b"),
            ("revision", "l"),
            ("created_at", "l"),
            ("updated_at", "l"),
        ],
        &["org_id", "id"],
        &[
            "org_id>0",
            "length(id)=36",
            "owner_id>0",
            "length(title) BETWEEN 1 AND 200",
            "provider IN ('claude','codex')",
            "length(model) BETWEEN 1 AND 80",
            // Firm default (§5.3): private, and only a migration can widen it.
            // This holds for BOTH kinds — a `team` session's own transcript is
            // still the invoker's; only the published reply is team-visible.
            "visibility='private'",
            // `chat` = a saved Agent Chat the user manages. `team` = the
            // per-(user, chat) holder for explicit team invocations, which must
            // be bound to the chat it publishes into and is never listed as a
            // saved session.
            "kind IN ('chat','team')",
            "(kind='chat') OR (kind='team' AND bound_chat_id IS NOT NULL)",
            "(bound_chat_id IS NULL AND bound_kind IS NULL) OR (bound_chat_id IS NOT NULL AND bound_kind='chat')",
            "revision>=1",
            "created_at>=0",
            "updated_at>=created_at",
        ],
        &[&["id"]],
    );
    // The owner must be a member of the owning org: the session's tenancy is a
    // stored fact, not something a request can assert.
    foreign(
        &mut sessions,
        "neev_agent_sessions",
        &["org_id", "owner_id"],
        "neev_memberships",
        &["org_id", "user_id"],
    );
    foreign(
        &mut sessions,
        "neev_agent_sessions",
        &["org_id"],
        "neev_organizations",
        &["id"],
    );
    // A bound chat must be a chat in the SAME org (composite FK carries the
    // tenant), mirroring `neev_resources.origin_chat_id`.
    foreign(
        &mut sessions,
        "neev_agent_sessions",
        &["org_id", "bound_chat_id", "bound_kind"],
        "neev_resources",
        &["org_id", "id", "kind"],
    );

    let mut turns = table(
        "neev_agent_turns",
        &[
            ("org_id", "i"),
            ("session_id", "t"),
            ("sequence", "l"),
            ("id", "t"),
            ("run_id", "t"),
            ("request_id", "t?"),
            ("prompt", "t"),
            ("response", "t"),
            ("status", "t"),
            ("error_code", "t?"),
            ("provider", "t"),
            ("model", "t"),
            ("created_at", "l"),
            ("updated_at", "l"),
            ("completed_at", "l?"),
        ],
        &["org_id", "session_id", "sequence"],
        &[
            "org_id>0",
            "sequence>=1",
            "length(id)=36",
            "length(run_id) BETWEEN 1 AND 64",
            "request_id IS NULL OR length(request_id)=36",
            "length(prompt) BETWEEN 1 AND 32768",
            "length(response)<=262144",
            "status IN ('running','completed','failed','canceled')",
            TURN_ERROR_CODE_CHECK,
            "provider IN ('claude','codex')",
            "length(model) BETWEEN 1 AND 80",
            "created_at>=0",
            "updated_at>=created_at",
            "completed_at IS NULL OR completed_at>=created_at",
            TURN_STATUS_SHAPE,
        ],
        &[
            &["org_id", "id"],
            &["org_id", "run_id"],
            &["run_id"],
            &["org_id", "session_id", "request_id"],
        ],
    );
    foreign(
        &mut turns,
        "neev_agent_turns",
        &["org_id", "session_id"],
        "neev_agent_sessions",
        &["org_id", "id"],
    );

    vec![sessions, turns]
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
        // The session list: one owner's live sessions, most-recent first.
        index(
            "idx_neev_agent_sessions_owner",
            "neev_agent_sessions",
            &["org_id", "owner_id", "archived", "updated_at"],
        ),
        // Startup reconciliation scans exactly the live turns.
        index(
            "idx_neev_agent_turns_status",
            "neev_agent_turns",
            &["status", "updated_at"],
        ),
    ]
}

/// The additive `neev_messages` columns, as `(name, ADD COLUMN statement)` in
/// apply order. Each has a row-preserving default, so every existing message
/// keeps its content and becomes `author_kind='human'` — which is what it was.
fn message_columns() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "author_kind",
            "ALTER TABLE neev_messages ADD COLUMN author_kind TEXT NOT NULL DEFAULT 'human' CHECK(author_kind IN ('human','agent'))",
        ),
        (
            "agent_label",
            "ALTER TABLE neev_messages ADD COLUMN agent_label TEXT CHECK(agent_label IS NULL OR length(agent_label) BETWEEN 1 AND 80)",
        ),
    ]
}

/// The additive session columns of layer 12.
///
/// Nullable with no default, so every session that exists when this runs keeps
/// exactly the state it had: NULL means unfiled, which is what all of them are.
fn session_columns() -> Vec<(&'static str, &'static str)> {
    vec![(
        "node_id",
        "ALTER TABLE neev_agent_sessions ADD COLUMN node_id INTEGER CHECK(node_id IS NULL OR node_id>0)",
    )]
}

/// Whether `table.column` already exists. `ADD COLUMN` is not idempotent on
/// either backend, and this upgrade can meet a `neev_messages` that already
/// carries the columns (a replayed migration, or a database whose session
/// tables were removed but whose message columns were not), so each add is
/// guarded rather than assumed to be the first.
async fn has_column<C: ConnectionTrait>(
    db: &C,
    table: &str,
    column: &str,
) -> Result<bool, AuthError> {
    let sql = match db.get_database_backend() {
        DbBackend::Sqlite => "SELECT 1 AS present FROM pragma_table_info($1) WHERE name=$2",
        DbBackend::Postgres => "SELECT 1 AS present FROM information_schema.columns WHERE table_schema=current_schema() AND table_name=$1 AND column_name=$2",
        _ => return Err(AuthError::Unavailable),
    };
    Ok(super::one(db, sql, vec![table.into(), column.into()])
        .await?
        .is_some())
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
    for (column, statement) in message_columns() {
        if !has_column(tx, "neev_messages", column).await? {
            super::execute(tx, statement, vec![]).await?;
        }
    }
    validate(tx).await
}

/// Apply layer 12. Guarded per column, so a replayed migration — or a database
/// that already carries `node_id` — is a no-op rather than an error.
pub(super) async fn link_nodes(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    for (column, statement) in session_columns() {
        if !has_column(tx, "neev_agent_sessions", column).await? {
            super::execute(tx, statement, vec![]).await?;
        }
    }
    validate_node_link(tx).await
}

/// Prove layer 12's invariants against stored rows.
///
/// Separate from [`validate`] because that one describes layer 11, which is a
/// database without this column: a v11 marker must still validate, so the node
/// checks can only run once the marker says 12.
pub(super) async fn validate_node_link<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    let sqlite = match db.get_database_backend() {
        DbBackend::Sqlite => true,
        DbBackend::Postgres => false,
        _ => return Err(AuthError::Unavailable),
    };
    let mut queries = vec![
        // A filed session points at a node in its OWN org. With no declarable
        // foreign key this is the check that keeps the pointer from naming
        // another tenant's project, or a node that no longer exists.
        "SELECT 1 AS invalid FROM neev_agent_sessions s LEFT JOIN neev_nodes n ON n.org_id=s.org_id AND n.id=s.node_id WHERE s.node_id IS NOT NULL AND n.id IS NULL LIMIT 1",
        // Only a saved chat can be filed. A `team` holder is internal and is
        // never listed, so placing one in the tree would surface a session the
        // user never created.
        "SELECT 1 AS invalid FROM neev_agent_sessions WHERE node_id IS NOT NULL AND kind<>'chat' LIMIT 1",
    ];
    // SQLite accepts any storage class the CHECK happens to satisfy, so a
    // hand-edited row could hold a string here and still pass `node_id>0`.
    if sqlite {
        queries.push(
            "SELECT 1 AS invalid FROM neev_agent_sessions WHERE node_id IS NOT NULL AND typeof(node_id)<>'integer' LIMIT 1",
        );
    }
    for query in queries {
        if any(db, query).await? {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}

async fn any<C: ConnectionTrait>(db: &C, sql: &str) -> Result<bool, AuthError> {
    Ok(super::one(db, sql, vec![]).await?.is_some())
}

/// SQLite is dynamically typed, so a corrupted/hand-edited row can hold a value
/// of the wrong storage class that every declared CHECK still accepts. Select
/// `typeof(col)` alongside each column there and reject anything unexpected.
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

fn storage_class(
    row: &QueryResult,
    col: &str,
    sqlite: bool,
    allowed: &[&str],
) -> Result<(), AuthError> {
    if !sqlite {
        return Ok(());
    }
    let actual: String = row
        .try_get("", &format!("{col}__t"))
        .map_err(database_error)?;
    if allowed.contains(&actual.as_str()) {
        Ok(())
    } else {
        Err(AuthError::Unavailable)
    }
}

fn text(row: &QueryResult, col: &str, sqlite: bool) -> Result<String, AuthError> {
    storage_class(row, col, sqlite, &["text"])?;
    row.try_get("", col).map_err(database_error)
}

fn opt_text(row: &QueryResult, col: &str, sqlite: bool) -> Result<Option<String>, AuthError> {
    storage_class(row, col, sqlite, &["text", "null"])?;
    row.try_get("", col).map_err(database_error)
}

fn int(row: &QueryResult, col: &str, sqlite: bool) -> Result<i64, AuthError> {
    storage_class(row, col, sqlite, &["integer"])?;
    if sqlite {
        row.try_get::<i64>("", col).map_err(database_error)
    } else {
        row.try_get::<i32>("", col)
            .map(i64::from)
            .map_err(database_error)
    }
}

fn big(row: &QueryResult, col: &str, sqlite: bool) -> Result<i64, AuthError> {
    storage_class(row, col, sqlite, &["integer"])?;
    row.try_get::<i64>("", col).map_err(database_error)
}

fn opt_big(row: &QueryResult, col: &str, sqlite: bool) -> Result<Option<i64>, AuthError> {
    storage_class(row, col, sqlite, &["integer", "null"])?;
    row.try_get::<Option<i64>>("", col).map_err(database_error)
}

/// A bounded keyset page: never load the whole table into memory.
const PAGE: usize = 256;

fn uuid_like(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Re-prove the schema-11 contract against persisted rows, independently of the
/// declared constraints (which a restored dump or a hand-edited SQLite file can
/// lack entirely) and of any surviving index.
pub(super) async fn validate<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    let sqlite = match db.get_database_backend() {
        DbBackend::Sqlite => true,
        DbBackend::Postgres => false,
        _ => return Err(AuthError::Unavailable),
    };
    validate_sessions(db, sqlite).await?;
    validate_turns(db, sqlite).await?;
    validate_messages(db, sqlite).await?;

    // Phase 2 — uniqueness, re-derived rather than trusted to an index.
    for query in [
        "SELECT 1 AS invalid FROM neev_agent_sessions GROUP BY org_id,id HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_agent_sessions GROUP BY id HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_agent_turns GROUP BY org_id,session_id,sequence HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_agent_turns GROUP BY org_id,id HAVING COUNT(*)<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_agent_turns GROUP BY run_id HAVING COUNT(*)<>1 LIMIT 1",
        // A replayed requestId must resolve to ONE turn, never a duplicate run.
        "SELECT 1 AS invalid FROM neev_agent_turns WHERE request_id IS NOT NULL GROUP BY org_id,session_id,request_id HAVING COUNT(*)<>1 LIMIT 1",
    ] {
        if any(db, query).await? {
            return Err(AuthError::Unavailable);
        }
    }

    // Phase 3 — references and the tenant boundary, which no declared
    // constraint expresses on a restored dump.
    for query in [
        // Every session owner still has a membership row in the owning org
        // (memberships are deactivated, never deleted, so this stays true).
        "SELECT 1 AS invalid FROM neev_agent_sessions s LEFT JOIN neev_memberships m ON m.org_id=s.org_id AND m.user_id=s.owner_id WHERE m.user_id IS NULL LIMIT 1",
        "SELECT 1 AS invalid FROM neev_agent_sessions s LEFT JOIN neev_organizations o ON o.id=s.org_id WHERE o.id IS NULL LIMIT 1",
        // A bound chat is a chat in the SAME org — never a cross-tenant resource.
        "SELECT 1 AS invalid FROM neev_agent_sessions s LEFT JOIN neev_resources r ON r.org_id=s.org_id AND r.id=s.bound_chat_id AND r.kind='chat' WHERE s.bound_chat_id IS NOT NULL AND r.id IS NULL LIMIT 1",
        // Every turn belongs to a session in its own org.
        "SELECT 1 AS invalid FROM neev_agent_turns t LEFT JOIN neev_agent_sessions s ON s.org_id=t.org_id AND s.id=t.session_id WHERE s.id IS NULL LIMIT 1",
        // A turn never contradicts its session's provider.
        "SELECT 1 AS invalid FROM neev_agent_turns t JOIN neev_agent_sessions s ON s.org_id=t.org_id AND s.id=t.session_id WHERE t.provider<>s.provider LIMIT 1",
        // An agent-authored message names its provider, and only those do.
        "SELECT 1 AS invalid FROM neev_messages WHERE (author_kind='agent')<>(agent_label IS NOT NULL) LIMIT 1",
    ] {
        if any(db, query).await? {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}

async fn validate_sessions<C: ConnectionTrait>(db: &C, sqlite: bool) -> Result<(), AuthError> {
    let cols = [
        "org_id",
        "id",
        "owner_id",
        "title",
        "provider",
        "model",
        "visibility",
        "kind",
        "bound_chat_id",
        "bound_kind",
        "archived",
        "revision",
        "created_at",
        "updated_at",
    ];
    let mut cursor = String::new();
    loop {
        let sql = format!(
            "SELECT {} FROM neev_agent_sessions WHERE id>$1 ORDER BY id LIMIT {PAGE}",
            type_cols(sqlite, &cols)
        );
        let rows = db
            .query_all(super::statement(db, &sql, vec![cursor.clone().into()]))
            .await
            .map_err(database_error)?;
        if rows.is_empty() {
            return Ok(());
        }
        for row in &rows {
            let org_id = int(row, "org_id", sqlite)?;
            let id = text(row, "id", sqlite)?;
            let owner_id = int(row, "owner_id", sqlite)?;
            let title = text(row, "title", sqlite)?;
            let provider = text(row, "provider", sqlite)?;
            let model = text(row, "model", sqlite)?;
            let visibility = text(row, "visibility", sqlite)?;
            let kind = text(row, "kind", sqlite)?;
            let bound_chat_id = opt_text(row, "bound_chat_id", sqlite)?;
            let bound_kind = opt_text(row, "bound_kind", sqlite)?;
            // `archived` is BOOLEAN: an integer 0/1 on SQLite, a bool on PG.
            let archived = if sqlite {
                let value = big(row, "archived", sqlite)?;
                if !matches!(value, 0 | 1) {
                    return Err(AuthError::Unavailable);
                }
                value == 1
            } else {
                row.try_get::<bool>("", "archived")
                    .map_err(database_error)?
            };
            let _ = archived;
            let revision = big(row, "revision", sqlite)?;
            let created_at = big(row, "created_at", sqlite)?;
            let updated_at = big(row, "updated_at", sqlite)?;
            if org_id <= 0 || org_id > i64::from(i32::MAX)
                || !uuid_like(&id)
                || owner_id <= 0 || owner_id > i64::from(i32::MAX)
                || !(1..=200).contains(&title.chars().count())
                || !matches!(provider.as_str(), "claude" | "codex")
                || !(1..=80).contains(&model.chars().count())
                || visibility != "private"
                || !matches!(kind.as_str(), "chat" | "team")
                // A team session exists only to publish into its bound chat.
                || (kind == "team" && bound_chat_id.is_none())
                || bound_chat_id.is_some() != bound_kind.is_some()
                || bound_kind.as_deref().is_some_and(|kind| kind != "chat")
                || bound_chat_id.as_deref().is_some_and(|value| !uuid_like(value))
                || revision < 1
                || !(0..=253_402_300_799).contains(&created_at)
                || updated_at < created_at
                || updated_at > 253_402_300_799
            {
                return Err(AuthError::Unavailable);
            }
            cursor = id;
        }
        if rows.len() < PAGE {
            return Ok(());
        }
    }
}

async fn validate_turns<C: ConnectionTrait>(db: &C, sqlite: bool) -> Result<(), AuthError> {
    let cols = [
        "org_id",
        "session_id",
        "sequence",
        "id",
        "run_id",
        "request_id",
        "prompt",
        "response",
        "status",
        "error_code",
        "provider",
        "model",
        "created_at",
        "updated_at",
        "completed_at",
    ];
    let mut cursor = String::new();
    loop {
        let sql = format!(
            "SELECT {} FROM neev_agent_turns WHERE id>$1 ORDER BY id LIMIT {PAGE}",
            type_cols(sqlite, &cols)
        );
        let rows = db
            .query_all(super::statement(db, &sql, vec![cursor.clone().into()]))
            .await
            .map_err(database_error)?;
        if rows.is_empty() {
            return Ok(());
        }
        for row in &rows {
            let org_id = int(row, "org_id", sqlite)?;
            let session_id = text(row, "session_id", sqlite)?;
            let sequence = big(row, "sequence", sqlite)?;
            let id = text(row, "id", sqlite)?;
            let run_id = text(row, "run_id", sqlite)?;
            let request_id = opt_text(row, "request_id", sqlite)?;
            let prompt = text(row, "prompt", sqlite)?;
            let response = text(row, "response", sqlite)?;
            let status = text(row, "status", sqlite)?;
            let error_code = opt_text(row, "error_code", sqlite)?;
            let provider = text(row, "provider", sqlite)?;
            let model = text(row, "model", sqlite)?;
            let created_at = big(row, "created_at", sqlite)?;
            let updated_at = big(row, "updated_at", sqlite)?;
            let completed_at = opt_big(row, "completed_at", sqlite)?;
            let live = status == "running";
            let terminal_error = matches!(status.as_str(), "failed" | "canceled");
            if org_id <= 0 || org_id > i64::from(i32::MAX)
                || !uuid_like(&session_id)
                || sequence < 1
                || !uuid_like(&id)
                || !(1..=64).contains(&run_id.len())
                || !run_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                || request_id.as_deref().is_some_and(|value| !uuid_like(value))
                || !(1..=32768).contains(&prompt.chars().count())
                || response.chars().count() > 262_144
                || !matches!(
                    status.as_str(),
                    "running" | "completed" | "failed" | "canceled"
                )
                || error_code
                    .as_deref()
                    .is_some_and(|code| !TURN_ERROR_CODES.contains(&code))
                || !matches!(provider.as_str(), "claude" | "codex")
                || !(1..=80).contains(&model.chars().count())
                || !(0..=253_402_300_799).contains(&created_at)
                || updated_at < created_at
                || updated_at > 253_402_300_799
                || completed_at.is_some_and(|value| value < created_at || value > 253_402_300_799)
                // The status/completion/error shape, re-proved per row:
                // a live turn has no completion, and an error code appears on
                // exactly the two failure statuses.
                || live != completed_at.is_none()
                || error_code.is_some() != terminal_error
            {
                return Err(AuthError::Unavailable);
            }
            cursor = id;
        }
        if rows.len() < PAGE {
            return Ok(());
        }
    }
}

/// The two additive message columns must exist with the right domain on every
/// persisted row — including the pairing SQLite cannot express as a CHECK.
async fn validate_messages<C: ConnectionTrait>(db: &C, sqlite: bool) -> Result<(), AuthError> {
    let cols = ["id", "author_kind", "agent_label"];
    let mut cursor = String::new();
    loop {
        let sql = format!(
            "SELECT {} FROM neev_messages WHERE id>$1 ORDER BY id LIMIT {PAGE}",
            type_cols(sqlite, &cols)
        );
        let rows = db
            .query_all(super::statement(db, &sql, vec![cursor.clone().into()]))
            .await
            .map_err(database_error)?;
        if rows.is_empty() {
            return Ok(());
        }
        for row in &rows {
            let id = text(row, "id", sqlite)?;
            let author_kind = text(row, "author_kind", sqlite)?;
            let agent_label = opt_text(row, "agent_label", sqlite)?;
            if !matches!(author_kind.as_str(), "human" | "agent")
                || (author_kind == "agent") != agent_label.is_some()
                || agent_label
                    .as_deref()
                    .is_some_and(|label| !(1..=80).contains(&label.chars().count()))
            {
                return Err(AuthError::Unavailable);
            }
            cursor = id;
        }
        if rows.len() < PAGE {
            return Ok(());
        }
    }
}
