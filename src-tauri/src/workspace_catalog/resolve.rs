use super::{CatalogError, RepositoryDefault, Scope, WorkspaceNode, WorkspaceNodeKind};

pub fn resolve_default(
    nodes: &[WorkspaceNode],
    scope: &Scope,
    node_id: i32,
) -> Result<Option<RepositoryDefault>, CatalogError> {
    let mut cursor = Some(node_id);
    let mut seen = std::collections::HashSet::new();
    let mut resolved = None;

    while let Some(id) = cursor {
        if !seen.insert(id) {
            return Err(CatalogError::InvalidInput("hierarchy cycle".into()));
        }
        let node = nodes
            .iter()
            .find(|node| node.id == id && node.org_id == scope.org_id)
            .ok_or(CatalogError::NotFound)?;
        validate_label(&node.name)?;
        validate_node_shape(&node.kind, node.parent_id, &node.repository_default)?;
        if resolved.is_none() {
            resolved = node.repository_default.clone();
        }
        cursor = node.parent_id;
    }

    Ok(resolved)
}

pub fn validate_node_shape(
    kind: &WorkspaceNodeKind,
    parent_id: Option<i32>,
    default: &Option<RepositoryDefault>,
) -> Result<(), CatalogError> {
    match kind {
        WorkspaceNodeKind::Project if parent_id.is_some() => {
            return Err(CatalogError::InvalidInput(
                "project cannot have a parent".into(),
            ));
        }
        WorkspaceNodeKind::Folder if parent_id.is_none() => {
            return Err(CatalogError::InvalidInput(
                "folder requires a parent".into(),
            ));
        }
        WorkspaceNodeKind::Folder if default.is_some() => {
            return Err(CatalogError::InvalidInput(
                "folder cannot own a repository default".into(),
            ));
        }
        WorkspaceNodeKind::WorkingDirectory if parent_id.is_none() => {
            return Err(CatalogError::InvalidInput(
                "working directory requires a parent".into(),
            ));
        }
        WorkspaceNodeKind::WorkingDirectory if default.is_none() => {
            return Err(CatalogError::InvalidInput(
                "working directory requires a repository default".into(),
            ));
        }
        _ => {}
    }

    if let Some(default) = default {
        validate_branch(&default.starting_branch)?;
    }

    Ok(())
}

pub fn validate_label(value: &str) -> Result<(), CatalogError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(CatalogError::InvalidInput("label cannot be empty".into()));
    }
    if trimmed.chars().count() > 200 {
        return Err(CatalogError::InvalidInput(
            "label cannot exceed 200 characters".into(),
        ));
    }
    Ok(())
}

pub fn validate_branch(value: &str) -> Result<(), CatalogError> {
    if value.is_empty() {
        return Err(CatalogError::InvalidInput("branch cannot be empty".into()));
    }
    if value.trim() != value {
        return Err(CatalogError::InvalidInput(
            "branch cannot have leading or trailing whitespace".into(),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(CatalogError::InvalidInput(
            "branch cannot contain control characters".into(),
        ));
    }
    Ok(())
}
