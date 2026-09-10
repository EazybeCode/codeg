use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "team_catalog_admission")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub resource_kind: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub resource_id: i32,
    pub org_id: i32,
    pub creator_user_id: i32,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
