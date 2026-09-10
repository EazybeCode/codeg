use std::sync::{atomic::{AtomicUsize, Ordering}, Arc};

use async_trait::async_trait;
use sea_orm::{sea_query::LockType, ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter, QuerySelect, Set, TransactionTrait};

use super::{
    change_visibility, connect_repository, create_chat, create_node, list_chats, list_nodes,
    list_repositories, read_chat, update_node,
    ChangeVisibilityInput, ConnectRepositoryInput, CreateChatInput, CreateNodeInput,
    TeamRepositoryAccess, UpdateNodeInput, VerifiedRepository, Visibility, WorkspaceNodeKind,
};
use crate::{
    db::entities::{organization_membership, team_catalog_admission, workspace_chat_draft, workspace_node, workspace_repository},
    team_access::{
        principal::ProofRequirement,
        test_auth::{cookie_headers, create_postgres_principal_fixture, drop_postgres_fixture, principal_fixture, wait_for_postgres_lock, ALICE_A_TOKEN, ALICE_B_TOKEN, BOB_A_TOKEN},
    },
    team_server::{handlers::ApiError, state::TeamState, test_server::team_state},
};

#[derive(Default)]
struct FakeAccess {
    calls: AtomicUsize,
}

impl FakeAccess {
    fn calls(&self) -> usize { self.calls.load(Ordering::SeqCst) }
}

#[async_trait]
impl TeamRepositoryAccess for FakeAccess {
    async fn verify(
        &self,
        _context: &crate::team_access::principal::AuthContext,
        full_name: &str,
        branch: &str,
    ) -> Result<VerifiedRepository, ApiError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(VerifiedRepository {
            github_repository_id: "9007199254740992".into(),
            full_name: full_name.into(),
            starting_branch: branch.into(),
        })
    }
}

async fn service_state() -> TeamState {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let shell = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    TeamState::new(
        fixture.db,
        shell.state.config,
        fixture.vault,
        shell.state.oauth,
        shell.state.onboarding,
        fixture.credentials,
        principals,
        fixture.clock,
    )
}

#[test]
fn catalog_wire_shapes_are_camel_case_strict_and_precision_safe() {
    let input: CreateChatInput = serde_json::from_value(serde_json::json!({
        "nodeId": 17,
        "repositoryOverride": {"repositoryId": 29, "startingBranch": "main"},
        "title": "Launch",
        "requestId": "123e4567-e89b-12d3-a456-426614174000",
        "visibility": "private"
    }))
    .unwrap();
    assert_eq!(input.node_id, 17);
    assert!(serde_json::from_value::<CreateChatInput>(serde_json::json!({
        "nodeId":17,"repositoryOverride":null,"title":null,
        "requestId":"123e4567-e89b-12d3-a456-426614174000",
        "visibility":"private","orgId":201
    })).is_err());
    assert!(serde_json::from_value::<ChangeVisibilityInput>(serde_json::json!({
        "expectedRevision": 9007199254740992_i64, "visibility":"private"
    })).is_err());
}

#[tokio::test]
async fn catalog_admission_and_visibility_are_principal_scoped() {
    let state = service_state().await;
    let alice = state.principals.resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached).await.unwrap();
    let alice_b = state.principals.resolve(&cookie_headers(ALICE_B_TOKEN), ProofRequirement::Cached).await.unwrap();
    let bob = state.principals.resolve(&cookie_headers(BOB_A_TOKEN), ProofRequirement::Cached).await.unwrap();
    let access = FakeAccess::default();

    let repository = connect_repository(&state, &alice, &access, ConnectRepositoryInput {
        full_name: "alice-a/repository".into(), starting_branch: "main".into(),
    }).await.unwrap_or_else(|_| panic!("connect repository"));
    let calls_after_connect = access.calls();
    let foreign = workspace_repository::ActiveModel {
        org_id: Set(alice_b.org_id), github_repository_id: Set("9191".into()),
        full_name: Set("foreign/repository".into()), verified_names: Set("[\"foreign/repository\"]".into()),
        created_at: Set(state.clock.now()), updated_at: Set(state.clock.now()), ..Default::default()
    }.insert(&state.db).await.unwrap();
    team_catalog_admission::ActiveModel {
        resource_kind: Set("repository".into()), resource_id: Set(foreign.id), org_id: Set(alice_b.org_id),
        creator_user_id: Set(alice_b.user_id), created_at: Set(state.clock.now()),
    }.insert(&state.db).await.unwrap();
    assert!(matches!(create_node(&state, &alice, &access, CreateNodeInput {
        parent_id: None, kind: WorkspaceNodeKind::Project, name: "Foreign".into(),
        repository_default: Some(super::RepositoryDefaultInput { repository_id: foreign.id, starting_branch: "main".into() }),
    }).await, Err(ApiError::NotFound)));
    assert_eq!(access.calls(), calls_after_connect);
    let node = create_node(&state, &alice, &access, CreateNodeInput {
        parent_id: None, kind: WorkspaceNodeKind::Project, name: "Atlas".into(),
        repository_default: Some(super::RepositoryDefaultInput { repository_id: repository.id, starting_branch: "main".into() }),
    }).await.unwrap_or_else(|_| panic!("create node"));
    assert_eq!(list_repositories(&state, &alice).await.unwrap_or_else(|_| panic!("list repositories")).len(), 1);
    assert_eq!(list_nodes(&state, &alice).await.unwrap_or_else(|_| panic!("list nodes")).len(), 1);
    let node = update_node(&state, &alice, &access, node.id, UpdateNodeInput {
        expected_revision: "0".into(), parent_id: None, name: "Atlas renamed".into(),
        repository_default: Some(super::RepositoryDefaultInput { repository_id: repository.id, starting_branch: "main".into() }),
    }).await.unwrap_or_else(|_| panic!("update node"));
    assert_eq!(node.revision, "1");
    assert!(matches!(update_node(&state, &alice, &access, node.id, UpdateNodeInput {
        expected_revision: "0".into(), parent_id: None, name: "stale".into(), repository_default: node.repository_default.clone(),
    }).await, Err(ApiError::Conflict)));
    let calls_before_chat = access.calls();
    let private = create_chat(&state, &alice, &access, CreateChatInput {
        node_id: node.id, repository_override: None, title: Some("Private".into()),
        request_id: "123e4567-e89b-12d3-a456-426614174000".into(), visibility: Visibility::Private,
    }).await.unwrap_or_else(|_| panic!("create chat"));
    assert!(matches!(read_chat(&state, &bob, private.id).await, Err(ApiError::NotFound)));
    assert!(list_chats(&state, &bob, None, None).await.unwrap_or_else(|_| panic!("list chats")).is_empty());
    let shared = change_visibility(&state, &alice, private.id, ChangeVisibilityInput {
        expected_revision: "0".into(), visibility: Visibility::Organization,
    }).await.unwrap_or_else(|_| panic!("share chat"));
    assert_eq!(shared.access_revision, "1");
    assert_eq!(read_chat(&state, &bob, shared.id).await.unwrap_or_else(|_| panic!("read shared chat")).id, shared.id);
    assert!(matches!(read_chat(&state, &alice_b, shared.id).await, Err(ApiError::NotFound)));
    assert!(matches!(change_visibility(&state, &bob, shared.id, ChangeVisibilityInput {
        expected_revision: "1".into(), visibility: Visibility::Private,
    }).await, Err(ApiError::NotFound)));
    let retried = create_chat(&state, &alice, &access, CreateChatInput {
        node_id: node.id, repository_override: None, title: Some("Private".into()),
        request_id: "123e4567-e89b-12d3-a456-426614174000".into(), visibility: Visibility::Private,
    }).await.unwrap_or_else(|_| panic!("retry chat"));
    assert_eq!(access.calls(), calls_before_chat + 2, "creation and retry each prove repository access");
    assert_eq!(retried.visibility, Visibility::Organization);
    assert!(matches!(create_chat(&state, &alice, &access, CreateChatInput {
        node_id: node.id, repository_override: None, title: Some("Changed".into()),
        request_id: "123e4567-e89b-12d3-a456-426614174000".into(), visibility: Visibility::Private,
    }).await, Err(ApiError::Conflict)));
    let private_again = change_visibility(&state, &alice, shared.id, ChangeVisibilityInput {
        expected_revision: "1".into(), visibility: Visibility::Private,
    }).await.unwrap_or_else(|_| panic!("make private"));
    assert_eq!(private_again.access_revision, "2");
    assert!(matches!(read_chat(&state, &bob, shared.id).await, Err(ApiError::NotFound)));
    let renamed = connect_repository(&state, &alice, &access, ConnectRepositoryInput {
        full_name: "alice-a/renamed".into(), starting_branch: "main".into(),
    }).await.unwrap_or_else(|_| panic!("reconnect renamed repository"));
    assert_eq!(renamed.id, repository.id);
    let snapshot = read_chat(&state, &alice, shared.id).await.unwrap_or_else(|_| panic!("read immutable snapshot"));
    assert_eq!(snapshot.full_name, "alice-a/repository");
    let mut active = workspace_chat_draft::Entity::find_by_id(shared.id).one(&state.db).await.unwrap().unwrap().into_active_model();
    active.execution_state = Set("provisioning".into());
    active.update(&state.db).await.unwrap();
    assert!(matches!(change_visibility(&state, &alice, shared.id, ChangeVisibilityInput {
        expected_revision: "2".into(), visibility: Visibility::Organization,
    }).await, Err(ApiError::Conflict)));
}

#[tokio::test]
async fn legacy_rows_are_not_admitted_by_read_or_update() {
    let state = service_state().await;
    let alice = state.principals.resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached).await.unwrap();
    let now = state.clock.now();
    let repository = workspace_repository::ActiveModel {
        org_id: Set(alice.org_id), github_repository_id: Set("9001".into()),
        full_name: Set("legacy/repo".into()), verified_names: Set("[\"legacy/repo\"]".into()),
        created_at: Set(now), updated_at: Set(now), ..Default::default()
    }.insert(&state.db).await.unwrap();
    let node = workspace_node::ActiveModel {
        org_id: Set(alice.org_id), parent_id: Set(None), kind: Set("project".into()),
        name: Set("Legacy".into()), repository_id: Set(Some(repository.id)), starting_branch: Set(Some("main".into())),
        revision: Set(0), created_at: Set(now), updated_at: Set(now), ..Default::default()
    }.insert(&state.db).await.unwrap();
    let chat = workspace_chat_draft::ActiveModel {
        org_id: Set(alice.org_id), node_id: Set(node.id), creator_user_id: Set(alice.user_id),
        repository_id: Set(repository.id), github_repository_id: Set("9001".into()), full_name: Set("legacy/repo".into()),
        starting_branch: Set("main".into()), title: Set(Some("Legacy private".into())), request_id: Set("legacy".into()),
        request_fingerprint: Set("legacy".into()), execution_state: Set("unprovisioned".into()), visibility: Set("organization".into()),
        access_revision: Set(0), creation_visibility: Set("private".into()), creation_fingerprint_version: Set(1),
        created_at: Set(now), updated_at: Set(now), ..Default::default()
    }.insert(&state.db).await.unwrap();
    assert!(matches!(read_chat(&state, &alice, chat.id).await, Err(ApiError::NotFound)));
    assert!(matches!(change_visibility(&state, &alice, chat.id, ChangeVisibilityInput { expected_revision: "0".into(), visibility: Visibility::Private }).await, Err(ApiError::NotFound)));
}

#[tokio::test]
async fn node_shapes_inheritance_and_cycles_are_fail_closed() {
    let state = service_state().await;
    let alice = state.principals.resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached).await.unwrap();
    let access = FakeAccess::default();
    let repository = connect_repository(&state, &alice, &access, ConnectRepositoryInput {
        full_name: "alice-a/tree".into(), starting_branch: "main".into(),
    }).await.unwrap_or_else(|_| panic!("connect repository"));
    assert!(matches!(create_node(&state, &alice, &access, CreateNodeInput {
        parent_id: None, kind: WorkspaceNodeKind::Folder, name: "orphan".into(), repository_default: None,
    }).await, Err(ApiError::InvalidRequest)));
    assert!(matches!(create_node(&state, &alice, &access, CreateNodeInput {
        parent_id: None, kind: WorkspaceNodeKind::WorkingDirectory, name: "missing repo".into(), repository_default: None,
    }).await, Err(ApiError::InvalidRequest)));
    let project = create_node(&state, &alice, &access, CreateNodeInput {
        parent_id: None, kind: WorkspaceNodeKind::Project, name: "Project".into(),
        repository_default: Some(super::RepositoryDefaultInput { repository_id: repository.id, starting_branch: "main".into() }),
    }).await.unwrap_or_else(|_| panic!("create project"));
    let calls_before_foreign_parent = access.calls();
    assert!(matches!(create_node(&state, &alice, &access, CreateNodeInput {
        parent_id: Some(999_999), kind: WorkspaceNodeKind::WorkingDirectory,
        name: "foreign parent".into(),
        repository_default: Some(super::RepositoryDefaultInput { repository_id: repository.id, starting_branch: "main".into() }),
    }).await, Err(ApiError::NotFound)));
    assert_eq!(access.calls(), calls_before_foreign_parent);
    assert!(matches!(create_node(&state, &alice, &access, CreateNodeInput {
        parent_id: Some(project.id), kind: WorkspaceNodeKind::Project, name: "nested project".into(), repository_default: None,
    }).await, Err(ApiError::InvalidRequest)));
    let folder = create_node(&state, &alice, &access, CreateNodeInput {
        parent_id: Some(project.id), kind: WorkspaceNodeKind::Folder, name: "Folder".into(), repository_default: None,
    }).await.unwrap_or_else(|_| panic!("create folder"));
    let child = create_node(&state, &alice, &access, CreateNodeInput {
        parent_id: Some(folder.id), kind: WorkspaceNodeKind::Folder, name: "Child".into(), repository_default: None,
    }).await.unwrap_or_else(|_| panic!("create child"));
    assert!(matches!(update_node(&state, &alice, &access, folder.id, UpdateNodeInput {
        expected_revision: "0".into(), parent_id: Some(child.id), name: "cycle".into(), repository_default: None,
    }).await, Err(ApiError::InvalidRequest)));
    let chat = create_chat(&state, &alice, &access, CreateChatInput {
        node_id: child.id, repository_override: None, title: None,
        request_id: "123e4567-e89b-12d3-a456-426614174001".into(), visibility: Visibility::Private,
    }).await.unwrap_or_else(|_| panic!("inherit ancestor default"));
    assert_eq!(chat.repository_id, repository.id);
    assert_eq!(chat.starting_branch, "main");
}

#[tokio::test]
async fn chat_pagination_applies_visibility_before_the_page_limit() {
    let state = service_state().await;
    let alice = state.principals.resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached).await.unwrap();
    let bob = state.principals.resolve(&cookie_headers(BOB_A_TOKEN), ProofRequirement::Cached).await.unwrap();
    let access = FakeAccess::default();
    let repository = connect_repository(&state, &alice, &access, ConnectRepositoryInput {
        full_name: "alice-a/paginated".into(), starting_branch: "main".into(),
    }).await.unwrap_or_else(|_| panic!("connect repository"));
    let node = create_node(&state, &alice, &access, CreateNodeInput {
        parent_id: None, kind: WorkspaceNodeKind::Project, name: "Pagination".into(),
        repository_default: Some(super::RepositoryDefaultInput { repository_id: repository.id, starting_branch: "main".into() }),
    }).await.unwrap_or_else(|_| panic!("create node"));
    let now = state.clock.now();
    let mut visible_ids = Vec::new();
    for sequence in 0..410 {
        let visible = sequence % 2 == 1;
        let row = workspace_chat_draft::ActiveModel {
            org_id: Set(alice.org_id), node_id: Set(node.id), creator_user_id: Set(alice.user_id),
            repository_id: Set(repository.id), github_repository_id: Set(repository.github_repository_id.clone()),
            full_name: Set(repository.full_name.clone()), starting_branch: Set("main".into()), title: Set(Some(format!("row {sequence}"))),
            request_id: Set(format!("fixture-{sequence}")), request_fingerprint: Set(format!("fingerprint-{sequence}")),
            execution_state: Set("unprovisioned".into()), visibility: Set(if visible { "organization" } else { "private" }.into()),
            access_revision: Set(0), creation_visibility: Set(if visible { "organization" } else { "private" }.into()),
            creation_fingerprint_version: Set(2), created_at: Set(now), updated_at: Set(now), ..Default::default()
        }.insert(&state.db).await.unwrap();
        team_catalog_admission::ActiveModel {
            resource_kind: Set("chat".into()), resource_id: Set(row.id), org_id: Set(alice.org_id),
            creator_user_id: Set(alice.user_id), created_at: Set(now),
        }.insert(&state.db).await.unwrap();
        if visible { visible_ids.push(row.id); }
    }
    let first = list_chats(&state, &bob, None, None).await.unwrap_or_else(|_| panic!("first page"));
    assert_eq!(first.len(), 200);
    assert_eq!(first.iter().map(|row| row.id).collect::<Vec<_>>(), visible_ids[..200]);
    let second = list_chats(&state, &bob, None, Some(first.last().unwrap().id)).await.unwrap_or_else(|_| panic!("second page"));
    assert_eq!(second.iter().map(|row| row.id).collect::<Vec<_>>(), visible_ids[200..]);
}

struct RevokingAccess {
    db: sea_orm::DatabaseConnection,
}

#[async_trait]
impl TeamRepositoryAccess for RevokingAccess {
    async fn verify(
        &self,
        context: &crate::team_access::principal::AuthContext,
        full_name: &str,
        branch: &str,
    ) -> Result<VerifiedRepository, ApiError> {
        let membership = organization_membership::Entity::find_by_id(context.membership_id)
            .one(&self.db).await.map_err(|_| ApiError::Unavailable)?.ok_or(ApiError::Unavailable)?;
        let mut active = membership.into_active_model();
        active.state = Set("revoked".into());
        active.authorization_revision = Set(context.authorization_revision + 1);
        active.update(&self.db).await.map_err(|_| ApiError::Unavailable)?;
        Ok(VerifiedRepository { github_repository_id: "919191".into(), full_name: full_name.into(), starting_branch: branch.into() })
    }
}

#[tokio::test]
async fn repository_connection_rechecks_principal_after_external_proof() {
    let state = service_state().await;
    let alice = state.principals.resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached).await.unwrap();
    let access = RevokingAccess { db: state.db.clone() };
    assert!(matches!(connect_repository(&state, &alice, &access, ConnectRepositoryInput {
        full_name: "alice-a/revoked".into(), starting_branch: "main".into(),
    }).await, Err(ApiError::Unauthorized)));
    assert_eq!(workspace_repository::Entity::find()
        .filter(workspace_repository::Column::GithubRepositoryId.eq("919191"))
        .all(&state.db).await.unwrap().len(), 0);
}

#[tokio::test]
#[ignore = "requires local disposable PostgreSQL"]
async fn postgres_org_lock_serializes_tree_admission_mutation() {
    use crate::db::entities::org;

    let (database_name, admin, fixture) = create_postgres_principal_fixture(Vec::new())
        .await
        .expect("create disposable PostgreSQL catalog fixture");
    let principals = fixture.service();
    let shell = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    let state = Arc::new(TeamState::new(
        fixture.db, shell.state.config, fixture.vault, shell.state.oauth,
        shell.state.onboarding, fixture.credentials, principals, fixture.clock,
    ));
    let outcome: Result<(), String> = async {
        let alice = state.principals.resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
            .await.map_err(|_| "resolve PostgreSQL principal".to_owned())?;
        let access = FakeAccess::default();
        let repository = connect_repository(&state, &alice, &access, ConnectRepositoryInput {
            full_name: "alice-a/locked".into(), starting_branch: "main".into(),
        }).await.map_err(|_| "connect PostgreSQL repository".to_owned())?;
        let project = create_node(&state, &alice, &access, CreateNodeInput {
            parent_id: None, kind: WorkspaceNodeKind::Project, name: "Locked".into(),
            repository_default: Some(super::RepositoryDefaultInput { repository_id: repository.id, starting_branch: "main".into() }),
        }).await.map_err(|_| "create PostgreSQL project".to_owned())?;
        let blocker = state.db.begin().await.map_err(|error| error.to_string())?;
        org::Entity::find_by_id(alice.org_id).lock(LockType::Update).one(&blocker)
            .await.map_err(|error| error.to_string())?.ok_or("missing PostgreSQL org")?;
        let update_state = Arc::clone(&state);
        let update_context = alice.clone();
        let create = tokio::spawn(async move {
            create_node(&update_state, &update_context, &FakeAccess::default(), CreateNodeInput {
                parent_id: Some(project.id), kind: WorkspaceNodeKind::Folder,
                name: "Must not survive".into(), repository_default: None,
            }).await
        });
        wait_for_postgres_lock(&admin, &database_name, "FROM \"orgs\"").await?;
        team_catalog_admission::Entity::delete_by_id(("node".to_owned(), project.id))
            .exec(&blocker).await.map_err(|error| error.to_string())?;
        blocker.commit().await.map_err(|error| error.to_string())?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), create)
            .await.map_err(|_| "timed out waiting for catalog mutation".to_owned())?
            .map_err(|error| error.to_string())?;
        if !matches!(result, Err(ApiError::NotFound)) {
            return Err("tree mutation did not observe post-lock admission state".into());
        }
        let inserted = workspace_node::Entity::find()
            .filter(workspace_node::Column::Name.eq("Must not survive"))
            .count(&state.db).await.map_err(|error| error.to_string())?;
        if inserted != 0 { return Err("rejected tree mutation persisted a node".into()); }
        Ok(())
    }.await;
    if let Err(error) = outcome { panic!("{error}; preserved database {database_name}"); }
    let db: DatabaseConnection = state.db.clone();
    drop(state);
    drop_postgres_fixture(&database_name, admin, db).await
        .expect("drop verified PostgreSQL catalog fixture");
}
