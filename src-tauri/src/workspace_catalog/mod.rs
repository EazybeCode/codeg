mod resolve;
mod types;

pub use resolve::{resolve_default, validate_branch, validate_label, validate_node_shape};
pub use types::{
    CatalogError, RepositoryDefault, Scope, WorkspaceNode, WorkspaceNodeKind,
};

#[cfg(test)]
mod tests;
