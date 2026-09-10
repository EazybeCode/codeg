use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use sea_orm::{ConnectionTrait, Statement};

        let connection = manager.get_connection();
        let backend = connection.get_database_backend();
        let access_revision_type = match backend {
            sea_orm::DbBackend::Sqlite => "INTEGER",
            sea_orm::DbBackend::Postgres => "BIGINT",
            sea_orm::DbBackend::MySql => "BIGINT",
        };
        let access_revision_check = match backend {
            sea_orm::DbBackend::Sqlite => {
                "typeof(access_revision) = 'integer' AND access_revision >= 0"
            }
            sea_orm::DbBackend::Postgres | sea_orm::DbBackend::MySql => {
                "access_revision >= 0"
            }
        };
        let statements = [
            "ALTER TABLE workspace_chat_draft ADD COLUMN visibility TEXT NOT NULL DEFAULT 'private' CONSTRAINT ck_workspace_chat_draft_visibility CHECK (visibility IN ('private', 'organization'))".to_owned(),
            format!("ALTER TABLE workspace_chat_draft ADD COLUMN access_revision {access_revision_type} NOT NULL DEFAULT 0 CONSTRAINT ck_workspace_chat_draft_access_revision CHECK ({access_revision_check})"),
            "ALTER TABLE workspace_chat_draft ADD COLUMN creation_visibility TEXT NOT NULL DEFAULT 'private' CONSTRAINT ck_workspace_chat_draft_creation_visibility CHECK (creation_visibility IN ('private', 'organization'))".to_owned(),
            "ALTER TABLE workspace_chat_draft ADD COLUMN creation_fingerprint_version INTEGER NOT NULL DEFAULT 1 CONSTRAINT ck_workspace_chat_draft_creation_fingerprint_version CHECK (creation_fingerprint_version IN (1, 2)) CONSTRAINT ck_workspace_chat_draft_creation_contract CHECK (creation_fingerprint_version <> 1 OR creation_visibility = 'private')".to_owned(),
        ];
        for statement in statements {
            connection
                .execute(Statement::from_string(backend, statement))
                .await?;
        }
        manager
            .create_index(
                Index::create()
                    .name("idx_workspace_chat_draft_org_visibility_creator_id")
                    .table(WorkspaceChatDraft::Table)
                    .col(WorkspaceChatDraft::OrgId)
                    .col(WorkspaceChatDraft::Visibility)
                    .col(WorkspaceChatDraft::CreatorUserId)
                    .col(WorkspaceChatDraft::Id)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use sea_orm::{ConnectionTrait, Statement};

        let connection = manager.get_connection();
        let contains_data = connection
            .query_one(Statement::from_string(
                connection.get_database_backend(),
                "SELECT id FROM workspace_chat_draft LIMIT 1".to_owned(),
            ))
            .await?
            .is_some();
        if contains_data {
            return Err(DbErr::Custom(
                "chat privacy migration cannot be reverted while draft rows exist".into(),
            ));
        }

        manager
            .drop_index(
                Index::drop()
                    .name("idx_workspace_chat_draft_org_visibility_creator_id")
                    .table(WorkspaceChatDraft::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        for column in [
            WorkspaceChatDraft::CreationFingerprintVersion,
            WorkspaceChatDraft::CreationVisibility,
            WorkspaceChatDraft::AccessRevision,
            WorkspaceChatDraft::Visibility,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(WorkspaceChatDraft::Table)
                        .drop_column(column)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(Copy, Clone, DeriveIden)]
enum WorkspaceChatDraft {
    Table,
    Id,
    OrgId,
    CreatorUserId,
    Visibility,
    AccessRevision,
    CreationVisibility,
    CreationFingerprintVersion,
}
