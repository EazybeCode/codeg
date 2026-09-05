use sea_orm_migration::prelude::*;

/// Multi-tenancy foundation: `orgs`, `users`, `sessions`.
///
/// WorkOS becomes a multi-org platform (decision 0006). Identity comes from
/// GitHub (App "EazyWorkOS"): a GitHub account → a `users` row, a GitHub org →
/// an `orgs` row, one user mapped to one org for now. `sessions` are login
/// sessions (signed cookie id → user). Every other tenant table gets an
/// `org_id` in a later migration; this one just lays the identity tables.
///
/// Backend-agnostic (SQLite for local dev, Postgres in production via
/// DATABASE_URL). No DB-level foreign keys — matching this codebase's existing
/// convention of tolerant indexed columns enforced in app logic.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // --- orgs -----------------------------------------------------------
        manager
            .create_table(
                Table::create()
                    .table(Orgs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Orgs::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // GitHub org/account numeric id (stable across renames).
                    .col(
                        ColumnDef::new(Orgs::GithubOrgId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Orgs::Login).text().not_null())
                    .col(ColumnDef::new(Orgs::Name).text().null())
                    .col(ColumnDef::new(Orgs::AvatarUrl).text().null())
                    .col(
                        ColumnDef::new(Orgs::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Orgs::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_orgs_github_org_id")
                    .table(Orgs::Table)
                    .col(Orgs::GithubOrgId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // --- users ----------------------------------------------------------
        manager
            .create_table(
                Table::create()
                    .table(Users::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Users::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Users::GithubUserId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Users::Login).text().not_null())
                    .col(ColumnDef::new(Users::Name).text().null())
                    .col(ColumnDef::new(Users::Email).text().null())
                    .col(ColumnDef::new(Users::AvatarUrl).text().null())
                    // 1 user → 1 org for now. Indexed, no DB FK (app-enforced).
                    .col(ColumnDef::new(Users::OrgId).integer().not_null())
                    .col(
                        ColumnDef::new(Users::Role)
                            .text()
                            .not_null()
                            .default("member"),
                    )
                    // GitHub user access token + refresh, ENCRYPTED at rest.
                    .col(ColumnDef::new(Users::GhTokenEnc).text().null())
                    .col(ColumnDef::new(Users::GhRefreshTokenEnc).text().null())
                    .col(
                        ColumnDef::new(Users::GhTokenExpiresAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Users::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Users::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_users_github_user_id")
                    .table(Users::Table)
                    .col(Users::GithubUserId)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_users_org_id")
                    .table(Users::Table)
                    .col(Users::OrgId)
                    .to_owned(),
            )
            .await?;

        // --- sessions (login sessions) --------------------------------------
        manager
            .create_table(
                Table::create()
                    .table(Sessions::Table)
                    .if_not_exists()
                    // Opaque random id stored in the signed cookie.
                    .col(ColumnDef::new(Sessions::Id).text().not_null().primary_key())
                    .col(ColumnDef::new(Sessions::UserId).integer().not_null())
                    .col(
                        ColumnDef::new(Sessions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Sessions::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_sessions_user_id")
                    .table(Sessions::Table)
                    .col(Sessions::UserId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Sessions::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Users::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Orgs::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Orgs {
    Table,
    Id,
    GithubOrgId,
    Login,
    Name,
    AvatarUrl,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    GithubUserId,
    Login,
    Name,
    Email,
    AvatarUrl,
    OrgId,
    Role,
    GhTokenEnc,
    GhRefreshTokenEnc,
    GhTokenExpiresAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Sessions {
    Table,
    Id,
    UserId,
    CreatedAt,
    ExpiresAt,
}
