use sea_orm::FromQueryResult;

#[derive(FromQueryResult)]
pub(crate) struct Account {
    pub(crate) id: i32,
    pub(crate) auth_revision: i64,
    pub(crate) disabled: bool,
}

#[derive(FromQueryResult)]
pub(crate) struct Session {
    pub(crate) id: String,
    pub(crate) user_id: i32,
    pub(crate) account_revision: i64,
    pub(crate) org_id: Option<i32>,
    pub(crate) member_id: Option<i32>,
    pub(crate) member_revision: Option<i64>,
    pub(crate) org_revision: Option<i64>,
    pub(crate) authenticated_at: i64,
    pub(crate) expires_at: i64,
    pub(crate) revoked_at: Option<i64>,
}

#[derive(FromQueryResult)]
pub(crate) struct OrganizationRow {
    pub(crate) id: i32,
    pub(crate) revision: i64,
    pub(crate) disabled: bool,
}

#[derive(FromQueryResult)]
pub(crate) struct Membership {
    pub(crate) id: i32,
    pub(crate) revision: i64,
    pub(crate) active: bool,
    pub(crate) role: String,
}
