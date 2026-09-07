use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "workspace_writer_lease")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub draft_id: i32,
    pub org_id: i32,
    pub creator_user_id: i32,
    #[sea_orm(unique)]
    pub owner_token: String,
    #[sea_orm(unique)]
    pub connection_id: Option<String>,
    pub expires_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
