use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "team_oauth_attempt")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false, column_type = "Text")]
    pub state_hash: String,
    #[sea_orm(column_type = "Text")]
    pub browser_hash: String,
    #[sea_orm(column_type = "Text")]
    pub encrypted_verifier: String,
    pub created_at: DateTimeUtc,
    pub expires_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
