//! Move to Trash and restore for projects and folders (schema 23).
//!
//! A trash *batch* hides a node and every descendant that was not already in
//! the trash, and archives — with the existing per-resource `archived` flag —
//! every chat and document on those nodes that was not already archived, and
//! the actor's own Agent Chat sessions filed there that were not already. The
//! batch is recorded in `neev_node_trash`, so restore undoes exactly that and
//! nothing else: items trashed on their own before or after stay trashed.
//!
//! Nothing is deleted and nothing is re-parented. A restored node is back under
//! its unchanged `parent_id`, and a restored resource keeps its own
//! `visibility`, so a private resource can never come back as organization
//! content. Because knowledge eligibility already requires `archived=false`,
//! trashed content leaves org knowledge and returns on restore with no change
//! to the knowledge module.
//!
//! Trash refuses (writing nothing) while an agent is running anywhere in the
//! subtree, while a resource in it is leased for writing, or when the subtree
//! holds another member's private resource or Agent Chat (sessions are always
//! owner-private): hiding someone else's private work is not the actor's call,
//! and changing its audience never is.

use std::collections::{HashMap, HashSet};

use sea_orm::{ConnectionTrait, DatabaseTransaction, Value};

use crate::neev::{
    auth::session,
    db,
    error::{database_error, AuthError},
    startup::ServerState,
    types::{NodeId, NodeKind, OrgContext},
};

/// The most batches one listing returns. The org node cap (1000) bounds it anyway.
const LIST_LIMIT: i64 = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blocker {
    /// An agent turn is running on a session in the subtree.
    AgentActive,
    /// A resource in the subtree is leased for writing.
    InUse,
    /// The subtree holds another member's private resource.
    PrivateContent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrashError {
    Auth(AuthError),
    /// Absent, in another org, already in the trash, or not a batch root.
    NotFound,
    /// Refused before writing anything. Deliberately no count: blockers can be
    /// other members' private work, and how much of it there is is not ours to
    /// reveal (the session ACL's own rule, see `agents::sessions`).
    Blocked {
        blocker: Blocker,
    },
    /// Restore needs the original parent back first.
    ParentTrashed,
}

impl From<AuthError> for TrashError {
    fn from(value: AuthError) -> Self {
        Self::Auth(value)
    }
}

/// What the confirmation dialog shows. Counts are only of what the actor can
/// read (or owns); blockers are yes/no, so nothing about other members' private
/// work — not even how much — leaves this module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Preview {
    pub folders: i64,
    pub documents: i64,
    pub chats: i64,
    /// The actor's own standalone Agent Chats (sessions filed here, not bound
    /// to a chat resource). A bound session is its chat's, already in `chats`.
    pub agent_chats: i64,
    pub agent_active: bool,
    pub in_use: bool,
    pub private_others: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Trashed {
    pub batch_id: String,
    pub nodes: i64,
    pub resources: i64,
    pub sessions: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Restored {
    pub nodes: i64,
    pub resources: i64,
    pub sessions: i64,
}

/// One batch in the trash, described from the viewer's side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrashItem {
    pub node_id: NodeId,
    pub kind: NodeKind,
    pub name: String,
    pub parent_id: Option<NodeId>,
    pub parent_name: Option<String>,
    /// The original parent is itself in the trash; restore it first.
    pub parent_trashed: bool,
    pub trashed_by: i32,
    pub trashed_by_name: String,
    pub trashed_at: i64,
    pub folders: i64,
    pub documents: i64,
    pub chats: i64,
    /// The viewer's own standalone Agent Chats in the batch.
    pub agent_chats: i64,
}

/// `r` readable by `$user` (alias `s` is the LEFT JOINed origin chat). Mirrors
/// `content::resources::list`, so a count never includes what the viewer
/// cannot open.
fn visible(user: &str) -> String {
    format!(
        "((r.origin_chat_id IS NULL AND (r.visibility='organization' OR r.creator_id={user})) OR \
(r.origin_chat_id IS NOT NULL AND (COALESCE(s.visibility,'')='organization' OR COALESCE(s.creator_id,0)={user})))"
    )
}

const ORIGIN_JOIN: &str =
    "LEFT JOIN neev_resources s ON s.org_id=r.org_id AND s.id=r.origin_chat_id AND s.kind='chat'";

/// True when this node row is in the trash (in any batch).
pub(crate) async fn node_trashed<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    node_id: NodeId,
) -> Result<bool, AuthError> {
    Ok(db::one(
        db,
        "SELECT 1 AS hit FROM neev_node_trash WHERE org_id=$1 AND item_kind='node' AND item_id=$2",
        vec![org_id.into(), node_id.to_string().into()],
    )
    .await?
    .is_some())
}

/// True when a folder trash archived this resource. Its metadata is frozen
/// until the batch is restored, so it cannot be unarchived, moved or re-shared
/// from inside the trash.
pub(crate) async fn resource_trashed<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    resource_id: &str,
) -> Result<bool, AuthError> {
    Ok(db::one(
        db,
        "SELECT 1 AS hit FROM neev_node_trash WHERE org_id=$1 AND item_kind='resource' AND item_id=$2",
        vec![org_id.into(), resource_id.into()],
    )
    .await?
    .is_some())
}

/// True when a folder trash archived this Agent Chat session. Like a batch
/// resource, it cannot be unarchived or re-filed from inside the trash.
pub(crate) async fn session_trashed<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    session_id: &str,
) -> Result<bool, AuthError> {
    Ok(db::one(
        db,
        "SELECT 1 AS hit FROM neev_node_trash WHERE org_id=$1 AND item_kind='session' AND item_id=$2",
        vec![org_id.into(), session_id.into()],
    )
    .await?
    .is_some())
}

async fn begin(state: &ServerState, actor: &OrgContext) -> Result<DatabaseTransaction, AuthError> {
    let tx = db::begin(&state.database).await?;
    db::lock_admission_guard(&tx).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    Ok(tx)
}

struct OrgNode {
    parent_id: Option<NodeId>,
    kind: String,
    revision: i64,
    trashed: bool,
}

async fn org_nodes(
    tx: &DatabaseTransaction,
    org_id: i32,
) -> Result<HashMap<NodeId, OrgNode>, AuthError> {
    let rows = tx
        .query_all(db::statement(
            tx,
            "SELECT n.id,n.parent_id,n.kind,n.revision,CASE WHEN t.item_id IS NULL THEN 0 ELSE 1 END AS trashed FROM neev_nodes n LEFT JOIN neev_node_trash t ON t.org_id=n.org_id AND t.item_kind='node' AND t.item_id=CAST(n.id AS TEXT) WHERE n.org_id=$1",
            vec![org_id.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut nodes = HashMap::with_capacity(rows.len());
    for row in rows {
        let trashed: i32 = row.try_get("", "trashed").map_err(database_error)?;
        nodes.insert(
            row.try_get("", "id").map_err(database_error)?,
            OrgNode {
                parent_id: row.try_get("", "parent_id").map_err(database_error)?,
                kind: row.try_get("", "kind").map_err(database_error)?,
                revision: row.try_get("", "revision").map_err(database_error)?,
                trashed: trashed != 0,
            },
        );
    }
    Ok(nodes)
}

/// The live subtree under `root` (root first). A descendant that is already in
/// the trash belongs to its own batch, and so does everything under it.
fn live_subtree(nodes: &HashMap<NodeId, OrgNode>, root: NodeId) -> Vec<NodeId> {
    let mut children: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for (id, node) in nodes {
        if let Some(parent) = node.parent_id {
            children.entry(parent).or_default().push(*id);
        }
    }
    let mut out = vec![root];
    let mut seen = HashSet::from([root]);
    let mut index = 0;
    while index < out.len() {
        let mut next = children.get(&out[index]).cloned().unwrap_or_default();
        next.sort_unstable();
        for child in next {
            if !nodes[&child].trashed && seen.insert(child) {
                out.push(child);
            }
        }
        index += 1;
    }
    out
}

/// `$first,$first+1,…` for an IN list.
fn placeholders(first: usize, count: usize) -> String {
    (first..first + count)
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(",")
}

struct Plan {
    root_revision: i64,
    nodes: Vec<NodeId>,
    /// Unarchived resources on the subtree, with kind and actor visibility.
    resources: Vec<(String, String, bool)>,
    /// Unarchived Agent Chat sessions filed on the subtree: id, owned by the
    /// actor, standalone (not bound to a chat resource).
    sessions: Vec<(String, bool, bool)>,
    agent_active: i64,
    in_use: i64,
}

async fn plan(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    node_id: NodeId,
    now: i64,
) -> Result<Plan, TrashError> {
    if node_id <= 0 {
        return Err(TrashError::NotFound);
    }
    let nodes = org_nodes(tx, actor.org_id).await?;
    let root = nodes.get(&node_id).ok_or(TrashError::NotFound)?;
    if root.trashed || !matches!(root.kind.as_str(), "project" | "folder") {
        return Err(TrashError::NotFound);
    }
    let root_revision = root.revision;
    let subtree = live_subtree(&nodes, node_id);
    let user = actor.identity.user_id;
    let mut values: Vec<Value> = vec![actor.org_id.into(), user.into()];
    values.extend(subtree.iter().map(|id| Value::from(*id)));
    let list = placeholders(3, subtree.len());
    let rows = tx
        .query_all(db::statement(
            tx,
            &format!(
                "SELECT r.id,r.kind,CASE WHEN {} THEN 1 ELSE 0 END AS readable FROM neev_resources r {ORIGIN_JOIN} WHERE r.org_id=$1 AND r.archived=false AND r.node_id IN ({list}) ORDER BY r.id",
                visible("$2")
            ),
            values,
        ))
        .await
        .map_err(database_error)?;
    let mut resources = Vec::with_capacity(rows.len());
    for row in rows {
        let readable: i32 = row.try_get("", "readable").map_err(database_error)?;
        resources.push((
            row.try_get("", "id").map_err(database_error)?,
            row.try_get("", "kind").map_err(database_error)?,
            readable != 0,
        ));
    }
    // Agent Chats filed on the subtree. Their ACL is the session's own: private
    // to the owner (`agents::sessions::owned`), never shared. A team holder is
    // never filed (`node_id` is NULL), so `kind='chat'` is the whole set.
    let mut node_values: Vec<Value> = vec![actor.org_id.into()];
    node_values.extend(subtree.iter().map(|id| Value::from(*id)));
    let node_list = placeholders(2, subtree.len());
    let mut session_values: Vec<Value> = vec![actor.org_id.into(), user.into()];
    session_values.extend(subtree.iter().map(|id| Value::from(*id)));
    let session_rows = tx
        .query_all(db::statement(
            tx,
            &format!(
                "SELECT g.id,CASE WHEN g.owner_id=$2 THEN 1 ELSE 0 END AS mine,CASE WHEN g.bound_chat_id IS NULL THEN 1 ELSE 0 END AS standalone FROM neev_agent_sessions g WHERE g.org_id=$1 AND g.kind='chat' AND g.archived=false AND g.node_id IN ({list}) ORDER BY g.id"
            ),
            session_values,
        ))
        .await
        .map_err(database_error)?;
    let mut sessions = Vec::with_capacity(session_rows.len());
    for row in session_rows {
        let mine: i32 = row.try_get("", "mine").map_err(database_error)?;
        let standalone: i32 = row.try_get("", "standalone").map_err(database_error)?;
        sessions.push((
            row.try_get("", "id").map_err(database_error)?,
            mine != 0,
            standalone != 0,
        ));
    }
    // A run on an Agent Chat in the subtree, or on a session bound to a chat in it.
    let agent_active: i64 = db::one(
        tx,
        &format!(
            "SELECT COUNT(*) AS n FROM neev_agent_turns t JOIN neev_agent_sessions g ON g.org_id=t.org_id AND g.id=t.session_id WHERE t.org_id=$1 AND t.status='running' AND (g.node_id IN ({node_list}) OR g.bound_chat_id IN (SELECT c.id FROM neev_resources c WHERE c.org_id=$1 AND c.node_id IN ({node_list})))"
        ),
        node_values,
    )
    .await?
    .ok_or(AuthError::Unavailable)?
    .try_get("", "n")
    .map_err(database_error)?;
    let mut lease_values: Vec<Value> = vec![actor.org_id.into(), now.into()];
    lease_values.extend(subtree.iter().map(|id| Value::from(*id)));
    let in_use: i64 = db::one(
        tx,
        &format!(
            "SELECT COUNT(*) AS n FROM neev_content_leases l JOIN neev_resources r ON r.org_id=l.org_id AND r.id=l.resource_id WHERE l.org_id=$1 AND l.expires_at>$2 AND r.archived=false AND r.node_id IN ({list})"
        ),
        lease_values,
    )
    .await?
    .ok_or(AuthError::Unavailable)?
    .try_get("", "n")
    .map_err(database_error)?;
    Ok(Plan {
        root_revision,
        nodes: subtree,
        resources,
        sessions,
        agent_active,
        in_use,
    })
}

fn preview_of(plan: &Plan) -> Preview {
    let count = |kind: &str| {
        plan.resources
            .iter()
            .filter(|(_, k, readable)| *readable && k == kind)
            .count() as i64
    };
    Preview {
        folders: plan.nodes.len() as i64 - 1,
        documents: count("document"),
        chats: count("chat"),
        agent_chats: plan
            .sessions
            .iter()
            .filter(|(_, mine, standalone)| *mine && *standalone)
            .count() as i64,
        agent_active: plan.agent_active > 0,
        in_use: plan.in_use > 0,
        // Another member's private resources or their (always private) Agent
        // Chats: trashing would hide work the actor cannot see. Whether, never
        // how many.
        private_others: plan.resources.iter().any(|(_, _, readable)| !readable)
            || plan.sessions.iter().any(|(_, mine, _)| !mine),
    }
}

/// What a Move to Trash of `node_id` would take with it, and what blocks it.
pub(crate) async fn preview(
    state: &ServerState,
    actor: &OrgContext,
    node_id: NodeId,
) -> Result<Preview, TrashError> {
    let tx = begin(state, actor).await?;
    let plan = plan(&tx, actor, node_id, state.clock.now_unix()).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(preview_of(&plan))
}

/// Bump the auth revision of resources, and of documents bound to those that
/// are chats, exactly as `content::resources::update` does for an archive.
async fn touch(
    tx: &DatabaseTransaction,
    org_id: i32,
    ids: &[String],
    archived: bool,
    now: i64,
) -> Result<i64, AuthError> {
    let mut changed = 0;
    for id in ids {
        let result = db::execute(
            tx,
            "UPDATE neev_resources SET archived=$1,auth_revision=auth_revision+1,updated_at=$2 WHERE org_id=$3 AND id=$4 AND archived=$5",
            vec![archived.into(), now.into(), org_id.into(), id.clone().into(), (!archived).into()],
        )
        .await?;
        changed += result.rows_affected() as i64;
        db::execute(
            tx,
            "UPDATE neev_resources SET auth_revision=auth_revision+1,updated_at=$1 WHERE org_id=$2 AND origin_chat_id=$3",
            vec![now.into(), org_id.into(), id.clone().into()],
        )
        .await?;
    }
    Ok(changed)
}

/// Archive or unarchive Agent Chat sessions, only from the opposite state, with
/// the revision bump every other session write makes (so an open editor holding
/// the old revision conflicts). Turns, attachments and history are untouched.
async fn touch_sessions(
    tx: &DatabaseTransaction,
    org_id: i32,
    ids: &[String],
    archived: bool,
    now: i64,
) -> Result<i64, AuthError> {
    let mut changed = 0;
    for id in ids {
        let result = db::execute(
            tx,
            "UPDATE neev_agent_sessions SET archived=$1,revision=revision+1,updated_at=$2 WHERE org_id=$3 AND id=$4 AND archived=$5",
            vec![archived.into(), now.into(), org_id.into(), id.clone().into(), (!archived).into()],
        )
        .await?;
        changed += result.rows_affected() as i64;
    }
    Ok(changed)
}

async fn bump_node(
    tx: &DatabaseTransaction,
    org_id: i32,
    node_id: NodeId,
    revision: i64,
) -> Result<(), AuthError> {
    let next = revision.checked_add(1).ok_or(AuthError::Conflict)?;
    let updated = db::execute(
        tx,
        "UPDATE neev_nodes SET revision=$1 WHERE id=$2 AND org_id=$3 AND revision=$4",
        vec![next.into(), node_id.into(), org_id.into(), revision.into()],
    )
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AuthError::Conflict);
    }
    Ok(())
}

/// Move `node_id` and its live subtree to the trash. `expected_revision` is the
/// node revision the caller confirmed against.
pub(crate) async fn trash(
    state: &ServerState,
    actor: &OrgContext,
    node_id: NodeId,
    expected_revision: i64,
) -> Result<Trashed, TrashError> {
    if expected_revision <= 0 {
        return Err(TrashError::Auth(AuthError::Invalid));
    }
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(TrashError::Auth(AuthError::Invalid));
    }
    let tx = begin(state, actor).await?;
    // Lock the root first so a concurrent rename/move/trash serializes here.
    db::one(
        &tx,
        &format!(
            "SELECT id FROM neev_nodes WHERE id=$1 AND org_id=$2{}",
            session::lock(&tx)
        ),
        vec![node_id.into(), actor.org_id.into()],
    )
    .await?
    .ok_or(TrashError::NotFound)?;
    let plan = plan(&tx, actor, node_id, now).await?;
    if plan.root_revision != expected_revision {
        return Err(TrashError::Auth(AuthError::Conflict));
    }
    let preview = preview_of(&plan);
    for (blocker, blocked) in [
        (Blocker::AgentActive, preview.agent_active),
        (Blocker::InUse, preview.in_use),
        (Blocker::PrivateContent, preview.private_others),
    ] {
        if blocked {
            return Err(TrashError::Blocked { blocker });
        }
    }
    let batch_id = uuid::Uuid::new_v4().to_string();
    let insert = "INSERT INTO neev_node_trash(org_id,batch_id,item_kind,item_id,root_node_id,trashed_by,trashed_at) VALUES($1,$2,$3,$4,$5,$6,$7)";
    for id in &plan.nodes {
        db::execute(
            &tx,
            insert,
            vec![
                actor.org_id.into(),
                batch_id.clone().into(),
                "node".into(),
                id.to_string().into(),
                node_id.into(),
                actor.identity.user_id.into(),
                now.into(),
            ],
        )
        .await?;
    }
    let ids: Vec<String> = plan.resources.iter().map(|(id, _, _)| id.clone()).collect();
    for id in &ids {
        db::execute(
            &tx,
            insert,
            vec![
                actor.org_id.into(),
                batch_id.clone().into(),
                "resource".into(),
                id.clone().into(),
                node_id.into(),
                actor.identity.user_id.into(),
                now.into(),
            ],
        )
        .await?;
    }
    // Only the actor's own sessions reach here: anyone else's blocked above.
    let session_ids: Vec<String> = plan.sessions.iter().map(|(id, _, _)| id.clone()).collect();
    for id in &session_ids {
        db::execute(
            &tx,
            insert,
            vec![
                actor.org_id.into(),
                batch_id.clone().into(),
                "session".into(),
                id.clone().into(),
                node_id.into(),
                actor.identity.user_id.into(),
                now.into(),
            ],
        )
        .await?;
    }
    let archived = touch(&tx, actor.org_id, &ids, true, now).await?;
    if archived != ids.len() as i64 {
        return Err(TrashError::Auth(AuthError::Conflict));
    }
    let sessions = touch_sessions(&tx, actor.org_id, &session_ids, true, now).await?;
    if sessions != session_ids.len() as i64 {
        return Err(TrashError::Auth(AuthError::Conflict));
    }
    bump_node(&tx, actor.org_id, node_id, plan.root_revision).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(Trashed {
        batch_id,
        nodes: plan.nodes.len() as i64,
        resources: archived,
        sessions,
    })
}

/// Restore the batch whose root is `node_id` to its original location.
pub(crate) async fn restore(
    state: &ServerState,
    actor: &OrgContext,
    node_id: NodeId,
) -> Result<Restored, TrashError> {
    if node_id <= 0 {
        return Err(TrashError::NotFound);
    }
    let now = state.clock.now_unix();
    let tx = begin(state, actor).await?;
    let node = db::one(
        &tx,
        &format!(
            "SELECT parent_id,revision FROM neev_nodes WHERE id=$1 AND org_id=$2{}",
            session::lock(&tx)
        ),
        vec![node_id.into(), actor.org_id.into()],
    )
    .await?
    .ok_or(TrashError::NotFound)?;
    let parent_id: Option<NodeId> = node.try_get("", "parent_id").map_err(database_error)?;
    let revision: i64 = node.try_get("", "revision").map_err(database_error)?;
    let batch: String = db::one(
        &tx,
        "SELECT batch_id FROM neev_node_trash WHERE org_id=$1 AND item_kind='node' AND item_id=$2 AND root_node_id=$3",
        vec![actor.org_id.into(), node_id.to_string().into(), node_id.into()],
    )
    .await?
    .ok_or(TrashError::NotFound)?
    .try_get("", "batch_id")
    .map_err(database_error)?;
    if let Some(parent) = parent_id {
        if node_trashed(&tx, actor.org_id, parent).await? {
            return Err(TrashError::ParentTrashed);
        }
    }
    let items = tx
        .query_all(db::statement(
            &tx,
            "SELECT item_kind,item_id FROM neev_node_trash WHERE org_id=$1 AND batch_id=$2 ORDER BY item_kind,item_id",
            vec![actor.org_id.into(), batch.clone().into()],
        ))
        .await
        .map_err(database_error)?;
    let mut nodes = 0;
    let mut ids = Vec::new();
    let mut session_ids = Vec::new();
    for row in items {
        let kind: String = row.try_get("", "item_kind").map_err(database_error)?;
        let id: String = row.try_get("", "item_id").map_err(database_error)?;
        match kind.as_str() {
            "resource" => ids.push(id),
            "session" => session_ids.push(id),
            _ => nodes += 1,
        }
    }
    db::execute(
        &tx,
        "DELETE FROM neev_node_trash WHERE org_id=$1 AND batch_id=$2",
        vec![actor.org_id.into(), batch.into()],
    )
    .await?;
    // Only what is still archived; visibility is never touched.
    let resources = touch(&tx, actor.org_id, &ids, false, now).await?;
    // Sessions come back to their owner exactly as they were: still private,
    // still filed where they were, with every turn and attachment.
    let sessions = touch_sessions(&tx, actor.org_id, &session_ids, false, now).await?;
    bump_node(&tx, actor.org_id, node_id, revision).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(Restored {
        nodes,
        resources,
        sessions,
    })
}

/// Every batch in the actor's org trash, newest first.
pub(crate) async fn list(
    state: &ServerState,
    actor: &OrgContext,
) -> Result<Vec<TrashItem>, TrashError> {
    let tx = begin(state, actor).await?;
    let rows = tx
        .query_all(db::statement(
            &tx,
            &format!(
                "SELECT t.batch_id,t.root_node_id,t.trashed_by,t.trashed_at,n.kind,n.name,n.parent_id,p.name AS parent_name,\
CASE WHEN pt.item_id IS NULL THEN 0 ELSE 1 END AS parent_trashed,COALESCE(a.display_name,'') AS trashed_by_name \
FROM neev_node_trash t JOIN neev_nodes n ON n.org_id=t.org_id AND n.id=t.root_node_id \
LEFT JOIN neev_nodes p ON p.org_id=n.org_id AND p.id=n.parent_id \
LEFT JOIN neev_node_trash pt ON pt.org_id=n.org_id AND pt.item_kind='node' AND pt.item_id=CAST(n.parent_id AS TEXT) \
LEFT JOIN neev_accounts a ON a.id=t.trashed_by \
WHERE t.org_id=$1 AND t.item_kind='node' AND t.item_id=CAST(t.root_node_id AS TEXT) \
ORDER BY t.trashed_at DESC,t.root_node_id DESC LIMIT {LIST_LIMIT}"
            ),
            vec![actor.org_id.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let batch: String = row.try_get("", "batch_id").map_err(database_error)?;
        let kind: String = row.try_get("", "kind").map_err(database_error)?;
        let kind = match kind.as_str() {
            "project" => NodeKind::Project,
            "folder" => NodeKind::Folder,
            _ => return Err(TrashError::Auth(AuthError::Unavailable)),
        };
        let nodes: i64 = db::one(
            &tx,
            "SELECT COUNT(*) AS n FROM neev_node_trash WHERE org_id=$1 AND batch_id=$2 AND item_kind='node'",
            vec![actor.org_id.into(), batch.clone().into()],
        )
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "n")
        .map_err(database_error)?;
        let counts = tx
            .query_all(db::statement(
                &tx,
                &format!(
                    "SELECT r.kind,COUNT(*) AS n FROM neev_node_trash t JOIN neev_resources r ON r.org_id=t.org_id AND r.id=t.item_id {ORIGIN_JOIN} WHERE t.org_id=$1 AND t.batch_id=$2 AND t.item_kind='resource' AND {} GROUP BY r.kind",
                    visible("$3")
                ),
                vec![actor.org_id.into(), batch.clone().into(), actor.identity.user_id.into()],
            ))
            .await
            .map_err(database_error)?;
        let (mut documents, mut chats) = (0, 0);
        for count in counts {
            let kind: String = count.try_get("", "kind").map_err(database_error)?;
            let n: i64 = count.try_get("", "n").map_err(database_error)?;
            match kind.as_str() {
                "document" => documents = n,
                "chat" => chats = n,
                _ => {}
            }
        }
        // Sessions are owner-private: a viewer counts only their own.
        let agent_chats: i64 = db::one(
            &tx,
            "SELECT COUNT(*) AS n FROM neev_node_trash t JOIN neev_agent_sessions g ON g.org_id=t.org_id AND g.id=t.item_id WHERE t.org_id=$1 AND t.batch_id=$2 AND t.item_kind='session' AND g.owner_id=$3 AND g.bound_chat_id IS NULL",
            vec![actor.org_id.into(), batch.clone().into(), actor.identity.user_id.into()],
        )
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "n")
        .map_err(database_error)?;
        let parent_trashed: i32 = row.try_get("", "parent_trashed").map_err(database_error)?;
        items.push(TrashItem {
            node_id: row.try_get("", "root_node_id").map_err(database_error)?,
            kind,
            name: row.try_get("", "name").map_err(database_error)?,
            parent_id: row.try_get("", "parent_id").map_err(database_error)?,
            parent_name: row.try_get("", "parent_name").map_err(database_error)?,
            parent_trashed: parent_trashed != 0,
            trashed_by: row.try_get("", "trashed_by").map_err(database_error)?,
            trashed_by_name: row.try_get("", "trashed_by_name").map_err(database_error)?,
            trashed_at: row.try_get("", "trashed_at").map_err(database_error)?,
            folders: nodes - 1,
            documents,
            chats,
            agent_chats,
        });
    }
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(items)
}
