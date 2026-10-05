//! Eligibility and readability: the leak guard (spec §4 + Amendment 1, A4).
//!
//! `eligible(r) = member_can_read(r) AND NOT archived(r) AND NOT archived(bound chat)`,
//! where `member_can_read` mirrors `content/access.rs::authorized` for a
//! NON-creator member of the same org. If `access.rs` changes, this SQL must
//! change; `access_tests::eligibility_matches_access_*` enforces parity and
//! the implication `eligible ⇒ authorized(read)`.

use super::types::*;
use crate::neev::db;
use sea_orm::{ConnectionTrait, QueryResult};
use std::collections::HashMap;

/// `r` is the resource alias. Mirrors `access.rs` for a non-creator member,
/// plus the archived exclusion (Amendment 1 §A1.3, verbatim).
pub(crate) fn eligible_predicate(r: &str) -> String {
    // Schema 24: nothing inside a restricted folder or project is org-wide,
    // whatever its own visibility says — by its own node, or its bound chat's.
    format!(
        "({r}.archived=false AND {r}.kind IN ('document','chat') AND (\
({r}.origin_chat_id IS NULL AND {r}.visibility='organization' AND NOT EXISTS (SELECT 1 FROM neev_restricted_nodes x WHERE x.org_id={r}.org_id AND x.node_id={r}.node_id)) OR \
({r}.origin_chat_id IS NOT NULL AND {r}.kind='document' AND NOT EXISTS (SELECT 1 FROM neev_restricted_nodes x WHERE x.org_id={r}.org_id AND x.node_id={r}.node_id) AND EXISTS (SELECT 1 FROM neev_resources c \
WHERE c.org_id={r}.org_id AND c.id={r}.origin_chat_id AND c.kind='chat' AND c.origin_chat_id IS NULL \
AND c.visibility='organization' AND c.archived=false AND NOT EXISTS (SELECT 1 FROM neev_restricted_nodes y WHERE y.org_id=c.org_id AND y.node_id=c.node_id)))))"
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EligibleSource {
    pub id: String,
    pub kind: SourceKind,
    pub title: String,
    pub node_id: i32,
    pub auth_revision: i64,
    pub content_revision: i64,
    pub creator_id: i32,
}

const COLUMNS: &str = "r.id,r.kind,r.title,r.node_id,r.auth_revision,r.content_revision,r.creator_id";

fn row_to_source(row: &QueryResult) -> Result<EligibleSource, KnowledgeError> {
    let kind: String = row.try_get("", "kind").map_err(db_error)?;
    Ok(EligibleSource {
        id: row.try_get("", "id").map_err(db_error)?,
        kind: SourceKind::parse(&kind).ok_or_else(KnowledgeError::storage)?,
        title: row.try_get("", "title").map_err(db_error)?,
        node_id: row.try_get("", "node_id").map_err(db_error)?,
        auth_revision: row.try_get("", "auth_revision").map_err(db_error)?,
        content_revision: row.try_get("", "content_revision").map_err(db_error)?,
        creator_id: row.try_get("", "creator_id").map_err(db_error)?,
    })
}

pub(crate) async fn eligible_sources<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
) -> Result<Vec<EligibleSource>, KnowledgeError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM neev_resources r WHERE r.org_id=$1 AND {} ORDER BY r.id",
        eligible_predicate("r")
    );
    let rows = db
        .query_all(db::statement(db, &sql, vec![org_id.into()]))
        .await
        .map_err(db_error)?;
    rows.iter().map(row_to_source).collect()
}

pub(crate) async fn is_eligible<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    source_id: &str,
) -> Result<Option<EligibleSource>, KnowledgeError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM neev_resources r WHERE r.org_id=$1 AND r.id=$2 AND {}",
        eligible_predicate("r")
    );
    match db::one(db, &sql, vec![org_id.into(), source_id.to_owned().into()]).await? {
        Some(row) => Ok(Some(row_to_source(&row)?)),
        None => Ok(None),
    }
}

/// A page revision is readable iff EVERY dependency snapshot's source is
/// currently eligible. `unreadable_sources` counts distinct sources.
pub(crate) async fn revision_readable<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    page_id: &str,
    revision: i32,
) -> Result<Readable, KnowledgeError> {
    let sql = format!(
        "SELECT COUNT(DISTINCT s.source_id) AS bad FROM neev_knowledge_revision_deps d \
         JOIN neev_knowledge_snapshots s ON s.org_id=d.org_id AND s.id=d.snapshot_id \
         JOIN neev_resources r ON r.org_id=s.org_id AND r.id=s.source_id \
         WHERE d.org_id=$1 AND d.page_id=$2 AND d.revision=$3 AND NOT {}",
        eligible_predicate("r")
    );
    let row = db::one(
        db,
        &sql,
        vec![org_id.into(), page_id.to_owned().into(), revision.into()],
    )
    .await?
    .ok_or_else(KnowledgeError::storage)?;
    let bad: i64 = row.try_get("", "bad").map_err(db_error)?;
    Ok(if bad == 0 {
        Readable::Readable
    } else {
        Readable::Withheld {
            unreadable_sources: bad,
        }
    })
}

/// Readability of every ACTIVE page's head revision in one query.
pub(crate) async fn readable_heads<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
) -> Result<HashMap<String, Readable>, KnowledgeError> {
    let sql = format!(
        "SELECT p.id AS page_id, (SELECT COUNT(DISTINCT s.source_id) FROM neev_knowledge_revision_deps d \
           JOIN neev_knowledge_snapshots s ON s.org_id=d.org_id AND s.id=d.snapshot_id \
           JOIN neev_resources r ON r.org_id=s.org_id AND r.id=s.source_id \
           WHERE d.org_id=p.org_id AND d.page_id=p.id AND d.revision=p.head_revision AND NOT {}) AS bad \
         FROM neev_knowledge_pages p WHERE p.org_id=$1 AND p.status='active' AND p.head_revision IS NOT NULL",
        eligible_predicate("r")
    );
    let rows = db
        .query_all(db::statement(db, &sql, vec![org_id.into()]))
        .await
        .map_err(db_error)?;
    let mut out = HashMap::with_capacity(rows.len());
    for row in rows {
        let page: String = row.try_get("", "page_id").map_err(db_error)?;
        let bad: i64 = row.try_get("", "bad").map_err(db_error)?;
        out.insert(
            page,
            if bad == 0 {
                Readable::Readable
            } else {
                Readable::Withheld {
                    unreadable_sources: bad,
                }
            },
        );
    }
    Ok(out)
}
