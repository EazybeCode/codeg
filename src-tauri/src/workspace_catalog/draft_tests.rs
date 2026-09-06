use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, Set,
};

use super::{
    create_draft, create_node, list_drafts, list_nodes, update_node, CatalogError,
    CreateDraftInput, CreateNodeInput, RepositoryAccess, RepositoryDefault, Scope,
    UpdateNodeInput, VerifiedRepository, WorkspaceNodeKind,
};
use crate::db::{
    entities::{conversation, org, user, workspace_chat_draft, workspace_repository},
    test_helpers::fresh_in_memory_db,
    AppDatabase,
};

#[derive(Clone)]
pub(crate) struct FakeRepositoryAccess {
    repositories: Arc<HashMap<(String, String), VerifiedRepository>>,
    calls: Arc<Mutex<Vec<(Scope, String, String)>>>,
    revoked: Arc<Mutex<bool>>,
    barrier: Option<Arc<tokio::sync::Barrier>>,
}

impl FakeRepositoryAccess {
    pub(crate) fn new(entries: &[(&str, &str, &str, &str)]) -> Self {
        let repositories = entries
            .iter()
            .map(|(full_name, branch, github_id, canonical_name)| {
                (
                    ((*full_name).into(), (*branch).into()),
                    VerifiedRepository {
                        github_repository_id: (*github_id).into(),
                        full_name: (*canonical_name).into(),
                        starting_branch: (*branch).into(),
                    },
                )
            })
            .collect();
        Self {
            repositories: Arc::new(repositories),
            calls: Arc::new(Mutex::new(Vec::new())),
            revoked: Arc::new(Mutex::new(false)),
            barrier: None,
        }
    }

    fn concurrent(mut self, parties: usize) -> Self {
        self.barrier = Some(Arc::new(tokio::sync::Barrier::new(parties)));
        self
    }

    fn revoke(&self) {
        *self.revoked.lock().unwrap() = true;
    }
}

#[async_trait]
impl RepositoryAccess for FakeRepositoryAccess {
    async fn verify(
        &self,
        scope: &Scope,
        full_name: &str,
        branch: &str,
    ) -> Result<VerifiedRepository, CatalogError> {
        self.calls
            .lock()
            .unwrap()
            .push((scope.clone(), full_name.into(), branch.into()));
        if let Some(barrier) = &self.barrier {
            barrier.wait().await;
        }
        if *self.revoked.lock().unwrap() {
            return Err(CatalogError::RepositoryUnavailable);
        }
        self.repositories
            .get(&(full_name.into(), branch.into()))
            .cloned()
            .ok_or(CatalogError::RepositoryUnavailable)
    }
}

struct DefaultChangingAccess {
    db: DatabaseConnection,
    scope: Scope,
    update: Mutex<Option<UpdateNodeInput>>,
    verified: VerifiedRepository,
}

#[async_trait]
impl RepositoryAccess for DefaultChangingAccess {
    async fn verify(
        &self,
        _scope: &Scope,
        _full_name: &str,
        _branch: &str,
    ) -> Result<VerifiedRepository, CatalogError> {
        let update = self.update.lock().unwrap().take();
        if let Some(update) = update {
            update_node(&self.db, &self.scope, update).await.unwrap();
        }
        Ok(self.verified.clone())
    }
}

async fn seed_scope(db: &AppDatabase, scope: &Scope, github_org_id: i64) {
    let now = Utc::now();
    org::ActiveModel {
        id: Set(scope.org_id),
        github_org_id: Set(github_org_id),
        login: Set(format!("org-{}", scope.org_id)),
        name: Set(None),
        avatar_url: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .unwrap();
    user::ActiveModel {
        id: Set(scope.user_id),
        github_user_id: Set(github_org_id * 10),
        login: Set(format!("user-{}", scope.user_id)),
        name: Set(None),
        email: Set(None),
        avatar_url: Set(None),
        org_id: Set(scope.org_id),
        role: Set("member".into()),
        gh_token_enc: Set(None),
        gh_refresh_token_enc: Set(None),
        gh_token_expires_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .unwrap();
}

pub(crate) async fn setup() -> (AppDatabase, Scope, Scope) {
    let db = fresh_in_memory_db().await;
    let scope = Scope {
        org_id: 1,
        user_id: 1,
    };
    let other = Scope {
        org_id: 2,
        user_id: 2,
    };
    seed_scope(&db, &scope, 100).await;
    seed_scope(&db, &other, 200).await;
    (db, scope, other)
}

pub(crate) async fn seed_repository(
    db: &AppDatabase,
    org_id: i32,
    github_repository_id: &str,
    full_name: &str,
) -> workspace_repository::Model {
    let now = Utc::now();
    workspace_repository::ActiveModel {
        id: NotSet,
        org_id: Set(org_id),
        github_repository_id: Set(github_repository_id.into()),
        full_name: Set(full_name.into()),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .unwrap()
}

fn node_input(
    parent_id: Option<i32>,
    kind: WorkspaceNodeKind,
    name: &str,
    repository_default: Option<RepositoryDefault>,
) -> CreateNodeInput {
    CreateNodeInput {
        parent_id,
        kind,
        name: name.into(),
        repository_default,
    }
}

fn draft_input(
    node_id: i32,
    repository_override: Option<RepositoryDefault>,
    title: Option<&str>,
    request_id: &str,
) -> CreateDraftInput {
    CreateDraftInput {
        node_id,
        repository_override,
        title: title.map(Into::into),
        request_id: request_id.into(),
    }
}

#[tokio::test]
async fn catalog_draft_requires_an_override_or_inherited_default() {
    let (db, scope, _) = setup().await;
    let project = create_node(
        &db.conn,
        &scope,
        node_input(None, WorkspaceNodeKind::Project, "Unbound", None),
    )
    .await
    .unwrap();
    let access = FakeRepositoryAccess::new(&[]);

    assert_eq!(
        create_draft(
            &db.conn,
            &scope,
            &access,
            draft_input(
                project.id,
                None,
                None,
                "0c58d910-3e1a-43c9-9462-785335828829",
            ),
        )
        .await,
        Err(CatalogError::RepositoryRequired)
    );
    assert!(access.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn catalog_draft_captures_inherited_repository_without_legacy_conversation() {
    let (db, scope, _) = setup().await;
    let connected = seed_repository(&db, scope.org_id, "123", "acme/docs").await;
    let project = create_node(
        &db.conn,
        &scope,
        node_input(
            None,
            WorkspaceNodeKind::Project,
            "Launch",
            Some(RepositoryDefault {
                repository_id: connected.id,
                starting_branch: "main".into(),
            }),
        ),
    )
    .await
    .unwrap();
    let nested = create_node(
        &db.conn,
        &scope,
        node_input(
            Some(project.id),
            WorkspaceNodeKind::Folder,
            "Plans",
            None,
        ),
    )
    .await
    .unwrap();
    let access = FakeRepositoryAccess::new(&[("acme/docs", "main", "123", "Acme/docs")]);

    let draft = create_draft(
        &db.conn,
        &scope,
        &access,
        CreateDraftInput {
            node_id: nested.id,
            repository_override: None,
            title: Some("Launch plan".into()),
            request_id: "5fdbf5b3-e44d-47b2-8b7b-5af5a2cd31b8".into(),
        },
    )
    .await
    .unwrap();

    assert_eq!(draft.repository_id, connected.id);
    assert_eq!(draft.github_repository_id, "123");
    assert_eq!(draft.full_name, "Acme/docs");
    assert_eq!(draft.starting_branch, "main");
    assert_eq!(draft.execution_state, "unprovisioned");
    assert_eq!(
        conversation::Entity::find()
            .count(&db.conn)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn catalog_draft_override_does_not_change_parent_or_folder_defaults() {
    let (db, scope, _) = setup().await;
    let repository = seed_repository(&db, scope.org_id, "123", "acme/docs").await;
    let project = create_node(
        &db.conn,
        &scope,
        node_input(None, WorkspaceNodeKind::Project, "Launch", None),
    )
    .await
    .unwrap();
    let folder = create_node(
        &db.conn,
        &scope,
        node_input(
            Some(project.id),
            WorkspaceNodeKind::Folder,
            "Plans",
            None,
        ),
    )
    .await
    .unwrap();
    let access = FakeRepositoryAccess::new(&[("acme/docs", "topic", "123", "acme/docs")]);

    create_draft(
        &db.conn,
        &scope,
        &access,
        draft_input(
            folder.id,
            Some(RepositoryDefault {
                repository_id: repository.id,
                starting_branch: "topic".into(),
            }),
            Some("  Override draft  "),
            "e0b5261a-e6d7-4078-9b91-639fd18c7084",
        ),
    )
    .await
    .unwrap();

    let nodes = list_nodes(&db.conn, &scope).await.unwrap();
    assert!(nodes
        .iter()
        .find(|node| node.id == project.id)
        .unwrap()
        .repository_default
        .is_none());
    assert!(nodes
        .iter()
        .find(|node| node.id == folder.id)
        .unwrap()
        .repository_default
        .is_none());
    assert_eq!(
        list_drafts(&db.conn, &scope, folder.id).await.unwrap()[0]
            .title
            .as_deref(),
        Some("Override draft")
    );
}

#[tokio::test]
async fn catalog_draft_retry_keeps_original_snapshot_after_default_and_parent_changes() {
    let (db, scope, _) = setup().await;
    let original = seed_repository(&db, scope.org_id, "123", "acme/original").await;
    let replacement = seed_repository(&db, scope.org_id, "456", "acme/replacement").await;
    let first_project = create_node(
        &db.conn,
        &scope,
        node_input(
            None,
            WorkspaceNodeKind::Project,
            "First",
            Some(RepositoryDefault {
                repository_id: original.id,
                starting_branch: "main".into(),
            }),
        ),
    )
    .await
    .unwrap();
    let second_project = create_node(
        &db.conn,
        &scope,
        node_input(
            None,
            WorkspaceNodeKind::Project,
            "Second",
            Some(RepositoryDefault {
                repository_id: replacement.id,
                starting_branch: "next".into(),
            }),
        ),
    )
    .await
    .unwrap();
    let folder = create_node(
        &db.conn,
        &scope,
        node_input(
            Some(first_project.id),
            WorkspaceNodeKind::Folder,
            "Plans",
            None,
        ),
    )
    .await
    .unwrap();
    let access = FakeRepositoryAccess::new(&[
        ("acme/original", "main", "123", "acme/original"),
        ("acme/replacement", "next", "456", "acme/replacement"),
    ]);
    let input = draft_input(
        folder.id,
        None,
        Some("Stable"),
        "d27b6266-822f-4a21-b383-bb0c9a1c51a8",
    );
    let first = create_draft(&db.conn, &scope, &access, input.clone())
        .await
        .unwrap();

    update_node(
        &db.conn,
        &scope,
        UpdateNodeInput {
            node_id: first_project.id,
            expected_revision: first_project.revision,
            parent_id: None,
            name: first_project.name.clone(),
            repository_default: Some(RepositoryDefault {
                repository_id: replacement.id,
                starting_branch: "next".into(),
            }),
        },
    )
    .await
    .unwrap();
    update_node(
        &db.conn,
        &scope,
        UpdateNodeInput {
            node_id: folder.id,
            expected_revision: folder.revision,
            parent_id: Some(second_project.id),
            name: folder.name.clone(),
            repository_default: None,
        },
    )
    .await
    .unwrap();

    let retried = create_draft(&db.conn, &scope, &access, input).await.unwrap();
    assert_eq!(retried, first);
    assert_eq!(retried.repository_id, original.id);
    assert_eq!(retried.starting_branch, "main");
    let calls = access.calls.lock().unwrap();
    assert_eq!(calls.last().unwrap().1, "acme/original");
    assert_eq!(calls.last().unwrap().2, "main");
}

#[tokio::test]
async fn catalog_draft_rejects_cross_org_override_and_verified_identity_mismatch() {
    let (db, scope, other) = setup().await;
    let foreign = seed_repository(&db, other.org_id, "999", "other/private").await;
    let local = seed_repository(&db, scope.org_id, "123", "acme/docs").await;
    let project = create_node(
        &db.conn,
        &scope,
        node_input(None, WorkspaceNodeKind::Project, "Launch", None),
    )
    .await
    .unwrap();

    let cross_org = FakeRepositoryAccess::new(&[("other/private", "main", "999", "other/private")]);
    assert_eq!(
        create_draft(
            &db.conn,
            &scope,
            &cross_org,
            draft_input(
                project.id,
                Some(RepositoryDefault {
                    repository_id: foreign.id,
                    starting_branch: "main".into(),
                }),
                None,
                "bfe47769-5cb1-4a6e-bf09-a43a7684263b",
            ),
        )
        .await,
        Err(CatalogError::NotFound)
    );

    let mismatch = FakeRepositoryAccess::new(&[("acme/docs", "main", "456", "acme/docs")]);
    assert_eq!(
        create_draft(
            &db.conn,
            &scope,
            &mismatch,
            draft_input(
                project.id,
                Some(RepositoryDefault {
                    repository_id: local.id,
                    starting_branch: "main".into(),
                }),
                None,
                "42a7c57c-bab5-40bf-a115-97b053222b92",
            ),
        )
        .await,
        Err(CatalogError::RepositoryUnavailable)
    );
}

#[tokio::test]
async fn catalog_draft_equal_concurrent_retries_create_one_row_and_conflicts_reject() {
    let (db, scope, _) = setup().await;
    let repository = seed_repository(&db, scope.org_id, "123", "acme/docs").await;
    let project = create_node(
        &db.conn,
        &scope,
        node_input(
            None,
            WorkspaceNodeKind::Project,
            "Launch",
            Some(RepositoryDefault {
                repository_id: repository.id,
                starting_branch: "main".into(),
            }),
        ),
    )
    .await
    .unwrap();
    let access = FakeRepositoryAccess::new(&[("acme/docs", "main", "123", "acme/docs")])
        .concurrent(2);
    let input = draft_input(
        project.id,
        None,
        Some("One"),
        "a31e7b75-0c18-47ea-a93e-d01cf7ad91fd",
    );

    let (left, right) = tokio::join!(
        create_draft(&db.conn, &scope, &access, input.clone()),
        create_draft(&db.conn, &scope, &access, input.clone()),
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert_eq!(left, right);
    assert_eq!(
        workspace_chat_draft::Entity::find()
            .count(&db.conn)
            .await
            .unwrap(),
        1
    );

    assert_eq!(
        create_draft(
            &db.conn,
            &scope,
            &access,
            CreateDraftInput {
                title: Some("Different".into()),
                ..input
            },
        )
        .await,
        Err(CatalogError::Conflict)
    );
}

#[tokio::test]
async fn catalog_draft_concurrent_different_bodies_allow_exactly_one_insert() {
    let (db, scope, _) = setup().await;
    let repository = seed_repository(&db, scope.org_id, "123", "acme/docs").await;
    let project = create_node(
        &db.conn,
        &scope,
        node_input(
            None,
            WorkspaceNodeKind::Project,
            "Launch",
            Some(RepositoryDefault {
                repository_id: repository.id,
                starting_branch: "main".into(),
            }),
        ),
    )
    .await
    .unwrap();
    let access = FakeRepositoryAccess::new(&[("acme/docs", "main", "123", "acme/docs")])
        .concurrent(2);
    let request_id = "60d6822e-f6f6-4a24-85cf-9ac71b719159";

    let (left, right) = tokio::join!(
        create_draft(
            &db.conn,
            &scope,
            &access,
            draft_input(project.id, None, Some("Left"), request_id),
        ),
        create_draft(
            &db.conn,
            &scope,
            &access,
            draft_input(project.id, None, Some("Right"), request_id),
        ),
    );

    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    assert!(matches!(left, Err(CatalogError::Conflict))
        || matches!(right, Err(CatalogError::Conflict)));
    assert_eq!(
        workspace_chat_draft::Entity::find()
            .count(&db.conn)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn catalog_draft_inherited_default_change_during_verification_conflicts() {
    let (db, scope, _) = setup().await;
    let original = seed_repository(&db, scope.org_id, "123", "acme/original").await;
    let replacement = seed_repository(&db, scope.org_id, "456", "acme/replacement").await;
    let project = create_node(
        &db.conn,
        &scope,
        node_input(
            None,
            WorkspaceNodeKind::Project,
            "Launch",
            Some(RepositoryDefault {
                repository_id: original.id,
                starting_branch: "main".into(),
            }),
        ),
    )
    .await
    .unwrap();
    let access = DefaultChangingAccess {
        db: db.conn.clone(),
        scope: scope.clone(),
        update: Mutex::new(Some(UpdateNodeInput {
            node_id: project.id,
            expected_revision: project.revision,
            parent_id: None,
            name: project.name.clone(),
            repository_default: Some(RepositoryDefault {
                repository_id: replacement.id,
                starting_branch: "next".into(),
            }),
        })),
        verified: VerifiedRepository {
            github_repository_id: "123".into(),
            full_name: "acme/original".into(),
            starting_branch: "main".into(),
        },
    };

    assert_eq!(
        create_draft(
            &db.conn,
            &scope,
            &access,
            draft_input(
                project.id,
                None,
                None,
                "e5eff8b3-0dc3-497c-934e-ec6c8bf3c782",
            ),
        )
        .await,
        Err(CatalogError::Conflict)
    );
    assert_eq!(
        workspace_chat_draft::Entity::find()
            .count(&db.conn)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn catalog_draft_retry_reverifies_access_and_list_is_creator_scoped() {
    let (db, scope, _) = setup().await;
    let second_user = Scope {
        org_id: scope.org_id,
        user_id: 3,
    };
    let now = Utc::now();
    user::ActiveModel {
        id: Set(second_user.user_id),
        github_user_id: Set(103),
        login: Set("bob".into()),
        name: Set(None),
        email: Set(None),
        avatar_url: Set(None),
        org_id: Set(scope.org_id),
        role: Set("member".into()),
        gh_token_enc: Set(None),
        gh_refresh_token_enc: Set(None),
        gh_token_expires_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .unwrap();
    let repository = seed_repository(&db, scope.org_id, "123", "acme/docs").await;
    let project = create_node(
        &db.conn,
        &scope,
        node_input(
            None,
            WorkspaceNodeKind::Project,
            "Launch",
            Some(RepositoryDefault {
                repository_id: repository.id,
                starting_branch: "main".into(),
            }),
        ),
    )
    .await
    .unwrap();
    let access = FakeRepositoryAccess::new(&[("acme/docs", "main", "123", "acme/docs")]);
    let input = draft_input(
        project.id,
        None,
        None,
        "90f5e1b3-e04e-4ea6-b707-d0f9dc475e01",
    );
    create_draft(&db.conn, &scope, &access, input.clone())
        .await
        .unwrap();
    create_draft(
        &db.conn,
        &second_user,
        &access,
        draft_input(
            project.id,
            None,
            None,
            "3de441ca-481c-4401-9413-a5e59b27ba54",
        ),
    )
    .await
    .unwrap();

    assert_eq!(
        list_drafts(&db.conn, &scope, project.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        list_drafts(&db.conn, &second_user, project.id)
            .await
            .unwrap()
            .len(),
        1
    );

    access.revoke();
    assert_eq!(
        create_draft(&db.conn, &scope, &access, input).await,
        Err(CatalogError::RepositoryUnavailable)
    );
    assert_eq!(
        workspace_chat_draft::Entity::find()
            .filter(workspace_chat_draft::Column::OrgId.eq(scope.org_id))
            .count(&db.conn)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn catalog_draft_validates_uuid_and_normalizes_optional_title() {
    let (db, scope, _) = setup().await;
    let repository = seed_repository(&db, scope.org_id, "123", "acme/docs").await;
    let project = create_node(
        &db.conn,
        &scope,
        node_input(
            None,
            WorkspaceNodeKind::Project,
            "Launch",
            Some(RepositoryDefault {
                repository_id: repository.id,
                starting_branch: "main".into(),
            }),
        ),
    )
    .await
    .unwrap();
    let access = FakeRepositoryAccess::new(&[("acme/docs", "main", "123", "acme/docs")]);

    assert!(matches!(
        create_draft(
            &db.conn,
            &scope,
            &access,
            draft_input(project.id, None, None, "not-a-uuid"),
        )
        .await,
        Err(CatalogError::InvalidInput(_))
    ));
    assert!(matches!(
        create_draft(
            &db.conn,
            &scope,
            &access,
            draft_input(
                project.id,
                None,
                Some(&"界".repeat(201)),
                "beeeed42-c373-4319-ac71-21179e9190f9",
            ),
        )
        .await,
        Err(CatalogError::InvalidInput(_))
    ));
    let blank = create_draft(
        &db.conn,
        &scope,
        &access,
        draft_input(
            project.id,
            None,
            Some("  \n  "),
            "2ca1fa75-f3d3-4a8a-b045-da83765c1764",
        ),
    )
    .await
    .unwrap();
    assert_eq!(blank.title, None);
}

#[test]
fn catalog_draft_request_uses_camel_case_and_response_uses_snake_case() {
    let input: CreateDraftInput = serde_json::from_value(serde_json::json!({
        "nodeId": 7,
        "repositoryOverride": {
            "repository_id": 9,
            "starting_branch": "main"
        },
        "title": "Launch",
        "requestId": "5fdbf5b3-e44d-47b2-8b7b-5af5a2cd31b8"
    }))
    .unwrap();
    assert_eq!(input.node_id, 7);
    assert!(serde_json::from_value::<CreateDraftInput>(serde_json::json!({
        "nodeId": 7,
        "repositoryOverride": null,
        "title": null,
        "requestId": "5fdbf5b3-e44d-47b2-8b7b-5af5a2cd31b8",
        "unexpected": true
    }))
    .is_err());

    let response = serde_json::to_value(super::WorkspaceChatDraft {
        id: 1,
        org_id: 2,
        node_id: 7,
        creator_user_id: 3,
        repository_id: 9,
        github_repository_id: "123".into(),
        full_name: "acme/docs".into(),
        starting_branch: "main".into(),
        title: Some("Launch".into()),
        execution_state: "unprovisioned".into(),
    })
    .unwrap();
    assert_eq!(response["github_repository_id"], "123");
    assert!(response.get("githubRepositoryId").is_none());
}
