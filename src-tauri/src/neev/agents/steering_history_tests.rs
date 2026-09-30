//! Behavioral tests for steering history persistence with shared chat publication.
//!
//! These tests verify the contract of `accept_steer_input_and_publish`:
//! - Reservation: steer inputs are properly recorded before transport
//! - History: accepted inputs appear in the turn's history
//! - Authorization: shared chat publication re-proves write access

use super::accept_steer_input_and_publish;
use crate::neev::agents::sessions::{self, AgentSession, SessionInput, TurnStatus};
use crate::neev::catalog::nodes;
use crate::neev::content::{
    messages,
    resources,
    types::{ContentError, ResourceInput, ResourceKind, ResourcePatch, Visibility},
};
use crate::neev::db;
use crate::neev::startup::ServerState;
use crate::neev::test_support as h;
use crate::neev::types::{NodeInput, NodeKind, OrgContext};
use std::sync::Arc;

const NOW: i64 = 1_700_000_000;

struct Harness {
    state: Arc<ServerState>,
    database: sea_orm::DatabaseConnection,
    org: i32,
    owner: OrgContext,
}

impl Harness {
    async fn new() -> Self {
        let database = h::database().await;
        let user = h::native_account(&database, "owner", "owner@example.test").await;
        let (org, _) = h::organization(&database, user, "admin").await;
        let state = Arc::new(
            ServerState::new(
                database.clone(),
                h::preview_config(),
                Arc::new(h::TestClock::new(NOW)),
            )
            .unwrap(),
        );
        let owner = h::scoped_context(&database, user, org, NOW).await;
        Self {
            state,
            database,
            org,
            owner,
        }
    }

    /// A second member of the SAME org.
    async fn peer(&self) -> OrgContext {
        let user = h::native_account(&self.database, "peer", "peer@example.test").await;
        h::add_membership(&self.database, self.org, user, "member").await;
        h::scoped_context(&self.database, user, self.org, NOW).await
    }

    /// Create a private agent session (not bound to any chat).
    async fn private_session(&self) -> AgentSession {
        sessions::create(
            &self.state,
            &self.owner,
            SessionInput {
                title: "Private Agent Chat".into(),
                provider: "claude".into(),
                model: "default".into(),
                bound_chat_id: None,
                node_id: None,
                effort: None,
                fast: false,
                request_id: uuid::Uuid::new_v4().to_string(),
            },
        )
        .await
        .unwrap()
    }

    /// Create a shared chat with the given visibility.
    async fn chat(&self, actor: &OrgContext, visibility: Visibility) -> String {
        let node = nodes::create(
            &self.state,
            actor,
            NodeInput {
                parent_id: None,
                kind: NodeKind::Project,
                name: "Project".into(),
            },
        )
        .await
        .unwrap()
        .id;
        let chat = resources::create(
            &self.state,
            actor,
            ResourceInput {
                node_id: node,
                kind: ResourceKind::Chat,
                title: "Chat".into(),
                origin_chat_id: None,
                request_id: uuid::Uuid::new_v4().to_string(),
            },
        )
        .await
        .unwrap();
        if visibility == Visibility::Organization {
            resources::update(
                &self.state,
                actor,
                &chat.id,
                ResourcePatch {
                    expected_auth_revision: chat.auth_revision,
                    title: chat.title.clone(),
                    node_id: chat.node_id,
                    visibility: Visibility::Organization,
                    archived: false,
                },
            )
            .await
            .unwrap();
        }
        chat.id
    }

    /// Create a turn in a session (simulating a run that would receive steering input).
    async fn create_turn(&self, session_id: &str, prompt: &str) -> sessions::AgentTurn {
        let (turn, _created) = sessions::begin_turn(
            &self.state,
            &self.owner,
            session_id,
            prompt,
            Some(&uuid::Uuid::new_v4().to_string()),
        )
        .await
        .unwrap();
        turn
    }

    /// Complete a turn (so the session is not blocked for new turns).
    async fn complete_turn(&self, run_id: &str) {
        sessions::finish(
            &self.database,
            run_id,
            "Test response",
            None,
            TurnStatus::Completed,
            None,
            NOW,
        )
        .await
        .unwrap();
    }

    /// Reserve a steer input (simulating the HTTP handler's persistence-first write).
    async fn reserve_steer_input(
        &self,
        session_id: &str,
        turn_sequence: i64,
        request_id: &str,
        text: &str,
    ) -> db::ReserveResult {
        db::record_steer_input(
            &self.database,
            self.org,
            session_id,
            turn_sequence,
            request_id,
            text,
            NOW,
        )
        .await
        .unwrap()
    }
}

// --- RESERVATION TESTS ---

#[tokio::test]
async fn reservation_won_on_first_attempt() {
    let h = Harness::new().await;
    let session = h.private_session().await;
    let turn = h.create_turn(&session.id, "Hello").await;

    let request_id = uuid::Uuid::new_v4().to_string();
    let result = h
        .reserve_steer_input(&session.id, turn.sequence, &request_id, "Follow-up text")
        .await;

    assert_eq!(result, db::ReserveResult::Won);
}

#[tokio::test]
async fn reservation_already_exists_on_retry_with_same_text() {
    let h = Harness::new().await;
    let session = h.private_session().await;
    let turn = h.create_turn(&session.id, "Hello").await;

    let request_id = uuid::Uuid::new_v4().to_string();
    let text = "Follow-up text";

    // First attempt wins
    let first = h
        .reserve_steer_input(&session.id, turn.sequence, &request_id, text)
        .await;
    assert_eq!(first, db::ReserveResult::Won);

    // Retry with same text returns AlreadyExists
    let retry = h
        .reserve_steer_input(&session.id, turn.sequence, &request_id, text)
        .await;
    assert_eq!(retry, db::ReserveResult::AlreadyExists);
}

#[tokio::test]
async fn reservation_text_mismatch_on_retry_with_different_text() {
    let h = Harness::new().await;
    let session = h.private_session().await;
    let turn = h.create_turn(&session.id, "Hello").await;

    let request_id = uuid::Uuid::new_v4().to_string();

    // First attempt wins
    let first = h
        .reserve_steer_input(&session.id, turn.sequence, &request_id, "Original text")
        .await;
    assert_eq!(first, db::ReserveResult::Won);

    // Retry with different text returns TextMismatch
    let retry = h
        .reserve_steer_input(&session.id, turn.sequence, &request_id, "Different text")
        .await;
    assert_eq!(retry, db::ReserveResult::TextMismatch);
}

// --- HISTORY TESTS ---

#[tokio::test]
async fn accepted_input_appears_in_turn_history() {
    let h = Harness::new().await;
    let session = h.private_session().await;
    let turn = h.create_turn(&session.id, "Hello").await;

    let request_id = uuid::Uuid::new_v4().to_string();
    let text = "Follow-up text";

    // Reserve and accept the input
    h.reserve_steer_input(&session.id, turn.sequence, &request_id, text)
        .await;

    accept_steer_input_and_publish(
        &h.state,
        &h.owner,
        &session.id,
        turn.sequence,
        &request_id,
        text,
        100, // response_offset
        NOW,
        None, // no bound chat
    )
    .await
    .unwrap();

    // Verify the input is recorded as accepted
    let inputs = db::list_steer_inputs(&h.database, h.org, &session.id, turn.sequence)
        .await
        .unwrap();

    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].request_id, request_id);
    assert_eq!(inputs[0].text, text);
    assert!(inputs[0].accepted);
    assert_eq!(inputs[0].response_offset, Some(100));
}

#[tokio::test]
async fn multiple_inputs_appear_in_history_ordered_by_offset() {
    let h = Harness::new().await;
    let session = h.private_session().await;
    let turn = h.create_turn(&session.id, "Hello").await;

    // Send three inputs at different offsets (not in order)
    let inputs_data = [
        (uuid::Uuid::new_v4().to_string(), "Second input", 200i64),
        (uuid::Uuid::new_v4().to_string(), "First input", 100i64),
        (uuid::Uuid::new_v4().to_string(), "Third input", 300i64),
    ];

    for (request_id, text, offset) in &inputs_data {
        h.reserve_steer_input(&session.id, turn.sequence, request_id, text)
            .await;
        accept_steer_input_and_publish(
            &h.state,
            &h.owner,
            &session.id,
            turn.sequence,
            request_id,
            text,
            *offset,
            NOW,
            None,
        )
        .await
        .unwrap();
    }

    // Verify they appear ordered by response_offset
    let inputs = db::list_steer_inputs(&h.database, h.org, &session.id, turn.sequence)
        .await
        .unwrap();

    assert_eq!(inputs.len(), 3);
    assert_eq!(inputs[0].text, "First input");
    assert_eq!(inputs[0].response_offset, Some(100));
    assert_eq!(inputs[1].text, "Second input");
    assert_eq!(inputs[1].response_offset, Some(200));
    assert_eq!(inputs[2].text, "Third input");
    assert_eq!(inputs[2].response_offset, Some(300));
}

// --- AUTHORIZATION TESTS (SHARED CHAT) ---

#[tokio::test]
async fn shared_chat_receives_user_message_on_accept() {
    let h = Harness::new().await;

    // Create an org-visible chat
    let chat_id = h.chat(&h.owner, Visibility::Organization).await;

    // Create a private session (simulating what team_invoke would create internally)
    let session = h.private_session().await;
    let turn = h.create_turn(&session.id, "Hello agent").await;

    let request_id = uuid::Uuid::new_v4().to_string();
    let text = "Follow-up in shared chat";

    // Reserve and accept with bound chat
    h.reserve_steer_input(&session.id, turn.sequence, &request_id, text)
        .await;

    let result = accept_steer_input_and_publish(
        &h.state,
        &h.owner,
        &session.id,
        turn.sequence,
        &request_id,
        text,
        150,
        NOW,
        Some(&chat_id), // bound to shared chat
    )
    .await
    .unwrap();

    // Verify the message was published
    assert!(result.message_sequence.is_some());
    let seq = result.message_sequence.unwrap();

    // Verify the message appears in the chat
    let page = messages::list(&h.state, &h.owner, &chat_id, None)
        .await
        .unwrap();

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].body, text);
    assert_eq!(page.items[0].sequence, seq);
    assert_eq!(
        page.items[0].author_kind,
        crate::neev::content::types::AuthorKind::Human
    );
    assert_eq!(page.items[0].author_id, h.owner.identity.user_id);
}

#[tokio::test]
async fn shared_chat_message_is_idempotent_on_retry() {
    let h = Harness::new().await;
    let chat_id = h.chat(&h.owner, Visibility::Organization).await;
    let session = h.private_session().await;
    let turn = h.create_turn(&session.id, "Hello").await;

    let request_id = uuid::Uuid::new_v4().to_string();
    let text = "Idempotent message";

    h.reserve_steer_input(&session.id, turn.sequence, &request_id, text)
        .await;

    // First accept
    let first = accept_steer_input_and_publish(
        &h.state,
        &h.owner,
        &session.id,
        turn.sequence,
        &request_id,
        text,
        100,
        NOW,
        Some(&chat_id),
    )
    .await
    .unwrap();

    // Retry with same request_id
    let retry = accept_steer_input_and_publish(
        &h.state,
        &h.owner,
        &session.id,
        turn.sequence,
        &request_id,
        text,
        100,
        NOW,
        Some(&chat_id),
    )
    .await
    .unwrap();

    // Same sequence returned
    assert_eq!(first.message_sequence, retry.message_sequence);

    // Only one message in the chat
    let page = messages::list(&h.state, &h.owner, &chat_id, None)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
}

#[tokio::test]
async fn authorization_lost_prevents_shared_chat_publish() {
    let h = Harness::new().await;
    let peer = h.peer().await;

    // Peer creates a private chat (owner cannot write to it)
    let private_chat = h.chat(&peer, Visibility::Private).await;

    let session = h.private_session().await;
    let turn = h.create_turn(&session.id, "Hello").await;

    let request_id = uuid::Uuid::new_v4().to_string();
    let text = "Unauthorized message";

    h.reserve_steer_input(&session.id, turn.sequence, &request_id, text)
        .await;

    // Owner tries to publish to peer's private chat - should fail
    let result = accept_steer_input_and_publish(
        &h.state,
        &h.owner,
        &session.id,
        turn.sequence,
        &request_id,
        text,
        100,
        NOW,
        Some(&private_chat),
    )
    .await;

    // Should fail with auth error (NotFound because private chat is invisible)
    assert!(matches!(result, Err(ContentError::NotFound)));

    // Acceptance and publication are one transaction: the failed publish
    // leaves no accepted history, the reservation still pending, and no
    // message in the chat.
    let inputs = db::list_steer_inputs(&h.database, h.org, &session.id, turn.sequence)
        .await
        .unwrap();
    assert!(inputs.is_empty());
    let stored = db::get_steer_input(&h.database, h.org, &session.id, turn.sequence, &request_id)
        .await
        .unwrap()
        .expect("reservation survives the rollback");
    assert!(!stored.accepted);
    assert_eq!(stored.response_offset, None);
    let page = messages::list(&h.state, &peer, &private_chat, None)
        .await
        .unwrap();
    assert!(page.items.is_empty());
}

#[tokio::test]
async fn private_session_accepts_without_chat_message() {
    let h = Harness::new().await;
    let session = h.private_session().await;
    let turn = h.create_turn(&session.id, "Hello").await;

    let request_id = uuid::Uuid::new_v4().to_string();
    let text = "Private follow-up";

    h.reserve_steer_input(&session.id, turn.sequence, &request_id, text)
        .await;

    let result = accept_steer_input_and_publish(
        &h.state,
        &h.owner,
        &session.id,
        turn.sequence,
        &request_id,
        text,
        100,
        NOW,
        None, // no bound chat
    )
    .await
    .unwrap();

    // No message sequence because no chat was bound
    assert_eq!(result.message_sequence, None);
    assert_eq!(result.response_offset, 100);

    // Steer input is still recorded
    let inputs = db::list_steer_inputs(&h.database, h.org, &session.id, turn.sequence)
        .await
        .unwrap();
    assert_eq!(inputs.len(), 1);
}

#[tokio::test]
async fn archived_chat_prevents_publish() {
    let h = Harness::new().await;
    let chat_id = h.chat(&h.owner, Visibility::Organization).await;

    // Archive the chat
    let chat = resources::get(&h.state, &h.owner, &chat_id).await.unwrap();
    resources::update(
        &h.state,
        &h.owner,
        &chat_id,
        ResourcePatch {
            expected_auth_revision: chat.auth_revision,
            title: chat.title.clone(),
            node_id: chat.node_id,
            visibility: Visibility::Organization,
            archived: true,
        },
    )
    .await
    .unwrap();

    let session = h.private_session().await;
    let turn = h.create_turn(&session.id, "Hello").await;

    let request_id = uuid::Uuid::new_v4().to_string();
    let text = "Message to archived chat";

    h.reserve_steer_input(&session.id, turn.sequence, &request_id, text)
        .await;

    let result = accept_steer_input_and_publish(
        &h.state,
        &h.owner,
        &session.id,
        turn.sequence,
        &request_id,
        text,
        100,
        NOW,
        Some(&chat_id),
    )
    .await;

    // Should fail because chat is archived
    assert!(matches!(result, Err(ContentError::Archived)));
}

#[tokio::test]
async fn text_is_bounded_in_utf8_bytes_like_the_adapters() {
    let h = Harness::new().await;
    let session = h.private_session().await;
    let turn = h.create_turn(&session.id, "Hello").await;

    // 2048 four-byte scalars is exactly the cap; one more byte is over it even
    // though the scalar count is far below the old 32768-character limit.
    let at_cap = "\u{1F600}".repeat(super::STEER_TEXT_MAX_BYTES / 4);
    let over_cap = format!("{at_cap}a");
    for (text, ok) in [(at_cap.as_str(), true), (over_cap.as_str(), false)] {
        let request_id = uuid::Uuid::new_v4().to_string();
        h.reserve_steer_input(&session.id, turn.sequence, &request_id, text)
            .await;
        let result = accept_steer_input_and_publish(
            &h.state,
            &h.owner,
            &session.id,
            turn.sequence,
            &request_id,
            text,
            7,
            NOW,
            None,
        )
        .await;
        assert_eq!(result.is_ok(), ok, "len {}", text.len());
    }
    let inputs = db::list_steer_inputs(&h.database, h.org, &session.id, turn.sequence)
        .await
        .unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].text.len(), super::STEER_TEXT_MAX_BYTES);
}

#[tokio::test]
async fn retry_after_success_keeps_the_first_offset() {
    let h = Harness::new().await;
    let session = h.private_session().await;
    let turn = h.create_turn(&session.id, "Hello").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    h.reserve_steer_input(&session.id, turn.sequence, &request_id, "once")
        .await;
    for offset in [42, 99] {
        accept_steer_input_and_publish(
            &h.state,
            &h.owner,
            &session.id,
            turn.sequence,
            &request_id,
            "once",
            offset,
            NOW,
            None,
        )
        .await
        .unwrap();
    }
    let inputs = db::list_steer_inputs(&h.database, h.org, &session.id, turn.sequence)
        .await
        .unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].response_offset, Some(42));
}
