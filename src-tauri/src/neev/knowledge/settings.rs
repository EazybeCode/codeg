//! The per-org capture setting (admins only, optimistic revision).

use super::types::*;
use crate::neev::{
    auth::session, content::resources, db, startup::ServerState, types::OrgContext, types::Role,
};
use sea_orm::ConnectionTrait;

/// An absent row means capture off, revision 0.
pub(crate) async fn get<C: ConnectionTrait>(db: &C, org_id: i32) -> Result<Settings, KnowledgeError> {
    let row = db::one(
        db,
        "SELECT capture_enabled,revision,capture_blocked FROM neev_knowledge_settings WHERE org_id=$1",
        vec![org_id.into()],
    )
    .await?;
    Ok(match row {
        None => Settings {
            capture_enabled: false,
            revision: 0,
            capture_blocked: None,
        },
        Some(row) => Settings {
            capture_enabled: row.try_get("", "capture_enabled").map_err(db_error)?,
            revision: row.try_get("", "revision").map_err(db_error)?,
            capture_blocked: row.try_get("", "capture_blocked").map_err(db_error)?,
        },
    })
}

/// Read for an admin (session re-checked).
pub(crate) async fn read(state: &ServerState, actor: &OrgContext) -> Result<Settings, KnowledgeError> {
    if actor.role != Role::Admin {
        return Err(KnowledgeError::forbidden());
    }
    let tx = resources::begin(state, actor).await?;
    let current = get(&tx, actor.org_id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(db_error)?;
    Ok(current)
}

/// Change the capture setting. Admins only; a stale `expected_revision` is a
/// conflict and changes nothing.
pub(crate) async fn set(
    state: &ServerState,
    actor: &OrgContext,
    capture_enabled: bool,
    expected_revision: i64,
) -> Result<Settings, KnowledgeError> {
    if actor.role != Role::Admin {
        return Err(KnowledgeError::forbidden());
    }
    if expected_revision < 0 {
        return Err(KnowledgeError::invalid());
    }
    let now = state.clock.now_unix();
    let tx = resources::begin(state, actor).await?;
    let current = get(&tx, actor.org_id).await?;
    if current.revision != expected_revision {
        return Err(KnowledgeError::conflict());
    }
    let next = current.revision.checked_add(1).ok_or_else(KnowledgeError::conflict)?;
    if current.revision == 0 {
        db::execute(
            &tx,
            "INSERT INTO neev_knowledge_settings(org_id,capture_enabled,capture_blocked,revision,updated_by,updated_at) VALUES($1,$2,NULL,$3,$4,$5)",
            vec![
                actor.org_id.into(),
                capture_enabled.into(),
                next.into(),
                actor.identity.user_id.into(),
                now.into(),
            ],
        )
        .await?;
    } else {
        let changed = db::execute(
            &tx,
            "UPDATE neev_knowledge_settings SET capture_enabled=$1,revision=$2,updated_by=$3,updated_at=$4 WHERE org_id=$5 AND revision=$6",
            vec![
                capture_enabled.into(),
                next.into(),
                actor.identity.user_id.into(),
                now.into(),
                actor.org_id.into(),
                current.revision.into(),
            ],
        )
        .await?;
        if changed.rows_affected() != 1 {
            return Err(KnowledgeError::conflict());
        }
    }
    super::log::append(
        &tx,
        actor.org_id,
        Some(actor.identity.user_id),
        if capture_enabled {
            "capture_enabled"
        } else {
            "capture_disabled"
        },
        None,
        None,
        "",
        now,
    )
    .await?;
    session::recheck(&tx, actor, now).await?;
    tx.commit().await.map_err(db_error)?;
    Ok(Settings {
        capture_enabled,
        revision: next,
        capture_blocked: current.capture_blocked,
    })
}
