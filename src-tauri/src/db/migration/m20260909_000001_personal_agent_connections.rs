use sea_orm_migration::prelude::*;

const CONNECTION_ID_LENGTH_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_connection_id_length""#;
const CONNECTION_ORG_ID_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_connection_org_id_positive""#;
const CONNECTION_USER_ID_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_connection_user_id_positive""#;
const CONNECTION_PROVIDER_CHECK: &str = r#"CONSTRAINT "ck_team_agent_connection_provider""#;
const CONNECTION_GENERATION_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_connection_generation_positive""#;
const CONNECTION_STATE_CHECK: &str = r#"CONSTRAINT "ck_team_agent_connection_state""#;
const CONNECTION_ACCOUNT_LABEL_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_connection_account_label_length""#;
const CONNECTION_ERROR_CODE_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_connection_error_code""#;
const CONNECTION_STORAGE_HANDLE_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_connection_storage_handle_length""#;
const CONNECTION_STATE_SHAPE_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_connection_state_shape""#;
const CONNECTION_UPDATED_AT_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_connection_updated_at_order""#;

const ATTEMPT_ID_LENGTH_CHECK: &str = r#"CONSTRAINT "ck_team_agent_attempt_id_length""#;
const ATTEMPT_CONNECTION_ID_LENGTH_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_attempt_connection_id_length""#;
const ATTEMPT_GENERATION_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_attempt_generation_positive""#;
const ATTEMPT_SESSION_ID_LENGTH_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_attempt_session_id_length""#;
const ATTEMPT_STATE_CHECK: &str = r#"CONSTRAINT "ck_team_agent_attempt_state""#;
const ATTEMPT_EXPIRES_AT_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_attempt_expires_at_order""#;
const ATTEMPT_UPDATED_AT_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_attempt_updated_at_order""#;

const OPERATION_ID_LENGTH_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_operation_id_length""#;
const OPERATION_CONNECTION_ID_LENGTH_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_operation_connection_id_length""#;
const OPERATION_KIND_CHECK: &str = r#"CONSTRAINT "ck_team_agent_operation_kind""#;
const OPERATION_REQUEST_KEY_LENGTH_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_operation_request_key_length""#;
const OPERATION_TARGET_ATTEMPT_ID_LENGTH_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_operation_target_attempt_id_length""#;
const OPERATION_TARGET_GENERATION_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_operation_target_generation_positive""#;
const OPERATION_RESULT_GENERATION_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_operation_result_generation""#;
const OPERATION_STATE_CHECK: &str = r#"CONSTRAINT "ck_team_agent_operation_state""#;
const OPERATION_LEASE_OWNER_LENGTH_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_operation_lease_owner_length""#;
const OPERATION_ERROR_CODE_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_operation_error_code""#;
const OPERATION_LEASE_SHAPE_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_operation_lease_shape""#;
const OPERATION_TARGET_SHAPE_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_operation_target_shape""#;
const OPERATION_EXPIRES_AT_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_operation_expires_at_order""#;
const OPERATION_UPDATED_AT_CHECK: &str =
    r#"CONSTRAINT "ck_team_agent_operation_updated_at_order""#;

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
        use sea_orm::ConnectionTrait;

        let connection = manager.get_connection();
        let backend = connection.get_database_backend();
        for table in [
            "team_agent_connection",
            "team_agent_attempt",
            "team_agent_operation",
        ] {
            let contains_data = connection
                .query_one(sea_orm::Statement::from_string(
                    backend,
                    format!("SELECT 1 AS present FROM {table} LIMIT 1"),
                ))
                .await?
                .is_some();
            if contains_data {
                return Err(DbErr::Custom(
                    "personal agent metadata migration cannot be reverted while rows exist"
                        .into(),
                ));
            }
        }

        manager
            .drop_table(
                Table::drop()
                    .table(TeamAgentOperation::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(TeamAgentAttempt::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(TeamAgentConnection::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

fn required_length<I: IntoColumnRef + Clone>(column: I, maximum: i32) -> SimpleExpr {
    Func::char_length(Expr::col(column.clone()))
        .gte(1)
        .and(Func::char_length(Expr::col(column)).lte(maximum))
}

fn optional_length<I: IntoColumnRef + Clone>(column: I, maximum: i32) -> SimpleExpr {
    Expr::col(column.clone())
        .is_null()
        .or(required_length(column, maximum))
}

fn valid_error_code<I: IntoColumnRef + Clone>(column: I) -> SimpleExpr {
    Expr::col(column.clone()).is_null().or(Expr::col(column).is_in([
        "provider_unavailable",
        "reauth_required",
        "unsupported_account",
        "expired",
        "canceled",
        "cleanup_failed",
        "interrupted",
    ]))
}

pub(crate) fn table_statements() -> Vec<TableCreateStatement> {
    let connected_shape = Expr::col(TeamAgentConnection::State)
        .eq("connected")
        .and(Expr::col(TeamAgentConnection::AccountLabel).is_not_null())
        .and(Expr::col(TeamAgentConnection::CheckedAt).is_not_null())
        .and(Expr::col(TeamAgentConnection::StorageHandle).is_not_null())
        .and(Expr::col(TeamAgentConnection::ErrorCode).is_null());
    let disconnected_shape = Expr::col(TeamAgentConnection::State)
        .eq("disconnected")
        .and(Expr::col(TeamAgentConnection::AccountLabel).is_null())
        .and(Expr::col(TeamAgentConnection::CheckedAt).is_null())
        .and(Expr::col(TeamAgentConnection::StorageHandle).is_null())
        .and(Expr::col(TeamAgentConnection::ErrorCode).is_null());
    let transitional_shape = Expr::col(TeamAgentConnection::State).is_in([
        "connecting",
        "reconnect_required",
        "disconnecting",
        "unavailable",
    ]);
    let connection_state_shape = connected_shape
        .or(disconnected_shape)
        .or(transitional_shape);

    let operation_lease_shape = Expr::col(TeamAgentOperation::State)
        .eq("leased")
        .and(Expr::col(TeamAgentOperation::LeaseOwner).is_not_null())
        .and(Expr::col(TeamAgentOperation::LeaseExpiresAt).is_not_null())
        .or(
            Expr::col(TeamAgentOperation::State)
                .ne("leased")
                .and(Expr::col(TeamAgentOperation::LeaseOwner).is_null())
                .and(Expr::col(TeamAgentOperation::LeaseExpiresAt).is_null()),
        );
    let operation_target_shape = Expr::col(TeamAgentOperation::Kind)
        .is_in(["connect", "cancel"])
        .and(Expr::col(TeamAgentOperation::TargetAttemptId).is_not_null())
        .or(
            Expr::col(TeamAgentOperation::Kind)
                .eq("disconnect")
                .and(Expr::col(TeamAgentOperation::TargetAttemptId).is_null()),
        );

    vec![
        Table::create()
            .table(TeamAgentConnection::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(TeamAgentConnection::Id)
                    .text()
                    .not_null()
                    .primary_key()
                    .extra(CONNECTION_ID_LENGTH_CHECK)
                    .check(required_length(TeamAgentConnection::Id, 64)),
            )
            .col(
                ColumnDef::new(TeamAgentConnection::OrgId)
                    .integer()
                    .not_null()
                    .extra(CONNECTION_ORG_ID_CHECK)
                    .check(Expr::col(TeamAgentConnection::OrgId).gt(0)),
            )
            .col(
                ColumnDef::new(TeamAgentConnection::UserId)
                    .integer()
                    .not_null()
                    .extra(CONNECTION_USER_ID_CHECK)
                    .check(Expr::col(TeamAgentConnection::UserId).gt(0)),
            )
            .col(
                ColumnDef::new(TeamAgentConnection::Provider)
                    .text()
                    .not_null()
                    .extra(CONNECTION_PROVIDER_CHECK)
                    .check(Expr::col(TeamAgentConnection::Provider).is_in(["claude", "codex"])),
            )
            .col(
                ColumnDef::new(TeamAgentConnection::Generation)
                    .big_integer()
                    .not_null()
                    .extra(CONNECTION_GENERATION_CHECK)
                    .check(Expr::col(TeamAgentConnection::Generation).gt(0)),
            )
            .col(
                ColumnDef::new(TeamAgentConnection::State)
                    .text()
                    .not_null()
                    .extra(CONNECTION_STATE_CHECK)
                    .check(Expr::col(TeamAgentConnection::State).is_in([
                        "disconnected",
                        "connecting",
                        "connected",
                        "reconnect_required",
                        "disconnecting",
                        "unavailable",
                    ])),
            )
            .col(
                ColumnDef::new(TeamAgentConnection::AccountLabel)
                    .text()
                    .null()
                    .extra(CONNECTION_ACCOUNT_LABEL_CHECK)
                    .check(optional_length(TeamAgentConnection::AccountLabel, 160)),
            )
            .col(
                ColumnDef::new(TeamAgentConnection::CheckedAt)
                    .timestamp_with_time_zone()
                    .null(),
            )
            .col(
                ColumnDef::new(TeamAgentConnection::ErrorCode)
                    .text()
                    .null()
                    .extra(CONNECTION_ERROR_CODE_CHECK)
                    .check(valid_error_code(TeamAgentConnection::ErrorCode)),
            )
            .col(
                ColumnDef::new(TeamAgentConnection::StorageHandle)
                    .text()
                    .null()
                    .extra(CONNECTION_STORAGE_HANDLE_CHECK)
                    .check(optional_length(TeamAgentConnection::StorageHandle, 64)),
            )
            .col(
                ColumnDef::new(TeamAgentConnection::CreatedAt)
                    .timestamp_with_time_zone()
                    .not_null()
                    .extra(CONNECTION_STATE_SHAPE_CHECK)
                    .check(connection_state_shape),
            )
            .col(
                ColumnDef::new(TeamAgentConnection::UpdatedAt)
                    .timestamp_with_time_zone()
                    .not_null()
                    .extra(CONNECTION_UPDATED_AT_CHECK)
                    .check(
                        Expr::col(TeamAgentConnection::UpdatedAt)
                            .gte(Expr::col(TeamAgentConnection::CreatedAt)),
                    ),
            )
            .foreign_key(
                ForeignKey::create()
                    .name("fk_team_agent_connection_org_id")
                    .from(TeamAgentConnection::Table, TeamAgentConnection::OrgId)
                    .to(Orgs::Table, Orgs::Id)
                    .on_delete(ForeignKeyAction::Restrict)
                    .on_update(ForeignKeyAction::Restrict),
            )
            .foreign_key(
                ForeignKey::create()
                    .name("fk_team_agent_connection_user_id")
                    .from(TeamAgentConnection::Table, TeamAgentConnection::UserId)
                    .to(Users::Table, Users::Id)
                    .on_delete(ForeignKeyAction::Restrict)
                    .on_update(ForeignKeyAction::Restrict),
            )
            .to_owned(),
        Table::create()
            .table(TeamAgentAttempt::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(TeamAgentAttempt::Id)
                    .text()
                    .not_null()
                    .primary_key()
                    .extra(ATTEMPT_ID_LENGTH_CHECK)
                    .check(required_length(TeamAgentAttempt::Id, 64)),
            )
            .col(
                ColumnDef::new(TeamAgentAttempt::ConnectionId)
                    .text()
                    .not_null()
                    .extra(ATTEMPT_CONNECTION_ID_LENGTH_CHECK)
                    .check(required_length(TeamAgentAttempt::ConnectionId, 64)),
            )
            .col(
                ColumnDef::new(TeamAgentAttempt::Generation)
                    .big_integer()
                    .not_null()
                    .extra(ATTEMPT_GENERATION_CHECK)
                    .check(Expr::col(TeamAgentAttempt::Generation).gt(0)),
            )
            .col(
                ColumnDef::new(TeamAgentAttempt::SessionId)
                    .text()
                    .not_null()
                    .extra(ATTEMPT_SESSION_ID_LENGTH_CHECK)
                    .check(required_length(TeamAgentAttempt::SessionId, 128)),
            )
            .col(
                ColumnDef::new(TeamAgentAttempt::State)
                    .text()
                    .not_null()
                    .extra(ATTEMPT_STATE_CHECK)
                    .check(Expr::col(TeamAgentAttempt::State).is_in([
                        "pending",
                        "completed",
                        "canceled",
                        "expired",
                        "failed",
                    ])),
            )
            .col(
                ColumnDef::new(TeamAgentAttempt::CreatedAt)
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .col(
                ColumnDef::new(TeamAgentAttempt::ExpiresAt)
                    .timestamp_with_time_zone()
                    .not_null()
                    .extra(ATTEMPT_EXPIRES_AT_CHECK)
                    .check(
                        Expr::col(TeamAgentAttempt::ExpiresAt)
                            .gt(Expr::col(TeamAgentAttempt::CreatedAt)),
                    ),
            )
            .col(
                ColumnDef::new(TeamAgentAttempt::UpdatedAt)
                    .timestamp_with_time_zone()
                    .not_null()
                    .extra(ATTEMPT_UPDATED_AT_CHECK)
                    .check(
                        Expr::col(TeamAgentAttempt::UpdatedAt)
                            .gte(Expr::col(TeamAgentAttempt::CreatedAt)),
                    ),
            )
            .foreign_key(
                ForeignKey::create()
                    .name("fk_team_agent_attempt_connection_id")
                    .from(TeamAgentAttempt::Table, TeamAgentAttempt::ConnectionId)
                    .to(TeamAgentConnection::Table, TeamAgentConnection::Id)
                    .on_delete(ForeignKeyAction::Restrict)
                    .on_update(ForeignKeyAction::Restrict),
            )
            .to_owned(),
        Table::create()
            .table(TeamAgentOperation::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(TeamAgentOperation::Id)
                    .text()
                    .not_null()
                    .primary_key()
                    .extra(OPERATION_ID_LENGTH_CHECK)
                    .check(required_length(TeamAgentOperation::Id, 64)),
            )
            .col(
                ColumnDef::new(TeamAgentOperation::ConnectionId)
                    .text()
                    .not_null()
                    .extra(OPERATION_CONNECTION_ID_LENGTH_CHECK)
                    .check(required_length(TeamAgentOperation::ConnectionId, 64)),
            )
            .col(
                ColumnDef::new(TeamAgentOperation::Kind)
                    .text()
                    .not_null()
                    .extra(OPERATION_KIND_CHECK)
                    .check(Expr::col(TeamAgentOperation::Kind).is_in([
                        "connect",
                        "cancel",
                        "disconnect",
                    ])),
            )
            .col(
                ColumnDef::new(TeamAgentOperation::RequestKey)
                    .text()
                    .not_null()
                    .extra(OPERATION_REQUEST_KEY_LENGTH_CHECK)
                    .check(required_length(TeamAgentOperation::RequestKey, 64)),
            )
            .col(
                ColumnDef::new(TeamAgentOperation::TargetAttemptId)
                    .text()
                    .null()
                    .extra(OPERATION_TARGET_ATTEMPT_ID_LENGTH_CHECK)
                    .check(optional_length(TeamAgentOperation::TargetAttemptId, 64)),
            )
            .col(
                ColumnDef::new(TeamAgentOperation::TargetGeneration)
                    .big_integer()
                    .not_null()
                    .extra(OPERATION_TARGET_GENERATION_CHECK)
                    .check(Expr::col(TeamAgentOperation::TargetGeneration).gt(0)),
            )
            .col(
                ColumnDef::new(TeamAgentOperation::ResultGeneration)
                    .big_integer()
                    .not_null()
                    .extra(OPERATION_RESULT_GENERATION_CHECK)
                    .check(
                        Expr::col(TeamAgentOperation::ResultGeneration)
                            .gte(Expr::col(TeamAgentOperation::TargetGeneration)),
                    ),
            )
            .col(
                ColumnDef::new(TeamAgentOperation::State)
                    .text()
                    .not_null()
                    .extra(OPERATION_STATE_CHECK)
                    .check(Expr::col(TeamAgentOperation::State).is_in([
                        "queued",
                        "leased",
                        "completed",
                        "abandoned",
                    ])),
            )
            .col(
                ColumnDef::new(TeamAgentOperation::LeaseOwner)
                    .text()
                    .null()
                    .extra(OPERATION_LEASE_OWNER_LENGTH_CHECK)
                    .check(optional_length(TeamAgentOperation::LeaseOwner, 64)),
            )
            .col(
                ColumnDef::new(TeamAgentOperation::LeaseExpiresAt)
                    .timestamp_with_time_zone()
                    .null(),
            )
            .col(
                ColumnDef::new(TeamAgentOperation::ErrorCode)
                    .text()
                    .null()
                    .extra(OPERATION_ERROR_CODE_CHECK)
                    .check(valid_error_code(TeamAgentOperation::ErrorCode)),
            )
            .col(
                ColumnDef::new(TeamAgentOperation::CreatedAt)
                    .timestamp_with_time_zone()
                    .not_null()
                    .extra(OPERATION_LEASE_SHAPE_CHECK)
                    .check(operation_lease_shape)
                    .extra(OPERATION_TARGET_SHAPE_CHECK)
                    .check(operation_target_shape),
            )
            .col(
                ColumnDef::new(TeamAgentOperation::ExpiresAt)
                    .timestamp_with_time_zone()
                    .not_null()
                    .extra(OPERATION_EXPIRES_AT_CHECK)
                    .check(
                        Expr::col(TeamAgentOperation::ExpiresAt)
                            .gt(Expr::col(TeamAgentOperation::CreatedAt)),
                    ),
            )
            .col(
                ColumnDef::new(TeamAgentOperation::UpdatedAt)
                    .timestamp_with_time_zone()
                    .not_null()
                    .extra(OPERATION_UPDATED_AT_CHECK)
                    .check(
                        Expr::col(TeamAgentOperation::UpdatedAt)
                            .gte(Expr::col(TeamAgentOperation::CreatedAt)),
                    ),
            )
            .foreign_key(
                ForeignKey::create()
                    .name("fk_team_agent_operation_connection_id")
                    .from(TeamAgentOperation::Table, TeamAgentOperation::ConnectionId)
                    .to(TeamAgentConnection::Table, TeamAgentConnection::Id)
                    .on_delete(ForeignKeyAction::Restrict)
                    .on_update(ForeignKeyAction::Restrict),
            )
            .to_owned(),
    ]
}

pub(crate) fn index_statements() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_team_agent_connection_org_user_provider")
            .table(TeamAgentConnection::Table)
            .col(TeamAgentConnection::OrgId)
            .col(TeamAgentConnection::UserId)
            .col(TeamAgentConnection::Provider)
            .unique()
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("idx_team_agent_attempt_connection_generation")
            .table(TeamAgentAttempt::Table)
            .col(TeamAgentAttempt::ConnectionId)
            .col(TeamAgentAttempt::Generation)
            .unique()
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("idx_team_agent_attempt_state_expires_at")
            .table(TeamAgentAttempt::Table)
            .col(TeamAgentAttempt::State)
            .col(TeamAgentAttempt::ExpiresAt)
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("idx_team_agent_operation_connection_kind_request_key")
            .table(TeamAgentOperation::Table)
            .col(TeamAgentOperation::ConnectionId)
            .col(TeamAgentOperation::Kind)
            .col(TeamAgentOperation::RequestKey)
            .unique()
            .if_not_exists()
            .to_owned(),
        Index::create()
            .name("idx_team_agent_operation_state_lease_expires_at")
            .table(TeamAgentOperation::Table)
            .col(TeamAgentOperation::State)
            .col(TeamAgentOperation::LeaseExpiresAt)
            .if_not_exists()
            .to_owned(),
    ]
}

#[derive(Copy, Clone, DeriveIden)]
enum TeamAgentConnection {
    Table,
    Id,
    OrgId,
    UserId,
    Provider,
    Generation,
    State,
    AccountLabel,
    CheckedAt,
    ErrorCode,
    StorageHandle,
    CreatedAt,
    UpdatedAt,
}

#[derive(Copy, Clone, DeriveIden)]
enum TeamAgentAttempt {
    Table,
    Id,
    ConnectionId,
    Generation,
    SessionId,
    State,
    CreatedAt,
    ExpiresAt,
    UpdatedAt,
}

#[derive(Copy, Clone, DeriveIden)]
enum TeamAgentOperation {
    Table,
    Id,
    ConnectionId,
    Kind,
    RequestKey,
    TargetAttemptId,
    TargetGeneration,
    ResultGeneration,
    State,
    LeaseOwner,
    LeaseExpiresAt,
    ErrorCode,
    CreatedAt,
    ExpiresAt,
    UpdatedAt,
}

#[derive(Copy, Clone, DeriveIden)]
enum Orgs {
    Table,
    Id,
}

#[derive(Copy, Clone, DeriveIden)]
enum Users {
    Table,
    Id,
}
