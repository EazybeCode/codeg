use super::types::*;
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
}

pub(super) fn canonical_id(id: &str) -> Result<(), ContentError> {
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
pub(super) fn visibility_name(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Private => "private",
        Visibility::Organization => "organization",
    }
}

async fn load(tx: &DatabaseTransaction, org_id: i32, id: &str) -> Result<Resource, ContentError> {
    canonical_id(id)?;
    let row=db::one(tx,"SELECT id,org_id,node_id,kind,creator_id,title,visibility,origin_chat_id,auth_revision,content_revision,archived,created_at,updated_at FROM neev_resources WHERE org_id=$1 AND id=$2",vec![org_id.into(),id.into()]).await?.ok_or(ContentError::NotFound)?;
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
        visibility: match row.visibility.as_str() {
            "private" => Visibility::Private,
            "organization" => Visibility::Organization,
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
    if audience.visibility == Visibility::Private && audience.creator_id != actor.identity.user_id {
        return Err(ContentError::NotFound);
    }
    resource.visibility = audience.visibility;
    if write && (resource.archived || source.as_ref().is_some_and(|chat| chat.archived)) {
        return Err(ContentError::Archived);
    }
    Ok(resource)
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
