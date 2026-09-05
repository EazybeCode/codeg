use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// A user = a GitHub account, mapped to exactly one org (for now). GitHub
/// access/refresh tokens are stored ENCRYPTED (`gh_token_enc` /
/// `gh_refresh_token_enc`) and used to act on git as the user.
/// See `m20260905_000001_multitenancy` and decision 0006.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// GitHub user numeric id — stable across login renames. Unique.
    pub github_user_id: i64,
    #[sea_orm(column_type = "Text")]
    pub login: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub name: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub email: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub avatar_url: Option<String>,
    /// Tenant this user belongs to. Indexed; app-enforced (no DB FK).
    pub org_id: i32,
    /// `owner` | `member`.
    #[sea_orm(column_type = "Text")]
    pub role: String,
    /// Encrypted GitHub tokens — never expose raw over the API.
    #[sea_orm(column_type = "Text", nullable)]
    #[serde(skip_serializing)]
    pub gh_token_enc: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    #[serde(skip_serializing)]
    pub gh_refresh_token_enc: Option<String>,
    pub gh_token_expires_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::org::Entity",
        from = "Column::OrgId",
        to = "super::org::Column::Id"
    )]
    Org,
    #[sea_orm(has_many = "super::session::Entity")]
    Sessions,
}

impl Related<super::org::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Org.def()
    }
}

impl Related<super::session::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Sessions.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
