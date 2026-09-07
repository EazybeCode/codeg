use std::sync::Arc;
use axum::{extract::Extension, http::{HeaderMap, StatusCode}, Json};
use serde::Serialize;
use crate::{app_state::AppState, db::entities::org};
use sea_orm::EntityTrait;

#[derive(Serialize)]
pub struct WorkspaceIdentity {
    user: PersonIdentity,
    organization: OrganizationIdentity,
}

#[derive(Serialize)]
struct PersonIdentity { id: i32, login: String, name: Option<String>, role: String }
#[derive(Serialize)]
struct OrganizationIdentity { id: i32, login: String, name: Option<String> }

async fn identity_core(headers: &HeaderMap, db: &sea_orm::DatabaseConnection) -> Result<Option<WorkspaceIdentity>, sea_orm::DbErr> {
    let Some(person) = super::github_auth::current_user_result(headers, db).await? else { return Ok(None); };
    let Some(organization) = org::Entity::find_by_id(person.org_id).one(db).await? else { return Ok(None); };
    // Deliberately project a small public DTO. Never serialize the database user
    // row, which also contains email, token ciphertext and refresh metadata.
    Ok(Some(WorkspaceIdentity {
        user: PersonIdentity { id: person.id, login: person.login, name: person.name, role: person.role },
        organization: OrganizationIdentity { id: organization.id, login: organization.login, name: organization.name },
    }))
}

pub async fn me(Extension(state): Extension<Arc<AppState>>, headers: HeaderMap) -> Result<Json<Option<WorkspaceIdentity>>, StatusCode> {
    identity_core(&headers, &state.db.conn).await.map(Json).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    use crate::db::{entities::{session, user}, test_helpers::fresh_in_memory_db};

    #[tokio::test]
    async fn identity_database_failure_is_not_anonymous() {
        let db = fresh_in_memory_db().await;
        db.conn.clone().close().await.unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("cookie", "workos_session=any-session".parse().unwrap());
        assert!(identity_core(&headers, &db.conn).await.is_err());
    }

    #[tokio::test]
    async fn identity_is_session_scoped_and_never_serializes_credentials() {
        let db = fresh_in_memory_db().await;
        let now = chrono::Utc::now();
        let organization = org::ActiveModel { github_org_id: Set(100), login: Set("acme".into()), name: Set(Some("Acme Product".into())), created_at: Set(now), updated_at: Set(now), ..Default::default() }.insert(&db.conn).await.unwrap();
        let person = user::ActiveModel {
            github_user_id: Set(200), login: Set("maya".into()), name: Set(Some("Maya Rao".into())),
            email: Set(Some("private@example.invalid".into())), org_id: Set(organization.id), role: Set("member".into()),
            gh_token_enc: Set(Some("secret-access".into())), gh_refresh_token_enc: Set(Some("secret-refresh".into())),
            created_at: Set(now), updated_at: Set(now), ..Default::default()
        }.insert(&db.conn).await.unwrap();
        session::ActiveModel { id: Set("session-test".into()), user_id: Set(person.id), created_at: Set(now), expires_at: Set(now + chrono::Duration::hours(1)) }.insert(&db.conn).await.unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("cookie", "workos_session=session-test".parse().unwrap());
        let identity = identity_core(&headers, &db.conn).await.unwrap().unwrap();
        let value = serde_json::to_value(identity).unwrap();
        assert_eq!(value, serde_json::json!({
            "user": { "id": person.id, "login": "maya", "name": "Maya Rao", "role": "member" },
            "organization": { "id": organization.id, "login": "acme", "name": "Acme Product" }
        }));
        assert!(identity_core(&HeaderMap::new(), &db.conn).await.unwrap().is_none());
        headers.insert("cookie", "workos_session=not-a-session".parse().unwrap());
        assert!(identity_core(&headers, &db.conn).await.unwrap().is_none());
    }
}
