use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "team_agent_operation")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false, column_type = "Text")]
    pub id: String,
    #[sea_orm(column_type = "Text")]
    pub connection_id: String,
    #[sea_orm(column_type = "Text")]
    pub kind: String,
    #[sea_orm(column_type = "Text")]
    pub request_key: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub target_attempt_id: Option<String>,
    pub target_generation: i64,
    pub result_generation: i64,
    #[sea_orm(column_type = "Text")]
    pub state: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<DateTimeUtc>,
    #[sea_orm(column_type = "Text", nullable)]
    pub error_code: Option<String>,
    pub created_at: DateTimeUtc,
    pub expires_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
