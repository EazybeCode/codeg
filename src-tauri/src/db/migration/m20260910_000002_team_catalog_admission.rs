use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(TeamCatalogAdmission::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TeamCatalogAdmission::ResourceKind)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TeamCatalogAdmission::ResourceId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TeamCatalogAdmission::OrgId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TeamCatalogAdmission::CreatorUserId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TeamCatalogAdmission::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(TeamCatalogAdmission::ResourceKind)
                            .col(TeamCatalogAdmission::ResourceId),
                    )
                    .check(
                        Expr::col(TeamCatalogAdmission::ResourceKind)
                            .is_in(["repository", "node", "chat"]),
                    )
                    .check(Expr::col(TeamCatalogAdmission::ResourceId).gt(0))
                    .check(Expr::col(TeamCatalogAdmission::OrgId).gt(0))
                    .check(Expr::col(TeamCatalogAdmission::CreatorUserId).gt(0))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_team_catalog_admission_org_kind_resource")
                    .table(TeamCatalogAdmission::Table)
                    .col(TeamCatalogAdmission::OrgId)
                    .col(TeamCatalogAdmission::ResourceKind)
                    .col(TeamCatalogAdmission::ResourceId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use sea_orm::{EntityTrait, QuerySelect};

        if crate::db::entities::team_catalog_admission::Entity::find()
            .limit(1)
            .one(manager.get_connection())
            .await?
            .is_some()
        {
            return Err(DbErr::Custom(
                "team catalog admission migration cannot be reverted while admissions exist"
                    .into(),
            ));
        }
        manager
            .drop_table(
                Table::drop()
                    .table(TeamCatalogAdmission::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum TeamCatalogAdmission {
    Table,
    ResourceKind,
    ResourceId,
    OrgId,
    CreatorUserId,
    CreatedAt,
}
