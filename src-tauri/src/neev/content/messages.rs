use super::{access, resources, types::*};
use crate::neev::{
    auth::session, db, error::database_error, org, startup::ServerState, types::OrgContext,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, QueryResult};

fn message(row: &QueryResult) -> Result<Message, ContentError> {
    Ok(Message {
        id: row.try_get("", "id").map_err(database_error)?,
        sequence: row.try_get("", "sequence").map_err(database_error)?,
        author_id: row.try_get("", "author_id").map_err(database_error)?,
        author_name: row.try_get("", "author_name").map_err(database_error)?,
        body: row.try_get("", "body").map_err(database_error)?,
        created_at: row.try_get("", "created_at").map_err(database_error)?,
    })
}

async fn load(
    tx: &DatabaseTransaction,
    org_id: i32,
    chat_id: &str,
    id: &str,
) -> Result<Message, ContentError> {
    let row=db::one(tx,"SELECT m.id,m.sequence,m.author_id,a.display_name AS author_name,m.body,m.created_at FROM neev_messages m JOIN neev_accounts a ON a.id=m.author_id WHERE m.org_id=$1 AND m.chat_id=$2 AND m.id=$3",vec![org_id.into(),chat_id.into(),id.into()]).await?.ok_or(ContentError::NotFound)?;
    message(&row)
}

pub(crate) async fn append(
    state: &ServerState,
    actor: &OrgContext,
    chat_id: &str,
    request_id: &str,
    expected_auth_revision: i64,
    body: &str,
) -> Result<Message, ContentError> {
    if body.is_empty() || body.len() > 32768 || expected_auth_revision < 1 {
        return Err(ContentError::Invalid);
    }
    let request = org::normalize_request_id(request_id).map_err(|_| ContentError::Invalid)?;
    let hash = org::digest_parts(&[
        "content.message.append",
        &actor.org_id.to_string(),
        chat_id,
        &expected_auth_revision.to_string(),
        body,
    ]);
    let tx = resources::begin(state, actor).await?;
    access::lock_resources(&tx, actor, chat_id, false).await?;
    let chat = access::authorized(&tx, actor, chat_id, false).await?;
    if chat.kind != ResourceKind::Chat {
        return Err(ContentError::NotFound);
    }
    // A committed retry returns the existing message even after a rename/archive,
    // but only while the actor retains current read authority over this chat.
    if let Some(id) =
        resources::receipt(&tx, actor, "content.message.append", &request, &hash).await?
    {
        let result = load(&tx, actor.org_id, chat_id, &id).await?;
        session::recheck(&tx, actor, state.clock.now_unix()).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(result);
    }
    access::authorized(&tx, actor, chat_id, true).await?;
    if chat.auth_revision != expected_auth_revision {
        return Err(ContentError::Conflict);
    }
    let sequence = chat
        .content_revision
        .checked_add(1)
        .ok_or(ContentError::Conflict)?;
    let bytes = i64::try_from(body.len()).map_err(|_| ContentError::Invalid)?;
    resources::initialize_usage(&tx, actor.org_id).await?;
    let charged=db::execute(&tx,"UPDATE neev_content_usage SET charged_bytes=charged_bytes+$1,messages=messages+1 WHERE org_id=$2 AND messages<100000 AND charged_bytes<=1073741824-$1",vec![bytes.into(),actor.org_id.into()]).await?;
    if charged.rows_affected() != 1 {
        return Err(ContentError::Quota);
    }
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid);
    }
    let id = uuid::Uuid::new_v4().to_string();
    db::execute(&tx,"INSERT INTO neev_messages(org_id,chat_id,resource_kind,id,sequence,author_id,body,byte_length,created_at) VALUES($1,$2,'chat',$3,$4,$5,$6,$7,$8)",vec![actor.org_id.into(),chat_id.into(),id.clone().into(),sequence.into(),actor.identity.user_id.into(),body.into(),bytes.into(),now.into()]).await?;
    db::execute(
        &tx,
        "UPDATE neev_resources SET content_revision=$1,updated_at=$2 WHERE org_id=$3 AND id=$4",
        vec![
            sequence.into(),
            now.into(),
            actor.org_id.into(),
            chat_id.into(),
        ],
    )
    .await?;
    resources::record_receipt(&tx, actor, "content.message.append", &request, &hash, &id).await?;
    let result = load(&tx, actor.org_id, chat_id, &id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

pub(crate) async fn list(
    state: &ServerState,
    actor: &OrgContext,
    chat_id: &str,
    after: Option<i64>,
) -> Result<Page<Message>, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let chat = access::authorized(&tx, actor, chat_id, false).await?;
    if chat.kind != ResourceKind::Chat {
        return Err(ContentError::NotFound);
    }
    if let Some(cursor) = after {
        if cursor < 1
            || db::one(
                &tx,
                "SELECT id FROM neev_messages WHERE org_id=$1 AND chat_id=$2 AND sequence=$3",
                vec![actor.org_id.into(), chat_id.into(), cursor.into()],
            )
            .await?
            .is_none()
        {
            return Err(ContentError::NotFound);
        }
    }
    let rows=tx.query_all(db::statement(&tx,"SELECT m.id,m.sequence,m.author_id,a.display_name AS author_name,m.body,m.created_at FROM neev_messages m JOIN neev_accounts a ON a.id=m.author_id WHERE m.org_id=$1 AND m.chat_id=$2 AND m.sequence>$3 ORDER BY m.sequence LIMIT 101",vec![actor.org_id.into(),chat_id.into(),after.unwrap_or(0).into()])).await.map_err(database_error)?;
    let has_more = rows.len() > 100;
    let items = rows
        .iter()
        .take(100)
        .map(message)
        .collect::<Result<Vec<_>, _>>()?;
    let next_after = if has_more {
        items.last().map(|message| message.sequence.to_string())
    } else {
        None
    };
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(Page { items, next_after })
}
