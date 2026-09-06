use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "workspace_chat_execution")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub draft_id: i32,
    pub org_id: i32,
    pub agent_type: String,
    pub state: String,
    #[sea_orm(unique)]
    pub allocation_id: String,
    pub starting_commit: String,
    pub branch_name: String,
    pub worktree_relpath: String,
    #[sea_orm(unique)]
    pub legacy_folder_id: Option<i32>,
    #[sea_orm(unique)]
    pub legacy_conversation_id: Option<i32>,
    pub revision: i64,
    pub started_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub last_error_code: Option<String>,
    pub provisioning_owner_token: Option<String>,
    pub provisioning_lease_expires_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
