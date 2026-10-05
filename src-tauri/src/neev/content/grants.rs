//! Sharing with members and teams (schema 24): the ONE effective-access rule,
//! and the grant management built on it.
//!
//! The rule, for a resource (a bound document uses its chat):
//! - its creator is `Owner`;
//! - `organization` is `Edit` for every member (unchanged);
//! - `private` is nobody else — the private exception: folder grants never
//!   reach it;
//! - `shared` (stored as `private` + a `resource` mark, so a rollback can only
//!   narrow) is the best of the direct grants to the reader or a team they are
//!   in, and the grants on its node or any ancestor node.
//!
//! [`level`] answers it for one resource; [`Audience::readable`] states the
//! same rule as SQL for lists, counts and notices. The two are kept in step by
//! `grants_tests::the_rust_rule_and_the_sql_rule_agree`.

use super::{access, resources, types::*};
use crate::neev::{
    auth::session,
    db,
    error::{database_error, AuthError},
    startup::ServerState,
    types::{OrgContext, Role},
};
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use std::collections::{HashMap, HashSet};

/// The ancestor walk is bounded even though node writes already refuse cycles.
const MAX_DEPTH: usize = 1000;
/// Far above the organization node limit (1000); more is corruption.
const MAX_ORG_NODES: usize = 5000;
/// Grants per target, so an access view is always one bounded answer.
pub(crate) const MAX_GRANTS_PER_TARGET: i64 = 200;
const MAX_SHARED_SESSIONS: i64 = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    View,
    Edit,
    Owner,
}

impl Level {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Edit => "edit",
            Self::Owner => "owner",
        }
    }

    fn stored(value: &str) -> Result<Self, ContentError> {
        match value {
            "view" => Ok(Self::View),
            "edit" => Ok(Self::Edit),
            _ => Err(ContentError::Storage),
        }
    }
}

/// Who a grant names. A team id is the canonical UUID text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Principal {
    User(i32),
    Team(String),
}

impl Principal {
    fn kind(&self) -> &'static str {
        match self {
            Self::User(_) => "user",
            Self::Team(_) => "team",
        }
    }

    fn id(&self) -> String {
        match self {
            Self::User(id) => id.to_string(),
            Self::Team(id) => id.clone(),
        }
    }
}

/// A principal as shown in an access view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PrincipalView {
    User {
        user_id: i32,
        display_name: String,
        avatar_url: Option<String>,
    },
    Team {
        team_id: String,
        name: String,
        member_count: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Grant {
    pub principal: PrincipalView,
    pub level: Level,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InheritedGrant {
    pub node_id: i32,
    pub node_name: String,
    pub principal: PrincipalView,
    pub level: Level,
}

/// Refusals the sharing endpoints name, beyond the shared content taxonomy.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SharingError {
    Content(ContentError),
    /// A grant on a resource whose visibility is not `shared`.
    NotShared,
    /// A team name already used in this organization (case-insensitive).
    NameTaken,
}

impl From<ContentError> for SharingError {
    fn from(value: ContentError) -> Self {
        Self::Content(value)
    }
}

impl From<AuthError> for SharingError {
    fn from(value: AuthError) -> Self {
        Self::Content(ContentError::Auth(value))
    }
}

/// `g` (a `neev_grants` alias) names `user` — directly, or through a team —
/// and only while `user` holds an ACTIVE membership of the grant's org on an
/// account that is not disabled (both branches alike). `user` is an SQL
/// expression: a placeholder or an integer literal.
fn names_reader(g: &str, user: &str) -> String {
    format!(
        "(({g}.principal_kind='user' AND {g}.principal_id=CAST({user} AS TEXT) AND EXISTS (SELECT 1 FROM neev_memberships um JOIN neev_accounts ua ON ua.id=um.user_id WHERE um.org_id={g}.org_id AND um.user_id={user} AND um.active=true AND ua.disabled=false)) OR ({g}.principal_kind='team' AND EXISTS (SELECT 1 FROM neev_team_members tm JOIN neev_memberships tmm ON tmm.org_id=tm.org_id AND tmm.user_id=tm.user_id AND tmm.active=true JOIN neev_accounts ta ON ta.id=tm.user_id AND ta.disabled=false WHERE tm.org_id={g}.org_id AND tm.team_id={g}.principal_id AND tm.user_id={user})))"
    )
}

/// The best level among `rows` of `level` text.
fn best(rows: &[sea_orm::QueryResult]) -> Result<Option<Level>, ContentError> {
    let mut out = None;
    for row in rows {
        let level = Level::stored(&row.try_get::<String>("", "level").map_err(database_error)?)?;
        out = out.max(Some(level));
    }
    Ok(out)
}

/// Node grants that reach `user`, by node: the best level on each node.
async fn node_grants<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    user: i32,
) -> Result<HashMap<i32, Level>, ContentError> {
    let rows = db
        .query_all(db::statement(
            db,
            &format!(
                "SELECT g.target_id,g.level FROM neev_grants g WHERE g.org_id=$1 AND g.target_kind='node' AND {}",
                names_reader("g", "$2")
            ),
            vec![org_id.into(), user.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut out: HashMap<i32, Level> = HashMap::new();
    for row in &rows {
        let node: i32 = row
            .try_get::<String>("", "target_id")
            .map_err(database_error)?
            .parse()
            .map_err(|_| ContentError::Storage)?;
        let level = Level::stored(&row.try_get::<String>("", "level").map_err(database_error)?)?;
        let entry = out.entry(node).or_insert(level);
        *entry = (*entry).max(level);
    }
    Ok(out)
}

/// One node as the folder rule needs it.
#[derive(Clone, Copy, Debug)]
struct NodeFacts {
    id: i32,
    parent: Option<i32>,
    creator: i32,
    restricted: bool,
}

/// What one reader may do at one node: the folder rule (schema 24, v2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Place {
    /// Every restricted node on the chain admits the reader.
    pub visible: bool,
    /// The node is inside a restricted region.
    pub restricted_region: bool,
    /// The nearest restricted node at or above this one.
    pub restricted_root: Option<i32>,
    /// The outermost restricted node at or above this one: its creator
    /// controls every restriction and grant inside (P-1/P-8).
    pub outermost_root: Option<i32>,
    /// Folder level: Edit for everyone in an open region; in a restricted
    /// region Owner for the nearest restricted root's creator, else the best
    /// grant between here and that root. `None` when not visible.
    pub folder: Option<Level>,
    /// What folder grants give a `shared` item here: grants from this node up
    /// to the nearest restricted root (or the top), never from above it.
    pub inherited: Option<Level>,
}

/// THE folder rule. `chain` is the node and its ancestors, nearest first.
///
/// A restricted node X admits its own creator, the creator of the OUTERMOST
/// restricted node above it (who controls every restriction inside — so a
/// nested restriction can never strand content), and anyone holding a grant
/// on a node between X and the node being viewed. Grants above X never reach
/// in.
fn compute(chain: &[NodeFacts], grants: &HashMap<i32, Level>, user: i32) -> Place {
    let outermost = chain.iter().rev().find(|node| node.restricted).copied();
    let mut acc: Option<Level> = None;
    let mut visible = true;
    let mut first: Option<(i32, bool, Option<Level>)> = None;
    for node in chain {
        acc = acc.max(grants.get(&node.id).copied());
        if node.restricted {
            let controller = node.creator == user
                || outermost.is_some_and(|outer| outer.creator == user);
            if !controller && acc.is_none() {
                visible = false;
            }
            if first.is_none() {
                first = Some((node.id, controller, acc));
            }
        }
    }
    match first {
        None => Place {
            visible: true,
            restricted_region: false,
            restricted_root: None,
            outermost_root: None,
            folder: Some(Level::Edit),
            inherited: acc,
        },
        Some((root, controller, segment)) => {
            let folder = if controller { Some(Level::Owner) } else { segment };
            Place {
                visible,
                restricted_region: true,
                restricted_root: Some(root),
                outermost_root: outermost.map(|outer| outer.id),
                folder: if visible { folder } else { None },
                inherited: if visible {
                    segment.max(controller.then_some(Level::Edit))
                } else {
                    None
                },
            }
        }
    }
}

/// The level a reader has on a resource at `place`, given its direct grant.
fn resource_rule(resource: &Resource, user: i32, place: Place, direct: Option<Level>) -> Option<Level> {
    if resource.creator_id == user {
        return Some(Level::Owner);
    }
    if !place.visible {
        // An explicit share reaches into a hidden folder; nothing else does.
        return match resource.visibility {
            Visibility::Shared => direct,
            _ => None,
        };
    }
    match resource.visibility {
        Visibility::Organization => {
            if place.restricted_region {
                place.folder.map(|level| level.min(Level::Edit))
            } else {
                Some(Level::Edit)
            }
        }
        Visibility::Shared => direct.max(place.inherited),
        Visibility::Private => None,
    }
}

const NODE_FACTS: &str = "SELECT n.id,n.parent_id,n.created_by,(SELECT COUNT(*) FROM neev_shared_marks k WHERE k.org_id=n.org_id AND k.mark_kind='node_restricted' AND k.target_id=CAST(n.id AS TEXT)) AS restricted FROM neev_nodes n";

fn facts(row: &sea_orm::QueryResult) -> Result<NodeFacts, ContentError> {
    Ok(NodeFacts {
        id: row.try_get("", "id").map_err(database_error)?,
        parent: row.try_get("", "parent_id").map_err(database_error)?,
        creator: row.try_get("", "created_by").map_err(database_error)?,
        restricted: row.try_get::<i64>("", "restricted").map_err(database_error)? > 0,
    })
}

/// `node_id` and its ancestors, nearest first.
async fn chain<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    node_id: i32,
) -> Result<Vec<NodeFacts>, ContentError> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = Some(node_id);
    while let Some(id) = cursor {
        if !seen.insert(id) || seen.len() > MAX_DEPTH {
            return Err(ContentError::Storage);
        }
        let row = db::one(
            db,
            &format!("{NODE_FACTS} WHERE n.org_id=$1 AND n.id=$2"),
            vec![org_id.into(), id.into()],
        )
        .await?
        .ok_or(ContentError::Storage)?;
        let node = facts(&row)?;
        cursor = node.parent;
        out.push(node);
    }
    Ok(out)
}

/// The folder rule for one node and one reader.
pub(crate) async fn place<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    user: i32,
    node_id: i32,
) -> Result<Place, ContentError> {
    let grants = node_grants(db, org_id, user).await?;
    Ok(compute(&chain(db, org_id, node_id).await?, &grants, user))
}

async fn direct_level<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    user: i32,
    id: &str,
) -> Result<Option<Level>, ContentError> {
    let direct = db
        .query_all(db::statement(
            db,
            &format!(
                "SELECT g.level FROM neev_grants g WHERE g.org_id=$1 AND g.target_kind='resource' AND g.target_id=$2 AND {}",
                names_reader("g", "$3")
            ),
            vec![org_id.into(), id.into(), user.into()],
        ))
        .await
        .map_err(database_error)?;
    best(&direct)
}

/// What `user` may do with `audience` — the resource itself, or for a bound
/// document the chat whose audience it takes. `None` is "cannot see it".
pub(crate) async fn level<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    user: i32,
    audience: &Resource,
) -> Result<Option<Level>, ContentError> {
    if audience.org_id != org_id {
        return Ok(None);
    }
    if audience.creator_id == user {
        return Ok(Some(Level::Owner));
    }
    if audience.visibility == Visibility::Private {
        return Ok(None);
    }
    let place = place(db, org_id, user, audience.node_id).await?;
    let direct = if audience.visibility == Visibility::Shared {
        direct_level(db, org_id, user, &audience.id).await?
    } else {
        None
    };
    Ok(resource_rule(audience, user, place, direct))
}

/// The whole organization's tree as one reader sees it (the folder rule for
/// every node at once). For lists, the catalog and SQL predicates.
pub(crate) struct Tree {
    user: i32,
    nodes: HashMap<i32, NodeFacts>,
    places: HashMap<i32, Place>,
}

impl Tree {
    pub(crate) async fn load<C: ConnectionTrait>(
        db: &C,
        org_id: i32,
        user: i32,
    ) -> Result<Self, ContentError> {
        let rows = db
            .query_all(db::statement(
                db,
                &format!("{NODE_FACTS} WHERE n.org_id=$1 LIMIT {}", MAX_ORG_NODES + 1),
                vec![org_id.into()],
            ))
            .await
            .map_err(database_error)?;
        if rows.len() > MAX_ORG_NODES {
            return Err(ContentError::Storage);
        }
        let mut nodes = HashMap::with_capacity(rows.len());
        for row in &rows {
            let node = facts(row)?;
            nodes.insert(node.id, node);
        }
        let grants = node_grants(db, org_id, user).await?;
        let mut places = HashMap::with_capacity(nodes.len());
        for &id in nodes.keys() {
            let mut path = Vec::new();
            let mut cursor = Some(id);
            while let Some(at) = cursor {
                if path.len() >= MAX_DEPTH {
                    return Err(ContentError::Storage);
                }
                let node = *nodes.get(&at).ok_or(ContentError::Storage)?;
                path.push(node);
                cursor = node.parent;
            }
            places.insert(id, compute(&path, &grants, user));
        }
        Ok(Self { user, nodes, places })
    }

    pub(crate) fn place(&self, node_id: i32) -> Option<Place> {
        self.places.get(&node_id).copied()
    }

    pub(crate) fn visible(&self, node_id: i32) -> bool {
        self.place(node_id).is_some_and(|place| place.visible)
    }

    /// Visible with folder level Edit or Owner: may hold new or moved-in items.
    pub(crate) fn placeable(&self, node_id: i32) -> bool {
        self.place(node_id)
            .is_some_and(|place| place.visible && place.folder >= Some(Level::Edit))
    }

    pub(crate) fn creator(&self, node_id: i32) -> Option<i32> {
        self.nodes.get(&node_id).map(|node| node.creator)
    }

    pub(crate) fn parent(&self, node_id: i32) -> Option<i32> {
        self.nodes.get(&node_id).and_then(|node| node.parent)
    }

    pub(crate) fn restricted(&self, node_id: i32) -> bool {
        self.nodes.get(&node_id).is_some_and(|node| node.restricted)
    }

    /// The restricted nodes at or above `node_id`.
    pub(crate) fn restricted_roots(&self, node_id: i32) -> Vec<i32> {
        let mut out = Vec::new();
        let mut cursor = Some(node_id);
        let mut depth = 0;
        while let Some(at) = cursor {
            depth += 1;
            if depth > MAX_DEPTH {
                break;
            }
            if self.restricted(at) {
                out.push(at);
            }
            cursor = self.parent(at);
        }
        out
    }

    /// Moving something from under `from` to under `to` leaves these
    /// restricted roots; only their creator may widen it so.
    pub(crate) fn may_leave(&self, from: i32, to: i32) -> bool {
        let after = self.restricted_roots(to);
        let before = self.restricted_roots(from);
        // The outermost root's creator controls every restriction inside it.
        let outer = before.last().copied();
        before
            .into_iter()
            .filter(|root| !after.contains(root))
            .all(|root| {
                self.creator(root) == Some(self.user)
                    || outer.is_some_and(|outer| self.creator(outer) == Some(self.user))
            })
    }

    /// `node_id` and everything below it.
    pub(crate) fn subtree(&self, node_id: i32) -> Vec<i32> {
        let mut out = Vec::new();
        for &id in self.nodes.keys() {
            let mut cursor = Some(id);
            let mut depth = 0;
            while let Some(at) = cursor {
                depth += 1;
                if depth > MAX_DEPTH {
                    break;
                }
                if at == node_id {
                    out.push(id);
                    break;
                }
                cursor = self.parent(at);
            }
        }
        out.sort_unstable();
        out
    }

    /// The creator of the outermost restricted node at or above `node_id`:
    /// who controls the restriction there today.
    fn controller(&self, node_id: i32) -> Option<i32> {
        self.restricted_roots(node_id).last().and_then(|root| self.creator(*root))
    }

    /// T-1: a change that alters which restrictions govern `node_id`'s subtree
    /// (restricting or reopening it, or moving it across a restriction
    /// boundary) is only for someone who can see ALL of that subtree and
    /// already controls every restriction inside it — otherwise it would take
    /// over, expose or strand someone else's restricted area. `node_id` itself
    /// is skipped when it is the node being restricted or reopened.
    pub(crate) fn controls_subtree(&self, node_id: i32, skip_self: bool) -> bool {
        self.subtree(node_id).into_iter().all(|id| {
            self.visible(id)
                && ((skip_self && id == node_id)
                    || !self.restricted(id)
                    || self.controller(id) == Some(self.user))
        })
    }

    /// Moving out from under `from` to the top level (no new parent): every
    /// restricted root left must be the mover's, or the outermost one's.
    pub(crate) fn may_leave_to_top(&self, from: i32) -> bool {
        let before = self.restricted_roots(from);
        let outer = before.last().copied();
        before.into_iter().all(|root| {
            self.creator(root) == Some(self.user)
                || outer.is_some_and(|outer| self.creator(outer) == Some(self.user))
        })
    }

    /// Nodes this reader cannot see.
    pub(crate) fn hidden(&self) -> Vec<i32> {
        let mut out: Vec<i32> = self
            .places
            .iter()
            .filter(|(_, place)| !place.visible)
            .map(|(id, _)| *id)
            .collect();
        out.sort_unstable();
        out
    }

    /// Nodes whose folder grants reach a `shared` item there.
    fn inheriting(&self) -> Vec<i32> {
        let mut out: Vec<i32> = self
            .places
            .iter()
            .filter(|(_, place)| place.visible && place.inherited.is_some())
            .map(|(id, _)| *id)
            .collect();
        out.sort_unstable();
        out
    }
}

/// The reader's side of the rule, precomputed once per statement, as SQL.
pub(crate) struct Audience {
    user: i32,
    hidden: Vec<i32>,
    inheriting: Vec<i32>,
}

fn id_list(ids: &[i32]) -> String {
    ids.iter().map(i32::to_string).collect::<Vec<_>>().join(",")
}

impl Audience {
    pub(crate) async fn load<C: ConnectionTrait>(
        db: &C,
        org_id: i32,
        user: i32,
    ) -> Result<Self, ContentError> {
        Ok(Self::from_tree(&Tree::load(db, org_id, user).await?))
    }

    pub(crate) fn from_tree(tree: &Tree) -> Self {
        Self { user: tree.user, hidden: tree.hidden(), inheriting: tree.inheriting() }
    }

    /// Nodes hidden from this reader, as an SQL `IN` list body (`NULL` when
    /// none, so `x IN (NULL)` matches nothing).
    pub(crate) fn hidden_list(&self) -> String {
        if self.hidden.is_empty() {
            "NULL".to_owned()
        } else {
            id_list(&self.hidden)
        }
    }

    /// `r` (a non-bound resource alias) is readable. Only integer literals are
    /// inlined, so the fragment adds no placeholder to the caller's statement.
    fn own(&self, r: &str) -> String {
        let user = self.user.to_string();
        let open = if self.hidden.is_empty() {
            String::new()
        } else {
            format!(" AND {r}.node_id NOT IN ({})", id_list(&self.hidden))
        };
        let inherited = if self.inheriting.is_empty() {
            String::new()
        } else {
            format!(" OR {r}.node_id IN ({})", id_list(&self.inheriting))
        };
        format!(
            "({r}.creator_id={user} OR ({r}.visibility='organization'{open}) OR ({r}.visibility='private' AND EXISTS (SELECT 1 FROM neev_shared_marks k WHERE k.org_id={r}.org_id AND k.mark_kind='resource' AND k.target_id={r}.id) AND (EXISTS (SELECT 1 FROM neev_grants g WHERE g.org_id={r}.org_id AND g.target_kind='resource' AND g.target_id={r}.id AND {}){inherited})))",
            names_reader("g", &user)
        )
    }

    /// `r` is readable, where `s` is the LEFT JOINed origin chat of `r`: a
    /// bound document has its chat's audience.
    pub(crate) fn readable(&self, r: &str, s: &str) -> String {
        format!(
            "(({r}.origin_chat_id IS NULL AND {}) OR ({r}.origin_chat_id IS NOT NULL AND {s}.id IS NOT NULL AND {}))",
            self.own(r),
            self.own(s)
        )
    }

    /// `r` is a readable chat or other non-bound resource.
    pub(crate) fn readable_unbound(&self, r: &str) -> String {
        format!("({r}.origin_chat_id IS NULL AND {})", self.own(r))
    }
}

/// Make the stored mark match `visibility` for a resource just written with
/// `access::visibility_name(visibility)`. Leaving `shared` ends every direct
/// grant, so a later return to `shared` starts from nobody. A bound document
/// never carries a mark: its audience is its chat's.
pub(crate) async fn store_visibility(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    id: &str,
    visibility: Visibility,
    bound: bool,
    now: i64,
) -> Result<(), ContentError> {
    if visibility == Visibility::Shared && !bound {
        db::execute(
            tx,
            "INSERT INTO neev_shared_marks(org_id,mark_kind,target_id,set_by,created_at) VALUES($1,'resource',$2,$3,$4) ON CONFLICT(org_id,mark_kind,target_id) DO NOTHING",
            vec![actor.org_id.into(), id.into(), actor.identity.user_id.into(), now.into()],
        )
        .await?;
    } else {
        db::execute(
            tx,
            "DELETE FROM neev_shared_marks WHERE org_id=$1 AND mark_kind='resource' AND target_id=$2",
            vec![actor.org_id.into(), id.into()],
        )
        .await?;
        db::execute(
            tx,
            "DELETE FROM neev_grants WHERE org_id=$1 AND target_kind='resource' AND target_id=$2",
            vec![actor.org_id.into(), id.into()],
        )
        .await?;
    }
    Ok(())
}

/// Membership removal: the user's own grants and team places go in the same
/// transaction, so a later re-invite starts with no shares at all. Teams they
/// left get a new revision.
pub(crate) async fn revoke_member(
    tx: &DatabaseTransaction,
    org_id: i32,
    user: i32,
    now: i64,
) -> Result<(), AuthError> {
    db::execute(
        tx,
        "DELETE FROM neev_grants WHERE org_id=$1 AND principal_kind='user' AND principal_id=$2",
        vec![org_id.into(), user.to_string().into()],
    )
    .await?;
    db::execute(
        tx,
        "UPDATE neev_teams SET revision=revision+1,updated_at=$1 WHERE org_id=$2 AND id IN (SELECT team_id FROM neev_team_members WHERE org_id=$2 AND user_id=$3)",
        vec![now.into(), org_id.into(), user.into()],
    )
    .await?;
    db::execute(
        tx,
        "DELETE FROM neev_team_members WHERE org_id=$1 AND user_id=$2",
        vec![org_id.into(), user.into()],
    )
    .await?;
    Ok(())
}

async fn principal_view<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    kind: &str,
    id: &str,
) -> Result<Option<PrincipalView>, ContentError> {
    match kind {
        "user" => {
            let user: i32 = id.parse().map_err(|_| ContentError::Storage)?;
            let row = db::one(
                db,
                "SELECT a.display_name,a.avatar_url FROM neev_memberships m JOIN neev_accounts a ON a.id=m.user_id WHERE m.org_id=$1 AND m.user_id=$2 AND m.active=true",
                vec![org_id.into(), user.into()],
            )
            .await?;
            row.map(|row| {
                Ok(PrincipalView::User {
                    user_id: user,
                    display_name: row.try_get("", "display_name").map_err(database_error)?,
                    avatar_url: row.try_get("", "avatar_url").map_err(database_error)?,
                })
            })
            .transpose()
        }
        "team" => {
            let row = db::one(
                db,
                "SELECT t.name,(SELECT COUNT(*) FROM neev_team_members tm WHERE tm.org_id=t.org_id AND tm.team_id=t.id) AS members FROM neev_teams t WHERE t.org_id=$1 AND t.id=$2",
                vec![org_id.into(), id.into()],
            )
            .await?;
            row.map(|row| {
                Ok(PrincipalView::Team {
                    team_id: id.to_owned(),
                    name: row.try_get("", "name").map_err(database_error)?,
                    member_count: row.try_get("", "members").map_err(database_error)?,
                })
            })
            .transpose()
        }
        _ => Err(ContentError::Storage),
    }
}

/// Grants on one target, named. A grant to a user who is no longer an active
/// member names nobody and is not shown (removal deleted it anyway).
async fn direct<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    kind: &str,
    id: &str,
) -> Result<Vec<Grant>, ContentError> {
    let rows = db
        .query_all(db::statement(
            db,
            &format!("SELECT principal_kind,principal_id,level FROM neev_grants WHERE org_id=$1 AND target_kind=$2 AND target_id=$3 ORDER BY principal_kind,principal_id LIMIT {MAX_GRANTS_PER_TARGET}"),
            vec![org_id.into(), kind.into(), id.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let principal_kind: String = row.try_get("", "principal_kind").map_err(database_error)?;
        let principal_id: String = row.try_get("", "principal_id").map_err(database_error)?;
        let level = Level::stored(&row.try_get::<String>("", "level").map_err(database_error)?)?;
        if let Some(principal) = principal_view(db, org_id, &principal_kind, &principal_id).await? {
            out.push(Grant { principal, level });
        }
    }
    Ok(out)
}

/// Grants on `node_id` (when `include_self`) and its ancestors, nearest first —
/// only nodes the reader can see, and never past the nearest restricted root
/// (grants above it do not reach in, and a hidden ancestor's name never shows).
async fn inherited<C: ConnectionTrait>(
    db: &C,
    tree: &Tree,
    org_id: i32,
    node_id: i32,
    include_self: bool,
) -> Result<Vec<InheritedGrant>, ContentError> {
    let mut out = Vec::new();
    let mut cursor = Some(node_id);
    let mut depth = 0;
    while let Some(id) = cursor {
        depth += 1;
        if depth > MAX_DEPTH {
            return Err(ContentError::Storage);
        }
        if (id != node_id || include_self) && tree.visible(id) {
            let name: String = db::one(
                db,
                "SELECT name FROM neev_nodes WHERE org_id=$1 AND id=$2",
                vec![org_id.into(), id.into()],
            )
            .await?
            .ok_or(ContentError::Storage)?
            .try_get("", "name")
            .map_err(database_error)?;
            for grant in direct(db, org_id, "node", &id.to_string()).await? {
                out.push(InheritedGrant {
                    node_id: id,
                    node_name: name.clone(),
                    principal: grant.principal,
                    level: grant.level,
                });
            }
        }
        if tree.restricted(id) {
            break;
        }
        cursor = tree.parent(id);
    }
    Ok(out)
}

/// A principal a grant may name: an active member of the actor's org other
/// than `owner`, or one of the org's teams. Anything else is not found.
async fn eligible(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    principal: &Principal,
    owner: i32,
) -> Result<(), ContentError> {
    match principal {
        Principal::User(user) => {
            if *user == owner {
                return Err(ContentError::Invalid);
            }
            db::one(
                tx,
                "SELECT 1 AS hit FROM neev_memberships m JOIN neev_accounts a ON a.id=m.user_id WHERE m.org_id=$1 AND m.user_id=$2 AND m.active=true AND a.disabled=false",
                vec![actor.org_id.into(), (*user).into()],
            )
            .await?
            .ok_or(ContentError::NotFound)?;
        }
        Principal::Team(team) => {
            access::canonical_id(team)?;
            db::one(
                tx,
                "SELECT 1 AS hit FROM neev_teams WHERE org_id=$1 AND id=$2",
                vec![actor.org_id.into(), team.clone().into()],
            )
            .await?
            .ok_or(ContentError::NotFound)?;
        }
    }
    Ok(())
}

/// Upsert (`Some(level)`) or remove (`None`) one grant, bounded per target.
async fn write_grant(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    kind: &str,
    target: &str,
    principal: &Principal,
    level: Option<Level>,
    now: i64,
) -> Result<(), ContentError> {
    match level {
        Some(level) => {
            let count: i64 = db::one(
                tx,
                "SELECT COUNT(*) AS n FROM neev_grants WHERE org_id=$1 AND target_kind=$2 AND target_id=$3 AND NOT (principal_kind=$4 AND principal_id=$5)",
                vec![actor.org_id.into(), kind.into(), target.into(), principal.kind().into(), principal.id().into()],
            )
            .await?
            .ok_or(ContentError::Storage)?
            .try_get("", "n")
            .map_err(database_error)?;
            if count >= MAX_GRANTS_PER_TARGET {
                return Err(ContentError::Quota);
            }
            db::execute(
                tx,
                "INSERT INTO neev_grants(org_id,target_kind,target_id,principal_kind,principal_id,level,granted_by,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(org_id,target_kind,target_id,principal_kind,principal_id) DO UPDATE SET level=excluded.level,granted_by=excluded.granted_by,created_at=excluded.created_at",
                vec![
                    actor.org_id.into(),
                    kind.into(),
                    target.into(),
                    principal.kind().into(),
                    principal.id().into(),
                    level.name().into(),
                    actor.identity.user_id.into(),
                    now.into(),
                ],
            )
            .await?;
        }
        None => {
            db::execute(
                tx,
                "DELETE FROM neev_grants WHERE org_id=$1 AND target_kind=$2 AND target_id=$3 AND principal_kind=$4 AND principal_id=$5",
                vec![actor.org_id.into(), kind.into(), target.into(), principal.kind().into(), principal.id().into()],
            )
            .await?;
        }
    }
    Ok(())
}

fn now(state: &ServerState) -> Result<i64, ContentError> {
    let now = state.clock.now_unix();
    if now < 0 {
        Err(ContentError::Invalid)
    } else {
        Ok(now)
    }
}

// ---------------------------------------------------------------- resources

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResourceAccess {
    pub resource_id: String,
    pub visibility: Visibility,
    pub auth_revision: i64,
    pub level: Level,
    pub can_manage: bool,
    pub bound_to: Option<String>,
    pub direct: Vec<Grant>,
    /// Grants on the containing folder/project and its ancestors. They apply
    /// only while the visibility is `shared`.
    pub inherited: Vec<InheritedGrant>,
}

async fn resource_view(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    id: &str,
) -> Result<ResourceAccess, ContentError> {
    let (resource, level) = access::resolve(tx, actor, id, false).await?;
    // A bound document shows its chat's sharing, which is what decides it.
    let audience = match &resource.origin_chat_id {
        Some(chat) => access::load(tx, actor.org_id, chat).await?,
        None => resource.clone(),
    };
    let direct = if audience.visibility == Visibility::Shared {
        direct(tx, actor.org_id, "resource", &audience.id).await?
    } else {
        Vec::new()
    };
    Ok(ResourceAccess {
        resource_id: resource.id.clone(),
        visibility: resource.visibility,
        auth_revision: resource.auth_revision,
        level,
        can_manage: resource.origin_chat_id.is_none() && resource.creator_id == actor.identity.user_id,
        bound_to: resource.origin_chat_id.clone(),
        direct,
        inherited: {
            let tree = Tree::load(tx, actor.org_id, actor.identity.user_id).await?;
            inherited(tx, &tree, actor.org_id, audience.node_id, true).await?
        },
    })
}

/// Anyone who can read the resource may see who else can.
pub(crate) async fn resource_access(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
) -> Result<ResourceAccess, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let result = resource_view(&tx, actor, id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

/// The creator adds, changes (`Some`) or removes (`None`) one direct grant on
/// a `shared` resource. Bumps `auth_revision` on it and on its bound documents,
/// which is what leases, editors and the knowledge layer key on.
pub(crate) async fn set_resource_grant(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    expected_auth_revision: i64,
    principal: &Principal,
    level: Option<Level>,
) -> Result<ResourceAccess, SharingError> {
    if expected_auth_revision < 1 {
        return Err(ContentError::Invalid.into());
    }
    let now = now(state)?;
    let tx = resources::begin(state, actor).await?;
    let locked = access::lock_resources(&tx, actor, id, true).await?;
    let (current, _) = access::resolve(&tx, actor, id, false).await?;
    if current.origin_chat_id.is_some() {
        return Err(ContentError::Invalid.into());
    }
    if current.creator_id != actor.identity.user_id {
        return Err(ContentError::Auth(AuthError::Forbidden).into());
    }
    if current.auth_revision != expected_auth_revision {
        return Err(ContentError::Conflict.into());
    }
    if current.visibility != Visibility::Shared {
        return Err(SharingError::NotShared);
    }
    if crate::neev::catalog::trash::resource_trashed(&tx, actor.org_id, id).await? {
        return Err(ContentError::Archived.into());
    }
    if level.is_some() {
        eligible(&tx, actor, principal, current.creator_id).await?;
    } else if let Principal::Team(team) = principal {
        access::canonical_id(team)?;
    }
    write_grant(&tx, actor, "resource", id, principal, level, now).await?;
    for target in &locked {
        db::execute(
            &tx,
            "UPDATE neev_resources SET auth_revision=auth_revision+1,updated_at=$1 WHERE org_id=$2 AND id=$3",
            vec![now.into(), actor.org_id.into(), target.clone().into()],
        )
        .await?;
    }
    let result = resource_view(&tx, actor, id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

// -------------------------------------------------------------------- nodes

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NodeAccess {
    pub node_id: i32,
    pub revision: i64,
    pub can_manage: bool,
    /// May restrict or reopen this node (`can_restrict_node`).
    pub can_restrict: bool,
    pub restricted: bool,
    /// The caller's folder level here.
    pub level: Level,
    pub direct: Vec<Grant>,
    pub inherited: Vec<InheritedGrant>,
}

/// (revision, creator) of a live project or folder of the actor's org that
/// the actor can SEE; a hidden one is not found, exactly like a missing one.
async fn node_row(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    tree: &Tree,
    node_id: i32,
    locked: bool,
) -> Result<(i64, i32), ContentError> {
    if node_id <= 0 || !tree.visible(node_id) {
        return Err(ContentError::NotFound);
    }
    let row = db::one(
        tx,
        &format!(
            "SELECT kind,revision,created_by FROM neev_nodes WHERE org_id=$1 AND id=$2{}",
            if locked { session::lock(tx) } else { "" }
        ),
        vec![actor.org_id.into(), node_id.into()],
    )
    .await?
    .ok_or(ContentError::NotFound)?;
    let kind: String = row.try_get("", "kind").map_err(database_error)?;
    if !matches!(kind.as_str(), "project" | "folder") {
        return Err(ContentError::NotFound);
    }
    if crate::neev::catalog::trash::node_trashed(tx, actor.org_id, node_id).await? {
        return Err(ContentError::NotFound);
    }
    Ok((
        row.try_get("", "revision").map_err(database_error)?,
        row.try_get("", "created_by").map_err(database_error)?,
    ))
}

/// Who may change a node's grants (and its new-item default): in an open
/// region its creator or an admin; in a restricted region ONLY the outermost
/// restricted root's creator — an Edit recipient works inside a restriction
/// but never re-shares it, not even through a subfolder they created. Admins
/// are not exempt from restriction.
pub(crate) fn can_manage_node(tree: &Tree, actor: &OrgContext, node_id: i32) -> bool {
    let Some(place) = tree.place(node_id) else {
        return false;
    };
    if !place.visible {
        return false;
    }
    match place.outermost_root {
        Some(root) => tree.creator(root) == Some(actor.identity.user_id),
        None => tree.creator(node_id) == Some(actor.identity.user_id) || actor.role == Role::Admin,
    }
}

/// Who may restrict or reopen a node (P-8): outside any restriction its
/// creator; inside one, only the creator of the outermost restricted node
/// above it — so an Edit recipient can never create a restriction that hides
/// content from the person who controls the region. No admin bypass.
pub(crate) fn can_restrict_node(tree: &Tree, actor: &OrgContext, node_id: i32) -> bool {
    if !tree.visible(node_id) {
        return false;
    }
    let above = tree
        .parent(node_id)
        .map(|parent| tree.restricted_roots(parent))
        .unwrap_or_default();
    match above.last() {
        Some(outer) => tree.creator(*outer) == Some(actor.identity.user_id),
        None => tree.creator(node_id) == Some(actor.identity.user_id),
    }
}

async fn node_view(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    node_id: i32,
) -> Result<NodeAccess, ContentError> {
    let tree = Tree::load(tx, actor.org_id, actor.identity.user_id).await?;
    let (revision, _) = node_row(tx, actor, &tree, node_id, false).await?;
    let place = tree.place(node_id).ok_or(ContentError::NotFound)?;
    Ok(NodeAccess {
        node_id,
        revision,
        can_manage: can_manage_node(&tree, actor, node_id),
        can_restrict: can_restrict_node(&tree, actor, node_id),
        restricted: tree.restricted(node_id),
        level: place.folder.ok_or(ContentError::NotFound)?,
        direct: direct(tx, actor.org_id, "node", &node_id.to_string()).await?,
        inherited: inherited(tx, &tree, actor.org_id, node_id, false).await?,
    })
}

/// Anyone who can see the node.
pub(crate) async fn node_access(
    state: &ServerState,
    actor: &OrgContext,
    node_id: i32,
) -> Result<NodeAccess, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let result = node_view(&tx, actor, node_id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

async fn bump_node(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    node_id: i32,
    revision: i64,
) -> Result<(), ContentError> {
    let next = revision.checked_add(1).ok_or(ContentError::Conflict)?;
    let bumped = db::execute(
        tx,
        "UPDATE neev_nodes SET revision=$1 WHERE org_id=$2 AND id=$3 AND revision=$4",
        vec![next.into(), actor.org_id.into(), node_id.into(), revision.into()],
    )
    .await?;
    if bumped.rows_affected() != 1 {
        return Err(ContentError::Conflict);
    }
    Ok(())
}

/// `can_manage_node`. Bumps the node revision, exactly as a default-visibility
/// change does; touches no resource row (a folder grant is read live).
pub(crate) async fn set_node_grant(
    state: &ServerState,
    actor: &OrgContext,
    node_id: i32,
    expected_revision: i64,
    principal: &Principal,
    level: Option<Level>,
) -> Result<NodeAccess, ContentError> {
    if expected_revision <= 0 {
        return Err(ContentError::Invalid);
    }
    let now = now(state)?;
    let tx = resources::begin(state, actor).await?;
    let tree = Tree::load(&tx, actor.org_id, actor.identity.user_id).await?;
    let (revision, _) = node_row(&tx, actor, &tree, node_id, true).await?;
    if !can_manage_node(&tree, actor, node_id) {
        return Err(ContentError::Auth(AuthError::Forbidden));
    }
    if revision != expected_revision {
        return Err(ContentError::Conflict);
    }
    if level.is_some() {
        // No one is the owner of a folder's content as a whole: any active
        // member (the creator included) or team may be named.
        eligible(&tx, actor, principal, 0).await?;
    } else if let Principal::Team(team) = principal {
        access::canonical_id(team)?;
    }
    bump_node(&tx, actor, node_id, revision).await?;
    write_grant(&tx, actor, "node", &node_id.to_string(), principal, level, now).await?;
    let result = node_view(&tx, actor, node_id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

/// Restrict (hide from everyone but its controllers and its recipients) or
/// reopen a project or folder: `can_restrict_node`. Grants are kept either
/// way. Rewrites the derived region in the same transaction.
pub(crate) async fn set_node_restricted(
    state: &ServerState,
    actor: &OrgContext,
    node_id: i32,
    expected_revision: i64,
    restricted: bool,
) -> Result<NodeAccess, ContentError> {
    if expected_revision <= 0 {
        return Err(ContentError::Invalid);
    }
    let now = now(state)?;
    let tx = resources::begin(state, actor).await?;
    let tree = Tree::load(&tx, actor.org_id, actor.identity.user_id).await?;
    let (revision, _) = node_row(&tx, actor, &tree, node_id, true).await?;
    if !can_restrict_node(&tree, actor, node_id) || !tree.controls_subtree(node_id, true) {
        return Err(ContentError::Auth(AuthError::Forbidden));
    }
    if revision != expected_revision {
        return Err(ContentError::Conflict);
    }
    if tree.restricted(node_id) != restricted {
        bump_node(&tx, actor, node_id, revision).await?;
        if restricted {
            db::execute(
                &tx,
                "INSERT INTO neev_shared_marks(org_id,mark_kind,target_id,set_by,created_at) VALUES($1,'node_restricted',$2,$3,$4)",
                vec![actor.org_id.into(), node_id.to_string().into(), actor.identity.user_id.into(), now.into()],
            )
            .await?;
        } else {
            db::execute(
                &tx,
                "DELETE FROM neev_shared_marks WHERE org_id=$1 AND mark_kind='node_restricted' AND target_id=$2",
                vec![actor.org_id.into(), node_id.to_string().into()],
            )
            .await?;
        }
        crate::neev::db::member_sharing_migration::refresh_region(&tx, actor.org_id).await?;
    }
    let result = node_view(&tx, actor, node_id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

/// Chats and documents the reader can open whose folder is hidden from them —
/// shared with them directly, or their own — newest first. Opened by id; the
/// folder's name never travels.
pub(crate) async fn shared_in_hidden(
    state: &ServerState,
    actor: &OrgContext,
) -> Result<Vec<Resource>, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let audience = Audience::load(&tx, actor.org_id, actor.identity.user_id).await?;
    let rows = tx
        .query_all(db::statement(
            &tx,
            &format!(
                "SELECT r.id FROM neev_resources r LEFT JOIN neev_resources s ON s.org_id=r.org_id AND s.id=r.origin_chat_id AND s.kind='chat' WHERE r.org_id=$1 AND r.archived=false AND r.node_id IN ({}) AND {} ORDER BY r.updated_at DESC,r.id LIMIT 200",
                audience.hidden_list(),
                audience.readable("r", "s")
            ),
            vec![actor.org_id.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: String = row.try_get("", "id").map_err(database_error)?;
        out.push(access::authorized(&tx, actor, &id, false).await?);
    }
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(out)
}

// ----------------------------------------------------------------- sessions

/// A standalone Agent Chat a reader may view: their own, or one shared with
/// them. Shareable sessions are `chat` sessions with no bound chat (a bound
/// one's context is a chat the viewer may not read), never an org-knowledge
/// job's hidden session; a viewer sees only a live (unarchived) one.
pub(crate) fn session_readable_sql(s: &str, user: &str) -> String {
    format!(
        "({s}.owner_id={user} OR ({s}.kind='chat' AND {s}.bound_chat_id IS NULL AND {s}.archived=false AND NOT EXISTS (SELECT 1 FROM neev_knowledge_jobs j WHERE j.org_id={s}.org_id AND j.session_id={s}.id) AND EXISTS (SELECT 1 FROM neev_grants g WHERE g.org_id={s}.org_id AND g.target_kind='session' AND g.target_id={s}.id AND {})))",
        names_reader("g", user)
    )
}

/// The reader's level on a session: `Owner`, `View`, or not found.
pub(crate) async fn session_level<C: ConnectionTrait>(
    db: &C,
    actor: &OrgContext,
    id: &str,
) -> Result<Level, ContentError> {
    access::canonical_id(id)?;
    let row = db::one(
        db,
        &format!(
            "SELECT s.owner_id FROM neev_agent_sessions s WHERE s.org_id=$1 AND s.id=$2 AND {}",
            session_readable_sql("s", "$3")
        ),
        vec![actor.org_id.into(), id.into(), actor.identity.user_id.into()],
    )
    .await?
    .ok_or(ContentError::NotFound)?;
    let owner: i32 = row.try_get("", "owner_id").map_err(database_error)?;
    Ok(if owner == actor.identity.user_id { Level::Owner } else { Level::View })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionAccess {
    pub session_id: String,
    pub revision: i64,
    pub level: Level,
    pub can_manage: bool,
    pub direct: Vec<Grant>,
}

async fn session_view(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    id: &str,
) -> Result<SessionAccess, ContentError> {
    let level = session_level(tx, actor, id).await?;
    let revision: i64 = db::one(
        tx,
        "SELECT revision FROM neev_agent_sessions WHERE org_id=$1 AND id=$2",
        vec![actor.org_id.into(), id.into()],
    )
    .await?
    .ok_or(ContentError::NotFound)?
    .try_get("", "revision")
    .map_err(database_error)?;
    Ok(SessionAccess {
        session_id: id.to_owned(),
        revision,
        level,
        can_manage: level == Level::Owner,
        direct: direct(tx, actor.org_id, "session", id).await?,
    })
}

pub(crate) async fn session_access(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
) -> Result<SessionAccess, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let result = session_view(&tx, actor, id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

/// The owner shares a standalone Agent Chat for VIEWING only (`level` is View
/// or a removal); anything that is not their own shareable chat is not found.
/// Bumps the session revision.
pub(crate) async fn set_session_grant(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    expected_revision: i64,
    principal: &Principal,
    level: Option<Level>,
) -> Result<SessionAccess, ContentError> {
    access::canonical_id(id)?;
    if expected_revision <= 0 || level.is_some_and(|level| level != Level::View) {
        return Err(ContentError::Invalid);
    }
    let now = now(state)?;
    let tx = resources::begin(state, actor).await?;
    let row = db::one(
        &tx,
        &format!(
            "SELECT s.revision,s.archived FROM neev_agent_sessions s WHERE s.org_id=$1 AND s.id=$2 AND s.owner_id=$3 AND s.kind='chat' AND s.bound_chat_id IS NULL AND NOT EXISTS (SELECT 1 FROM neev_knowledge_jobs j WHERE j.org_id=s.org_id AND j.session_id=s.id){}",
            session::lock(&tx)
        ),
        vec![actor.org_id.into(), id.into(), actor.identity.user_id.into()],
    )
    .await?
    .ok_or(ContentError::NotFound)?;
    let revision: i64 = row.try_get("", "revision").map_err(database_error)?;
    let archived: bool = row.try_get("", "archived").map_err(database_error)?;
    if revision != expected_revision {
        return Err(ContentError::Conflict);
    }
    if level.is_some() {
        if archived {
            return Err(ContentError::Archived);
        }
        eligible(&tx, actor, principal, actor.identity.user_id).await?;
    } else if let Principal::Team(team) = principal {
        access::canonical_id(team)?;
    }
    let next = revision.checked_add(1).ok_or(ContentError::Conflict)?;
    let bumped = db::execute(
        &tx,
        "UPDATE neev_agent_sessions SET revision=$1 WHERE org_id=$2 AND id=$3 AND revision=$4",
        vec![next.into(), actor.org_id.into(), id.into(), expected_revision.into()],
    )
    .await?;
    if bumped.rows_affected() != 1 {
        return Err(ContentError::Conflict);
    }
    write_grant(&tx, actor, "session", id, principal, level, now).await?;
    let result = session_view(&tx, actor, id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

/// Agent Chats other members shared with the reader, newest first: the ids and
/// their owners. The session rows themselves are read by the caller.
pub(crate) async fn shared_session_ids(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
) -> Result<Vec<String>, ContentError> {
    let rows = tx
        .query_all(db::statement(
            tx,
            &format!(
                "SELECT s.id FROM neev_agent_sessions s WHERE s.org_id=$1 AND s.owner_id<>$2 AND {} ORDER BY s.updated_at DESC,s.id LIMIT {MAX_SHARED_SESSIONS}",
                session_readable_sql("s", "$2")
            ),
            vec![actor.org_id.into(), actor.identity.user_id.into()],
        ))
        .await
        .map_err(database_error)?;
    rows.iter()
        .map(|row| row.try_get("", "id").map_err(database_error).map_err(ContentError::from))
        .collect()
}
