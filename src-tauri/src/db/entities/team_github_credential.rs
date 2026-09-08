use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "team_github_credential")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: i32,
    pub github_user_id: i64,
    pub generation: i64,
    #[sea_orm(column_type = "Text")]
    pub state: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub encrypted_payload: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub refresh_owner: Option<String>,
    pub refresh_expires_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
