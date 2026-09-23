//! Service-level guarantees for agent sessions: ownership, privacy, revision
//! conflicts, durable turn states, and the multi-turn context composer.

use super::*;
use crate::neev::catalog::nodes;
use crate::neev::content::{
    resources,
    types::{ResourceInput, ResourceKind, ResourcePatch, Visibility},
};
use crate::neev::test_support as h;
use crate::neev::types::{NodeInput, NodeKind};
use std::sync::Arc;

const NOW: i64 = 1_000;

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

    /// A second member of the SAME org — the interesting adversary for a
    /// "private by default" claim.
    async fn peer(&self) -> OrgContext {
        let user = h::native_account(&self.database, "peer", "peer@example.test").await;
        h::add_membership(&self.database, self.org, user, "member").await;
        h::scoped_context(&self.database, user, self.org, NOW).await
    }

    async fn session(&self) -> AgentSession {
        create(
            &self.state,
            &self.owner,
            SessionInput {
                title: "Launch plan".into(),
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
}

#[tokio::test]
async fn created_session_is_owned_private_and_listed() {
    let h = Harness::new().await;
    let session = h.session().await;
    assert_eq!(session.owner_id, h.owner.identity.user_id);
    assert_eq!(session.org_id, h.owner.org_id);
    assert_eq!(session.revision, 1);
    assert!(!session.archived);

    let listed = list(&h.state, &h.owner, false).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, session.id);

    // Stored as private — the schema pins this, and the row proves it.
    let visibility: String = crate::neev::db::one(
        &h.database,
        "SELECT visibility FROM neev_agent_sessions WHERE id=$1",
        vec![session.id.clone().into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "visibility")
    .unwrap();
    assert_eq!(visibility, "private");
}

#[tokio::test]
async fn another_member_of_the_same_org_cannot_see_or_touch_the_session() {
    let h = Harness::new().await;
    let session = h.session().await;
    let peer = h.peer().await;

    assert_eq!(get(&h.state, &peer, &session.id).await, Err(ContentError::NotFound));
    assert_eq!(list(&h.state, &peer, true).await.unwrap().len(), 0);
    assert_eq!(
        turns(&h.state, &peer, &session.id, None).await.err(),
        Some(ContentError::NotFound)
    );
    assert_eq!(
        rename(&h.state, &peer, &session.id, "theirs", 1).await.err(),
        Some(ContentError::NotFound)
    );
    assert_eq!(
        begin_turn(&h.state, &peer, &session.id, "hello", None).await.err(),
        Some(ContentError::NotFound)
    );
}

#[tokio::test]
async fn create_is_idempotent_for_a_repeated_request_id() {
    let h = Harness::new().await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let input = || SessionInput {
        title: "Launch plan".into(),
        provider: "claude".into(),
        model: "default".into(),
        bound_chat_id: None,
        node_id: None,
        effort: None,
        fast: false,
        request_id: request_id.clone(),
    };
    let first = create(&h.state, &h.owner, input()).await.unwrap();
    let second = create(&h.state, &h.owner, input()).await.unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(list(&h.state, &h.owner, true).await.unwrap().len(), 1);
}

#[tokio::test]
async fn rename_and_archive_are_revision_guarded() {
    let h = Harness::new().await;
    let session = h.session().await;
    // A stale revision loses rather than silently overwriting.
    assert_eq!(
        rename(&h.state, &h.owner, &session.id, "new", 99).await.err(),
        Some(ContentError::Conflict)
    );
    let renamed = rename(&h.state, &h.owner, &session.id, "  Renamed  ", session.revision)
        .await
        .unwrap();
    assert_eq!(renamed.title, "Renamed");
    assert_eq!(renamed.revision, session.revision + 1);
    // The same revision cannot be replayed.
    assert_eq!(
        rename(&h.state, &h.owner, &session.id, "again", session.revision)
            .await
            .err(),
        Some(ContentError::Conflict)
    );
    let archived = set_archived(&h.state, &h.owner, &session.id, true, renamed.revision)
        .await
        .unwrap();
    assert!(archived.archived);
    // Archived sessions leave the default list but remain reachable.
    assert_eq!(list(&h.state, &h.owner, false).await.unwrap().len(), 0);
    assert_eq!(list(&h.state, &h.owner, true).await.unwrap().len(), 1);
    assert_eq!(
        begin_turn(&h.state, &h.owner, &session.id, "hi", None).await.err(),
        Some(ContentError::Archived)
    );
    let reopened = set_archived(&h.state, &h.owner, &session.id, false, archived.revision)
        .await
        .unwrap();
    assert!(!reopened.archived);
    assert!(begin_turn(&h.state, &h.owner, &session.id, "hi", None).await.is_ok());
}

#[tokio::test]
async fn rename_rejects_empty_and_control_characters() {
    let h = Harness::new().await;
    let session = h.session().await;
    for bad in ["", "   ", "a\u{0}b", &"x".repeat(101)] {
        assert_eq!(
            rename(&h.state, &h.owner, &session.id, bad, session.revision)
                .await
                .err(),
            Some(ContentError::Invalid),
            "accepted {bad:?}"
        );
    }
}

#[tokio::test]
async fn a_turn_is_durable_before_the_runtime_is_asked() {
    let h = Harness::new().await;
    let session = h.session().await;
    let (turn, _) = begin_turn(&h.state, &h.owner, &session.id, "draft the memo", None)
        .await
        .unwrap();
    assert_eq!(turn.status, TurnStatus::Running);
    assert_eq!(turn.sequence, 1);
    assert_eq!(turn.prompt, "draft the memo");
    assert!(turn.response.is_empty());
    assert_eq!(turn.error_code, None);
    // It is already readable: a refresh before any output shows the prompt.
    let page = turns(&h.state, &h.owner, &session.id, None).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].run_id, turn.run_id);
}

#[tokio::test]
async fn a_session_runs_one_turn_at_a_time() {
    let h = Harness::new().await;
    let session = h.session().await;
    begin_turn(&h.state, &h.owner, &session.id, "first", None)
        .await
        .unwrap();
    // A second live turn would interleave two replies into one ordered history.
    assert_eq!(
        begin_turn(&h.state, &h.owner, &session.id, "second", None)
            .await
            .err(),
        Some(ContentError::Conflict)
    );
}

#[tokio::test]
async fn finishing_a_turn_settles_it_and_is_not_replayable() {
    let h = Harness::new().await;
    let session = h.session().await;
    let (turn, _) = begin_turn(&h.state, &h.owner, &session.id, "q", None).await.unwrap();
    persist_response(&h.database, &turn.run_id, "partial", NOW + 1)
        .await
        .unwrap();
    let mid = turns(&h.state, &h.owner, &session.id, None).await.unwrap();
    assert_eq!(mid.items[0].response, "partial");
    assert_eq!(mid.items[0].status, TurnStatus::Running);

    finish(
        &h.database,
        &turn.run_id,
        "partial and then some",
        None,
        TurnStatus::Completed,
        None,
        NOW + 2,
    )
    .await
    .unwrap();
    let done = turns(&h.state, &h.owner, &session.id, None).await.unwrap();
    assert_eq!(done.items[0].status, TurnStatus::Completed);
    assert_eq!(done.items[0].response, "partial and then some");
    assert_eq!(done.items[0].error_code, None);
    assert_eq!(done.items[0].completed_at, Some(NOW + 2));

    // A late cancel cannot rewrite a settled turn.
    finish(
        &h.database,
        &turn.run_id,
        "",
        None,
        TurnStatus::Canceled,
        Some("canceled"),
        NOW + 3,
    )
    .await
    .unwrap();
    let after = turns(&h.state, &h.owner, &session.id, None).await.unwrap();
    assert_eq!(after.items[0].status, TurnStatus::Completed);
    assert_eq!(after.items[0].response, "partial and then some");
}

#[tokio::test]
async fn a_failure_always_carries_a_reason() {
    let h = Harness::new().await;
    let session = h.session().await;
    let (turn, _) = begin_turn(&h.state, &h.owner, &session.id, "q", None).await.unwrap();
    // Even with no code supplied, the stored row is a well-formed failure — the
    // schema refuses a failed turn without one.
    finish(&h.database, &turn.run_id, "", None, TurnStatus::Failed, None, NOW + 1)
        .await
        .unwrap();
    let page = turns(&h.state, &h.owner, &session.id, None).await.unwrap();
    assert_eq!(page.items[0].status, TurnStatus::Failed);
    assert_eq!(page.items[0].error_code.as_deref(), Some("unavailable"));
}

#[tokio::test]
async fn finish_refuses_to_mark_a_turn_running() {
    let h = Harness::new().await;
    let session = h.session().await;
    let (turn, _) = begin_turn(&h.state, &h.owner, &session.id, "q", None).await.unwrap();
    assert_eq!(
        finish(
            &h.database,
            &turn.run_id,
            "",
            None,
            TurnStatus::Running,
            None,
            NOW + 1
        )
        .await
        .err(),
        Some(ContentError::Invalid)
    );
}

#[tokio::test]
async fn startup_reconciliation_settles_turns_left_running_by_a_dead_process() {
    let h = Harness::new().await;
    let session = h.session().await;
    let (turn, _) = begin_turn(&h.state, &h.owner, &session.id, "long task", None)
        .await
        .unwrap();
    persist_response(&h.database, &turn.run_id, "half an answer", NOW + 1)
        .await
        .unwrap();

    let settled = reconcile_interrupted(&h.database, NOW + 60).await.unwrap();
    assert_eq!(settled, 1);
    let page = turns(&h.state, &h.owner, &session.id, None).await.unwrap();
    assert_eq!(page.items[0].status, TurnStatus::Failed);
    assert_eq!(page.items[0].error_code.as_deref(), Some("interrupted"));
    // The partial output is kept: it is what the user actually saw.
    assert_eq!(page.items[0].response, "half an answer");
    // Idempotent — a second startup settles nothing.
    assert_eq!(reconcile_interrupted(&h.database, NOW + 61).await.unwrap(), 0);
    // And the session can take a new turn afterwards (no phantom live run).
    assert!(begin_turn(&h.state, &h.owner, &session.id, "retry", None).await.is_ok());
}

#[tokio::test]
async fn turn_by_run_is_scoped_to_the_owner() {
    let h = Harness::new().await;
    let session = h.session().await;
    let (turn, _) = begin_turn(&h.state, &h.owner, &session.id, "q", None).await.unwrap();
    let peer = h.peer().await;
    // Knowing a run id is not authority.
    assert_eq!(
        turn_by_run(&h.state, &peer, &turn.run_id).await.err(),
        Some(ContentError::NotFound)
    );
    assert_eq!(
        turn_by_run(&h.state, &h.owner, &turn.run_id)
            .await
            .unwrap()
            .id,
        turn.id
    );
}

#[tokio::test]
async fn binding_a_chat_requires_current_read_access_to_it() {
    let h = Harness::new().await;
    let peer = h.peer().await;
    // The peer's PRIVATE chat is invisible to the owner, so it cannot be bound.
    let private_chat = h.chat(&peer, Visibility::Private).await;
    assert_eq!(
        create(
            &h.state,
            &h.owner,
            SessionInput {
                title: "Bound".into(),
                provider: "claude".into(),
                model: "default".into(),
                bound_chat_id: Some(private_chat),
                node_id: None,
                effort: None,
                fast: false,
                request_id: uuid::Uuid::new_v4().to_string(),
            },
        )
        .await
        .err(),
        Some(ContentError::NotFound)
    );
    // An org-visible chat the owner can read is bindable.
    let shared_chat = h.chat(&peer, Visibility::Organization).await;
    let bound = create(
        &h.state,
        &h.owner,
        SessionInput {
            title: "Bound".into(),
            provider: "claude".into(),
            model: "default".into(),
            bound_chat_id: Some(shared_chat.clone()),
            node_id: None,
            effort: None,
            fast: false,
            request_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    assert_eq!(bound.bound_chat_id.as_deref(), Some(shared_chat.as_str()));
}

#[tokio::test]
async fn unknown_provider_or_model_is_refused() {
    let h = Harness::new().await;
    for (provider, model) in [("gpt", "default"), ("claude", ""), ("claude", &"m".repeat(81))] {
        assert_eq!(
            create(
                &h.state,
                &h.owner,
                SessionInput {
                    title: "T".into(),
                    provider: provider.into(),
                    model: model.into(),
                    bound_chat_id: None,
                    node_id: None,
                    effort: None,
                    fast: false,
                    request_id: uuid::Uuid::new_v4().to_string(),
                },
            )
            .await
            .err(),
            Some(ContentError::Invalid),
            "accepted {provider}/{model}"
        );
    }
}

#[tokio::test]
async fn a_session_cannot_be_created_with_a_model_its_runs_would_refuse() {
    let h = Harness::new().await;
    // Each of these passes a "no control characters, 1..=80" check but is
    // refused by the runtime's argv rule. Accepting them at creation would
    // produce a session whose EVERY turn fails — unrunnable, with no hint at
    // the point the user could still fix it.
    for bad in [
        "gpt 4o",
        "--dangerously-skip-permissions",
        "model;rm -rf /",
        "modèle",
    ] {
        assert_eq!(
            create(
                &h.state,
                &h.owner,
                SessionInput {
                    title: "T".into(),
                    provider: "claude".into(),
                    model: bad.into(),
                    bound_chat_id: None,
                    node_id: None,
                    effort: None,
                    fast: false,
                    request_id: uuid::Uuid::new_v4().to_string(),
                },
            )
            .await
            .err(),
            Some(ContentError::Invalid),
            "accepted model {bad:?} that the runtime would refuse"
        );
        assert!(
            !crate::neev::agents::execution::safe_model(bad),
            "test fixture {bad:?} is actually runnable"
        );
    }
    // A real model name is accepted, and so is the provider's own default.
    for good in ["default", "claude-opus-4-1", "gpt-5.1", "o3_mini"] {
        assert!(
            create(
                &h.state,
                &h.owner,
                SessionInput {
                    title: "T".into(),
                    provider: "claude".into(),
                    model: good.into(),
                    bound_chat_id: None,
                    node_id: None,
                    effort: None,
                    fast: false,
                    request_id: uuid::Uuid::new_v4().to_string(),
                },
            )
            .await
            .is_ok(),
            "rejected usable model {good:?}"
        );
    }
}

#[tokio::test]
async fn an_empty_or_oversized_prompt_is_refused() {
    let h = Harness::new().await;
    let session = h.session().await;
    for bad in ["".to_owned(), "x".repeat(32_769)] {
        assert_eq!(
            begin_turn(&h.state, &h.owner, &session.id, &bad, None).await.err(),
            Some(ContentError::Invalid)
        );
    }
}

#[tokio::test]
async fn turn_history_pages_in_order() {
    let h = Harness::new().await;
    let session = h.session().await;
    for index in 1..=5 {
        let (turn, _) = begin_turn(&h.state, &h.owner, &session.id, &format!("q{index}"), None)
            .await
            .unwrap();
        finish(
            &h.database,
            &turn.run_id,
            &format!("a{index}"),
            None,
            TurnStatus::Completed,
            None,
            NOW + index,
        )
        .await
        .unwrap();
    }
    let all = turns(&h.state, &h.owner, &session.id, None).await.unwrap();
    assert_eq!(all.items.len(), 5);
    assert_eq!(
        all.items.iter().map(|t| t.sequence).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
    // The cursor resumes where a reader left off.
    let tail = turns(&h.state, &h.owner, &session.id, Some(3)).await.unwrap();
    assert_eq!(
        tail.items.iter().map(|t| t.sequence).collect::<Vec<_>>(),
        vec![4, 5]
    );
}

#[test]
fn conversation_prompt_replays_only_completed_exchanges_oldest_first() {
    let turn = |sequence: i64, prompt: &str, response: &str, status: TurnStatus| AgentTurn {
        id: format!("id{sequence}"),
        session_id: "s".into(),
        sequence,
        run_id: format!("run{sequence}"),
        prompt: prompt.into(),
        response: response.into(),
        activity: Vec::new(),
        status,
        error_code: None,
        provider: "claude".into(),
        model: "default".into(),
        created_at: 0,
        updated_at: 0,
        completed_at: None,
    };
    let history = vec![
        turn(1, "first", "one", TurnStatus::Completed),
        // A failed turn produced nothing trustworthy: replaying it would invent
        // context the user never received.
        turn(2, "broken", "half", TurnStatus::Failed),
        turn(3, "second", "two", TurnStatus::Completed),
    ];
    let composed = conversation_prompt(&history, "third");
    assert_eq!(
        composed,
        "User: first\n\nAssistant: one\n\nUser: second\n\nAssistant: two\n\nUser: third"
    );
    assert!(!composed.contains("broken"));

    // With no usable history the prompt is sent verbatim — no empty scaffolding.
    assert_eq!(conversation_prompt(&[], "just this"), "just this");
    assert_eq!(
        conversation_prompt(&[turn(1, "p", "", TurnStatus::Completed)], "next"),
        "next"
    );
}

#[test]
fn conversation_prompt_drops_whole_old_exchanges_to_stay_in_budget() {
    let big = "x".repeat(20_000);
    let turn = |sequence: i64, response: &str| AgentTurn {
        id: format!("id{sequence}"),
        session_id: "s".into(),
        sequence,
        run_id: format!("run{sequence}"),
        prompt: "p".into(),
        response: response.into(),
        activity: Vec::new(),
        status: TurnStatus::Completed,
        error_code: None,
        provider: "claude".into(),
        model: "default".into(),
        created_at: 0,
        updated_at: 0,
        completed_at: None,
    };
    let history = vec![turn(1, "oldest"), turn(2, &big), turn(3, &big)];
    let composed = conversation_prompt(&history, "now");
    // The newest exchange always survives; the oldest is dropped ENTIRELY
    // rather than truncated mid-exchange.
    assert!(composed.ends_with("User: now"));
    assert!(!composed.contains("oldest"));
}

#[test]
fn failure_codes_map_cancel_distinctly_from_a_provider_fault() {
    assert_eq!(
        failure_code("canceled"),
        (TurnStatus::Canceled, "canceled")
    );
    assert_eq!(failure_code("io"), (TurnStatus::Failed, "io"));
    // An unrecognised reason is not invented into the domain.
    assert_eq!(failure_code("who knows"), (TurnStatus::Failed, "provider"));
}

#[test]
fn execution_errors_map_to_stored_codes() {
    use crate::neev::agents::execution::ExecutionError;
    assert_eq!(execution_error_code(&ExecutionError::Disabled), "disabled");
    assert_eq!(
        execution_error_code(&ExecutionError::Unavailable),
        "unavailable"
    );
    assert_eq!(execution_error_code(&ExecutionError::Isolation), "isolation");
    assert_eq!(execution_error_code(&ExecutionError::Provider), "provider");
    assert_eq!(
        execution_error_code(&ExecutionError::Unauthorized),
        "unauthorized"
    );
    // Every mapped code is one the schema actually permits.
    for error in [
        ExecutionError::Disabled,
        ExecutionError::Unauthorized,
        ExecutionError::Isolation,
        ExecutionError::Provider,
        ExecutionError::Unavailable,
    ] {
        assert!(crate::neev::db::TURN_ERROR_CODES.contains(&execution_error_code(&error)));
    }
}

/// A team invocation must not need the provider's catalogue to start.
///
/// Live regression: once Codex was connected, every ask in a team chat answered
/// 503 with the human's message already posted and no holder or turn behind it.
/// `ensure_team_session` was the one caller in this file that validated while
/// holding `resources::begin`'s transaction, and validation reaches discovery,
/// which opens its own transaction and waits on the `neev_schema_version` row
/// the outer one is holding. Before the provider was connected discovery
/// returned early and never touched the database, so connecting is what armed
/// it.
///
/// This test cannot reproduce the deadlock: it needs the Postgres-only
/// `FOR UPDATE` in `lock_admission_guard`, and the harness runs on SQLite. What
/// it does pin is the reason the deadlock was reachable — an ask claiming
/// nothing now settles without consulting the catalogue at all — and that the
/// narrowing did not turn into a blanket yes.
#[tokio::test]
async fn a_team_ask_claiming_nothing_needs_no_catalogue() {
    let harness = Harness::new().await;
    let chat = harness.chat(&harness.owner, Visibility::Organization).await;

    // Discovery cannot succeed here (no runtime is bound), which is exactly the
    // state a caller is in when the probe fails in production.
    let session = ensure_team_session(
        &harness.state,
        &harness.owner,
        &chat,
        "codex",
        "default",
        "Team invocation",
        None,
        false,
    )
    .await
    .expect("a default ask must not depend on the catalogue");
    assert_eq!(session.provider, "codex");
    assert_eq!(session.model, "default");
    assert_eq!(session.effort, None);
    assert!(!session.fast);
}

/// The narrowing above is about WHEN the catalogue is consulted, never about
/// what it decides. Anything actually claimed is still checked.
#[tokio::test]
async fn a_claimed_level_or_fast_mode_is_still_refused() {
    let harness = Harness::new().await;
    let validate = async |effort: Option<&str>, fast: bool| {
        crate::neev::agents::capabilities::validated_for(
            &harness.state,
            &harness.owner,
            "codex",
            "default",
            effort,
            fast,
        )
        .await
    };

    // Claiming nothing: settled without the catalogue.
    assert_eq!(validate(None, false).await, Ok((None, false)));
    // An empty field is the same statement as an absent one.
    assert_eq!(validate(Some(""), false).await, Ok((None, false)));

    // A level codex takes is still accepted...
    assert_eq!(
        validate(Some("medium"), false).await,
        Ok((Some("medium".into()), false))
    );
    // ...and one it does not is still refused, catalogue or no catalogue.
    assert_eq!(validate(Some("max"), false).await, Err(()));
    assert_eq!(validate(Some("nonsense"), false).await, Err(()));
    // Fast mode is a claim too, and it is answered from the static profile when
    // the catalogue cannot be reached: codex's speed controls are provider-wide,
    // so its `default` does offer it...
    assert_eq!(validate(None, true).await, Ok((None, true)));
    // ...while claude's `default` does not, because the CLI picks the model and
    // offering Fast would promise something about one nobody named. An
    // unreachable catalogue is no licence to enable it.
    assert_eq!(
        crate::neev::agents::capabilities::validated_for(
            &harness.state,
            &harness.owner,
            "claude",
            "default",
            None,
            true,
        )
        .await,
        Err(())
    );
}

/// An unknown model is not waved through by the short circuit.
#[tokio::test]
async fn an_unsafe_model_is_refused_before_anything_else() {
    let harness = Harness::new().await;
    for model in ["--oops", "", "has space"] {
        assert_eq!(
            crate::neev::agents::capabilities::validated_for(
                &harness.state,
                &harness.owner,
                "codex",
                model,
                None,
                false,
            )
            .await,
            Err(()),
            "{model} must not be accepted"
        );
    }
}
