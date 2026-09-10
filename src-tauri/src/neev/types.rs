pub(crate) type UserId = i32;
pub(crate) type OrgId = i32;
pub(crate) type MemberId = i32;
pub(crate) type NodeId = i32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Admin,
    Member,
}

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

pub(crate) struct IdentityContext {
    pub(crate) user_id: UserId,
    pub(crate) session_id: String,
    pub(crate) auth_revision: i64,
    pub(crate) authenticated_at: i64,
}

pub(crate) struct OrgContext {
    pub(crate) identity: IdentityContext,
    pub(crate) org_id: OrgId,
    pub(crate) member_id: MemberId,
    pub(crate) member_revision: i64,
    pub(crate) org_revision: i64,
    pub(crate) role: Role,
}

pub(crate) struct SessionSecrets {
    pub(crate) session: String,
    pub(crate) csrf: String,
    pub(crate) expires_at: i64,
}

pub(crate) struct Organization {
    pub(crate) id: OrgId,
    pub(crate) name: String,
    pub(crate) owner_user_id: UserId,
    pub(crate) revision: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeKind {
    Project,
    Folder,
    WorkingDirectory,
}

pub(crate) struct NodeInput {
    pub(crate) parent_id: Option<NodeId>,
    pub(crate) kind: NodeKind,
    pub(crate) name: String,
}

pub(crate) struct Node {
    pub(crate) id: NodeId,
    pub(crate) org_id: OrgId,
    pub(crate) parent_id: Option<NodeId>,
    pub(crate) kind: NodeKind,
    pub(crate) name: String,
    pub(crate) revision: i64,
}
