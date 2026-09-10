mod draft_service;
pub(crate) mod request_fingerprint;
mod resolve;
mod repository_access;
mod service;
mod types;

pub use draft_service::{create_draft, list_drafts};
pub(crate) use draft_service::to_workspace_chat_draft;
pub(crate) use service::{begin_scoped_write, verify_repository, verify_scope, verified_repository_names};
pub use resolve::{resolve_default, validate_branch, validate_label, validate_node_shape};
pub use repository_access::{GitHubRepositoryAccess, RepositoryAccess, VerifiedRepository};
pub(crate) use repository_access::validate_repository_full_name;
pub use service::{connect_repository, create_node, list_nodes, list_repositories, update_node};
pub(crate) use service::{
    create_node_with_repository_identity, update_node_with_repository_identity,
    RepositoryIdentityExpectation,
};
pub use types::{
    CatalogError, CreateDraftInput, CreateNodeInput, RepositoryDefault, Scope, UpdateNodeInput,
    WorkspaceChatDraft, WorkspaceNode, WorkspaceNodeKind, WorkspaceRepository,
};

#[cfg(test)]
pub(crate) mod draft_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod persistence_tests;
#[cfg(test)]
mod repository_tests;
