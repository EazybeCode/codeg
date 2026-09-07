use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use crate::db::entities::workspace_writer_lease::{Entity, Column};
        let mut table = sea_orm::Schema::new(manager.get_database_backend()).create_table_from_entity(Entity);
        table.foreign_key(ForeignKey::create().from(Entity, Column::DraftId)
            .to(Alias::new("workspace_chat_execution"), Alias::new("draft_id"))
            .on_delete(ForeignKeyAction::Restrict));
        manager.create_table(table).await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use sea_orm::{EntityTrait, QuerySelect};
        use crate::db::entities::workspace_writer_lease::Entity;
        if Entity::find().limit(1).one(manager.get_connection()).await?.is_some() {
            return Err(DbErr::Custom("cannot remove active workspace writer leases".into()));
        }
        manager.drop_table(Table::drop().table(Entity).to_owned()).await
    }
}
