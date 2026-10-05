use std::collections::HashSet;

use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult};

use crate::neev::{
    auth::session,
    content::{grants::Tree, types::ContentError},
    db,
    error::{database_error, AuthError},
    startup::ServerState,
    types::{Node, NodeId, NodeInput, NodeKind, OrgContext},
};

const PAGE_SIZE: usize = 200;

/// The reader's view of the org tree (restricted folders, schema 24).
async fn tree<C: ConnectionTrait>(db: &C, actor: &OrgContext) -> Result<Tree, AuthError> {
    Tree::load(db, actor.org_id, actor.identity.user_id)
        .await
        .map_err(|error| match error {
            ContentError::Auth(error) => error,
            _ => AuthError::Unavailable,
        })
}
const ORGANIZATION_NODE_LIMIT: i64 = 1000;

#[derive(FromQueryResult)]
struct NodeRow {
    id: NodeId,
    org_id: i32,
    parent_id: Option<NodeId>,
    kind: String,
    name: String,
    revision: i64,
}

pub(crate) async fn list(
    state: &ServerState,
    actor: &OrgContext,
    after: Option<NodeId>,
) -> Result<Vec<Node>, AuthError> {
    Ok(list_page(state, actor, after).await?.0)
}

pub(crate) async fn list_page(
    state: &ServerState,
    actor: &OrgContext,
    after: Option<NodeId>,
) -> Result<(Vec<Node>, Option<NodeId>), AuthError> {
    if after.is_some_and(|id| id <= 0) {
        return Err(AuthError::Invalid);
    }
    let tx = db::begin(&state.database).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    if let Some(cursor) = after {
        load_node(&tx, actor.org_id, cursor, false)
            .await?
            .ok_or(AuthError::Forbidden)?;
    }
    let rows = tx
        .query_all(db::statement(
            &tx,
            // Nodes in the trash (schema 23) are hidden; restore brings them back.
            "SELECT id,org_id,parent_id,kind,name,revision FROM neev_nodes n WHERE org_id=$1 AND id>$2 AND NOT EXISTS (SELECT 1 FROM neev_node_trash t WHERE t.org_id=n.org_id AND t.item_kind='node' AND t.item_id=CAST(n.id AS TEXT)) ORDER BY id LIMIT $3",
            vec![
                actor.org_id.into(),
                after.unwrap_or(0).into(),
                i64::try_from(PAGE_SIZE + 1)
                    .map_err(|_| AuthError::Unavailable)?
                    .into(),
            ],
        ))
        .await
        .map_err(database_error)?;
    let mut nodes = rows
        .iter()
        .map(|row| NodeRow::from_query_result(row, "").map_err(database_error))
        .map(|row| row.and_then(node))
        .collect::<Result<Vec<_>, _>>()?;
    let next_after = if nodes.len() > PAGE_SIZE {
        nodes.truncate(PAGE_SIZE);
        nodes.last().map(|node| node.id)
    } else {
        None
    };
    // Only what this reader may see. A visible node whose parent is hidden is
    // a shared root: its parent id travels, never its name. The page cursor is
    // positional, so a hidden row only shortens a page.
    let view = tree(&tx, actor).await?;
    nodes.retain(|node| view.visible(node.id));
    tx.commit().await.map_err(database_error)?;
    Ok((nodes, next_after))
}

pub(crate) async fn create(
    state: &ServerState,
    actor: &OrgContext,
    input: NodeInput,
) -> Result<Node, AuthError> {
    let input = validate_input(input)?;
    let tx = db::begin(&state.database).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    let count = db::one(
        &tx,
        "SELECT COUNT(*) AS n FROM neev_nodes WHERE org_id=$1",
        vec![actor.org_id.into()],
    )
    .await?
    .ok_or(AuthError::Unavailable)?
    .try_get::<i64>("", "n")
    .map_err(database_error)?;
    if count >= ORGANIZATION_NODE_LIMIT {
        return Err(AuthError::Limited);
    }
    if let Some(parent_id) = input.parent_id {
        validate_ancestor_chain(&tx, actor.org_id, parent_id, None).await?;
        // A child goes only where the creator may place things: a hidden
        // parent is as good as missing; a View-only one is refused.
        let view = tree(&tx, actor).await?;
        if !view.visible(parent_id) {
            return Err(AuthError::Forbidden);
        }
        if !view.placeable(parent_id) {
            return Err(AuthError::Forbidden);
        }
    }
    let row = db::one(
        &tx,
        "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,$2,$3,$4,1,$5) RETURNING id,org_id,parent_id,kind,name,revision",
        vec![
            actor.org_id.into(),
            input.parent_id.into(),
            kind_name(input.kind).into(),
            input.name.into(),
            actor.identity.user_id.into(),
        ],
    )
    .await?
    .ok_or(AuthError::Unavailable)?;
    let created = node(NodeRow::from_query_result(&row, "").map_err(database_error)?)?;
    if created.parent_id.is_some() {
        crate::neev::db::member_sharing_migration::refresh_region(&tx, actor.org_id).await?;
    }
    tx.commit().await.map_err(database_error)?;
    Ok(created)
}

/// Every `neev_nodes` write that changes a `parent_id` (or adds a node) must
/// call `member_sharing_migration::refresh_region` in the same transaction: the
/// org wiki reads the derived restricted region, the access rule reads the
/// marks and the tree, and the two must never disagree.
pub(crate) async fn update(
    state: &ServerState,
    actor: &OrgContext,
    node_id: NodeId,
    expected_revision: i64,
    input: NodeInput,
) -> Result<Node, AuthError> {
    if node_id <= 0 || expected_revision <= 0 {
        return Err(AuthError::Invalid);
    }
    let input = validate_input(input)?;
    let tx = db::begin(&state.database).await?;
    // The same global guard Move to Trash and restore take (`catalog::trash`).
    // Trash plans a subtree from its parent links and then writes a ledger row
    // per node; a re-parent committing between those two steps would hide a node
    // that had already moved to a live folder. Serializing here closes that on
    // PostgreSQL. (SQLite already serializes: `db::begin` takes the one writer
    // lock before any read.)
    db::lock_admission_guard(&tx).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    let current = load_node(&tx, actor.org_id, node_id, true)
        .await?
        .ok_or(AuthError::Forbidden)?;
    parse_kind(&current.kind)?;
    if super::trash::node_trashed(&tx, actor.org_id, node_id).await? {
        return Err(AuthError::Forbidden);
    }
    // Renaming or moving needs Edit on the node; a hidden one is missing.
    let view = tree(&tx, actor).await?;
    if !view.placeable(node_id) {
        return Err(AuthError::Forbidden);
    }
    if current.revision != expected_revision {
        return Err(AuthError::Conflict);
    }
    if let Some(parent_id) = input.parent_id {
        if parent_id == node_id {
            return Err(AuthError::Invalid);
        }
        validate_ancestor_chain(&tx, actor.org_id, parent_id, Some(node_id)).await?;
    }
    if input.parent_id != current.parent_id {
        match input.parent_id {
            // Into a place the mover may place things, and never out of a
            // restricted region unless they created every root being left.
            Some(parent_id) => {
                if !view.placeable(parent_id) {
                    return Err(AuthError::Forbidden);
                }
                if let Some(from) = current.parent_id {
                    if !view.may_leave(from, parent_id) {
                        return Err(AuthError::Forbidden);
                    }
                }
                // Moving INTO a restriction hides the node (and everything in
                // it) from everyone outside that restriction: it is restricting
                // the node, which only its creator may do. Without this any
                // member could hide another's open folder inside their own.
                let before = current
                    .parent_id
                    .map(|from| view.restricted_roots(from))
                    .unwrap_or_default();
                let entering = view
                    .restricted_roots(parent_id)
                    .into_iter()
                    .any(|root| !before.contains(&root));
                if entering && view.creator(node_id) != Some(actor.identity.user_id) {
                    return Err(AuthError::Forbidden);
                }
            }
            None => {
                if let Some(from) = current.parent_id {
                    if !view.may_leave_to_top(from) {
                        return Err(AuthError::Forbidden);
                    }
                }
            }
        }
        // T-1: crossing a restriction boundary changes who controls every
        // restriction below; only someone who sees the whole subtree and
        // already controls each of those restrictions may do it.
        let before = current.parent_id.map(|from| view.restricted_roots(from)).unwrap_or_default();
        let after = input.parent_id.map(|to| view.restricted_roots(to)).unwrap_or_default();
        if before != after && !view.controls_subtree(node_id, false) {
            return Err(AuthError::Forbidden);
        }
    }
    let next_revision = current.revision.checked_add(1).ok_or(AuthError::Conflict)?;
    let updated = db::execute(
        &tx,
        "UPDATE neev_nodes SET parent_id=$1,kind=$2,name=$3,revision=$4 WHERE id=$5 AND org_id=$6 AND revision=$7",
        vec![
            input.parent_id.into(),
            kind_name(input.kind).into(),
            input.name.into(),
            next_revision.into(),
            node_id.into(),
            actor.org_id.into(),
            expected_revision.into(),
        ],
    )
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AuthError::Conflict);
    }
    if input.parent_id != current.parent_id {
        crate::neev::db::member_sharing_migration::refresh_region(&tx, actor.org_id).await?;
    }
    let updated = load_node(&tx, actor.org_id, node_id, false)
        .await?
        .ok_or(AuthError::Unavailable)?;
    let result = node(updated)?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

pub(crate) async fn get(
    state: &ServerState,
    actor: &OrgContext,
    node_id: NodeId,
) -> Result<Option<Node>, AuthError> {
    if node_id <= 0 {
        return Ok(None);
    }
    let tx = db::begin(&state.database).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    let result = if super::trash::node_trashed(&tx, actor.org_id, node_id).await?
        || !tree(&tx, actor).await?.visible(node_id)
    {
        None
    } else {
        load_node(&tx, actor.org_id, node_id, false)
            .await?
            .map(node)
            .transpose()?
    };
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

fn validate_input(input: NodeInput) -> Result<NodeInput, AuthError> {
    if input.parent_id.is_some_and(|id| id <= 0) {
        return Err(AuthError::Invalid);
    }
    match (input.kind, input.parent_id) {
        (NodeKind::WorkingDirectory, _) => return Err(AuthError::Unavailable),
        (NodeKind::Project, Some(_)) | (NodeKind::Folder, None) => return Err(AuthError::Invalid),
        (NodeKind::Project, None) | (NodeKind::Folder, Some(_)) => {}
    }
    let name = input.name.trim();
    if !(1..=100).contains(&name.chars().count()) || name.chars().any(char::is_control) {
        return Err(AuthError::Invalid);
    }
    Ok(NodeInput {
        parent_id: input.parent_id,
        kind: input.kind,
        name: name.to_owned(),
    })
}

async fn validate_ancestor_chain(
    tx: &DatabaseTransaction,
    org_id: i32,
    parent_id: NodeId,
    moving_node: Option<NodeId>,
) -> Result<(), AuthError> {
    let mut cursor = Some(parent_id);
    let mut seen = HashSet::new();
    while let Some(id) = cursor {
        if moving_node == Some(id) || !seen.insert(id) {
            return Err(AuthError::Invalid);
        }
        let ancestor = load_node(tx, org_id, id, true)
            .await?
            .ok_or(AuthError::Forbidden)?;
        if ancestor.kind == "working_directory" {
            return Err(AuthError::Unavailable);
        }
        if super::trash::node_trashed(tx, org_id, id).await? {
            return Err(AuthError::Forbidden);
        }
        cursor = ancestor.parent_id;
    }
    Ok(())
}

async fn load_node(
    tx: &DatabaseTransaction,
    org_id: i32,
    node_id: NodeId,
    locked: bool,
) -> Result<Option<NodeRow>, AuthError> {
    let row = db::one(
        tx,
        &format!(
            "SELECT id,org_id,parent_id,kind,name,revision FROM neev_nodes WHERE id=$1 AND org_id=$2{}",
            if locked { session::lock(tx) } else { "" }
        ),
        vec![node_id.into(), org_id.into()],
    )
    .await?;
    row.map(|row| NodeRow::from_query_result(&row, "").map_err(database_error))
        .transpose()
}

fn node(row: NodeRow) -> Result<Node, AuthError> {
    Ok(Node {
        id: row.id,
        org_id: row.org_id,
        parent_id: row.parent_id,
        kind: parse_kind(&row.kind)?,
        name: row.name,
        revision: row.revision,
    })
}

fn parse_kind(value: &str) -> Result<NodeKind, AuthError> {
    match value {
        "project" => Ok(NodeKind::Project),
        "folder" => Ok(NodeKind::Folder),
        "working_directory" => Err(AuthError::Unavailable),
        _ => Err(AuthError::Unavailable),
    }
}

fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Project => "project",
        NodeKind::Folder => "folder",
        NodeKind::WorkingDirectory => "working_directory",
    }
}
