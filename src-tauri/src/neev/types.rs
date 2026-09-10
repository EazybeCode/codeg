pub(crate) type UserId = i32;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Provider {
    Google,
    Github,
    Email,
}
pub(crate) struct VerifiedIdentity {
    pub(crate) provider: Provider,
    pub(crate) issuer: String,
    pub(crate) subject: String,
    pub(crate) verified_email: Option<String>,
    pub(crate) display_name: String,
    pub(crate) avatar_url: Option<String>,
}
pub(crate) enum SignInIntent {
    Login,
    Link {
        user_id: UserId,
        session_id: String,
        auth_revision: i64,
    },
}
