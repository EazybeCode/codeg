use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "workspace_chat_draft")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub org_id: i32,
    pub node_id: i32,
    pub creator_user_id: i32,
    pub repository_id: i32,
    #[sea_orm(column_type = "Text")]
    pub github_repository_id: String,
    #[sea_orm(column_type = "Text")]
    pub full_name: String,
    #[sea_orm(column_type = "Text")]
    pub starting_branch: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub title: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub request_id: String,
    #[sea_orm(column_type = "Text")]
    pub request_fingerprint: String,
    #[sea_orm(column_type = "Text")]
    pub execution_state: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
