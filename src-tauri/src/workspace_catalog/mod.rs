mod resolve;
mod repository_access;
mod service;
mod types;

pub use resolve::{resolve_default, validate_branch, validate_label, validate_node_shape};
pub use repository_access::{GitHubRepositoryAccess, RepositoryAccess, VerifiedRepository};
pub use service::{connect_repository, create_node, list_nodes, update_node};
pub use types::{
    CatalogError, CreateNodeInput, RepositoryDefault, Scope, UpdateNodeInput, WorkspaceNode,
    WorkspaceNodeKind, WorkspaceRepository,
};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod persistence_tests;
#[cfg(test)]
mod repository_tests;
