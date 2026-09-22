//! Which document resources are backed by the engine, recorded without a
//! schema change.
//!
//! The obvious storage is a mapping table. It cannot be used, and the reason is
//! verified rather than stylistic: `db::initialize` matches the database's
//! **entire** table inventory by exact set equality against a per-version
//! expected set, falling through to `AuthError::Unavailable`. A schema-14 image
//! opening a database that carries one extra table therefore matches no arm and
//! does not start — and the only approved rollback image is schema 14. A mapping
//! table would convert "engine documents are unreachable after a rollback" into
//! "Neev does not boot", discovered during an incident.
//!
//! So the binding is additive **rows** in the existing `neev_requests` receipt
//! table, which a schema-14 image reads only by `(user_id, action, request_id)`
//! for actions it knows and is otherwise unaffected by. That is a receipts table
//! carrying a binding fact; it is chosen because it is the only change shape the
//! exact-inventory migrator tolerates, and `docs/decisions/` says so plainly.
//!
//! There is no document id to store. A Neev document resource and its Docs
//! document share one UUIDv4, so the binding records only *that* a resource is
//! engine-backed.

use crate::neev::{
    content::types::{ContentError, Resource, ResourceKind},
    db,
    error::database_error,
    org,
    types::OrgContext,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction};

/// The receipt action naming an engine binding. Versioned, because a future
/// engine must be distinguishable from this one rather than silently inheriting
/// its rows.
pub(crate) const ACTION: &str = "engine.lasuite.v1";

/// Receipts are keyed by `(user_id, action, request_id)` — the table's primary
/// key — so both `bind` and `bound` are point reads rather than scans. The
/// creator is used as `user_id` because the resource row already carries
/// `creator_id` by the time the question is asked, and it is stable for the life
/// of the resource.
fn digest(org_id: i32, resource_id: &str) -> String {
    org::digest_parts(&[ACTION, &org_id.to_string(), resource_id])
}

/// Record that a resource is engine-backed.
///
/// Called inside the same transaction that inserts the resource, so a resource
/// and its binding are committed together or not at all. A resource that existed
/// without its binding would present as a legacy document with no content — a
/// blank editor that looks exactly like data loss.
pub(crate) async fn bind(
    tx: &DatabaseTransaction,
    org_id: i32,
    creator_id: i32,
    resource_id: &str,
) -> Result<(), ContentError> {
    db::execute(
        tx,
        "INSERT INTO neev_requests(user_id,action,request_id,body_hash,resource_id,expires_at) VALUES($1,$2,$3,$4,$5,$6)",
        vec![
            creator_id.into(),
            ACTION.into(),
            resource_id.into(),
            digest(org_id, resource_id).into(),
            resource_id.into(),
            // The binding lives as long as the document does. It is never pruned.
            i64::MAX.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Whether a resource is engine-backed.
///
/// The stored digest is recomputed from the resource's own organization and id
/// and compared, so a row cannot be read as a binding for a resource other than
/// the one it was written for — including across organizations, which the
/// receipt table's key does not itself scope.
pub(crate) async fn bound(
    tx: &DatabaseTransaction,
    resource: &Resource,
) -> Result<bool, ContentError> {
    let row = db::one(
        tx,
        "SELECT body_hash,resource_id FROM neev_requests WHERE user_id=$1 AND action=$2 AND request_id=$3",
        vec![
            resource.creator_id.into(),
            ACTION.into(),
            resource.id.clone().into(),
        ],
    )
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let stored: String = row
        .try_get("", "body_hash")
        .map_err(crate::neev::error::database_error)?;
    let id: String = row
        .try_get("", "resource_id")
        .map_err(crate::neev::error::database_error)?;
    Ok(id == resource.id && stored == digest(resource.org_id, &resource.id))
}

/// Whether a resource is engine-backed, read outside a resource transaction.
///
/// Used by the HTTP layer to label a response. It is a separate point read
/// rather than a join into the resource query so that nothing in the native
/// content path changes shape, and so a legacy document's read path is
/// untouched when the engine is not involved.
pub(crate) async fn bound_now<C: ConnectionTrait>(
    db: &C,
    resource: &Resource,
) -> Result<bool, ContentError> {
    let row = db::one(
        db,
        "SELECT body_hash,resource_id FROM neev_requests WHERE user_id=$1 AND action=$2 AND request_id=$3",
        vec![
            resource.creator_id.into(),
            ACTION.into(),
            resource.id.clone().into(),
        ],
    )
    .await?;
    matches(row, resource)
}

fn matches(
    row: Option<sea_orm::QueryResult>,
    resource: &Resource,
) -> Result<bool, ContentError> {
    let Some(row) = row else {
        return Ok(false);
    };
    let stored: String = row.try_get("", "body_hash").map_err(database_error)?;
    let id: String = row.try_get("", "resource_id").map_err(database_error)?;
    Ok(id == resource.id && stored == digest(resource.org_id, &resource.id))
}

/// Which of these resources are engine-backed, in **one** query.
///
/// A listing must not cost a round trip per row. The digest is still recomputed
/// and compared for each returned row, so batching does not weaken the check
/// that a binding belongs to the resource it is read for.
pub(crate) async fn bound_set<C: ConnectionTrait>(
    db: &C,
    resources: &[Resource],
) -> Result<std::collections::HashSet<String>, ContentError> {
    let documents: Vec<&Resource> = resources
        .iter()
        .filter(|resource| resource.kind == ResourceKind::Document)
        .collect();
    if documents.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let mut values: Vec<sea_orm::Value> = vec![ACTION.into()];
    let mut placeholders = Vec::with_capacity(documents.len());
    for resource in &documents {
        values.push(resource.id.clone().into());
        placeholders.push(format!("${}", values.len()));
    }
    let sql = format!(
        "SELECT user_id,body_hash,resource_id,request_id FROM neev_requests WHERE action=$1 AND request_id IN ({})",
        placeholders.join(",")
    );
    let rows = db
        .query_all(db::statement(db, &sql, values))
        .await
        .map_err(database_error)?;
    let mut bound = std::collections::HashSet::new();
    for row in rows {
        let request_id: String = row.try_get("", "request_id").map_err(database_error)?;
        let stored: String = row.try_get("", "body_hash").map_err(database_error)?;
        let resource_id: String = row.try_get("", "resource_id").map_err(database_error)?;
        let creator: i32 = row.try_get("", "user_id").map_err(database_error)?;
        let Some(resource) = documents.iter().find(|candidate| candidate.id == request_id) else {
            continue;
        };
        if resource_id == resource.id
            && creator == resource.creator_id
            && stored == digest(resource.org_id, &resource.id)
        {
            bound.insert(resource.id.clone());
        }
    }
    Ok(bound)
}

/// The engine a resource is served by, as reported to clients.
///
/// An older client that does not know the field treats its absence as native,
/// so this is additive in the response as well as in storage.
pub(crate) fn name(engine_backed: bool) -> &'static str {
    if engine_backed {
        "lasuite"
    } else {
        "native"
    }
}

/// Refuse a legacy native content operation on an engine-backed resource.
///
/// The legacy routes must not become a second way to read or write a document
/// the engine owns — an engine-backed resource has no native revisions, so a
/// legacy write would fork the document into two stores and a legacy read would
/// render it blank.
pub(crate) async fn refuse_if_bound(
    tx: &DatabaseTransaction,
    resource: &Resource,
) -> Result<(), ContentError> {
    if bound(tx, resource).await? {
        return Err(ContentError::NotFound);
    }
    Ok(())
}

/// Convenience for the proxy: the actor's organization must match the resource's
/// before a binding means anything to this caller.
pub(crate) fn same_organization(actor: &OrgContext, resource: &Resource) -> bool {
    actor.org_id == resource.org_id
}
