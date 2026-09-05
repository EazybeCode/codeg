use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// An organization = a GitHub org/account. Tenant boundary: every tenant-scoped
/// row is (or will be) keyed by `org_id`. Created/linked on GitHub login.
/// See `m20260905_000001_multitenancy` and decision 0006.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "orgs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// GitHub org/account numeric id — stable across org renames. Unique.
    pub github_org_id: i64,
    #[sea_orm(column_type = "Text")]
    pub login: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub name: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub avatar_url: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::user::Entity")]
    Users,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Users.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
