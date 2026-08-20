//! PK arena round CRUD commands. The `*_core` fns are mode-agnostic and
//! shared by the Tauri wrappers and the Axum handlers.

use crate::db::error::DbError;
use crate::db::service::pk_round_service;
use crate::db::AppDatabase;
use crate::db::entities::pk_round::PkRoundStatus;
use crate::models::{PkRoundConfig, PkRoundInfo};

// -- shared business logic (both modes) --

pub async fn pk_round_list_core(
    db: &AppDatabase,
    folder_id: Option<i32>,
) -> Result<Vec<PkRoundInfo>, DbError> {
    pk_round_service::list(&db.conn, folder_id).await
}

pub async fn pk_round_get_core(db: &AppDatabase, id: i32) -> Result<PkRoundInfo, DbError> {
    pk_round_service::get_info(&db.conn, id).await
}

pub async fn pk_round_create_core(
    db: &AppDatabase,
    folder_id: i32,
    task: String,
    config: PkRoundConfig,
) -> Result<PkRoundInfo, DbError> {
    let row = pk_round_service::create(&db.conn, folder_id, task, config).await?;
    pk_round_service::get_info(&db.conn, row.id).await
}

pub async fn pk_round_update_status_core(
    db: &AppDatabase,
    id: i32,
    status: String,
) -> Result<(), DbError> {
    let parsed = match status.as_str() {
        "ready" => PkRoundStatus::Ready,
        "running" => PkRoundStatus::Running,
        "finished" => PkRoundStatus::Finished,
        "canceled" => PkRoundStatus::Canceled,
        "interrupted" => PkRoundStatus::Interrupted,
        other => {
            return Err(DbError::Validation(format!("unknown pk_round status: {other}")));
        }
    };
    pk_round_service::update_status(&db.conn, id, parsed).await
}

pub async fn pk_round_delete_core(db: &AppDatabase, id: i32) -> Result<(), DbError> {
    pk_round_service::soft_delete(&db.conn, id).await
}

pub async fn pk_round_update_judge_core(
    db: &AppDatabase,
    id: i32,
    judge_result: Option<String>,
    judge_status: String,
) -> Result<(), DbError> {
    pk_round_service::update_judge(&db.conn, id, judge_result, judge_status).await
}

// -- Tauri command wrappers (desktop mode only) --

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn pk_round_list(
    db: tauri::State<'_, AppDatabase>,
    folder_id: Option<i32>,
) -> Result<Vec<PkRoundInfo>, DbError> {
    pk_round_list_core(&db, folder_id).await
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn pk_round_get(
    db: tauri::State<'_, AppDatabase>,
    id: i32,
) -> Result<PkRoundInfo, DbError> {
    pk_round_get_core(&db, id).await
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn pk_round_create(
    db: tauri::State<'_, AppDatabase>,
    folder_id: i32,
    task: String,
    config: PkRoundConfig,
) -> Result<PkRoundInfo, DbError> {
    pk_round_create_core(&db, folder_id, task, config).await
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn pk_round_update_status(
    db: tauri::State<'_, AppDatabase>,
    id: i32,
    status: String,
) -> Result<(), DbError> {
    pk_round_update_status_core(&db, id, status).await
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn pk_round_delete(
    db: tauri::State<'_, AppDatabase>,
    id: i32,
) -> Result<(), DbError> {
    pk_round_delete_core(&db, id).await
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn pk_round_update_judge(
    db: tauri::State<'_, AppDatabase>,
    id: i32,
    judge_result: Option<String>,
    judge_status: String,
) -> Result<(), DbError> {
    pk_round_update_judge_core(&db, id, judge_result, judge_status).await
}

#[cfg(test)]
mod tests {
    use sea_orm::EntityTrait;

    use super::*;
    use crate::db::entities::conversation;
    use crate::db::service::conversation_service;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::AgentType;

    #[tokio::test]
    async fn archiving_round_also_hides_its_pk_conversations() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/pk-archive").await;
        let round = pk_round_service::create(
            &db.conn,
            folder_id,
            "test task".into(),
            PkRoundConfig {
                agents: Vec::new(),
                permission_mode: "default".into(),
                bare_mode: false,
                effort: "default".into(),
                judge_agent: None,
                judge_dimensions: Vec::new(),
                base_commit: None,
            },
        )
        .await
        .unwrap();
        let conversation = conversation_service::create_pk(
            &db.conn,
            folder_id,
            AgentType::Qoder,
            Some("PK contestant".into()),
            None,
            round.id,
        )
        .await
        .unwrap();

        pk_round_delete_core(&db, round.id).await.unwrap();

        assert!(pk_round_service::list(&db.conn, None).await.unwrap().is_empty());
        let archived = conversation::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert!(archived.deleted_at.is_some());
    }
}
