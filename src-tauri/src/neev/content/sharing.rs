//! Folder sharing defaults (schema 21): what audience a NEW resource gets when
//! its creator does not choose one.
//!
//! A node either carries an explicit default (`private` / `shared` /
//! `organization`; `shared` is stored as `private` + a schema-24 mark) or
//! inherits its parent's; a chain with no explicit default is private. The
//! default is resolved once, inside the creating transaction, and MATERIALIZED
//! into the new resource's own `visibility` — it is never consulted by access
//! checks, so changing a default can never change who sees an existing item.

use super::{access, resources, types::*};
use crate::neev::{
    auth::session,
    db,
    error::database_error,
    startup::ServerState,
    types::{NodeId, OrgContext},
};
use sea_orm::{DatabaseTransaction, FromQueryResult};
use std::collections::HashSet;

/// The ancestor walk is bounded even though node writes already refuse cycles.
const MAX_DEPTH: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DefaultVisibility {
    Inherit,
    Private,
    /// New items use member/team sharing ("folder access"). Stored as a
    /// `private` default plus a `node_default` shared mark, so a rollback reads
    /// it as private.
    Shared,
    Organization,
}

impl DefaultVisibility {
    fn explicit(self) -> Option<Visibility> {
        match self {
            Self::Inherit => None,
            Self::Private => Some(Visibility::Private),
            Self::Shared => Some(Visibility::Shared),
            Self::Organization => Some(Visibility::Organization),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NodeSharing {
    pub node_id: NodeId,
    pub revision: i64,
    pub default_visibility: DefaultVisibility,
    pub effective_visibility: Visibility,
    /// The nearest ancestor whose explicit default applies; `None` when the
    /// node's own default is explicit or nothing in the chain is.
    pub inherited_from: Option<NodeId>,
    pub can_manage: bool,
}

#[derive(FromQueryResult)]
struct NodeRow {
    parent_id: Option<NodeId>,
    kind: String,
    revision: i64,
    default_visibility: Option<String>,
    shared_default: i64,
}

/// The raw row, whatever its kind; `None` when absent from this org.
async fn load_any(
    tx: &DatabaseTransaction,
    org_id: i32,
    node_id: NodeId,
    locked: bool,
) -> Result<Option<NodeRow>, ContentError> {
    // FOR UPDATE cannot apply to the nullable side of an outer join in
    // PostgreSQL, so the default is a scalar subquery and only the node is locked.
    let row = db::one(
        tx,
        &format!(
            "SELECT n.parent_id,n.kind,n.revision,(SELECT d.default_visibility FROM neev_node_sharing_defaults d WHERE d.org_id=n.org_id AND d.node_id=n.id) AS default_visibility,(SELECT COUNT(*) FROM neev_shared_marks k WHERE k.org_id=n.org_id AND k.mark_kind='node_default' AND k.target_id=CAST(n.id AS TEXT)) AS shared_default FROM neev_nodes n WHERE n.org_id=$1 AND n.id=$2{}",
            if locked { session::lock(tx) } else { "" }
        ),
        vec![org_id.into(), node_id.into()],
    )
    .await?;
    row.map(|row| NodeRow::from_query_result(&row, "").map_err(database_error))
        .transpose()
        .map_err(ContentError::from)
}

/// A project or folder of the actor's org; anything else is not found.
async fn load_node(
    tx: &DatabaseTransaction,
    org_id: i32,
    node_id: NodeId,
    locked: bool,
) -> Result<NodeRow, ContentError> {
    if node_id <= 0 {
        return Err(ContentError::NotFound);
    }
    let row = load_any(tx, org_id, node_id, locked)
        .await?
        .ok_or(ContentError::NotFound)?;
    if !matches!(row.kind.as_str(), "project" | "folder") {
        return Err(ContentError::NotFound);
    }
    Ok(row)
}

fn parse(node: &NodeRow) -> Result<DefaultVisibility, ContentError> {
    match (node.default_visibility.as_deref(), node.shared_default) {
        (None, 0) => Ok(DefaultVisibility::Inherit),
        (Some("private"), 0) => Ok(DefaultVisibility::Private),
        (Some("private"), _) => Ok(DefaultVisibility::Shared),
        (Some("organization"), 0) => Ok(DefaultVisibility::Organization),
        _ => Err(ContentError::Storage),
    }
}

/// (own setting, effective audience, the ancestor it came from). Reads inside
/// the caller's transaction, after its admission guard and session recheck.
///
/// Ancestors are read WITHOUT row locks, deliberately. A creation racing a
/// default change on some ancestor linearizes before or after it, and the
/// materialized audience is the one either state would give — never a mix.
/// Locking the chain would serialize every creation in a tree behind any
/// default or rename write, for no stronger guarantee.
async fn resolve_from(
    tx: &DatabaseTransaction,
    org_id: i32,
    node_id: NodeId,
    node: &NodeRow,
) -> Result<(DefaultVisibility, Visibility, Option<NodeId>), ContentError> {
    let own = parse(node)?;
    if let Some(explicit) = own.explicit() {
        return Ok((own, explicit, None));
    }
    let mut seen = HashSet::from([node_id]);
    let mut cursor = node.parent_id;
    while let Some(id) = cursor {
        if !seen.insert(id) || seen.len() > MAX_DEPTH {
            return Err(ContentError::Storage);
        }
        // The node FK forbids a dangling or foreign parent; treat one as corrupt.
        let ancestor = load_any(tx, org_id, id, false)
            .await?
            .ok_or(ContentError::Storage)?;
        // Node writes no longer allow a folder below a working directory, but a
        // legacy chain could have one. The walk stops there and the fallback is
        // private: never broader than schema 20, and creation still succeeds.
        if !matches!(ancestor.kind.as_str(), "project" | "folder") {
            break;
        }
        if let Some(explicit) = parse(&ancestor)?.explicit() {
            return Ok((own, explicit, Some(id)));
        }
        cursor = ancestor.parent_id;
    }
    Ok((own, Visibility::Private, None))
}

/// The audience a new resource on `node_id` gets when its creator states none.
pub(crate) async fn resolve(
    tx: &DatabaseTransaction,
    org_id: i32,
    node_id: NodeId,
) -> Result<Visibility, ContentError> {
    let node = load_node(tx, org_id, node_id, false).await?;
    Ok(resolve_from(tx, org_id, node_id, &node).await?.1)
}

/// The same rule node grants use (`grants::can_manage_node`): creator or
/// admin in an open region, creator or the restricted root's creator inside one.
fn can_manage(tree: &super::grants::Tree, actor: &OrgContext, node_id: NodeId) -> bool {
    super::grants::can_manage_node(tree, actor, node_id)
}

/// A node the actor cannot see is not found, exactly as a missing one.
async fn seen(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    node_id: NodeId,
) -> Result<super::grants::Tree, ContentError> {
    let tree = super::grants::Tree::load(tx, actor.org_id, actor.identity.user_id).await?;
    if !tree.visible(node_id) {
        return Err(ContentError::NotFound);
    }
    Ok(tree)
}

async fn snapshot(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    node_id: NodeId,
    node: &NodeRow,
) -> Result<NodeSharing, ContentError> {
    let (default_visibility, effective_visibility, inherited_from) =
        resolve_from(tx, actor.org_id, node_id, node).await?;
    let tree = seen(tx, actor, node_id).await?;
    Ok(NodeSharing {
        node_id,
        revision: node.revision,
        default_visibility,
        effective_visibility,
        // A default inherited from a folder the reader cannot see names none.
        inherited_from: inherited_from.filter(|id| tree.visible(*id)),
        can_manage: can_manage(&tree, actor, node_id),
    })
}

/// Any current member of the node's organization may read its default.
pub(crate) async fn get(
    state: &ServerState,
    actor: &OrgContext,
    node_id: NodeId,
) -> Result<NodeSharing, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let node = load_node(&tx, actor.org_id, node_id, false).await?;
    let result = snapshot(&tx, actor, node_id, &node).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

/// Only the node's creator or an organization admin may change its default.
/// Bumps the node revision (optimistic concurrency shared with rename/move) and
/// touches no resource row.
pub(crate) async fn set(
    state: &ServerState,
    actor: &OrgContext,
    node_id: NodeId,
    expected_revision: i64,
    requested: DefaultVisibility,
) -> Result<NodeSharing, ContentError> {
    if expected_revision <= 0 {
        return Err(ContentError::Invalid);
    }
    let tx = resources::begin(state, actor).await?;
    let node = load_node(&tx, actor.org_id, node_id, true).await?;
    let tree = seen(&tx, actor, node_id).await?;
    if !can_manage(&tree, actor, node_id) {
        return Err(ContentError::Auth(crate::neev::error::AuthError::Forbidden));
    }
    if node.revision != expected_revision {
        return Err(ContentError::Conflict);
    }
    if parse(&node)? == requested {
        let result = snapshot(&tx, actor, node_id, &node).await?;
        session::recheck(&tx, actor, state.clock.now_unix()).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(result);
    }
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid);
    }
    let next = node.revision.checked_add(1).ok_or(ContentError::Conflict)?;
    let bumped = db::execute(
        &tx,
        "UPDATE neev_nodes SET revision=$1 WHERE org_id=$2 AND id=$3 AND revision=$4",
        vec![
            next.into(),
            actor.org_id.into(),
            node_id.into(),
            expected_revision.into(),
        ],
    )
    .await?;
    if bumped.rows_affected() != 1 {
        return Err(ContentError::Conflict);
    }
    db::execute(
        &tx,
        "DELETE FROM neev_shared_marks WHERE org_id=$1 AND mark_kind='node_default' AND target_id=$2",
        vec![actor.org_id.into(), node_id.to_string().into()],
    )
    .await?;
    if requested == DefaultVisibility::Shared {
        db::execute(
            &tx,
            "INSERT INTO neev_shared_marks(org_id,mark_kind,target_id,set_by,created_at) VALUES($1,'node_default',$2,$3,$4)",
            vec![
                actor.org_id.into(),
                node_id.to_string().into(),
                actor.identity.user_id.into(),
                now.into(),
            ],
        )
        .await?;
    }
    match requested.explicit() {
        None => {
            db::execute(
                &tx,
                "DELETE FROM neev_node_sharing_defaults WHERE org_id=$1 AND node_id=$2",
                vec![actor.org_id.into(), node_id.into()],
            )
            .await?;
        }
        Some(visibility) => {
            db::execute(
                &tx,
                "INSERT INTO neev_node_sharing_defaults(org_id,node_id,default_visibility,set_by,updated_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(org_id,node_id) DO UPDATE SET default_visibility=excluded.default_visibility,set_by=excluded.set_by,updated_at=excluded.updated_at",
                vec![
                    actor.org_id.into(),
                    node_id.into(),
                    access::visibility_name(visibility).into(),
                    actor.identity.user_id.into(),
                    now.into(),
                ],
            )
            .await?;
        }
    }
    let node = load_node(&tx, actor.org_id, node_id, false).await?;
    let result = snapshot(&tx, actor, node_id, &node).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}
