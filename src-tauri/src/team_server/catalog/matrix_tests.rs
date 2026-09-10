use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use chrono::Duration;
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, EntityTrait, IntoActiveModel, Set,
    Statement,
};

use super::*;
use crate::{
    db::entities::{
        organization_membership, team_catalog_admission, workspace_chat_draft,
        workspace_chat_execution, workspace_node, workspace_repository,
        workspace_writer_lease,
    },
    team_access::{
        principal::{AuthContext, ProofRequirement},
        test_auth::{
            cookie_headers, principal_fixture, TestClock, ALICE_A_TOKEN, ALICE_B_TOKEN,
            BOB_A_TOKEN,
        },
    },
    team_server::{handlers::ApiError, state::TeamState, test_server::team_state},
};

#[derive(Default)]
struct MatrixAccess {
    calls: AtomicUsize,
}

#[async_trait]
impl TeamRepositoryAccess for MatrixAccess {
    async fn verify(
        &self,
        _context: &AuthContext,
        full_name: &str,
        branch: &str,
    ) -> Result<VerifiedRepository, ApiError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(VerifiedRepository {
            github_repository_id: "8080".into(),
            full_name: full_name.into(),
            starting_branch: branch.into(),
        })
    }
}

async fn state_and_clock() -> (TeamState, Arc<TestClock>) {
    let fixture = principal_fixture(Vec::new()).await;
    let clock = fixture.clock.clone();
    let principals = fixture.service();
    let shell = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    (
        TeamState::new(
            fixture.db,
            shell.state.config,
            fixture.vault,
            shell.state.oauth,
            shell.state.onboarding,
            fixture.credentials,
            principals,
            fixture.clock,
        ),
        clock,
    )
}

async fn state() -> TeamState {
    state_and_clock().await.0
}

async fn principal(state: &TeamState, token: &str) -> AuthContext {
    state
        .principals
        .resolve(&cookie_headers(token), ProofRequirement::Cached)
        .await
        .unwrap()
}

async fn repository_and_project(
    state: &TeamState,
    alice: &AuthContext,
    access: &MatrixAccess,
) -> (RepositoryDto, NodeDto) {
    let repository = connect_repository(
        state,
        alice,
        access,
        ConnectRepositoryInput {
            full_name: "alice-a/matrix".into(),
            starting_branch: "main".into(),
        },
    )
    .await
    .unwrap_or_else(|_| panic!("connect matrix repository"));
    let project = create_node(
        state,
        alice,
        access,
        CreateNodeInput {
            parent_id: None,
            kind: WorkspaceNodeKind::Project,
            name: "Matrix".into(),
            repository_default: Some(RepositoryDefaultInput {
                repository_id: repository.id,
                starting_branch: "main".into(),
            }),
        },
    )
    .await
    .unwrap_or_else(|_| panic!("create matrix project"));
    (repository, project)
}

async fn private_chat(
    state: &TeamState,
    alice: &AuthContext,
    access: &MatrixAccess,
    node_id: i32,
    request_id: &str,
) -> ChatDto {
    create_chat(
        state,
        alice,
        access,
        CreateChatInput {
            node_id,
            repository_override: None,
            title: Some("Private matrix chat".into()),
            request_id: request_id.into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap_or_else(|_| panic!("create private matrix chat"))
}

async fn seed_admissions(
    state: &TeamState,
    kind: &str,
    first_id: i32,
    count: i32,
    org_id: i32,
    creator_user_id: i32,
) {
    let last_id = first_id + count - 1;
    let sql = format!(
        "WITH RECURSIVE ids(id) AS (SELECT {first_id} UNION ALL SELECT id + 1 FROM ids WHERE id < {last_id}) INSERT INTO team_catalog_admission (resource_kind, resource_id, org_id, creator_user_id, created_at) SELECT '{kind}', id, {org_id}, {creator_user_id}, '2026-09-10T00:00:00Z' FROM ids"
    );
    state
        .db
        .execute(Statement::from_string(state.db.get_database_backend(), sql))
        .await
        .unwrap();
}

#[tokio::test]
async fn database_failures_propagate_unavailable_instead_of_hiding_or_truncating() {
    let state = state().await;
    let alice = principal(&state, ALICE_A_TOKEN).await;
    let access = MatrixAccess::default();
    let (_, project) = repository_and_project(&state, &alice, &access).await;
    let chat = private_chat(
        &state,
        &alice,
        &access,
        project.id,
        "123e4567-e89b-12d3-a456-426614174110",
    )
    .await;
    state
        .db
        .execute(Statement::from_string(
            state.db.get_database_backend(),
            "DROP TABLE workspace_node",
        ))
        .await
        .unwrap();

    assert!(matches!(
        read_chat(&state, &alice, chat.id).await,
        Err(ApiError::Unavailable)
    ));
    assert!(matches!(
        list_nodes(&state, &alice).await,
        Err(ApiError::Unavailable)
    ));
}

#[tokio::test]
async fn repository_node_and_creator_chat_caps_reject_without_truncation() {
    let repository_state = state().await;
    let alice = principal(&repository_state, ALICE_A_TOKEN).await;
    let access = MatrixAccess::default();
    seed_admissions(&repository_state, "repository", 10_000, 1000, alice.org_id, alice.user_id).await;
    assert!(matches!(
        connect_repository(
            &repository_state,
            &alice,
            &access,
            ConnectRepositoryInput {
                full_name: "alice-a/over-cap".into(),
                starting_branch: "main".into(),
            },
        )
        .await,
        Err(ApiError::Conflict)
    ));

    let node_state = state().await;
    let alice = principal(&node_state, ALICE_A_TOKEN).await;
    seed_admissions(&node_state, "node", 20_000, 1000, alice.org_id, alice.user_id).await;
    assert!(matches!(
        create_node(
            &node_state,
            &alice,
            &MatrixAccess::default(),
            CreateNodeInput {
                parent_id: None,
                kind: WorkspaceNodeKind::Project,
                name: "Over cap".into(),
                repository_default: None,
            },
        )
        .await,
        Err(ApiError::Conflict)
    ));

    let chat_state = state().await;
    let alice = principal(&chat_state, ALICE_A_TOKEN).await;
    let access = MatrixAccess::default();
    let (_, project) = repository_and_project(&chat_state, &alice, &access).await;
    seed_admissions(&chat_state, "chat", 30_000, 1000, alice.org_id, alice.user_id).await;
    assert!(matches!(
        create_chat(
            &chat_state,
            &alice,
            &access,
            CreateChatInput {
                node_id: project.id,
                repository_override: None,
                title: None,
                request_id: "123e4567-e89b-12d3-a456-426614174111".into(),
                visibility: Visibility::Private,
            },
        )
        .await,
        Err(ApiError::Conflict)
    ));
}

#[tokio::test]
async fn admin_cannot_read_or_mutate_another_users_private_chat_and_unknown_ids_match() {
    let state = state().await;
    let alice = principal(&state, ALICE_A_TOKEN).await;
    let access = MatrixAccess::default();
    let (_, project) = repository_and_project(&state, &alice, &access).await;
    let chat = private_chat(
        &state,
        &alice,
        &access,
        project.id,
        "123e4567-e89b-12d3-a456-426614174112",
    )
    .await;
    let membership = organization_membership::Entity::find_by_id(402)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    let mut active = membership.into_active_model();
    active.role = Set("admin".into());
    active.update(&state.db).await.unwrap();
    let admin = principal(&state, BOB_A_TOKEN).await;

    for id in [chat.id, 999_999] {
        assert!(matches!(
            read_chat(&state, &admin, id).await,
            Err(ApiError::NotFound)
        ));
        assert!(matches!(
            change_visibility(
                &state,
                &admin,
                id,
                ChangeVisibilityInput {
                    expected_revision: "0".into(),
                    visibility: Visibility::Organization,
                },
            )
            .await,
            Err(ApiError::NotFound)
        ));
    }
}

async fn insert_admitted_node(
    state: &TeamState,
    context: &AuthContext,
    id: i32,
    parent_id: Option<i32>,
    org_id: i32,
    kind: &str,
) {
    let now = state.clock.now();
    workspace_node::ActiveModel {
        id: Set(id),
        org_id: Set(org_id),
        parent_id: Set(parent_id),
        kind: Set(kind.into()),
        name: Set(format!("corrupt-{id}")),
        repository_id: Set(None),
        starting_branch: Set(None),
        revision: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&state.db)
    .await
    .unwrap();
    team_catalog_admission::ActiveModel {
        resource_kind: Set("node".into()),
        resource_id: Set(id),
        org_id: Set(org_id),
        creator_user_id: Set(context.user_id),
        created_at: Set(now),
    }
    .insert(&state.db)
    .await
    .unwrap();
}

async fn insert_admitted_chat(
    state: &TeamState,
    alice: &AuthContext,
    repository: &RepositoryDto,
    id: i32,
    node_id: i32,
) {
    let now = state.clock.now();
    workspace_chat_draft::ActiveModel {
        id: Set(id),
        org_id: Set(alice.org_id),
        node_id: Set(node_id),
        creator_user_id: Set(alice.user_id),
        repository_id: Set(repository.id),
        github_repository_id: Set(repository.github_repository_id.clone()),
        full_name: Set(repository.full_name.clone()),
        starting_branch: Set("main".into()),
        title: Set(Some(format!("hidden-{id}"))),
        request_id: Set(format!("hidden-request-{id}")),
        request_fingerprint: Set(format!("hidden-fingerprint-{id}")),
        execution_state: Set("unprovisioned".into()),
        visibility: Set("organization".into()),
        access_revision: Set(0),
        creation_visibility: Set("organization".into()),
        creation_fingerprint_version: Set(2),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&state.db)
    .await
    .unwrap();
    team_catalog_admission::ActiveModel {
        resource_kind: Set("chat".into()),
        resource_id: Set(id),
        org_id: Set(alice.org_id),
        creator_user_id: Set(alice.user_id),
        created_at: Set(now),
    }
    .insert(&state.db)
    .await
    .unwrap();
}

#[tokio::test]
async fn orphan_foreign_and_cyclic_admitted_ancestry_remains_invisible() {
    let state = state().await;
    let alice = principal(&state, ALICE_A_TOKEN).await;
    let alice_b = principal(&state, ALICE_B_TOKEN).await;
    let bob = principal(&state, BOB_A_TOKEN).await;
    let access = MatrixAccess::default();
    let (repository, _) = repository_and_project(&state, &alice, &access).await;

    insert_admitted_node(&state, &alice, 40_001, Some(499_999), alice.org_id, "folder").await;
    insert_admitted_node(&state, &alice, 40_002, Some(40_003), alice.org_id, "folder").await;
    insert_admitted_node(&state, &alice, 40_003, Some(40_002), alice.org_id, "folder").await;
    insert_admitted_node(&state, &alice_b, 40_004, None, alice_b.org_id, "project").await;
    insert_admitted_node(&state, &alice, 40_005, Some(40_004), alice.org_id, "folder").await;
    for (chat_id, node_id) in [(41_001, 40_001), (41_002, 40_002), (41_003, 40_005)] {
        insert_admitted_chat(&state, &alice, &repository, chat_id, node_id).await;
        assert!(matches!(
            read_chat(&state, &bob, chat_id).await,
            Err(ApiError::NotFound)
        ));
    }
    let nodes = list_nodes(&state, &alice)
        .await
        .unwrap_or_else(|_| panic!("list valid nodes around corruption"));
    for hidden in [40_001, 40_002, 40_003, 40_005] {
        assert!(!nodes.iter().any(|node| node.id == hidden));
    }
    assert!(list_chats(&state, &bob, None, None)
        .await
        .unwrap_or_else(|_| panic!("list chats around corruption"))
        .is_empty());
}

#[tokio::test]
async fn sharing_rejects_execution_states_rows_active_writers_and_revision_overflow() {
    let state = state().await;
    let alice = principal(&state, ALICE_A_TOKEN).await;
    let access = MatrixAccess::default();
    let (_, project) = repository_and_project(&state, &alice, &access).await;
    let chat = private_chat(
        &state,
        &alice,
        &access,
        project.id,
        "123e4567-e89b-12d3-a456-426614174113",
    )
    .await;
    for execution_state in ["provisioning", "active"] {
        let row = workspace_chat_draft::Entity::find_by_id(chat.id)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();
        let mut active = row.into_active_model();
        active.execution_state = Set(execution_state.into());
        active.update(&state.db).await.unwrap();
        assert!(matches!(
            change_visibility(
                &state,
                &alice,
                chat.id,
                ChangeVisibilityInput {
                    expected_revision: "0".into(),
                    visibility: Visibility::Organization,
                },
            )
            .await,
            Err(ApiError::Conflict)
        ));
    }
    let row = workspace_chat_draft::Entity::find_by_id(chat.id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    let mut active = row.into_active_model();
    active.execution_state = Set("unprovisioned".into());
    active.update(&state.db).await.unwrap();
    let now = state.clock.now();
    workspace_chat_execution::ActiveModel {
        draft_id: Set(chat.id),
        org_id: Set(alice.org_id),
        agent_type: Set("codex".into()),
        state: Set("ready".into()),
        allocation_id: Set("matrix-allocation".into()),
        starting_commit: Set("a".repeat(40)),
        branch_name: Set("workos/matrix".into()),
        worktree_relpath: Set("orgs/matrix/worktree".into()),
        legacy_folder_id: Set(None),
        legacy_conversation_id: Set(None),
        revision: Set(0),
        started_at: Set(now),
        updated_at: Set(now),
        last_error_code: Set(None),
        provisioning_owner_token: Set(None),
        provisioning_lease_expires_at: Set(None),
    }
    .insert(&state.db)
    .await
    .unwrap();
    assert!(matches!(
        change_visibility(
            &state,
            &alice,
            chat.id,
            ChangeVisibilityInput {
                expected_revision: "0".into(),
                visibility: Visibility::Organization,
            },
        )
        .await,
        Err(ApiError::Conflict)
    ));
    workspace_chat_execution::Entity::delete_by_id(chat.id)
        .exec(&state.db)
        .await
        .unwrap();
    state
        .db
        .execute(Statement::from_string(
            state.db.get_database_backend(),
            "PRAGMA foreign_keys=OFF",
        ))
        .await
        .unwrap();
    workspace_writer_lease::ActiveModel {
        draft_id: Set(chat.id),
        org_id: Set(alice.org_id),
        creator_user_id: Set(alice.user_id),
        owner_token: Set("matrix-writer".into()),
        connection_id: Set(None),
        expires_at: Set(now + Duration::minutes(5)),
        updated_at: Set(now),
    }
    .insert(&state.db)
    .await
    .unwrap();
    state
        .db
        .execute(Statement::from_string(
            state.db.get_database_backend(),
            "PRAGMA foreign_keys=ON",
        ))
        .await
        .unwrap();
    assert!(matches!(
        change_visibility(
            &state,
            &alice,
            chat.id,
            ChangeVisibilityInput {
                expected_revision: "0".into(),
                visibility: Visibility::Organization,
            },
        )
        .await,
        Err(ApiError::Conflict)
    ));
    workspace_writer_lease::Entity::delete_by_id(chat.id)
        .exec(&state.db)
        .await
        .unwrap();
    let row = workspace_chat_draft::Entity::find_by_id(chat.id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    let mut active = row.into_active_model();
    active.access_revision = Set(i64::MAX);
    active.update(&state.db).await.unwrap();
    assert!(matches!(
        change_visibility(
            &state,
            &alice,
            chat.id,
            ChangeVisibilityInput {
                expected_revision: i64::MAX.to_string(),
                visibility: Visibility::Organization,
            },
        )
        .await,
        Err(ApiError::Conflict)
    ));
}

struct ExpiringAccess {
    clock: Arc<TestClock>,
}

#[async_trait]
impl TeamRepositoryAccess for ExpiringAccess {
    async fn verify(
        &self,
        _context: &AuthContext,
        full_name: &str,
        branch: &str,
    ) -> Result<VerifiedRepository, ApiError> {
        self.clock.advance(Duration::hours(2));
        Ok(VerifiedRepository {
            github_repository_id: "8080".into(),
            full_name: full_name.into(),
            starting_branch: branch.into(),
        })
    }
}

#[tokio::test]
async fn expired_principal_after_repository_proof_persists_nothing() {
    let (state, clock) = state_and_clock().await;
    let alice = principal(&state, ALICE_A_TOKEN).await;
    assert!(matches!(
        connect_repository(
            &state,
            &alice,
            &ExpiringAccess { clock },
            ConnectRepositoryInput {
                full_name: "alice-a/expired".into(),
                starting_branch: "main".into(),
            },
        )
        .await,
        Err(ApiError::Unauthorized)
    ));
    assert!(workspace_repository::Entity::find()
        .all(&state.db)
        .await
        .unwrap()
        .is_empty());
    assert!(team_catalog_admission::Entity::find()
        .all(&state.db)
        .await
        .unwrap()
        .iter()
        .all(|row| row.resource_kind != "repository"));
}
