use super::{access, types::*};
use crate::neev::{
    auth::session, db, error::database_error, org, startup::ServerState, types::OrgContext,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sea_orm::{ConnectionTrait, DatabaseTransaction};

pub(crate) async fn begin(
    state: &ServerState,
    actor: &OrgContext,
) -> Result<DatabaseTransaction, ContentError> {
    let tx = db::begin(&state.database).await?;
    db::lock_admission_guard(&tx).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    Ok(tx)
}

fn title(value: &str) -> Result<String, ContentError> {
    if value.chars().any(char::is_control) {
        return Err(ContentError::Invalid);
    }
    let value = value.trim();
    if !(1..=100).contains(&value.chars().count()) {
        return Err(ContentError::Invalid);
    }
    Ok(value.to_owned())
}

async fn node(tx: &DatabaseTransaction, org_id: i32, node_id: i32) -> Result<(), ContentError> {
    let row = db::one(
        tx,
        "SELECT kind FROM neev_nodes WHERE org_id=$1 AND id=$2",
        vec![org_id.into(), node_id.into()],
    )
    .await?
    .ok_or(ContentError::NotFound)?;
    let kind: String = row.try_get("", "kind").map_err(database_error)?;
    if !matches!(kind.as_str(), "project" | "folder") {
        return Err(ContentError::NotFound);
    }
    Ok(())
}

pub(crate) async fn receipt(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    action: &str,
    request: &str,
    hash: &str,
) -> Result<Option<String>, ContentError> {
    let row=db::one(tx,"SELECT body_hash,resource_id FROM neev_requests WHERE user_id=$1 AND action=$2 AND request_id=$3",vec![actor.identity.user_id.into(),action.into(),request.into()]).await?;
    row.map(|row| {
        if row
            .try_get::<String>("", "body_hash")
            .map_err(database_error)?
            != hash
        {
            return Err(ContentError::Conflict);
        }
        Ok(row.try_get("", "resource_id").map_err(database_error)?)
    })
    .transpose()
}

pub(crate) async fn record_receipt(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    action: &str,
    request: &str,
    hash: &str,
    id: &str,
) -> Result<(), ContentError> {
    // Content receipts remain durable for the lifetime of the immutable history.
    db::execute(tx,"INSERT INTO neev_requests(user_id,action,request_id,body_hash,resource_id,expires_at) VALUES($1,$2,$3,$4,$5,$6)",vec![actor.identity.user_id.into(),action.into(),request.into(),hash.into(),id.into(),i64::MAX.into()]).await?;
    Ok(())
}

pub(super) async fn initialize_usage(
    tx: &DatabaseTransaction,
    org_id: i32,
) -> Result<(), ContentError> {
    db::execute(tx,"INSERT INTO neev_content_usage(org_id,charged_bytes,chats,documents,messages) VALUES($1,0,0,0,0) ON CONFLICT(org_id) DO NOTHING",vec![org_id.into()]).await?;
    Ok(())
}

/// Create a resource on the native engine — the behaviour every existing caller
/// already has.
pub(crate) async fn create(
    state: &ServerState,
    actor: &OrgContext,
    input: ResourceInput,
) -> Result<Resource, ContentError> {
    create_on(state, actor, input, false).await
}

/// Create a document backed by the La Suite engine.
///
/// The binding is written inside the **same** transaction as the resource row,
/// so the two commit together or not at all. Any gap between them would leave a
/// resource that presents as a legacy document with no native content — a blank
/// editor indistinguishable from data loss.
///
/// Idempotency is the existing receipt, not new machinery: a repeated
/// `request_id` returns the original resource, and a repeated `request_id` with
/// a different body is a conflict. A retry therefore cannot produce a second
/// resource, and because the Docs document is provisioned with this resource's
/// own id, it cannot produce a second Docs document either.
pub(crate) async fn create_engine_document(
    state: &ServerState,
    actor: &OrgContext,
    input: ResourceInput,
) -> Result<Resource, ContentError> {
    if input.kind != ResourceKind::Document {
        return Err(ContentError::Invalid);
    }
    create_on(state, actor, input, true).await
}

async fn create_on(
    state: &ServerState,
    actor: &OrgContext,
    input: ResourceInput,
    engine: bool,
) -> Result<Resource, ContentError> {
    let title = title(&input.title)?;
    let request =
        org::normalize_request_id(&input.request_id).map_err(|_| ContentError::Invalid)?;
    if input.kind == ResourceKind::Chat && input.origin_chat_id.is_some() {
        return Err(ContentError::Invalid);
    }
    let hash = org::digest_parts(&[
        "content.resource.create",
        &actor.org_id.to_string(),
        &input.node_id.to_string(),
        access::kind_name(input.kind),
        &title,
        input.origin_chat_id.as_deref().unwrap_or(""),
        // The engine is part of the request identity. Without it, retrying a
        // native create with a request id already used for an engine create
        // would silently return the other kind of document rather than the
        // conflict it is.
        if engine { "engine.lasuite" } else { "native" },
    ]);
    let tx = begin(state, actor).await?;
    node(&tx, actor.org_id, input.node_id).await?;
    // A retry still needs current access, but does not consume quota a second time.
    if let Some(id) = receipt(&tx, actor, "content.resource.create", &request, &hash).await? {
        let result = access::authorized(&tx, actor, &id, false).await?;
        session::recheck(&tx, actor, state.clock.now_unix()).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(result);
    }
    let visibility = if let Some(source) = &input.origin_chat_id {
        access::lock_resources(&tx, actor, source, false).await?;
        let source = access::authorized(&tx, actor, source, true).await?;
        if source.kind != ResourceKind::Chat {
            return Err(ContentError::NotFound);
        }
        source.visibility
    } else {
        Visibility::Private
    };
    initialize_usage(&tx, actor.org_id).await?;
    let counter = match input.kind {
        ResourceKind::Chat => "chats",
        ResourceKind::Document => "documents",
    };
    let charge=db::execute(&tx,&format!("UPDATE neev_content_usage SET {counter}={counter}+1 WHERE org_id=$1 AND {counter}<10000"),vec![actor.org_id.into()]).await?;
    if charge.rows_affected() != 1 {
        return Err(ContentError::Quota);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid);
    }
    db::execute(&tx,"INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,1,0,false,$10,$10)",vec![actor.org_id.into(),id.clone().into(),access::kind_name(input.kind).into(),input.node_id.into(),actor.identity.user_id.into(),title.into(),access::visibility_name(visibility).into(),input.origin_chat_id.clone().into(),input.origin_chat_id.as_ref().map(|_|"chat".to_string()).into(),now.into()]).await?;
    record_receipt(&tx, actor, "content.resource.create", &request, &hash, &id).await?;
    if engine {
        crate::neev::engine::binding::bind(&tx, actor.org_id, actor.identity.user_id, &id).await?;
    }
    let result = access::authorized(&tx, actor, &id, false).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

pub(crate) async fn get(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
) -> Result<Resource, ContentError> {
    let tx = begin(state, actor).await?;
    let result = access::authorized(&tx, actor, id, false).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

pub(crate) async fn list(
    state: &ServerState,
    actor: &OrgContext,
    node_id: i32,
    kind: ResourceKind,
    archived: bool,
    after: Option<&str>,
) -> Result<Page<Resource>, ContentError> {
    let tx = begin(state, actor).await?;
    node(&tx, actor.org_id, node_id).await?;
    let mut last = String::new();
    if let Some(cursor) = after {
        if cursor.len() > 512 {
            return Err(ContentError::NotFound);
        }
        let decoded = URL_SAFE_NO_PAD
            .decode(cursor)
            .map_err(|_| ContentError::NotFound)?;
        let (org, node, cursor_kind, cursor_archived, id): (i32, i32, String, bool, String) =
            serde_json::from_slice(&decoded).map_err(|_| ContentError::NotFound)?;
        if (org, node, cursor_kind.as_str(), cursor_archived)
            != (actor.org_id, node_id, access::kind_name(kind), archived)
        {
            return Err(ContentError::NotFound);
        }
        let current = access::authorized(&tx, actor, &id, false).await?;
        if (current.node_id, current.kind, current.archived) != (node_id, kind, archived) {
            return Err(ContentError::NotFound);
        }
        last = id;
    }
    // Apply effective audience before LIMIT; hidden rows cannot change page sizes.
    let rows=tx.query_all(db::statement(&tx,"SELECT r.id FROM neev_resources r LEFT JOIN neev_resources s ON s.org_id=r.org_id AND s.id=r.origin_chat_id AND s.kind='chat' WHERE r.org_id=$1 AND r.node_id=$2 AND r.kind=$3 AND r.archived=$4 AND r.id>$5 AND ((r.origin_chat_id IS NULL AND (r.visibility='organization' OR r.creator_id=$6)) OR (r.origin_chat_id IS NOT NULL AND (s.visibility='organization' OR s.creator_id=$6))) ORDER BY r.id LIMIT 101",vec![actor.org_id.into(),node_id.into(),access::kind_name(kind).into(),archived.into(),last.into(),actor.identity.user_id.into()])).await.map_err(database_error)?;
    let has_more = rows.len() > 100;
    let mut items = Vec::new();
    for row in rows.iter().take(100) {
        let id: String = row.try_get("", "id").map_err(database_error)?;
        items.push(access::authorized(&tx, actor, &id, false).await?);
    }
    let next_after = if has_more {
        let last = items.last().ok_or(ContentError::Storage)?;
        Some(
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&(
                    actor.org_id,
                    node_id,
                    access::kind_name(kind),
                    archived,
                    &last.id,
                ))
                .map_err(|_| ContentError::Storage)?,
            ),
        )
    } else {
        None
    };
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(Page { items, next_after })
}

pub(crate) async fn update(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    patch: ResourcePatch,
) -> Result<Resource, ContentError> {
    let title = title(&patch.title)?;
    if patch.expected_auth_revision < 1 {
        return Err(ContentError::Invalid);
    }
    let tx = begin(state, actor).await?;
    let locked = access::lock_resources(&tx, actor, id, true).await?;
    // Metadata must permit creator unarchive; body writes use authorized(...,true).
    let current = access::authorized(&tx, actor, id, false).await?;
    if current.auth_revision != patch.expected_auth_revision {
        return Err(ContentError::Conflict);
    }
    if current.origin_chat_id.is_some() && patch.visibility != current.visibility {
        return Err(ContentError::Invalid);
    }
    if (patch.visibility != current.visibility
        || patch.archived != current.archived
        || patch.node_id != current.node_id)
        && current.creator_id != actor.identity.user_id
    {
        return Err(ContentError::NotFound);
    }
    node(&tx, actor.org_id, patch.node_id).await?;
    let now = state.clock.now_unix();
    for locked_id in &locked {
        if db::one(&tx,"SELECT id FROM neev_content_leases WHERE org_id=$1 AND resource_id=$2 AND expires_at>$3",vec![actor.org_id.into(),locked_id.clone().into(),now.into()]).await?.is_some() {return Err(ContentError::Conflict);}
    }
    let revision = current
        .auth_revision
        .checked_add(1)
        .ok_or(ContentError::Conflict)?;
    db::execute(&tx,"UPDATE neev_resources SET title=$1,node_id=$2,visibility=$3,archived=$4,auth_revision=$5,updated_at=$6 WHERE org_id=$7 AND id=$8",vec![title.into(),patch.node_id.into(),access::visibility_name(patch.visibility).into(),patch.archived.into(),revision.into(),now.into(),actor.org_id.into(),id.into()]).await?;
    if current.kind == ResourceKind::Chat {
        for attached in &locked {
            if attached == id {
                continue;
            }
            let row = db::one(
                &tx,
                "SELECT auth_revision FROM neev_resources WHERE org_id=$1 AND id=$2",
                vec![actor.org_id.into(), attached.clone().into()],
            )
            .await?
            .ok_or(ContentError::Storage)?;
            let revision = row
                .try_get::<i64>("", "auth_revision")
                .map_err(database_error)?
                .checked_add(1)
                .ok_or(ContentError::Conflict)?;
            db::execute(&tx,"UPDATE neev_resources SET auth_revision=$1,updated_at=$2 WHERE org_id=$3 AND id=$4",vec![revision.into(),now.into(),actor.org_id.into(),attached.clone().into()]).await?;
        }
    }
    let result = access::authorized(&tx, actor, id, false).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}
