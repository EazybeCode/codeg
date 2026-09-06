use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use crate::db::entities::workspace_chat_execution::{Entity, Column};
        use sea_orm::Schema;
        let backend = manager.get_database_backend();
        let mut table = Schema::new(backend).create_table_from_entity(Entity);
        table.foreign_key(
            ForeignKey::create()
                .from(Entity, Column::DraftId)
                .to(Alias::new("workspace_chat_draft"), Alias::new("id"))
                .on_delete(ForeignKeyAction::Restrict)
        );
        manager.create_table(table).await?;
        manager.create_index(
            Index::create().name("idx_workspace_chat_execution_org")
                .table(Entity).col(Column::OrgId).to_owned()
        ).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use sea_orm::{EntityTrait, QuerySelect};
        use crate::db::entities::workspace_chat_execution::Entity;
        if Entity::find().limit(1).one(manager.get_connection()).await?.is_some() {
            return Err(DbErr::Custom("workspace execution migration cannot be reverted while execution rows exist".into()));
        }
        manager.drop_table(Table::drop().table(Entity).to_owned()).await
    }
}
