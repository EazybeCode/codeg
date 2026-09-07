use sea_orm_migration::prelude::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use crate::db::entities::{workspace_repository, workspace_chat_draft};

#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.alter_table(Table::alter().table(workspace_repository::Entity)
            .add_column(ColumnDef::new(workspace_repository::Column::VerifiedNames).text().not_null().default("[]"))
            .to_owned()).await?;
        // Existing catalog names were verified on connection. Do not infer an
        // old name from filesystem paths or an arbitrary cache origin.
        for row in workspace_repository::Entity::find().all(manager.get_connection()).await? {
            let mut names = vec![row.full_name.clone()];
            // Draft names are immutable verified snapshots. Only this exact
            // organization/catalog-row/numerical-ID join establishes history.
            for draft in workspace_chat_draft::Entity::find()
                .filter(workspace_chat_draft::Column::OrgId.eq(row.org_id))
                .filter(workspace_chat_draft::Column::RepositoryId.eq(row.id))
                .filter(workspace_chat_draft::Column::GithubRepositoryId.eq(row.github_repository_id.clone()))
                .order_by_asc(workspace_chat_draft::Column::Id)
                .all(manager.get_connection()).await?
            {
                if !names.contains(&draft.full_name) { names.push(draft.full_name); }
            }
            if names.iter().any(|name| crate::workspace_catalog::validate_repository_full_name(name).is_err()) {
                return Err(DbErr::Custom("invalid verified repository name history".into()));
            }
            let names = serde_json::to_string(&names).map_err(|_| DbErr::Custom("invalid repository name history".into()))?;
            let mut active: workspace_repository::ActiveModel = row.into();
            active.verified_names = Set(names);
            active.update(manager.get_connection()).await?;
        }
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use sea_orm::QuerySelect;
        if workspace_repository::Entity::find().limit(1).one(manager.get_connection()).await?.is_some() {
            return Err(DbErr::Custom("cannot discard trusted repository history while repositories exist".into()));
        }
        manager.alter_table(Table::alter().table(workspace_repository::Entity)
            .drop_column(workspace_repository::Column::VerifiedNames).to_owned()).await
    }
}
