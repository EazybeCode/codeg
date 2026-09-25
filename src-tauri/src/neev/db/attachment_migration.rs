//! Additive attachments (schema15). An attachment is bound to the conversation
//! it was uploaded to, so every read re-proves THAT conversation's current ACL —
//! a private one's attachment is unreadable by a same-org member who is not
//! authorized for it, even knowing the key. The same rule document images
//! follow (`image_blob_migration`).
//!
//! **Two kinds of conversation own attachments, and they are different
//! entities.** A team chat is a `neev_resources` row; an Agent Chat is a
//! `neev_agent_sessions` row with its own ownership and visibility. Rather than
//! manufacture a hidden chat resource to hang a session's files on — which
//! would put rows in a table the user's own chat list reads — an attachment
//! carries two nullable owner columns with a real foreign key each and a CHECK
//! that exactly one is set. A composite FK containing a NULL is not enforced,
//! which is precisely the behaviour wanted here.
//!
//! `neev_attachment_refs` is the turn reference, and splits the same way:
//! a chat message (`neev_messages`) or an agent turn
//! (`neev_agent_turns`, keyed by session and sequence). It is what makes an
//! attachment survive a reload and later turns instead of leaving a dangling
//! mention in the text.
//!
//! `validate` re-proves the declared shape against what is actually stored, as
//! the other validators do: an imported dump carries no CHECK/UNIQUE/FK history,
//! so storage class, column domain, key uniqueness and referential integrity are
//! re-checked and any violation refuses the whole database rather than serving a
//! half-trusted blob.
use super::{database_error, one, statement, AuthError};
use sea_orm::sea_query::{
    Alias, ColumnDef, Expr, ForeignKey, ForeignKeyAction, Index, IndexCreateStatement, Table,
    TableCreateStatement,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, QueryResult, Value};

pub(super) const SCHEMA_VERSION: i64 = 15;
pub(super) const TABLE_NAMES: &[&str] = &["neev_attachments", "neev_attachment_refs"];

const NATIVE_MAX: i64 = 2_147_483_647;
const TIME_MAX: i64 = 253_402_300_799;
/// Mirrors `content::blob::ATTACHMENT_STORAGE_CEILING` — the tenant's whole
/// storage budget, NOT a per-file limit. Used only by the snapshot verifier as a
/// shape assertion; the table CHECK deliberately carries no upper bound, because
/// baking one is what made a legitimate 44 MB attachment unstorable.
const MAX_ATTACHMENT_BYTES: i64 = 256 * 1024 * 1024;
/// Mirrors `content::attachment::MAX_ATTACHMENTS_PER_MESSAGE`.
const MAX_PER_MESSAGE: i64 = 5;
const UUID_CHARS: usize = 36;
const HASH_CHARS: usize = 64;
const MAX_NAME_CHARS: i64 = 200;
const PAGE: usize = 500;
/// Exactly what the upload sniffs and the serve path re-classifies. Images and
/// PDF are magic-byte classified; text is accepted only as valid UTF-8.
const CONTENT_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "application/pdf",
    "text/plain",
];

fn tables() -> Vec<TableCreateStatement> {
    let mut blobs = Table::create();
    blobs.table(Alias::new("neev_attachments"));
    for (name, kind) in [
        ("org_id", "i"),
        ("key", "t"),
        ("chat_id", "t?"),
        ("session_id", "t?"),
        ("file_name", "t"),
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
        if kind != "t?" {
            column.not_null();
        }
        blobs.col(&mut column);
    }
    let mut primary = Index::create();
    primary.col(Alias::new("org_id")).col(Alias::new("key"));
    blobs.primary_key(&mut primary);
    let types = CONTENT_TYPES
        .iter()
        .map(|value| format!("'{value}'"))
        .collect::<Vec<_>>()
        .join(",");
    for check in [
        "org_id>0".to_owned(),
        format!("length(key)={UUID_CHARS}"),
        // Exactly one owner, and whichever it is has to look like an id.
        "((chat_id IS NULL) <> (session_id IS NULL))".to_owned(),
        format!("(chat_id IS NULL OR length(chat_id)={UUID_CHARS})"),
        format!("(session_id IS NULL OR length(session_id)={UUID_CHARS})"),
        format!("length(file_name) BETWEEN 1 AND {MAX_NAME_CHARS}"),
        format!("content_type IN ({types})"),
        // Lower bound only. An attachment has no size limit; what bounds it is the
        // per-org storage quota, which the application enforces and may raise
        // without a table rebuild. `attachment_byte_cap_migration` relaxes the
        // ranged CHECK that databases created before this carry.
        "byte_length>=1".to_owned(),
        format!("length(sha256)={HASH_CHARS}"),
        "created_by>0".to_owned(),
        "created_at>=0".to_owned(),
    ] {
        blobs.check(Expr::cust(&check));
    }
    // The attachment inherits its owner's audience. Both foreign keys are
    // composite and carry the tenant; a composite key containing a NULL is not
    // enforced, so each row is checked against exactly the owner it declares.
    let mut fk_chat = ForeignKey::create();
    fk_chat
        .from_tbl(Alias::new("neev_attachments"))
        .from_col(Alias::new("org_id"))
        .from_col(Alias::new("chat_id"))
        .to_tbl(Alias::new("neev_resources"))
        .to_col(Alias::new("org_id"))
        .to_col(Alias::new("id"));
    blobs.foreign_key(fk_chat.on_delete(ForeignKeyAction::Cascade));
    let mut fk_session = ForeignKey::create();
    fk_session
        .from_tbl(Alias::new("neev_attachments"))
        .from_col(Alias::new("org_id"))
        .from_col(Alias::new("session_id"))
        .to_tbl(Alias::new("neev_agent_sessions"))
        .to_col(Alias::new("org_id"))
        .to_col(Alias::new("id"));
    blobs.foreign_key(fk_session.on_delete(ForeignKeyAction::Cascade));
    let mut fk_by = ForeignKey::create();
    fk_by
        .from_tbl(Alias::new("neev_attachments"))
        .from_col(Alias::new("created_by"))
        .to_tbl(Alias::new("neev_accounts"))
        .to_col(Alias::new("id"));
    blobs.foreign_key(fk_by.on_delete(ForeignKeyAction::Restrict));

    // Which attachments went with which message. `position` orders them as the
    // sender arranged them, and is bounded so a message cannot carry more than
    // the per-run cap.
    let mut links = Table::create();
    links.table(Alias::new("neev_attachment_refs"));
    for (name, kind) in [
        ("org_id", "i"),
        ("attachment_key", "t"),
        ("message_id", "t?"),
        ("turn_session_id", "t?"),
        ("turn_sequence", "l?"),
        ("position", "i"),
    ] {
        let mut column = ColumnDef::new(Alias::new(name));
        match kind {
            "i" => {
                column.integer();
            }
            "l?" => {
                column.big_integer();
            }
            _ => {
                column.text();
            }
        }
        if !kind.ends_with('?') {
            column.not_null();
        }
        links.col(&mut column);
    }
    // No primary key: the target is split across nullable columns, so there is
    // no single column set every row fills. Uniqueness and lookup are declared
    // as indexes below rather than left to the writer.
    for check in [
        "org_id>0".to_owned(),
        format!("length(attachment_key)={UUID_CHARS}"),
        // Exactly one target: a chat message, or an agent turn by (session,
        // sequence). Never both, never neither.
        "((message_id IS NULL) <> (turn_session_id IS NULL))".to_owned(),
        "((turn_session_id IS NULL) = (turn_sequence IS NULL))".to_owned(),
        format!("(message_id IS NULL OR length(message_id)={UUID_CHARS})"),
        format!("(turn_session_id IS NULL OR length(turn_session_id)={UUID_CHARS})"),
        "(turn_sequence IS NULL OR turn_sequence>=0)".to_owned(),
        format!("position BETWEEN 0 AND {}", MAX_PER_MESSAGE - 1),
    ] {
        links.check(Expr::cust(&check));
    }
    let mut fk_message = ForeignKey::create();
    fk_message
        .from_tbl(Alias::new("neev_attachment_refs"))
        .from_col(Alias::new("org_id"))
        .from_col(Alias::new("message_id"))
        .to_tbl(Alias::new("neev_messages"))
        .to_col(Alias::new("org_id"))
        .to_col(Alias::new("id"));
    links.foreign_key(fk_message.on_delete(ForeignKeyAction::Cascade));
    let mut fk_turn = ForeignKey::create();
    fk_turn
        .from_tbl(Alias::new("neev_attachment_refs"))
        .from_col(Alias::new("org_id"))
        .from_col(Alias::new("turn_session_id"))
        .from_col(Alias::new("turn_sequence"))
        .to_tbl(Alias::new("neev_agent_turns"))
        .to_col(Alias::new("org_id"))
        .to_col(Alias::new("session_id"))
        .to_col(Alias::new("sequence"));
    links.foreign_key(fk_turn.on_delete(ForeignKeyAction::Cascade));
    let mut fk_attachment = ForeignKey::create();
    fk_attachment
        .from_tbl(Alias::new("neev_attachment_refs"))
        .from_col(Alias::new("org_id"))
        .from_col(Alias::new("attachment_key"))
        .to_tbl(Alias::new("neev_attachments"))
        .to_col(Alias::new("org_id"))
        .to_col(Alias::new("key"));
    links.foreign_key(fk_attachment.on_delete(ForeignKeyAction::Cascade));
    vec![blobs, links]
}

/// Indexes, each for a query this module actually issues or a duplicate it must
/// refuse.
///
/// The three UNIQUE ones are the writer's invariants made the database's: an
/// attachment may appear at most once against a given message or turn, and two
/// attachments cannot claim the same position in one send. Both backends treat
/// NULLs in a unique index as distinct, which is exactly what makes this work
/// across the split target — a row whose `message_id` is NULL is a turn ref and
/// never collides with a message ref.
fn indexes() -> Vec<IndexCreateStatement> {
    let mut out = Vec::new();
    let mut unique_message = Index::create();
    unique_message
        .name("neev_attachment_refs_message_key")
        .table(Alias::new("neev_attachment_refs"))
        .col(Alias::new("org_id"))
        .col(Alias::new("message_id"))
        .col(Alias::new("attachment_key"))
        .unique();
    out.push(unique_message.take());
    let mut unique_message_position = Index::create();
    unique_message_position
        .name("neev_attachment_refs_message_position")
        .table(Alias::new("neev_attachment_refs"))
        .col(Alias::new("org_id"))
        .col(Alias::new("message_id"))
        .col(Alias::new("position"))
        .unique();
    out.push(unique_message_position.take());
    let mut unique_turn = Index::create();
    unique_turn
        .name("neev_attachment_refs_turn_key")
        .table(Alias::new("neev_attachment_refs"))
        .col(Alias::new("org_id"))
        .col(Alias::new("turn_session_id"))
        .col(Alias::new("turn_sequence"))
        .col(Alias::new("attachment_key"))
        .unique();
    out.push(unique_turn.take());
    // Lookup: `for_turns` pages by (session, sequence).
    let mut turn_lookup = Index::create();
    turn_lookup
        .name("neev_attachment_refs_turn")
        .table(Alias::new("neev_attachment_refs"))
        .col(Alias::new("org_id"))
        .col(Alias::new("turn_session_id"))
        .col(Alias::new("turn_sequence"));
    out.push(turn_lookup.take());
    // The orphan sweep's LEFT JOIN, and the cascade from an attachment.
    let mut ref_key = Index::create();
    ref_key
        .name("neev_attachment_refs_attachment")
        .table(Alias::new("neev_attachment_refs"))
        .col(Alias::new("org_id"))
        .col(Alias::new("attachment_key"));
    out.push(ref_key.take());
    // Owner lookups, one per owner column.
    for (name, column) in [
        ("neev_attachments_chat", "chat_id"),
        ("neev_attachments_session", "session_id"),
    ] {
        let mut index = Index::create();
        index
            .name(name)
            .table(Alias::new("neev_attachments"))
            .col(Alias::new("org_id"))
            .col(Alias::new(column));
        out.push(index.take());
    }
    // The orphan sweep orders by age.
    let mut created = Index::create();
    created
        .name("neev_attachments_created")
        .table(Alias::new("neev_attachments"))
        .col(Alias::new("org_id"))
        .col(Alias::new("created_at"));
    out.push(created.take());
    out
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
    // Same discipline as every migration before it: publish nothing that fails
    // its own validator.
    validate(tx).await
}

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

pub(super) async fn validate<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    // Storage class, per backend, exactly as the sibling validators check it.
    if db.get_database_backend() == DbBackend::Postgres {
        let row = one(
            db,
            "SELECT COUNT(*) AS c FROM information_schema.columns WHERE table_schema=current_schema() AND ((table_name='neev_attachments' AND ((column_name IN ('org_id','created_by') AND data_type<>'integer') OR (column_name IN ('byte_length','created_at') AND data_type<>'bigint') OR (column_name IN ('key','chat_id','session_id','file_name','content_type','sha256') AND data_type<>'text'))) OR (table_name='neev_attachment_refs' AND ((column_name IN ('org_id','position') AND data_type<>'integer') OR (column_name IN ('message_id','attachment_key','turn_session_id') AND data_type<>'text') OR (column_name='turn_sequence' AND data_type<>'bigint'))))",
            vec![],
        )
        .await?
        .ok_or(AuthError::Unavailable)?;
        let wrong: i64 = row.try_get("", "c").map_err(database_error)?;
        if wrong != 0 {
            return Err(AuthError::Unavailable);
        }
    }
    // Row-level domain and referential integrity, paged.
    let mut cursor = String::new();
    loop {
        let rows = fetch_page(
            db,
            "SELECT org_id,key,chat_id,session_id,file_name,content_type,byte_length,sha256,created_by,created_at FROM neev_attachments WHERE key>$1 ORDER BY key LIMIT 500",
            vec![cursor.clone().into()],
        )
        .await?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let org_id: i32 = row.try_get("", "org_id").map_err(database_error)?;
            let key: String = row.try_get("", "key").map_err(database_error)?;
            let chat_id: Option<String> = row.try_get("", "chat_id").map_err(database_error)?;
            let session_id: Option<String> =
                row.try_get("", "session_id").map_err(database_error)?;
            let file_name: String = row.try_get("", "file_name").map_err(database_error)?;
            let content_type: String = row.try_get("", "content_type").map_err(database_error)?;
            let byte_length: i64 = row.try_get("", "byte_length").map_err(database_error)?;
            let sha256: String = row.try_get("", "sha256").map_err(database_error)?;
            let created_by: i32 = row.try_get("", "created_by").map_err(database_error)?;
            let created_at: i64 = row.try_get("", "created_at").map_err(database_error)?;
            let name_chars = file_name.chars().count() as i64;
            if org_id <= 0
                || i64::from(org_id) > NATIVE_MAX
                || !canonical_uuid(&key)
                // Exactly one owner, and it looks like an id.
                || chat_id.is_some() == session_id.is_some()
                || chat_id.as_deref().is_some_and(|value| !canonical_uuid(value))
                || session_id
                    .as_deref()
                    .is_some_and(|value| !canonical_uuid(value))
                || name_chars < 1
                || name_chars > MAX_NAME_CHARS
                || !CONTENT_TYPES.contains(&content_type.as_str())
                || byte_length < 1
                || byte_length > MAX_ATTACHMENT_BYTES
                || !canonical_hash(&sha256)
                || created_by <= 0
                || i64::from(created_by) > NATIVE_MAX
                || created_at < 0
                || created_at > TIME_MAX
            {
                return Err(AuthError::Unavailable);
            }
            cursor = key;
        }
        if rows.len() < PAGE {
            break;
        }
    }
    // Every attachment points at a chat in its own tenant, and every link points
    // at both an attachment and a message in that same tenant.
    for sql in [
        "SELECT COUNT(*) AS c FROM neev_attachments a LEFT JOIN neev_resources r ON r.org_id=a.org_id AND r.id=a.chat_id WHERE a.chat_id IS NOT NULL AND r.id IS NULL",
        "SELECT COUNT(*) AS c FROM neev_attachments a LEFT JOIN neev_agent_sessions s ON s.org_id=a.org_id AND s.id=a.session_id WHERE a.session_id IS NOT NULL AND s.id IS NULL",
        "SELECT COUNT(*) AS c FROM neev_attachments a LEFT JOIN neev_accounts u ON u.id=a.created_by WHERE u.id IS NULL",
        "SELECT COUNT(*) AS c FROM neev_attachment_refs l LEFT JOIN neev_attachments a ON a.org_id=l.org_id AND a.key=l.attachment_key WHERE a.key IS NULL",
        "SELECT COUNT(*) AS c FROM neev_attachment_refs l LEFT JOIN neev_messages m ON m.org_id=l.org_id AND m.id=l.message_id WHERE l.message_id IS NOT NULL AND m.id IS NULL",
        "SELECT COUNT(*) AS c FROM neev_attachment_refs l LEFT JOIN neev_agent_turns t ON t.org_id=l.org_id AND t.session_id=l.turn_session_id AND t.sequence=l.turn_sequence WHERE l.turn_session_id IS NOT NULL AND t.session_id IS NULL",
        // A ref must not carry an attachment from a DIFFERENT conversation than
        // the turn it hangs on: that would widen the attachment's audience.
        "SELECT COUNT(*) AS c FROM neev_attachment_refs l JOIN neev_attachments a ON a.org_id=l.org_id AND a.key=l.attachment_key JOIN neev_messages m ON m.org_id=l.org_id AND m.id=l.message_id WHERE a.chat_id IS NULL OR m.chat_id<>a.chat_id",
        "SELECT COUNT(*) AS c FROM neev_attachment_refs l JOIN neev_attachments a ON a.org_id=l.org_id AND a.key=l.attachment_key WHERE l.turn_session_id IS NOT NULL AND (a.session_id IS NULL OR l.turn_session_id<>a.session_id)",
    ] {
        let row = one(db, sql, vec![]).await?.ok_or(AuthError::Unavailable)?;
        let dangling: i64 = row.try_get("", "c").map_err(database_error)?;
        if dangling != 0 {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}
