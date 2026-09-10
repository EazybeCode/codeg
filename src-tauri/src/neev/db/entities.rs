use sea_orm::FromQueryResult;

#[derive(FromQueryResult)]
pub(crate) struct Account {
    pub(crate) id: i32,
    pub(crate) auth_revision: i64,
    pub(crate) disabled: bool,
}
