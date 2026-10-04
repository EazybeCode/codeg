//! Agent sessions and the project/folder trash (schema 23): no new session,
//! filing or run may start inside a trashed node, through any entry point;
//! an existing turn's idempotent replay still answers (without executing);
//! restore makes everything runnable again; another tenant learns nothing.

use super::*;
use crate::neev::catalog::{nodes, trash};
use crate::neev::content::{
    resources,
    types::{ResourceInput, ResourceKind},
};
use crate::neev::test_support as h;
use crate::neev::types::{Node, NodeInput, NodeKind};
use std::sync::Arc;

const NOW: i64 = 1_000;
const REQ_1: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1";
const REQ_2: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa2";

struct Harness {
    state: Arc<ServerState>,
    owner: OrgContext,
    foreign: OrgContext,
    project: Node,
    folder: Node,
}

impl Harness {
    async fn new() -> Self {
        let database = h::database().await;
        let user = h::native_account(&database, "owner", "owner@example.test").await;
        let other = h::native_account(&database, "foreign", "foreign@example.test").await;
        let (org, _) = h::organization(&database, user, "admin").await;
        let (foreign_org, _) = h::organization(&database, other, "admin").await;
        let state = Arc::new(
            ServerState::new(
                database.clone(),
                h::preview_config(),
                Arc::new(h::TestClock::new(NOW)),
            )
            .unwrap(),
        );
        let owner = h::scoped_context(&database, user, org, NOW).await;
        let foreign = h::scoped_context(&database, other, foreign_org, NOW).await;
        let project = node(&state, &owner, None, "Project").await;
        let folder = node(&state, &owner, Some(project.id), "Folder").await;
        Self {
            state,
            owner,
            foreign,
            project,
            folder,
        }
    }

    async fn session(
        &self,
        actor: &OrgContext,
        node_id: Option<i32>,
        bound: Option<&str>,
    ) -> Result<AgentSession, ContentError> {
        create(
            &self.state,
            actor,
            SessionInput {
                title: "Agent chat".into(),
                provider: "claude".into(),
                model: "default".into(),
                bound_chat_id: bound.map(str::to_owned),
                node_id,
                effort: None,
                fast: false,
                request_id: uuid::Uuid::new_v4().to_string(),
            },
        )
        .await
    }

    async fn chat_in_folder(&self) -> String {
        resources::create(
            &self.state,
            &self.owner,
            ResourceInput {
                node_id: self.folder.id,
                kind: ResourceKind::Chat,
                title: "Chat".into(),
                origin_chat_id: None,
                request_id: uuid::Uuid::new_v4().to_string(),
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn trash_folder(&self) {
        let revision = current_revision(&self.state, &self.owner, self.folder.id).await;
        trash::trash(&self.state, &self.owner, self.folder.id, revision)
            .await
            .unwrap();
    }

    async fn restore_folder(&self) {
        trash::restore(&self.state, &self.owner, self.folder.id)
            .await
            .unwrap();
    }

    /// Settle a running turn so the folder is not blocked by an active agent.
    async fn settle(&self, run_id: &str) {
        finish(
            &self.state.database,
            run_id,
            "done",
            None,
            TurnStatus::Completed,
            None,
            NOW,
        )
        .await
        .unwrap();
    }
}

async fn node(state: &ServerState, actor: &OrgContext, parent: Option<i32>, name: &str) -> Node {
    nodes::create(
        state,
        actor,
        NodeInput {
            parent_id: parent,
            kind: if parent.is_some() {
                NodeKind::Folder
            } else {
                NodeKind::Project
            },
            name: name.into(),
        },
    )
    .await
    .unwrap()
}

async fn current_revision(state: &ServerState, actor: &OrgContext, id: i32) -> i64 {
    nodes::list(state, actor, None)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.id == id)
        .unwrap()
        .revision
}

#[tokio::test]
async fn no_session_is_created_in_or_moved_into_a_trashed_folder() {
    let h = Harness::new().await;
    let unfiled = h.session(&h.owner, None, None).await.unwrap();
    h.trash_folder().await;

    assert_eq!(
        h.session(&h.owner, Some(h.folder.id), None).await.err(),
        Some(ContentError::NotFound)
    );
    assert_eq!(
        move_to_node(
            &h.state,
            &h.owner,
            &unfiled.id,
            Some(h.folder.id),
            unfiled.revision
        )
        .await
        .err(),
        Some(ContentError::NotFound)
    );
    // The live parent still takes it.
    let moved = move_to_node(
        &h.state,
        &h.owner,
        &unfiled.id,
        Some(h.project.id),
        unfiled.revision,
    )
    .await
    .unwrap();
    assert_eq!(moved.node_id, Some(h.project.id));

    h.restore_folder().await;
    let filed = h.session(&h.owner, Some(h.folder.id), None).await.unwrap();
    assert_eq!(filed.node_id, Some(h.folder.id));
    move_to_node(
        &h.state,
        &h.owner,
        &moved.id,
        Some(h.folder.id),
        moved.revision,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_filed_session_takes_no_new_turn_while_trashed_but_replays_and_resumes() {
    let h = Harness::new().await;
    let session = h.session(&h.owner, Some(h.folder.id), None).await.unwrap();
    let (first, created) = begin_turn(&h.state, &h.owner, &session.id, "hello", Some(REQ_1))
        .await
        .unwrap();
    assert!(created);
    h.settle(&first.run_id).await;
    h.trash_folder().await;

    // Every admission path refuses a NEW run inside the trash.
    assert_eq!(
        begin_turn(&h.state, &h.owner, &session.id, "again", None)
            .await
            .err(),
        Some(ContentError::Archived)
    );
    assert_eq!(
        begin_turn_with_attachments(&h.state, &h.owner, &session.id, "again", Some(REQ_2), &[])
            .await
            .err(),
        Some(ContentError::Archived)
    );
    // The turn that already exists is still the answer to its own retry, and the
    // caller is told it did not create it, so nothing executes.
    let (replayed, created) = begin_turn(&h.state, &h.owner, &session.id, "hello", Some(REQ_1))
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(replayed.id, first.id);
    // History stays readable.
    assert_eq!(
        turns(&h.state, &h.owner, &session.id, None)
            .await
            .unwrap()
            .items
            .len(),
        1
    );

    h.restore_folder().await;
    let (resumed, created) = begin_turn(&h.state, &h.owner, &session.id, "again", Some(REQ_2))
        .await
        .unwrap();
    assert!(created);
    assert_eq!(resumed.sequence, 2);
}

#[tokio::test]
async fn a_session_in_a_trashed_subfolder_is_refused_through_its_ancestor() {
    let h = Harness::new().await;
    let nested = node(&h.state, &h.owner, Some(h.folder.id), "Nested").await;
    let session = h.session(&h.owner, Some(nested.id), None).await.unwrap();
    // Trashing the PROJECT hides the folder and the nested folder with it.
    let revision = current_revision(&h.state, &h.owner, h.project.id).await;
    trash::trash(&h.state, &h.owner, h.project.id, revision)
        .await
        .unwrap();
    assert_eq!(
        begin_turn(&h.state, &h.owner, &session.id, "go", None)
            .await
            .err(),
        Some(ContentError::Archived)
    );
    trash::restore(&h.state, &h.owner, h.project.id)
        .await
        .unwrap();
    begin_turn(&h.state, &h.owner, &session.id, "go", None)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_unfiled_session_bound_to_a_trashed_chat_takes_no_new_turn() {
    let h = Harness::new().await;
    let chat = h.chat_in_folder().await;
    // Unfiled (no node_id): only its bound chat places it in the tree.
    let session = h.session(&h.owner, None, Some(&chat)).await.unwrap();
    assert_eq!(session.node_id, None);
    h.trash_folder().await;
    assert_eq!(
        begin_turn(&h.state, &h.owner, &session.id, "go", None)
            .await
            .err(),
        Some(ContentError::Archived)
    );
    // No new session may bind to it either, and no team holder is admitted.
    assert!(h.session(&h.owner, None, Some(&chat)).await.is_err());
    assert!(ensure_team_session(
        &h.state, &h.owner, &chat, "claude", "default", "Team", None, false
    )
    .await
    .is_err());

    h.restore_folder().await;
    begin_turn(&h.state, &h.owner, &session.id, "go", None)
        .await
        .unwrap();
    ensure_team_session(
        &h.state, &h.owner, &chat, "claude", "default", "Team", None, false,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn another_tenant_sees_no_difference_between_trashed_and_foreign_nodes() {
    let h = Harness::new().await;
    let live_foreign = h.session(&h.foreign, Some(h.project.id), None).await.err();
    h.trash_folder().await;
    let trashed_foreign = h.session(&h.foreign, Some(h.folder.id), None).await.err();
    // Both are the same "not found": the trash state of another org's node
    // is not observable.
    assert_eq!(live_foreign, Some(ContentError::NotFound));
    assert_eq!(trashed_foreign, Some(ContentError::NotFound));
    // The foreign org's own work is untouched by this org's trash.
    let own = node(&h.state, &h.foreign, None, "Theirs").await;
    let session = h.session(&h.foreign, Some(own.id), None).await.unwrap();
    begin_turn(&h.state, &h.foreign, &session.id, "go", None)
        .await
        .unwrap();
}
