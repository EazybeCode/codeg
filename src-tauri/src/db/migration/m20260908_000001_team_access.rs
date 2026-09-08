use sea_orm_migration::prelude::*;

const MEMBERSHIP_ROLE_CHECK: &str =
    r#"CONSTRAINT "ck_organization_membership_role""#;
const MEMBERSHIP_STATE_CHECK: &str =
    r#"CONSTRAINT "ck_organization_membership_state""#;
const MEMBERSHIP_AUTHORIZATION_REVISION_CHECK: &str =
    r#"CONSTRAINT "ck_organization_membership_authorization_revision_nonnegative""#;
const MEMBERSHIP_CREDENTIAL_GENERATION_CHECK: &str =
    r#"CONSTRAINT "ck_organization_membership_credential_generation_nonnegative""#;
const TEAM_SESSION_AUTHORIZATION_REVISION_CHECK: &str =
    r#"CONSTRAINT "ck_team_session_authorization_revision_nonnegative""#;
const TEAM_SESSION_CREDENTIAL_GENERATION_CHECK: &str =
    r#"CONSTRAINT "ck_team_session_credential_generation_nonnegative""#;

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
        let contains_data = crate::db::entities::team_session::Entity::find()
            .limit(1)
            .one(connection)
            .await?
            .is_some()
            || crate::db::entities::organization_membership::Entity::find()
                .limit(1)
                .one(connection)
                .await?
                .is_some();
        if contains_data {
            return Err(DbErr::Custom(
                "team access migration cannot be reverted while membership or session rows exist"
                    .into(),
            ));
        }

        manager
            .drop_table(
                Table::drop()
                    .table(TeamSession::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(OrganizationMembership::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

pub(super) fn table_statements() -> Vec<TableCreateStatement> {
    vec![
        Table::create()
            .table(OrganizationMembership::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(OrganizationMembership::Id)
                    .integer()
                    .not_null()
                    .auto_increment()
                    .primary_key(),
            )
            .col(
                ColumnDef::new(OrganizationMembership::OrgId)
                    .integer()
                    .not_null(),
            )
            .col(
                ColumnDef::new(OrganizationMembership::UserId)
                    .integer()
                    .not_null(),
            )
            .col(
                ColumnDef::new(OrganizationMembership::Role)
                    .text()
                    .not_null()
                    .extra(MEMBERSHIP_ROLE_CHECK)
                    .check(
                        Expr::col(OrganizationMembership::Role)
                            .is_in(["admin", "member"]),
                    ),
            )
            .col(
                ColumnDef::new(OrganizationMembership::State)
                    .text()
                    .not_null()
                    .extra(MEMBERSHIP_STATE_CHECK)
                    .check(
                        Expr::col(OrganizationMembership::State)
                            .is_in(["active", "revoked", "blocked"]),
                    ),
            )
            .col(
                ColumnDef::new(OrganizationMembership::AuthorizationRevision)
                    .big_integer()
                    .not_null()
                    .default(0)
                    .extra(MEMBERSHIP_AUTHORIZATION_REVISION_CHECK)
                    .check(Expr::col(OrganizationMembership::AuthorizationRevision).gte(0)),
            )
            .col(
                ColumnDef::new(OrganizationMembership::CredentialGeneration)
                    .big_integer()
                    .not_null()
                    .default(0)
                    .extra(MEMBERSHIP_CREDENTIAL_GENERATION_CHECK)
                    .check(Expr::col(OrganizationMembership::CredentialGeneration).gte(0)),
            )
            .col(
                ColumnDef::new(OrganizationMembership::VerifiedAt)
                    .timestamp_with_time_zone()
                    .null(),
            )
            .col(
                ColumnDef::new(OrganizationMembership::CreatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .col(
                ColumnDef::new(OrganizationMembership::UpdatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .to_owned(),
        Table::create()
            .table(TeamSession::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(TeamSession::TokenHash)
                    .text()
                    .not_null()
                    .primary_key(),
            )
            .col(ColumnDef::new(TeamSession::UserId).integer().not_null())
            .col(ColumnDef::new(TeamSession::OrgId).integer().not_null())
            .col(
                ColumnDef::new(TeamSession::MembershipId)
                    .integer()
                    .not_null(),
            )
            .col(
                ColumnDef::new(TeamSession::AuthorizationRevision)
                    .big_integer()
                    .not_null()
                    .extra(TEAM_SESSION_AUTHORIZATION_REVISION_CHECK)
                    .check(Expr::col(TeamSession::AuthorizationRevision).gte(0)),
            )
            .col(
                ColumnDef::new(TeamSession::CredentialGeneration)
                    .big_integer()
                    .not_null()
                    .extra(TEAM_SESSION_CREDENTIAL_GENERATION_CHECK)
                    .check(Expr::col(TeamSession::CredentialGeneration).gte(0)),
            )
            .col(ColumnDef::new(TeamSession::CsrfHash).text().not_null())
            .col(
                ColumnDef::new(TeamSession::CreatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .col(
                ColumnDef::new(TeamSession::ExpiresAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .to_owned(),
    ]
}

pub(super) fn index_statements() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("uidx_organization_membership_org_user")
            .table(OrganizationMembership::Table)
            .col(OrganizationMembership::OrgId)
            .col(OrganizationMembership::UserId)
            .unique()
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("idx_team_session_membership_id")
            .table(TeamSession::Table)
            .col(TeamSession::MembershipId)
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("idx_team_session_expires_at")
            .table(TeamSession::Table)
            .col(TeamSession::ExpiresAt)
            .if_not_exists()
            .to_owned(),
    ]
}

#[derive(DeriveIden)]
enum OrganizationMembership {
    Table,
    Id,
    OrgId,
    UserId,
    Role,
    State,
    AuthorizationRevision,
    CredentialGeneration,
    VerifiedAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum TeamSession {
    Table,
    TokenHash,
    UserId,
    OrgId,
    MembershipId,
    AuthorizationRevision,
    CredentialGeneration,
    CsrfHash,
    CreatedAt,
    ExpiresAt,
}
