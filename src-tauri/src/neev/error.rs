#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AuthError {
    #[error("authentication required")]
    Unauthorized,
    #[error("access denied")]
    Forbidden,
    #[error("state changed; retry")]
    Conflict,
    #[error("invalid input")]
    Invalid,
    #[error("try again later")]
    Limited,
    #[error("service unavailable")]
    Unavailable,
}

pub(crate) fn database_error(error: sea_orm::DbErr) -> AuthError {
    use sea_orm::{RuntimeErr, SqlErr};
    if matches!(error.sql_err(), Some(SqlErr::UniqueConstraintViolation(_))) {
        return AuthError::Conflict;
    }
    if let sea_orm::DbErr::Exec(RuntimeErr::SqlxError(ref inner))
    | sea_orm::DbErr::Query(RuntimeErr::SqlxError(ref inner)) = error
    {
        if inner
            .as_database_error()
            .and_then(|e| e.code())
            .is_some_and(|c| matches!(c.as_ref(), "40001" | "40P01" | "5" | "6"))
        {
            return AuthError::Conflict;
        }
    }
    AuthError::Unavailable
}
