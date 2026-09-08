use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "team_session")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false, column_type = "Text")]
    pub token_hash: String,
    pub user_id: i32,
    pub org_id: i32,
    pub membership_id: i32,
    pub authorization_revision: i64,
    pub credential_generation: i64,
    #[sea_orm(column_type = "Text")]
    pub csrf_hash: String,
    pub created_at: DateTimeUtc,
    pub expires_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
