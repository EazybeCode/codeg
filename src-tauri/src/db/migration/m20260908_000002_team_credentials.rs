use sea_orm_migration::prelude::*;

const PENDING_GITHUB_ID_CHECK: &str =
    r#"CONSTRAINT "ck_team_pending_onboarding_github_user_id_positive""#;
const CREDENTIAL_USER_ID_CHECK: &str =
    r#"CONSTRAINT "ck_team_github_credential_user_id_positive""#;
const CREDENTIAL_GITHUB_ID_CHECK: &str =
    r#"CONSTRAINT "ck_team_github_credential_github_user_id_positive""#;
const CREDENTIAL_GENERATION_CHECK: &str =
    r#"CONSTRAINT "ck_team_github_credential_generation_positive""#;
const CREDENTIAL_STATE_CHECK: &str = r#"CONSTRAINT "ck_team_github_credential_state""#;
const CREDENTIAL_STATE_SHAPE_CHECK: &str =
    r#"CONSTRAINT "ck_team_github_credential_state_shape""#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in table_statements() {
            manager.create_table(table).await?;
        }
        for index in index_statements() {
            manager.create_index(index).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use sea_orm::{EntityTrait, QuerySelect};

        let connection = manager.get_connection();
        let oauth_attempt_contains_data = crate::db::entities::team_oauth_attempt::Entity::find()
            .limit(1)
            .one(connection)
            .await?
            .is_some();
        let pending_onboarding_contains_data =
            crate::db::entities::team_pending_onboarding::Entity::find()
                .limit(1)
                .one(connection)
                .await?
                .is_some();
        let credential_contains_data = crate::db::entities::team_github_credential::Entity::find()
            .limit(1)
            .one(connection)
            .await?
            .is_some();
        if oauth_attempt_contains_data
            || pending_onboarding_contains_data
            || credential_contains_data
        {
            return Err(DbErr::Custom(
                "team credential migration cannot be reverted while credential rows exist".into(),
            ));
        }

        manager
            .drop_table(
                Table::drop()
                    .table(TeamGithubCredential::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(TeamPendingOnboarding::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(TeamOauthAttempt::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

pub(super) fn table_statements() -> Vec<TableCreateStatement> {
    let valid_credential_shape = Expr::col(TeamGithubCredential::State)
        .eq("active")
        .and(Expr::col(TeamGithubCredential::EncryptedPayload).is_not_null())
        .and(Expr::col(TeamGithubCredential::RefreshOwner).is_null())
        .and(Expr::col(TeamGithubCredential::RefreshExpiresAt).is_null())
        .or(
            Expr::col(TeamGithubCredential::State)
                .eq("refreshing")
                .and(Expr::col(TeamGithubCredential::EncryptedPayload).is_not_null())
                .and(Expr::col(TeamGithubCredential::RefreshOwner).is_not_null())
                .and(Expr::col(TeamGithubCredential::RefreshExpiresAt).is_not_null()),
        )
        .or(
            Expr::col(TeamGithubCredential::State)
                .eq("reauth_required")
                .and(Expr::col(TeamGithubCredential::EncryptedPayload).is_null())
                .and(Expr::col(TeamGithubCredential::RefreshOwner).is_null())
                .and(Expr::col(TeamGithubCredential::RefreshExpiresAt).is_null()),
        );

    vec![
        Table::create()
            .table(TeamOauthAttempt::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(TeamOauthAttempt::StateHash)
                    .text()
                    .not_null()
                    .primary_key(),
            )
            .col(
                ColumnDef::new(TeamOauthAttempt::BrowserHash)
                    .text()
                    .not_null(),
            )
            .col(
                ColumnDef::new(TeamOauthAttempt::EncryptedVerifier)
                    .text()
                    .not_null(),
            )
            .col(
                ColumnDef::new(TeamOauthAttempt::CreatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .col(
                ColumnDef::new(TeamOauthAttempt::ExpiresAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .to_owned(),
        Table::create()
            .table(TeamPendingOnboarding::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(TeamPendingOnboarding::TokenHash)
                    .text()
                    .not_null()
                    .primary_key(),
            )
            .col(
                ColumnDef::new(TeamPendingOnboarding::CsrfHash)
                    .text()
                    .not_null(),
            )
            .col(
                ColumnDef::new(TeamPendingOnboarding::GithubUserId)
                    .big_integer()
                    .not_null()
                    .extra(PENDING_GITHUB_ID_CHECK)
                    .check(Expr::col(TeamPendingOnboarding::GithubUserId).gt(0)),
            )
            .col(
                ColumnDef::new(TeamPendingOnboarding::EncryptedPayload)
                    .text()
                    .not_null(),
            )
            .col(
                ColumnDef::new(TeamPendingOnboarding::CreatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .col(
                ColumnDef::new(TeamPendingOnboarding::ExpiresAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .to_owned(),
        Table::create()
            .table(TeamGithubCredential::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(TeamGithubCredential::UserId)
                    .integer()
                    .not_null()
                    .primary_key()
                    .extra(CREDENTIAL_USER_ID_CHECK)
                    .check(Expr::col(TeamGithubCredential::UserId).gt(0)),
            )
            .col(
                ColumnDef::new(TeamGithubCredential::GithubUserId)
                    .big_integer()
                    .not_null()
                    .extra(CREDENTIAL_GITHUB_ID_CHECK)
                    .check(Expr::col(TeamGithubCredential::GithubUserId).gt(0)),
            )
            .col(
                ColumnDef::new(TeamGithubCredential::Generation)
                    .big_integer()
                    .not_null()
                    .extra(CREDENTIAL_GENERATION_CHECK)
                    .check(Expr::col(TeamGithubCredential::Generation).gt(0)),
            )
            .col(
                ColumnDef::new(TeamGithubCredential::State)
                    .text()
                    .not_null()
                    .extra(CREDENTIAL_STATE_CHECK)
                    .check(Expr::col(TeamGithubCredential::State).is_in([
                        "active",
                        "refreshing",
                        "reauth_required",
                    ])),
            )
            .col(
                ColumnDef::new(TeamGithubCredential::EncryptedPayload)
                    .text()
                    .null()
                    .extra(CREDENTIAL_STATE_SHAPE_CHECK)
                    .check(valid_credential_shape),
            )
            .col(
                ColumnDef::new(TeamGithubCredential::RefreshOwner)
                    .text()
                    .null(),
            )
            .col(
                ColumnDef::new(TeamGithubCredential::RefreshExpiresAt)
                    .timestamp_with_time_zone()
                    .null(),
            )
            .col(
                ColumnDef::new(TeamGithubCredential::CreatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .col(
                ColumnDef::new(TeamGithubCredential::UpdatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .to_owned(),
    ]
}

pub(super) fn index_statements() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_team_oauth_attempt_expires_at")
            .table(TeamOauthAttempt::Table)
            .col(TeamOauthAttempt::ExpiresAt)
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("idx_team_pending_onboarding_expires_at")
            .table(TeamPendingOnboarding::Table)
            .col(TeamPendingOnboarding::ExpiresAt)
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("uidx_team_github_credential_github_user_id")
            .table(TeamGithubCredential::Table)
            .col(TeamGithubCredential::GithubUserId)
            .unique()
            .if_not_exists()
            .to_owned(),
    ]
}

#[derive(Copy, Clone, DeriveIden)]
enum TeamOauthAttempt {
    Table,
    StateHash,
    BrowserHash,
    EncryptedVerifier,
    CreatedAt,
    ExpiresAt,
}

#[derive(Copy, Clone, DeriveIden)]
enum TeamPendingOnboarding {
    Table,
    TokenHash,
    CsrfHash,
    GithubUserId,
    EncryptedPayload,
    CreatedAt,
    ExpiresAt,
}

#[derive(Copy, Clone, DeriveIden)]
enum TeamGithubCredential {
    Table,
    UserId,
    GithubUserId,
    Generation,
    State,
    EncryptedPayload,
    RefreshOwner,
    RefreshExpiresAt,
    CreatedAt,
    UpdatedAt,
}
