use super::{
    resolve_default, validate_branch, validate_label, validate_node_shape, CatalogError,
    RepositoryDefault, Scope, WorkspaceNode, WorkspaceNodeKind,
};

fn node(
    id: i32,
    parent_id: Option<i32>,
    kind: WorkspaceNodeKind,
    repository_default: Option<RepositoryDefault>,
) -> WorkspaceNode {
    WorkspaceNode {
        id,
        org_id: 1,
        parent_id,
        kind,
        name: format!("node-{id}"),
        repository_default,
        revision: 0,
    }
}

#[test]
fn catalog_plain_folder_inherits_without_becoming_a_checkout() {
    let binding = RepositoryDefault {
        repository_id: 7,
        starting_branch: "main".into(),
    };
    let nodes = vec![
        node(
            1,
            None,
            WorkspaceNodeKind::Project,
            Some(binding.clone()),
        ),
        node(2, Some(1), WorkspaceNodeKind::Folder, None),
    ];
    let scope = Scope {
        org_id: 1,
        user_id: 2,
    };
    assert_eq!(resolve_default(&nodes, &scope, 2).unwrap(), Some(binding));
    assert!(nodes[1].repository_default.is_none());
}

#[test]
fn workspace_catalog_nearest_default_wins() {
    let project_binding = RepositoryDefault {
        repository_id: 7,
        starting_branch: "main".into(),
    };
    let working_directory_binding = RepositoryDefault {
        repository_id: 9,
        starting_branch: "feature/catalog".into(),
    };
    let nodes = vec![
        node(
            1,
            None,
            WorkspaceNodeKind::Project,
            Some(project_binding),
        ),
        node(2, Some(1), WorkspaceNodeKind::Folder, None),
        node(
            3,
            Some(2),
            WorkspaceNodeKind::WorkingDirectory,
            Some(working_directory_binding.clone()),
        ),
    ];

    assert_eq!(
        resolve_default(
            &nodes,
            &Scope {
                org_id: 1,
                user_id: 2,
            },
            3,
        )
        .unwrap(),
        Some(working_directory_binding)
    );
}

#[test]
fn workspace_catalog_cross_organization_id_is_not_found() {
    let mut foreign = node(1, None, WorkspaceNodeKind::Project, None);
    foreign.org_id = 2;

    assert_eq!(
        resolve_default(
            &[foreign],
            &Scope {
                org_id: 1,
                user_id: 2,
            },
            1,
        ),
        Err(CatalogError::NotFound)
    );
}

#[test]
fn workspace_catalog_missing_ancestor_is_not_found() {
    let nodes = vec![node(2, Some(99), WorkspaceNodeKind::Folder, None)];

    assert_eq!(
        resolve_default(
            &nodes,
            &Scope {
                org_id: 1,
                user_id: 2,
            },
            2,
        ),
        Err(CatalogError::NotFound)
    );
}

#[test]
fn workspace_catalog_cycle_is_invalid() {
    let nodes = vec![
        node(1, Some(2), WorkspaceNodeKind::Folder, None),
        node(2, Some(1), WorkspaceNodeKind::Folder, None),
    ];

    assert_eq!(
        resolve_default(
            &nodes,
            &Scope {
                org_id: 1,
                user_id: 2,
            },
            1,
        ),
        Err(CatalogError::InvalidInput("hierarchy cycle".into()))
    );
}

#[test]
fn workspace_catalog_validates_ancestors_after_finding_default() {
    let binding = RepositoryDefault {
        repository_id: 9,
        starting_branch: "main".into(),
    };
    let nodes = vec![
        node(1, None, WorkspaceNodeKind::Project, None),
        node(
            2,
            Some(1),
            WorkspaceNodeKind::Folder,
            Some(binding.clone()),
        ),
        node(
            3,
            Some(2),
            WorkspaceNodeKind::WorkingDirectory,
            Some(binding),
        ),
    ];

    assert!(matches!(
        resolve_default(
            &nodes,
            &Scope {
                org_id: 1,
                user_id: 2,
            },
            3,
        ),
        Err(CatalogError::InvalidInput(_))
    ));
}

#[test]
fn workspace_catalog_node_shapes_enforce_hierarchy_roles() {
    let binding = Some(RepositoryDefault {
        repository_id: 7,
        starting_branch: "main".into(),
    });

    assert!(validate_node_shape(&WorkspaceNodeKind::Project, None, &None).is_ok());
    assert!(validate_node_shape(&WorkspaceNodeKind::Project, None, &binding).is_ok());
    assert!(validate_node_shape(&WorkspaceNodeKind::Folder, Some(1), &None).is_ok());
    assert!(validate_node_shape(
        &WorkspaceNodeKind::WorkingDirectory,
        Some(1),
        &binding
    )
    .is_ok());

    for result in [
        validate_node_shape(&WorkspaceNodeKind::Project, Some(1), &None),
        validate_node_shape(&WorkspaceNodeKind::Folder, None, &None),
        validate_node_shape(&WorkspaceNodeKind::Folder, Some(1), &binding),
        validate_node_shape(&WorkspaceNodeKind::WorkingDirectory, None, &binding),
        validate_node_shape(&WorkspaceNodeKind::WorkingDirectory, Some(1), &None),
    ] {
        assert!(matches!(result, Err(CatalogError::InvalidInput(_))));
    }
}

#[test]
fn workspace_catalog_label_validation_uses_trimmed_unicode_length() {
    assert!(validate_label("  valid label  ").is_ok());
    assert!(validate_label(&"界".repeat(200)).is_ok());
    assert!(validate_label(&format!("  {}  ", "界".repeat(200))).is_ok());
    assert!(matches!(
        validate_label(" \n\t "),
        Err(CatalogError::InvalidInput(_))
    ));
    assert!(matches!(
        validate_label(&format!(" {} ", "界".repeat(201))),
        Err(CatalogError::InvalidInput(_))
    ));
}

#[test]
fn workspace_catalog_branch_validation_rejects_whitespace_and_controls() {
    assert!(validate_branch("feature/catalog").is_ok());
    assert!(validate_branch(&"a".repeat(201)).is_ok());

    for branch in ["", " main", "main ", "feature\nname", "feature\u{7f}name"] {
        assert!(matches!(
            validate_branch(branch),
            Err(CatalogError::InvalidInput(_))
        ));
    }
}

#[test]
fn workspace_catalog_resolver_validates_node_names_and_default_branches() {
    let scope = Scope {
        org_id: 1,
        user_id: 2,
    };
    let mut invalid_name = node(1, None, WorkspaceNodeKind::Project, None);
    invalid_name.name = "   ".into();
    assert!(matches!(
        resolve_default(&[invalid_name], &scope, 1),
        Err(CatalogError::InvalidInput(_))
    ));

    let invalid_branch = node(
        1,
        None,
        WorkspaceNodeKind::Project,
        Some(RepositoryDefault {
            repository_id: 7,
            starting_branch: " main".into(),
        }),
    );
    assert!(matches!(
        resolve_default(&[invalid_branch], &scope, 1),
        Err(CatalogError::InvalidInput(_))
    ));
}

#[test]
fn workspace_catalog_kind_serializes_as_snake_case() {
    assert_eq!(
        serde_json::to_string(&WorkspaceNodeKind::WorkingDirectory).unwrap(),
        "\"working_directory\""
    );
}

#[test]
fn workspace_catalog_local_child_with_foreign_ancestor_is_not_found() {
    let mut foreign_parent = node(1, None, WorkspaceNodeKind::Project, None);
    foreign_parent.org_id = 2;
    let local_child = node(2, Some(1), WorkspaceNodeKind::Folder, None);

    assert_eq!(
        resolve_default(
            &[foreign_parent, local_child],
            &Scope {
                org_id: 1,
                user_id: 1,
            },
            2,
        ),
        Err(CatalogError::NotFound)
    );
}
