use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The GitHub `owner/repo` a subfolder is a clone of, or NULL for a plain
        // organizational (container) subfolder. Paired with the folder's
        // `git_branch` (the checked-out branch of the clone). Nullable, no
        // default, no FK: matches every other folder column added via ALTER TABLE.
        manager
            .alter_table(
                Table::alter()
                    .table(Folder::Table)
                    .add_column(ColumnDef::new(Folder::RepoFullName).string().null())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Folder::Table)
                    .drop_column(Folder::RepoFullName)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Folder {
    Table,
    RepoFullName,
}
