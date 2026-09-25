//! Additive mentions (schema16). A mention is an IDENTITY stored beside the
//! message, never a name recovered from its text: the row says which account
//! was meant and which run of characters says so, so reading a message back can
//! never turn "@Ana" into whichever Ana the reader happens to guess at.
//!
//! `start_offset`/`length` are Unicode CODE POINTS into the message body. Both
//! sides of the wire can count those exactly — `char_indices()` here,
//! `Array.from` in the browser — where byte offsets would need an encoder on one
//! side and UTF-16 surrogate care on the other.
//!
//! `read_at` is the whole of the notification state. A mention row IS the
//! notice: there is no second queue to fall out of step with the message, and no
//! delivery that outlives the reader's authority — every read re-proves the
//! conversation's current ACL before the row is shown.
//!
//! Only the human send path writes here. An agent reply is published by a server
//! path that records no rows, so text an agent emits can never become a ping.
//!
//! `validate` re-proves the declared shape against what is actually stored, as
//! the other validators do: an imported dump carries no CHECK/UNIQUE/FK history,
//! so storage class, column domain and referential integrity are re-checked and
//! any violation refuses the whole database.
use super::{database_error, one, statement, AuthError};
use sea_orm::sea_query::{
    Alias, ColumnDef, Expr, ForeignKey, ForeignKeyAction, Index, IndexCreateStatement, Table,
    TableCreateStatement,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, QueryResult, Value};

pub(super) const SCHEMA_VERSION: i64 = 16;
pub(super) const TABLE_NAMES: &[&str] = &["neev_message_mentions"];

const NATIVE_MAX: i64 = 2_147_483_647;
const TIME_MAX: i64 = 253_402_300_799;
/// Mirrors `content::mentions::MAX_MENTIONS_PER_MESSAGE`.
const MAX_PER_MESSAGE: i64 = 10;
/// Mirrors `neev_messages.body`, so an offset can never point past any body.
const MAX_BODY_CHARS: i64 = 32768;
/// `@` plus the longest display name `neev_accounts` admits.
const MAX_MENTION_CHARS: i64 = 201;
const UUID_CHARS: usize = 36;
const PAGE: usize = 500;

fn tables() -> Vec<TableCreateStatement> {
    let mut mentions = Table::create();
    mentions.table(Alias::new("neev_message_mentions"));
    for (name, kind) in [
        ("org_id", "i"),
        ("message_id", "t"),
        ("position", "i"),
        ("user_id", "i"),
        ("start_offset", "l"),
        ("length", "l"),
        ("read_at", "l?"),
    ] {
        let mut column = ColumnDef::new(Alias::new(name));
        match kind {
            "i" => {
                column.integer();
            }
            "t" => {
                column.text();
            }
            _ => {
                column.big_integer();
            }
        }
        if !kind.ends_with('?') {
            column.not_null();
        }
        mentions.col(&mut column);
    }
    // One row per place a person is named, ordered as they were said.
    let mut primary = Index::create();
    primary
        .col(Alias::new("org_id"))
        .col(Alias::new("message_id"))
        .col(Alias::new("position"));
    mentions.primary_key(&mut primary);
    for check in [
        "org_id>0".to_owned(),
        format!("length(message_id)={UUID_CHARS}"),
        format!("position BETWEEN 0 AND {}", MAX_PER_MESSAGE - 1),
        "user_id>0".to_owned(),
        format!("start_offset BETWEEN 0 AND {MAX_BODY_CHARS}"),
        format!("length BETWEEN 2 AND {MAX_MENTION_CHARS}"),
        format!("start_offset+length<={MAX_BODY_CHARS}"),
        "(read_at IS NULL OR read_at>=0)".to_owned(),
    ] {
        mentions.check(Expr::cust(&check));
    }
    // The mention lives and dies with the message: a deleted conversation takes
    // its notices with it, which is the only way a notice cannot outlive the
    // thing it points at.
    let mut fk_message = ForeignKey::create();
    fk_message
        .from_tbl(Alias::new("neev_message_mentions"))
        .from_col(Alias::new("org_id"))
        .from_col(Alias::new("message_id"))
        .to_tbl(Alias::new("neev_messages"))
        .to_col(Alias::new("org_id"))
        .to_col(Alias::new("id"));
    mentions.foreign_key(fk_message.on_delete(ForeignKeyAction::Cascade));
    // Restrict, not cascade: an account that is still named by a message cannot
    // be erased out from under it, the same rule an attachment's uploader has.
    let mut fk_user = ForeignKey::create();
    fk_user
        .from_tbl(Alias::new("neev_message_mentions"))
        .from_col(Alias::new("user_id"))
        .to_tbl(Alias::new("neev_accounts"))
        .to_col(Alias::new("id"));
    mentions.foreign_key(fk_user.on_delete(ForeignKeyAction::Restrict));
    vec![mentions]
}

fn indexes() -> Vec<IndexCreateStatement> {
    let mut out = Vec::new();
    // The inbox: one reader's mentions, unread first. Every query this module
    // serves starts here.
    let mut inbox = Index::create();
    inbox
        .name("neev_message_mentions_inbox")
        .table(Alias::new("neev_message_mentions"))
        .col(Alias::new("user_id"))
        .col(Alias::new("org_id"))
        .col(Alias::new("read_at"));
    out.push(inbox.take());
    // Reading a conversation asks for the spans of a page of messages.
    let mut by_message = Index::create();
    by_message
        .name("neev_message_mentions_message")
        .table(Alias::new("neev_message_mentions"))
        .col(Alias::new("org_id"))
        .col(Alias::new("message_id"));
    out.push(by_message.take());
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
    if db.get_database_backend() == DbBackend::Postgres {
        let row = one(
            db,
            "SELECT COUNT(*) AS c FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='neev_message_mentions' AND ((column_name IN ('org_id','position','user_id') AND data_type<>'integer') OR (column_name IN ('start_offset','length','read_at') AND data_type<>'bigint') OR (column_name='message_id' AND data_type<>'text'))",
            vec![],
        )
        .await?
        .ok_or(AuthError::Unavailable)?;
        let wrong: i64 = row.try_get("", "c").map_err(database_error)?;
        if wrong != 0 {
            return Err(AuthError::Unavailable);
        }
    }
    // Row-level domain, paged by the primary key's leading columns.
    let mut cursor = (i32::MIN, String::new(), i32::MIN);
    loop {
        let rows = fetch_page(
            db,
            "SELECT org_id,message_id,position,user_id,start_offset,length,read_at FROM neev_message_mentions WHERE (org_id,message_id,position)>($1,$2,$3) ORDER BY org_id,message_id,position LIMIT 500",
            vec![cursor.0.into(), cursor.1.clone().into(), cursor.2.into()],
        )
        .await?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let org_id: i32 = row.try_get("", "org_id").map_err(database_error)?;
            let message_id: String = row.try_get("", "message_id").map_err(database_error)?;
            let position: i32 = row.try_get("", "position").map_err(database_error)?;
            let user_id: i32 = row.try_get("", "user_id").map_err(database_error)?;
            let start_offset: i64 = row.try_get("", "start_offset").map_err(database_error)?;
            let length: i64 = row.try_get("", "length").map_err(database_error)?;
            let read_at: Option<i64> = row.try_get("", "read_at").map_err(database_error)?;
            if org_id <= 0
                || i64::from(org_id) > NATIVE_MAX
                || !canonical_uuid(&message_id)
                || i64::from(position) < 0
                || i64::from(position) >= MAX_PER_MESSAGE
                || user_id <= 0
                || i64::from(user_id) > NATIVE_MAX
                || start_offset < 0
                || length < 2
                || length > MAX_MENTION_CHARS
                || start_offset + length > MAX_BODY_CHARS
                || read_at.is_some_and(|value| !(0..=TIME_MAX).contains(&value))
            {
                return Err(AuthError::Unavailable);
            }
            cursor = (org_id, message_id, position);
        }
        if rows.len() < PAGE {
            break;
        }
    }
    // Every mention points at a message in its own tenant and at an account that
    // exists, and no two rows in one message claim the same person twice at the
    // same place.
    for sql in [
        "SELECT COUNT(*) AS c FROM neev_message_mentions n LEFT JOIN neev_messages m ON m.org_id=n.org_id AND m.id=n.message_id WHERE m.id IS NULL",
        "SELECT COUNT(*) AS c FROM neev_message_mentions n LEFT JOIN neev_accounts a ON a.id=n.user_id WHERE a.id IS NULL",
        "SELECT COUNT(*) AS c FROM (SELECT org_id,message_id,position FROM neev_message_mentions GROUP BY org_id,message_id,position HAVING COUNT(*)>1) d",
    ] {
        let row = one(db, sql, vec![]).await?.ok_or(AuthError::Unavailable)?;
        let broken: i64 = row.try_get("", "c").map_err(database_error)?;
        if broken != 0 {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}
