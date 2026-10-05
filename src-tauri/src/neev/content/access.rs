use super::{
    grants::{self, Level},
    types::*,
};
use crate::neev::{auth::session, db, error::database_error, types::OrgContext};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult};

#[derive(FromQueryResult)]
struct ResourceRow {
    id: String,
    org_id: i32,
    node_id: i32,
    kind: String,
    creator_id: i32,
    title: String,
    visibility: String,
    origin_chat_id: Option<String>,
    auth_revision: i64,
    content_revision: i64,
    archived: bool,
    created_at: i64,
    updated_at: i64,
    shared: i64,
}

pub(crate) fn canonical_id(id: &str) -> Result<(), ContentError> {
    if uuid::Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id) {
        Ok(())
    } else {
        Err(ContentError::NotFound)
    }
}

pub(super) fn kind_name(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Chat => "chat",
        ResourceKind::Document => "document",
    }
}
/// The STORED `visibility` column value. `Shared` is stored as `private` plus
/// a shared mark (`grants::store_visibility`), so anything that ignores the
/// mark — an older binary, a downgrade — sees it as private: never wider.
pub(super) fn visibility_name(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Private | Visibility::Shared => "private",
        Visibility::Organization => "organization",
    }
}

/// The audience as the API names it.
pub(crate) fn visibility_label(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Private => "private",
        Visibility::Shared => "shared",
        Visibility::Organization => "organization",
    }
}

pub(crate) async fn load<C: ConnectionTrait>(
    tx: &C,
    org_id: i32,
    id: &str,
) -> Result<Resource, ContentError> {
    canonical_id(id)?;
    let row=db::one(tx,"SELECT r.id,r.org_id,r.node_id,r.kind,r.creator_id,r.title,r.visibility,r.origin_chat_id,r.auth_revision,r.content_revision,r.archived,r.created_at,r.updated_at,(SELECT COUNT(*) FROM neev_shared_marks k WHERE k.org_id=r.org_id AND k.mark_kind='resource' AND k.target_id=r.id) AS shared FROM neev_resources r WHERE r.org_id=$1 AND r.id=$2",vec![org_id.into(),id.into()]).await?.ok_or(ContentError::NotFound)?;
    let row = ResourceRow::from_query_result(&row, "").map_err(database_error)?;
    Ok(Resource {
        id: row.id,
        org_id: row.org_id,
        node_id: row.node_id,
        kind: match row.kind.as_str() {
            "chat" => ResourceKind::Chat,
            "document" => ResourceKind::Document,
            _ => return Err(ContentError::Storage),
        },
        creator_id: row.creator_id,
        title: row.title,
        visibility: match (row.visibility.as_str(), row.shared) {
            ("private", 0) => Visibility::Private,
            ("private", _) => Visibility::Shared,
            ("organization", _) => Visibility::Organization,
            _ => return Err(ContentError::Storage),
        },
        origin_chat_id: row.origin_chat_id,
        auth_revision: row.auth_revision,
        content_revision: row.content_revision,
        archived: row.archived,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// Caller holds the admission guard and session::recheck locks through commit.
/// Bound documents have the source's audience, never a second creator/private gate.
pub(crate) async fn authorized(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    id: &str,
    write: bool,
) -> Result<Resource, ContentError> {
    Ok(resolve(tx, actor, id, write).await?.0)
}

/// `authorized`, with the actor's level. Reading needs any level; writing (the
/// body, messages, files, agent invocation) needs Edit or Owner, so a View
/// grantee is refused exactly as a non-reader is.
pub(crate) async fn resolve(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    id: &str,
    write: bool,
) -> Result<(Resource, Level), ContentError> {
    let mut resource = load(tx, actor.org_id, id).await?;
    let source = if let Some(source) = &resource.origin_chat_id {
        let chat = load(tx, actor.org_id, source).await?;
        if resource.kind != ResourceKind::Document
            || chat.kind != ResourceKind::Chat
            || chat.origin_chat_id.is_some()
        {
            return Err(ContentError::NotFound);
        }
        Some(chat)
    } else {
        None
    };
    let audience = source.as_ref().unwrap_or(&resource);
    let level = grants::level(tx, actor.org_id, actor.identity.user_id, audience)
        .await?
        .ok_or(ContentError::NotFound)?;
    if write && level < Level::Edit {
        return Err(ContentError::NotFound);
    }
    resource.visibility = audience.visibility;
    if write && (resource.archived || source.as_ref().is_some_and(|chat| chat.archived)) {
        return Err(ContentError::Archived);
    }
    Ok((resource, level))
}

/// Discover the complete lock set before locking anything; PostgreSQL acquires
/// rows after ORDER BY. Admission/session locks keep the graph stable meanwhile.
pub(crate) async fn lock_resources(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    id: &str,
    include_attached: bool,
) -> Result<Vec<String>, ContentError> {
    canonical_id(id)?;
    let rows=tx.query_all(db::statement(tx,&format!("SELECT id FROM neev_resources WHERE org_id=$1 AND (id=$2 OR id=(SELECT origin_chat_id FROM neev_resources WHERE org_id=$1 AND id=$2) OR ($3 AND origin_chat_id=$2)) ORDER BY id{}",session::lock(tx)),vec![actor.org_id.into(),id.into(),include_attached.into()])).await.map_err(database_error)?;
    rows.iter()
        .map(|row| {
            row.try_get("", "id")
                .map_err(database_error)
                .map_err(ContentError::from)
        })
        .collect()
}
