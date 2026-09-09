use sea_orm_migration::prelude::*;

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
        use sea_orm::{ConnectionTrait, Statement};

        let conn = manager.get_connection();
        for table in [
            "workspace_chat_draft",
            "workspace_node",
            "workspace_repository",
        ] {
            let present = conn
                .query_one(Statement::from_string(
                    conn.get_database_backend(),
                    format!("SELECT id FROM {table} LIMIT 1"),
                ))
                .await?
                .is_some();
            if present {
                return Err(DbErr::Custom(
                    "workspace catalog migration cannot be reverted while catalog rows exist"
                        .into(),
                ));
            }
        }

        manager
            .drop_table(
                Table::drop()
                    .table(WorkspaceChatDraft::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(WorkspaceNode::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(WorkspaceRepository::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

fn table_statements() -> Vec<TableCreateStatement> {
    vec![
        Table::create()
            .table(WorkspaceRepository::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(WorkspaceRepository::Id)
                    .integer()
                    .not_null()
                    .auto_increment()
                    .primary_key(),
            )
            .col(ColumnDef::new(WorkspaceRepository::OrgId).integer().not_null())
            .col(
                ColumnDef::new(WorkspaceRepository::GithubRepositoryId)
                    .text()
                    .not_null(),
            )
            .col(
                ColumnDef::new(WorkspaceRepository::FullName)
                    .text()
                    .not_null(),
            )
            .col(
                ColumnDef::new(WorkspaceRepository::CreatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .col(
                ColumnDef::new(WorkspaceRepository::UpdatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .to_owned(),
        Table::create()
            .table(WorkspaceNode::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(WorkspaceNode::Id)
                    .integer()
                    .not_null()
                    .auto_increment()
                    .primary_key(),
            )
            .col(ColumnDef::new(WorkspaceNode::OrgId).integer().not_null())
            .col(ColumnDef::new(WorkspaceNode::ParentId).integer().null())
            .col(ColumnDef::new(WorkspaceNode::Kind).text().not_null())
            .col(ColumnDef::new(WorkspaceNode::Name).text().not_null())
            .col(ColumnDef::new(WorkspaceNode::RepositoryId).integer().null())
            .col(ColumnDef::new(WorkspaceNode::StartingBranch).text().null())
            .col(
                ColumnDef::new(WorkspaceNode::Revision)
                    .big_integer()
                    .not_null()
                    .default(0),
            )
            .col(
                ColumnDef::new(WorkspaceNode::CreatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .col(
                ColumnDef::new(WorkspaceNode::UpdatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .to_owned(),
        Table::create()
            .table(WorkspaceChatDraft::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(WorkspaceChatDraft::Id)
                    .integer()
                    .not_null()
                    .auto_increment()
                    .primary_key(),
            )
            .col(ColumnDef::new(WorkspaceChatDraft::OrgId).integer().not_null())
            .col(ColumnDef::new(WorkspaceChatDraft::NodeId).integer().not_null())
            .col(
                ColumnDef::new(WorkspaceChatDraft::CreatorUserId)
                    .integer()
                    .not_null(),
            )
            .col(
                ColumnDef::new(WorkspaceChatDraft::RepositoryId)
                    .integer()
                    .not_null(),
            )
            .col(
                ColumnDef::new(WorkspaceChatDraft::GithubRepositoryId)
                    .text()
                    .not_null(),
            )
            .col(ColumnDef::new(WorkspaceChatDraft::FullName).text().not_null())
            .col(
                ColumnDef::new(WorkspaceChatDraft::StartingBranch)
                    .text()
                    .not_null(),
            )
            .col(ColumnDef::new(WorkspaceChatDraft::Title).text().null())
            .col(
                ColumnDef::new(WorkspaceChatDraft::RequestId)
                    .text()
                    .not_null(),
            )
            .col(
                ColumnDef::new(WorkspaceChatDraft::RequestFingerprint)
                    .text()
                    .not_null(),
            )
            .col(
                ColumnDef::new(WorkspaceChatDraft::ExecutionState)
                    .text()
                    .not_null()
                    .default("unprovisioned"),
            )
            .col(
                ColumnDef::new(WorkspaceChatDraft::CreatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .col(
                ColumnDef::new(WorkspaceChatDraft::UpdatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .to_owned(),
    ]
}

fn index_statements() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_workspace_repository_org_id")
            .table(WorkspaceRepository::Table)
            .col(WorkspaceRepository::OrgId)
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("uidx_workspace_repository_org_github_repository")
            .table(WorkspaceRepository::Table)
            .col(WorkspaceRepository::OrgId)
            .col(WorkspaceRepository::GithubRepositoryId)
            .unique()
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("idx_workspace_node_org_id")
            .table(WorkspaceNode::Table)
            .col(WorkspaceNode::OrgId)
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("idx_workspace_node_org_parent")
            .table(WorkspaceNode::Table)
            .col(WorkspaceNode::OrgId)
            .col(WorkspaceNode::ParentId)
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("idx_workspace_chat_draft_org_id")
            .table(WorkspaceChatDraft::Table)
            .col(WorkspaceChatDraft::OrgId)
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("idx_workspace_chat_draft_org_node")
            .table(WorkspaceChatDraft::Table)
            .col(WorkspaceChatDraft::OrgId)
            .col(WorkspaceChatDraft::NodeId)
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("uidx_workspace_chat_draft_org_creator_request")
            .table(WorkspaceChatDraft::Table)
            .col(WorkspaceChatDraft::OrgId)
            .col(WorkspaceChatDraft::CreatorUserId)
            .col(WorkspaceChatDraft::RequestId)
            .unique()
            .if_not_exists()
            .to_owned(),
    ]
}

#[derive(DeriveIden)]
enum WorkspaceRepository {
    Table,
    Id,
    OrgId,
    GithubRepositoryId,
    FullName,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum WorkspaceNode {
    Table,
    Id,
    OrgId,
    ParentId,
    Kind,
    Name,
    RepositoryId,
    StartingBranch,
    Revision,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum WorkspaceChatDraft {
    Table,
    Id,
    OrgId,
    NodeId,
    CreatorUserId,
    RepositoryId,
    GithubRepositoryId,
    FullName,
    StartingBranch,
    Title,
    RequestId,
    RequestFingerprint,
    ExecutionState,
    CreatedAt,
    UpdatedAt,
}

#[cfg(test)]
mod tests {
    use sea_orm_migration::sea_query::{PostgresQueryBuilder, SqliteQueryBuilder};

    use super::{index_statements, table_statements};

    #[test]
    fn workspace_catalog_schema_builds_for_sqlite_and_postgres() {
        for table in table_statements() {
            assert!(!table.to_string(SqliteQueryBuilder).is_empty());
            assert!(!table.to_string(PostgresQueryBuilder).is_empty());
        }
        for index in index_statements() {
            assert!(!index.to_string(SqliteQueryBuilder).is_empty());
            assert!(!index.to_string(PostgresQueryBuilder).is_empty());
        }
    }
}
