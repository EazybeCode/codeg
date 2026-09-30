//! Schema 19 — steering inputs for mid-run user follow-ups (spec steering contract).
//!
//! Follows schema 18 (library). Adds a table to persist user follow-up text
//! sent mid-run, so retries cannot duplicate and future history includes accepted
//! input.
//!
//! **Persistence-first**: Input is written BEFORE transport to the provider. The
//! `accepted` column is initially false and flipped to true only after provider
//! acknowledgment. This means:
//! - A retry with the same `request_id` is identified and returns the stored outcome
//! - A crashed process leaves unacknowledged inputs visible, never lost
//! - The turn's history includes every follow-up the user sent
//!
//! **responseOffset**: When the input was accepted, this is the character offset
//! into the turn's response at which it arrived. Frontend uses it to interleave
//! user messages at the correct position in the display.

use super::{database_error, one, statement, AuthError};
use sea_orm::sea_query::{
    Alias, ColumnDef, Expr, ForeignKey, ForeignKeyAction, Index, IndexCreateStatement, Table,
    TableCreateStatement,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, QueryResult, Value};

pub(super) const SCHEMA_VERSION: i64 = 19;
pub(super) const TABLE_NAMES: &[&str] = &["neev_steer_inputs"];

const UUID_CHARS: usize = 36;
const MAX_TEXT_CHARS: i64 = 32768;
const TIME_MAX: i64 = 253_402_300_799;
const PAGE: usize = 500;

fn table() -> TableCreateStatement {
    let mut steer = Table::create();
    steer.table(Alias::new("neev_steer_inputs"));
    for (name, kind) in [
        ("org_id", "i"),
        ("session_id", "t"),
        ("turn_sequence", "l"),
        ("request_id", "t"),
        ("text", "t"),
        ("accepted", "b"),
        ("response_offset", "l?"),
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
            "l?" => {
                column.big_integer();
            }
            "b" => {
                column.boolean();
            }
            _ => {
                column.text();
            }
        }
        if !kind.ends_with('?') {
            column.not_null();
        }
        steer.col(&mut column);
    }
    // Primary key: (org_id, session_id, turn_sequence, request_id) — idempotency
    let mut primary = Index::create();
    primary
        .col(Alias::new("org_id"))
        .col(Alias::new("session_id"))
        .col(Alias::new("turn_sequence"))
        .col(Alias::new("request_id"));
    steer.primary_key(&mut primary);

    for check in [
        "org_id>0",
        &format!("length(session_id)={UUID_CHARS}"),
        "turn_sequence>=1",
        &format!("length(request_id)={UUID_CHARS}"),
        &format!("length(text) BETWEEN 1 AND {MAX_TEXT_CHARS}"),
        // response_offset is set only when accepted (PostgreSQL-portable boolean syntax)
        "(accepted=FALSE AND response_offset IS NULL) OR (accepted=TRUE AND response_offset IS NOT NULL)",
        "(response_offset IS NULL OR response_offset>=0)",
        "created_at>=0",
    ] {
        steer.check(Expr::cust(check));
    }

    // FK to the turn
    let mut fk_turn = ForeignKey::create();
    fk_turn
        .from_tbl(Alias::new("neev_steer_inputs"))
        .from_col(Alias::new("org_id"))
        .from_col(Alias::new("session_id"))
        .from_col(Alias::new("turn_sequence"))
        .to_tbl(Alias::new("neev_agent_turns"))
        .to_col(Alias::new("org_id"))
        .to_col(Alias::new("session_id"))
        .to_col(Alias::new("sequence"));
    steer.foreign_key(fk_turn.on_delete(ForeignKeyAction::Cascade));

    steer
}

fn indexes() -> Vec<IndexCreateStatement> {
    let mut out = Vec::new();

    // Lookup: inputs for a turn, ordered by creation
    let mut turn_lookup = Index::create();
    turn_lookup
        .name("neev_steer_inputs_turn")
        .table(Alias::new("neev_steer_inputs"))
        .col(Alias::new("org_id"))
        .col(Alias::new("session_id"))
        .col(Alias::new("turn_sequence"))
        .col(Alias::new("created_at"));
    out.push(turn_lookup.take());

    out
}

pub(super) async fn upgrade(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    let backend = tx.get_database_backend();
    tx.execute(backend.build(&table()))
        .await
        .map_err(database_error)?;
    for index in indexes() {
        tx.execute(backend.build(&index))
            .await
            .map_err(database_error)?;
    }
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
    // Storage class check for Postgres
    if db.get_database_backend() == DbBackend::Postgres {
        let row = one(
            db,
            "SELECT COUNT(*) AS c FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='neev_steer_inputs' AND ((column_name='org_id' AND data_type<>'integer') OR (column_name IN ('turn_sequence','response_offset','created_at') AND data_type<>'bigint') OR (column_name IN ('session_id','request_id','text') AND data_type<>'text') OR (column_name='accepted' AND data_type<>'boolean'))",
            vec![],
        )
        .await?
        .ok_or(AuthError::Unavailable)?;
        let wrong: i64 = row.try_get("", "c").map_err(database_error)?;
        if wrong != 0 {
            return Err(AuthError::Unavailable);
        }
    }

    // Row-level validation, paged
    let mut cursor_session = String::new();
    let mut cursor_seq: i64 = 0;
    let mut cursor_request = String::new();
    loop {
        let rows = fetch_page(
            db,
            "SELECT org_id,session_id,turn_sequence,request_id,text,accepted,response_offset,created_at FROM neev_steer_inputs WHERE (session_id,turn_sequence,request_id)>($1,$2,$3) ORDER BY session_id,turn_sequence,request_id LIMIT 500",
            vec![cursor_session.clone().into(), cursor_seq.into(), cursor_request.clone().into()],
        )
        .await?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let org_id: i32 = row.try_get("", "org_id").map_err(database_error)?;
            let session_id: String = row.try_get("", "session_id").map_err(database_error)?;
            let turn_sequence: i64 = row.try_get("", "turn_sequence").map_err(database_error)?;
            let request_id: String = row.try_get("", "request_id").map_err(database_error)?;
            let text: String = row.try_get("", "text").map_err(database_error)?;
            let accepted: bool = row.try_get("", "accepted").map_err(database_error)?;
            let response_offset: Option<i64> =
                row.try_get("", "response_offset").map_err(database_error)?;
            let created_at: i64 = row.try_get("", "created_at").map_err(database_error)?;

            let text_chars = text.chars().count() as i64;
            if org_id <= 0
                || !canonical_uuid(&session_id)
                || turn_sequence < 1
                || !canonical_uuid(&request_id)
                || text_chars < 1
                || text_chars > MAX_TEXT_CHARS
                // accepted and response_offset must match
                || (accepted && response_offset.is_none())
                || (!accepted && response_offset.is_some())
                || response_offset.is_some_and(|offset| offset < 0)
                || created_at < 0
                || created_at > TIME_MAX
            {
                return Err(AuthError::Unavailable);
            }
            cursor_session = session_id;
            cursor_seq = turn_sequence;
            cursor_request = request_id;
        }
        if rows.len() < PAGE {
            break;
        }
    }

    // Referential integrity: every steer input points at an existing turn
    let row = one(
        db,
        "SELECT COUNT(*) AS c FROM neev_steer_inputs s LEFT JOIN neev_agent_turns t ON t.org_id=s.org_id AND t.session_id=s.session_id AND t.sequence=s.turn_sequence WHERE t.session_id IS NULL",
        vec![],
    )
    .await?
    .ok_or(AuthError::Unavailable)?;
    let dangling: i64 = row.try_get("", "c").map_err(database_error)?;
    if dangling != 0 {
        return Err(AuthError::Unavailable);
    }

    Ok(())
}

// --- CRUD functions for steer inputs ---

/// A stored steering input, for loading from the database.
#[derive(Debug, Clone)]
pub(crate) struct StoredSteerInput {
    pub(crate) request_id: String,
    pub(crate) text: String,
    pub(crate) accepted: bool,
    pub(crate) response_offset: Option<i64>,
    pub(crate) created_at: i64,
}

/// Result of attempting to reserve a steering input for transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReserveResult {
    /// This caller won the reservation and must transport.
    Won,
    /// Already recorded with matching text — retry is safe (check accepted state).
    AlreadyExists,
    /// Already recorded with DIFFERENT text — reject this request.
    TextMismatch,
}

/// Atomically reserve a steering input BEFORE transport to the provider.
///
/// This implements persistence-first semantics with atomic winner-only transport:
/// - Uses INSERT ON CONFLICT to atomically reserve the `request_id`
/// - Returns `Won` only for the first caller, who must transport
/// - Returns `AlreadyExists` if same text already recorded (safe to check accepted)
/// - Returns `TextMismatch` if different text was recorded (reject)
///
/// The winner-only semantics prevent duplicate transport: concurrent retries
/// race to INSERT, exactly one wins, others see the existing row.
///
/// The input is stored with `accepted=false`; call `accept_steer_input` after
/// the provider acknowledges it.
pub(crate) async fn record_steer_input<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    session_id: &str,
    turn_sequence: i64,
    request_id: &str,
    text: &str,
    created_at: i64,
) -> Result<ReserveResult, AuthError> {
    // SQLite and PostgreSQL both support INSERT ... ON CONFLICT DO NOTHING
    // which gives atomic winner-only semantics. The first to insert wins.
    let affected = execute(
        db,
        "INSERT INTO neev_steer_inputs(org_id,session_id,turn_sequence,request_id,text,accepted,response_offset,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(org_id,session_id,turn_sequence,request_id) DO NOTHING",
        vec![
            org_id.into(),
            session_id.into(),
            turn_sequence.into(),
            request_id.into(),
            text.into(),
            false.into(), // accepted = false until provider ack
            Value::BigInt(None), // response_offset = NULL
            created_at.into(),
        ],
    )
    .await?;

    if affected > 0 {
        // We won the reservation — we must transport
        return Ok(ReserveResult::Won);
    }

    // Row already exists — verify the text matches (binding check)
    let existing = one(
        db,
        "SELECT text FROM neev_steer_inputs WHERE org_id=$1 AND session_id=$2 AND turn_sequence=$3 AND request_id=$4",
        vec![org_id.into(), session_id.into(), turn_sequence.into(), request_id.into()],
    )
    .await?
    .ok_or(AuthError::Unavailable)?; // Should exist if we got here

    let stored_text: String = existing.try_get("", "text").map_err(database_error)?;
    if stored_text == text {
        Ok(ReserveResult::AlreadyExists)
    } else {
        Ok(ReserveResult::TextMismatch)
    }
}

/// Mark a steering input as accepted after provider acknowledgment.
///
/// Sets `accepted=true` and records the `response_offset` (character position
/// in the response at which the input was inserted).
pub(crate) async fn accept_steer_input<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    session_id: &str,
    turn_sequence: i64,
    request_id: &str,
    response_offset: i64,
) -> Result<(), AuthError> {
    let affected = execute(
        db,
        "UPDATE neev_steer_inputs SET accepted=$1, response_offset=$2 WHERE org_id=$3 AND session_id=$4 AND turn_sequence=$5 AND request_id=$6 AND accepted=$7",
        vec![
            true.into(),
            response_offset.into(),
            org_id.into(),
            session_id.into(),
            turn_sequence.into(),
            request_id.into(),
            false.into(), // Only update if not already accepted
        ],
    )
    .await?;
    if affected == 0 {
        let existing = get_steer_input(db, org_id, session_id, turn_sequence, request_id).await?;
        if !existing.is_some_and(|input| input.accepted) {
            return Err(AuthError::Unavailable);
        }
    }
    Ok(())
}

/// Load all accepted steering inputs for a turn, sorted by response_offset.
///
/// Only returns inputs where `accepted=true`, so the frontend sees only what
/// the provider actually acknowledged.
pub(crate) async fn list_steer_inputs<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    session_id: &str,
    turn_sequence: i64,
) -> Result<Vec<StoredSteerInput>, AuthError> {
    let rows = db
        .query_all(statement(
            db,
            "SELECT request_id,text,accepted,response_offset,created_at FROM neev_steer_inputs WHERE org_id=$1 AND session_id=$2 AND turn_sequence=$3 AND accepted=$4 ORDER BY response_offset ASC",
            vec![org_id.into(), session_id.into(), turn_sequence.into(), true.into()],
        ))
        .await
        .map_err(database_error)?;

    let mut inputs = Vec::with_capacity(rows.len());
    for row in rows {
        inputs.push(StoredSteerInput {
            request_id: row.try_get("", "request_id").map_err(database_error)?,
            text: row.try_get("", "text").map_err(database_error)?,
            accepted: row.try_get("", "accepted").map_err(database_error)?,
            response_offset: row.try_get("", "response_offset").map_err(database_error)?,
            created_at: row.try_get("", "created_at").map_err(database_error)?,
        });
    }
    Ok(inputs)
}

/// Check if a steering input exists and get its current state.
///
/// Returns `Some(StoredSteerInput)` if found, `None` if not. This is used for
/// idempotency checking — if an input already exists and was accepted, the
/// HTTP handler can return immediately without re-sending to the provider.
pub(crate) async fn get_steer_input<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    session_id: &str,
    turn_sequence: i64,
    request_id: &str,
) -> Result<Option<StoredSteerInput>, AuthError> {
    let row = one(
        db,
        "SELECT request_id,text,accepted,response_offset,created_at FROM neev_steer_inputs WHERE org_id=$1 AND session_id=$2 AND turn_sequence=$3 AND request_id=$4",
        vec![org_id.into(), session_id.into(), turn_sequence.into(), request_id.into()],
    )
    .await?;

    match row {
        Some(row) => Ok(Some(StoredSteerInput {
            request_id: row.try_get("", "request_id").map_err(database_error)?,
            text: row.try_get("", "text").map_err(database_error)?,
            accepted: row.try_get("", "accepted").map_err(database_error)?,
            response_offset: row.try_get("", "response_offset").map_err(database_error)?,
            created_at: row.try_get("", "created_at").map_err(database_error)?,
        })),
        None => Ok(None),
    }
}

async fn execute<C: ConnectionTrait>(db: &C, sql: &str, args: Vec<Value>) -> Result<u64, AuthError> {
    let result = db
        .execute(statement(db, sql, args))
        .await
        .map_err(database_error)?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unit tests for ReserveResult semantics (no DB required)
    #[test]
    fn reserve_result_variants_are_distinct() {
        // Ensure all variants are distinguishable
        assert_ne!(ReserveResult::Won, ReserveResult::AlreadyExists);
        assert_ne!(ReserveResult::Won, ReserveResult::TextMismatch);
        assert_ne!(ReserveResult::AlreadyExists, ReserveResult::TextMismatch);
    }

    #[test]
    fn canonical_uuid_accepts_valid_lowercase_hyphenated() {
        assert!(canonical_uuid("11111111-1111-4111-8111-111111111111"));
        assert!(canonical_uuid("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"));
        assert!(canonical_uuid("00000000-0000-4000-8000-000000000000"));
    }

    #[test]
    fn canonical_uuid_rejects_uppercase() {
        // Uppercase is valid UUID but not canonical (must be lowercase hyphenated)
        assert!(!canonical_uuid("AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE"));
        assert!(!canonical_uuid("11111111-1111-4111-8111-11111111111A"));
    }

    #[test]
    fn canonical_uuid_rejects_no_hyphens() {
        assert!(!canonical_uuid("11111111111141118111111111111111"));
    }

    #[test]
    fn canonical_uuid_rejects_wrong_length() {
        assert!(!canonical_uuid("11111111-1111-4111-8111-1111111111"));
        assert!(!canonical_uuid("11111111-1111-4111-8111-11111111111111"));
    }

    #[test]
    fn canonical_uuid_rejects_malformed() {
        assert!(!canonical_uuid("not-a-uuid"));
        assert!(!canonical_uuid(""));
        assert!(!canonical_uuid("11111111-1111-4111-8111-gggggggggggg"));
    }

    #[test]
    fn stored_steer_input_debug_impl_exists() {
        // Ensure Debug is derived for logging/debugging
        let input = StoredSteerInput {
            request_id: "11111111-1111-4111-8111-111111111111".into(),
            text: "test".into(),
            accepted: false,
            response_offset: None,
            created_at: 1_700_000_000,
        };
        let debug_str = format!("{:?}", input);
        assert!(debug_str.contains("StoredSteerInput"));
        assert!(debug_str.contains("test"));
    }

    #[test]
    fn stored_steer_input_clone_is_independent() {
        let original = StoredSteerInput {
            request_id: "11111111-1111-4111-8111-111111111111".into(),
            text: "original".into(),
            accepted: true,
            response_offset: Some(42),
            created_at: 1_700_000_000,
        };
        let cloned = original.clone();
        assert_eq!(cloned.request_id, original.request_id);
        assert_eq!(cloned.text, original.text);
        assert_eq!(cloned.accepted, original.accepted);
        assert_eq!(cloned.response_offset, original.response_offset);
        assert_eq!(cloned.created_at, original.created_at);
    }
}
