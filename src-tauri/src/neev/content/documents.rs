use super::{access, resources, types::*};
use crate::neev::{
    auth::session, db, error::database_error, org, startup::ServerState, types::OrgContext,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, QueryResult};

const ACTION: &str = "content.document.save";
const REVISION_COLUMNS: &str = "r.revision,r.blob_key,r.sha256,r.byte_length,r.author_id,a.display_name AS author_name,r.created_at,r.parent_revision,r.restored_from";

fn revision(row: &QueryResult) -> Result<Revision, ContentError> {
    Ok(Revision {
        revision: row.try_get("", "revision").map_err(database_error)?,
        author_id: row.try_get("", "author_id").map_err(database_error)?,
        author_name: row.try_get("", "author_name").map_err(database_error)?,
        byte_length: row.try_get("", "byte_length").map_err(database_error)?,
        sha256: row.try_get("", "sha256").map_err(database_error)?,
        created_at: row.try_get("", "created_at").map_err(database_error)?,
        parent_revision: row.try_get("", "parent_revision").map_err(database_error)?,
        restored_from: row.try_get("", "restored_from").map_err(database_error)?,
    })
}

async fn load(
    tx: &DatabaseTransaction,
    org_id: i32,
    id: &str,
    number: i64,
) -> Result<(Revision, String), ContentError> {
    let row = db::one(tx, &format!("SELECT {REVISION_COLUMNS} FROM neev_document_revisions r JOIN neev_accounts a ON a.id=r.author_id WHERE r.org_id=$1 AND r.document_id=$2 AND r.revision=$3"), vec![org_id.into(),id.into(),number.into()]).await?.ok_or(ContentError::NotFound)?;
    Ok((
        revision(&row)?,
        row.try_get("", "blob_key").map_err(database_error)?,
    ))
}

async fn authorized(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    id: &str,
    write: bool,
) -> Result<Resource, ContentError> {
    let resource = access::authorized(tx, actor, id, write).await?;
    if resource.kind != ResourceKind::Document {
        return Err(ContentError::NotFound);
    }
    Ok(resource)
}

pub(crate) async fn save(
    state: &ServerState,
    actor: &OrgContext,
    document_id: &str,
    input: SaveInput,
) -> Result<Revision, ContentError> {
    save_revision(state, actor, document_id, input, None).await
}

async fn save_revision(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    input: SaveInput,
    restored_from: Option<i64>,
) -> Result<Revision, ContentError> {
    if input.markdown.len() > 1048576
        || input.expected_content_revision < 0
        || input.expected_auth_revision < 1
    {
        return Err(ContentError::Invalid);
    }
    access::canonical_id(id)?;
    let request =
        org::normalize_request_id(&input.request_id).map_err(|_| ContentError::Invalid)?;
    let hash = org::digest_parts(&[
        ACTION,
        &actor.org_id.to_string(),
        id,
        &input.expected_content_revision.to_string(),
        &input.expected_auth_revision.to_string(),
        &input.markdown,
        &restored_from.map(|n| n.to_string()).unwrap_or_default(),
    ]);
    let bytes = input.markdown.len() as i64;
    let lease_id = uuid::Uuid::new_v4().to_string();
    let key = uuid::Uuid::new_v4().to_string();

    // Reserve under admission -> session authority -> lexically ordered source/document locks.
    // The reservation is durable before any filesystem operation, including failed writes.
    let tx = resources::begin(state, actor).await?;
    access::lock_resources(&tx, actor, id, false).await?;
    let current = authorized(&tx, actor, id, false).await?;
    if let Some(number) = resources::receipt(&tx, actor, ACTION, &request, &hash).await? {
        let number = number.parse::<i64>().map_err(|_| ContentError::Storage)?;
        let (result, _) = load(&tx, actor.org_id, id, number).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(result);
    }
    authorized(&tx, actor, id, true).await?;
    if current.auth_revision != input.expected_auth_revision
        || current.content_revision != input.expected_content_revision
    {
        return Err(ContentError::Conflict);
    }
    if current.content_revision >= 1000 {
        return Err(ContentError::Quota);
    }
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid);
    }
    let expires = now.checked_add(60).ok_or(ContentError::Invalid)?;
    if let Some(lease) = db::one(
        &tx,
        "SELECT id,expires_at FROM neev_content_leases WHERE org_id=$1 AND resource_id=$2",
        vec![actor.org_id.into(), id.into()],
    )
    .await?
    {
        if lease
            .try_get::<i64>("", "expires_at")
            .map_err(database_error)?
            > now
        {
            return Err(ContentError::Conflict);
        }
        let expired_id: String = lease.try_get("", "id").map_err(database_error)?;
        db::execute(&tx, "DELETE FROM neev_content_leases WHERE org_id=$1 AND resource_id=$2 AND id=$3 AND expires_at<=$4", vec![actor.org_id.into(),id.into(),expired_id.into(),now.into()]).await?;
    }
    resources::initialize_usage(&tx, actor.org_id).await?;
    let charge = db::execute(&tx, "UPDATE neev_content_usage SET charged_bytes=charged_bytes+$1 WHERE org_id=$2 AND charged_bytes<=1073741824-$1", vec![bytes.into(),actor.org_id.into()]).await?;
    if charge.rows_affected() != 1 {
        return Err(ContentError::Quota);
    }
    db::execute(&tx, "INSERT INTO neev_content_leases(org_id,resource_id,id,actor_id,request_id,body_hash,blob_key,reserved_bytes,expected_content_revision,expected_auth_revision,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)", vec![actor.org_id.into(),id.into(),lease_id.clone().into(),actor.identity.user_id.into(),request.clone().into(),hash.clone().into(),key.clone().into(),bytes.into(),input.expected_content_revision.into(),input.expected_auth_revision.into(),expires.into()]).await?;
    tx.commit().await.map_err(database_error)?;

    #[cfg(test)]
    super::document_tests::boundary("reserved").await;
    let blob = state
        .blobs
        .put(actor.org_id, &key, input.markdown.as_bytes())
        .await?;
    #[cfg(test)]
    super::document_tests::boundary("written").await;

    // Reacquire all authority after I/O. Read the clock after potentially blocking
    // resource and session locks; request-start/reservation time cannot authorize publish.
    let tx = resources::begin(state, actor).await?;
    access::lock_resources(&tx, actor, id, false).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    let current = authorized(&tx, actor, id, true).await?;
    if current.auth_revision != input.expected_auth_revision
        || current.content_revision != input.expected_content_revision
    {
        return Err(ContentError::Conflict);
    }
    if current.content_revision >= 1000 {
        return Err(ContentError::Quota);
    }
    let now = state.clock.now_unix();
    let live = db::one(&tx, "SELECT id FROM neev_content_leases WHERE org_id=$1 AND resource_id=$2 AND id=$3 AND actor_id=$4 AND request_id=$5 AND body_hash=$6 AND blob_key=$7 AND reserved_bytes=$8 AND expected_content_revision=$9 AND expected_auth_revision=$10 AND expires_at>$11", vec![actor.org_id.into(),id.into(),lease_id.clone().into(),actor.identity.user_id.into(),request.clone().into(),hash.clone().into(),key.into(),bytes.into(),input.expected_content_revision.into(),input.expected_auth_revision.into(),now.into()]).await?;
    if live.is_none() {
        return Err(ContentError::Conflict);
    }
    let number = current.content_revision + 1;
    let parent = (current.content_revision > 0).then_some(current.content_revision);
    db::execute(&tx, "INSERT INTO neev_document_revisions(org_id,document_id,resource_kind,revision,blob_key,sha256,byte_length,author_id,created_at,parent_revision,restored_from) VALUES($1,$2,'document',$3,$4,$5,$6,$7,$8,$9,$10)", vec![actor.org_id.into(),id.into(),number.into(),blob.key.into(),blob.sha256.into(),blob.byte_length.into(),actor.identity.user_id.into(),now.into(),parent.into(),restored_from.into()]).await?;
    db::execute(
        &tx,
        "UPDATE neev_resources SET content_revision=$1,updated_at=$2 WHERE org_id=$3 AND id=$4",
        vec![number.into(), now.into(), actor.org_id.into(), id.into()],
    )
    .await?;
    resources::record_receipt(&tx, actor, ACTION, &request, &hash, &number.to_string()).await?;
    let (result, _) = load(&tx, actor.org_id, id, number).await?;
    // Publication statements can themselves wait on database locks. Expiry is
    // checked again at the final write, rolling back revision/head/receipt if late.
    let removed = db::execute(
        &tx,
        "DELETE FROM neev_content_leases WHERE org_id=$1 AND resource_id=$2 AND id=$3 AND expires_at>$4",
        vec![actor.org_id.into(), id.into(), lease_id.into(), state.clock.now_unix().into()],
    )
    .await?;
    // The DELETE itself may have waited; the original expiry is immutable for
    // this lease ID. Roll back even if its SQL predicate used a pre-wait clock.
    if removed.rows_affected() != 1 || state.clock.now_unix() >= expires {
        return Err(ContentError::Conflict);
    }
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

pub(crate) async fn history(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    after: Option<i64>,
) -> Result<Page<Revision>, ContentError> {
    let tx = resources::begin(state, actor).await?;
    access::lock_resources(&tx, actor, id, false).await?;
    authorized(&tx, actor, id, false).await?;
    if let Some(cursor) = after {
        if cursor < 1 {
            return Err(ContentError::NotFound);
        }
        load(&tx, actor.org_id, id, cursor).await?;
    }
    let rows = tx.query_all(db::statement(&tx, &format!("SELECT {REVISION_COLUMNS} FROM neev_document_revisions r JOIN neev_accounts a ON a.id=r.author_id WHERE r.org_id=$1 AND r.document_id=$2 AND r.revision>$3 ORDER BY r.revision LIMIT 101"), vec![actor.org_id.into(),id.into(),after.unwrap_or(0).into()])).await.map_err(database_error)?;
    let items = rows
        .iter()
        .take(100)
        .map(revision)
        .collect::<Result<Vec<_>, _>>()?;
    let next_after = if rows.len() > 100 {
        items.last().map(|r| r.revision.to_string())
    } else {
        None
    };
    tx.commit().await.map_err(database_error)?;
    Ok(Page { items, next_after })
}

pub(crate) async fn read(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    number: Option<i64>,
) -> Result<(Revision, Vec<u8>), ContentError> {
    let tx = resources::begin(state, actor).await?;
    access::lock_resources(&tx, actor, id, false).await?;
    let current = authorized(&tx, actor, id, false).await?;
    let (revision, key) = load(
        &tx,
        actor.org_id,
        id,
        number.unwrap_or(current.content_revision),
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    #[cfg(test)]
    super::document_tests::boundary("before_read").await;
    let bytes = state
        .blobs
        .read(actor.org_id, &key, revision.byte_length, &revision.sha256)
        .await?;
    #[cfg(test)]
    super::document_tests::boundary("read").await;
    let tx = resources::begin(state, actor).await?;
    access::lock_resources(&tx, actor, id, false).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    let rechecked = authorized(&tx, actor, id, false).await?;
    if rechecked.auth_revision != current.auth_revision {
        return Err(ContentError::Conflict);
    }
    tx.commit().await.map_err(database_error)?;
    Ok((revision, bytes))
}

pub(crate) async fn restore(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    source_revision: i64,
    request_id: &str,
    expected_content_revision: i64,
    expected_auth_revision: i64,
) -> Result<Revision, ContentError> {
    let (_, bytes) = read(state, actor, id, Some(source_revision)).await?;
    let markdown = String::from_utf8(bytes).map_err(|_| ContentError::Storage)?;
    save_revision(
        state,
        actor,
        id,
        SaveInput {
            request_id: request_id.to_owned(),
            expected_content_revision,
            expected_auth_revision,
            markdown,
        },
        Some(source_revision),
    )
    .await
}
