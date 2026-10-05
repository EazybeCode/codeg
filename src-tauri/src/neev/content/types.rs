use crate::neev::error::AuthError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResourceKind {
    Chat,
    Document,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Visibility {
    Private,
    /// The creator, plus member/team grants on it and on its folders.
    Shared,
    Organization,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resource {
    pub id: String,
    pub org_id: i32,
    pub node_id: i32,
    pub kind: ResourceKind,
    pub creator_id: i32,
    pub title: String,
    pub visibility: Visibility,
    pub origin_chat_id: Option<String>,
    pub auth_revision: i64,
    pub content_revision: i64,
    pub archived: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResourceInput {
    pub node_id: i32,
    pub kind: ResourceKind,
    pub title: String,
    pub origin_chat_id: Option<String>,
    pub request_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResourcePatch {
    pub expected_auth_revision: i64,
    pub title: String,
    pub node_id: i32,
    pub visibility: Visibility,
    pub archived: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Message {
    pub id: String,
    pub sequence: i64,
    pub author_id: i32,
    pub author_name: String,
    pub body: String,
    pub created_at: i64,
    /// Who wrote the text. `Agent` means a provider produced it in answer to an
    /// explicit team invocation; `author_id` still names the human who invoked
    /// it, so the message stays attributable without impersonating them.
    pub author_kind: AuthorKind,
    /// The provider that produced an agent message (`claude`/`codex`); `None`
    /// for a human one. The schema pairs these two exactly.
    pub agent_label: Option<String>,
}

/// Distinguishes an agent-authored team message from a human one (spec §5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthorKind {
    Human,
    Agent,
}

impl AuthorKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "human" => Some(Self::Human),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Revision {
    pub format: DocumentFormat,
    pub format_version: u8,
    pub revision: i64,
    pub author_id: i32,
    pub author_name: String,
    pub byte_length: i64,
    pub sha256: String,
    pub created_at: i64,
    pub parent_revision: Option<i64>,
    pub restored_from: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SaveInput {
    pub request_id: String,
    pub expected_content_revision: i64,
    pub expected_auth_revision: i64,
    pub markdown: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DocumentFormat {
    Markdown,
    Blocks,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RichSaveInput {
    pub request_id: String,
    pub expected_content_revision: i64,
    pub expected_auth_revision: i64,
    pub document: super::rich::RichDocument,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Page<T> {
    pub items: Vec<T>,
    pub next_after: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ContentError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error("content not found")]
    NotFound,
    #[error("invalid content input")]
    Invalid,
    #[error("content changed; retry")]
    Conflict,
    #[error("content quota exceeded")]
    Quota,
    #[error("content is archived")]
    Archived,
    #[error("content storage unavailable")]
    Storage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_content_contracts_have_the_required_shape() {
        let resource = Resource {
            id: "resource".into(),
            org_id: 1,
            node_id: 2,
            kind: ResourceKind::Document,
            creator_id: 3,
            title: "Document".into(),
            visibility: Visibility::Organization,
            origin_chat_id: Some("chat".into()),
            auth_revision: 4,
            content_revision: 5,
            archived: false,
            created_at: 6,
            updated_at: 7,
        };
        let input = ResourceInput {
            node_id: 2,
            kind: ResourceKind::Chat,
            title: "Chat".into(),
            origin_chat_id: None,
            request_id: "request".into(),
        };
        let patch = ResourcePatch {
            expected_auth_revision: 4,
            title: "Renamed".into(),
            node_id: 8,
            visibility: Visibility::Private,
            archived: true,
        };
        let message = Message {
            id: "message".into(),
            sequence: 9,
            author_id: 3,
            author_name: "Author".into(),
            body: "Hello".into(),
            created_at: 10,
            author_kind: AuthorKind::Human,
            agent_label: None,
        };
        let revision = Revision {
            format: DocumentFormat::Markdown,
            format_version: 1,
            revision: 11,
            author_id: 3,
            author_name: "Author".into(),
            byte_length: 12,
            sha256: "hash".into(),
            created_at: 13,
            parent_revision: Some(10),
            restored_from: Some(9),
        };
        let save = SaveInput {
            request_id: "save".into(),
            expected_content_revision: 5,
            expected_auth_revision: 4,
            markdown: "# Document".into(),
        };
        let page = Page {
            items: vec![message.clone()],
            next_after: Some("cursor".into()),
        };

        assert_eq!(resource.id, "resource");
        assert_eq!(resource.org_id, 1);
        assert_eq!(resource.node_id, 2);
        assert_eq!(resource.kind, ResourceKind::Document);
        assert_eq!(resource.creator_id, 3);
        assert_eq!(resource.title, "Document");
        assert_eq!(resource.visibility, Visibility::Organization);
        assert_eq!(resource.origin_chat_id.as_deref(), Some("chat"));
        assert_eq!(resource.auth_revision, 4);
        assert_eq!(resource.content_revision, 5);
        assert!(!resource.archived);
        assert_eq!(resource.created_at, 6);
        assert_eq!(resource.updated_at, 7);
        assert_eq!(input.node_id, 2);
        assert_eq!(input.kind, ResourceKind::Chat);
        assert_eq!(input.title, "Chat");
        assert_eq!(input.origin_chat_id, None);
        assert_eq!(input.request_id, "request");
        assert_eq!(patch.expected_auth_revision, 4);
        assert_eq!(patch.title, "Renamed");
        assert_eq!(patch.node_id, 8);
        assert_eq!(patch.visibility, Visibility::Private);
        assert!(patch.archived);
        assert_eq!(message.id, "message");
        assert_eq!(message.sequence, 9);
        assert_eq!(message.author_id, 3);
        assert_eq!(message.author_name, "Author");
        assert_eq!(message.body, "Hello");
        assert_eq!(message.created_at, 10);
        assert_eq!(revision.revision, 11);
        assert_eq!(revision.author_id, 3);
        assert_eq!(revision.author_name, "Author");
        assert_eq!(revision.byte_length, 12);
        assert_eq!(revision.sha256, "hash");
        assert_eq!(revision.created_at, 13);
        assert_eq!(revision.parent_revision, Some(10));
        assert_eq!(revision.restored_from, Some(9));
        assert_eq!(save.request_id, "save");
        assert_eq!(save.expected_content_revision, 5);
        assert_eq!(save.expected_auth_revision, 4);
        assert_eq!(save.markdown, "# Document");
        assert_eq!(page.items, vec![message]);
        assert_eq!(page.next_after.as_deref(), Some("cursor"));

        let _ = ContentError::Auth(AuthError::Unauthorized);
        let _ = ContentError::NotFound;
        let _ = ContentError::Invalid;
        let _ = ContentError::Conflict;
        let _ = ContentError::Quota;
        let _ = ContentError::Archived;
        let _ = ContentError::Storage;
    }
}
