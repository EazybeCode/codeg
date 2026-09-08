use sea_orm::{
    sea_query::{Expr, LockType},
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbBackend, EntityTrait, QueryFilter,
    QuerySelect,
};

use crate::db::entities::user;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum TeamAuthError {
    #[error("authentication required")]
    Unauthorized,
    #[error("access denied")]
    Forbidden,
    #[error("authentication state changed; retry")]
    Conflict,
    #[error("authentication service unavailable")]
    Unavailable,
}

pub(crate) trait TeamClock: Send + Sync {
    fn now(&self) -> chrono::DateTime<chrono::Utc>;
}

pub(crate) struct SystemClock;

impl TeamClock for SystemClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

pub(crate) async fn lock_user_identity(
    transaction: &DatabaseTransaction,
    user_id: i32,
    github_user_id: i64,
) -> Result<(), TeamAuthError> {
    if user_id <= 0 || github_user_id <= 0 {
        return Err(TeamAuthError::Unavailable);
    }
    let found = match transaction.get_database_backend() {
        DbBackend::Postgres => user::Entity::find_by_id(user_id)
            .filter(user::Column::GithubUserId.eq(github_user_id))
            .select_only()
            .column(user::Column::Id)
            .column(user::Column::GithubUserId)
            .lock(LockType::Update)
            .into_tuple::<(i32, i64)>()
            .one(transaction)
            .await,
        DbBackend::Sqlite => {
            let updated = user::Entity::update_many()
                .col_expr(
                    user::Column::UpdatedAt,
                    Expr::col(user::Column::UpdatedAt).into(),
                )
                .filter(user::Column::Id.eq(user_id))
                .filter(user::Column::GithubUserId.eq(github_user_id))
                .exec(transaction)
                .await
                .map(|result| (result.rows_affected == 1).then_some(()));
            return match updated {
                Ok(Some(())) => Ok(()),
                Ok(None) | Err(_) => Err(TeamAuthError::Unavailable),
            };
        }
        _ => user::Entity::find_by_id(user_id)
            .filter(user::Column::GithubUserId.eq(github_user_id))
            .select_only()
            .column(user::Column::Id)
            .column(user::Column::GithubUserId)
            .into_tuple::<(i32, i64)>()
            .one(transaction)
            .await,
    }
    .map_err(|_| TeamAuthError::Unavailable)?;
    found.map(|_| ()).ok_or(TeamAuthError::Unavailable)
}
