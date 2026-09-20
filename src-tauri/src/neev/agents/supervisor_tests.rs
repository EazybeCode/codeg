//! Dispatch behaviour, driven by a scripted supervisor: what we send, what we
//! do with the answer, and what we refuse to claim when there is no supervisor.

use super::*;
use crate::neev::agents::control_dto as dto;
use crate::neev::test_support as h;
use std::sync::Arc;

const NOW: i64 = 1_000;

/// A login ticket for a REAL account, because committing one goes through
/// `completion::complete`, which rechecks the actor's authority.
fn ticket_for(actor: &crate::neev::types::OrgContext) -> LoginTicket {
    LoginTicket {
        connection_id: "a".repeat(32),
        provider: Provider::Claude,
        generation: 1,
        org_id: actor.org_id,
        user_id: actor.identity.user_id,
        session_id: actor.identity.session_id.clone(),
        attempt_id: "b".repeat(32),
        operation_id: "c".repeat(32),
        lease_owner: "d".repeat(32),
        expires_at: NOW + 600,
    }
}

fn response(op: dto::Op, body: dto::ResultBody) -> dto::Response {
    dto::Response {
        coordinator: dto::Coordinator::new(
            dto::Counter::new("1".into()).unwrap(),
            dto::Id::new("1".repeat(32)).unwrap(),
        ),
        supervisor_boot_id: dto::Id::new("3".repeat(32)).unwrap(),
        sequence: dto::Counter::new("1".into()).unwrap(),
        request_id: dto::Id::new("5".repeat(32)).unwrap(),
        operation: op,
        outcome: dto::Outcome::Ok,
        body: Some(body),
        error: None,
    }
}

fn started() -> dto::Response {
    response(
        dto::Op::StartLogin,
        dto::ResultBody::StartLogin(dto::StartLoginResult {
            disposition: dto::Disposition::Accepted,
            poll_after_ms: dto::RetryMs::new(500).unwrap(),
        }),
    )
}

fn awaiting_claude() -> dto::Response {
    response(
        dto::Op::ObserveLogin,
        dto::ResultBody::ObserveLogin(dto::LoginSnapshot::AwaitingUser {
            expires_at: dto::UnixSec::new(NOW + 600).unwrap(),
            poll_after_ms: dto::RetryMs::new(500).unwrap(),
            challenge: dto::LoginChallenge::AuthorizationCode {
                verification_url: "https://claude.com/cai/oauth/authorize?x=1".into(),
                expires_at: dto::UnixSec::new(NOW + 500).unwrap(),
            },
        }),
    )
}

async fn state_with(
    supervisor: Supervisor,
) -> (Arc<ServerState>, crate::neev::types::OrgContext) {
    let database = h::database().await;
    let user = h::native_account(&database, "signer", "signer@example.test").await;
    let (org, _) = h::organization(&database, user, "admin").await;
    let mut state = ServerState::new(
        database.clone(),
        h::preview_config(),
        Arc::new(h::TestClock::new(NOW)),
    )
    .unwrap();
    state.supervisor = Arc::new(supervisor);
    let actor = h::scoped_context(&database, user, org, NOW).await;
    (Arc::new(state), actor)
}

#[tokio::test]
async fn with_no_supervisor_every_effect_is_refused_and_nothing_is_remembered() {
    let (state, actor) = state_with(Supervisor::Unavailable).await;
    assert!(!state.supervisor.is_available());
    assert_eq!(
        start_login(&state, &actor, &ticket_for(&actor)).await,
        Err(SupervisorError::Unavailable)
    );
    // Crucially: no login session is recorded, so nothing later can mistake
    // this for a sign-in that is under way.
    assert!(find_login(&state.agent_logins, &"b".repeat(32), actor.org_id, actor.identity.user_id).is_none());
}

#[tokio::test]
async fn starting_a_login_sends_the_servers_own_binding_and_remembers_it() {
    let (state, actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![Ok(
        started(),
    )])))
    .await;
    let ticket = ticket_for(&actor);
    start_login(&state, &actor, &ticket).await.unwrap();

    let Supervisor::Scripted(scripted) = state.supervisor.as_ref() else {
        panic!("scripted supervisor expected");
    };
    let sent = scripted.requests();
    assert_eq!(sent.len(), 1);
    let binding = sent[0].login_ticket().expect("start carries a ticket");
    // Every identifying field is the server's own stored value.
    assert_eq!(binding.binding.connection_id.as_str(), ticket.connection_id);
    assert_eq!(binding.binding.attempt_id.as_str(), ticket.attempt_id);
    assert_eq!(binding.binding.operation_id.as_str(), ticket.operation_id);
    assert_eq!(binding.binding.scope.org_id.get(), i64::from(actor.org_id));
    assert_eq!(binding.binding.scope.user_id.get(), i64::from(actor.identity.user_id));
    // Claude is now representable on the control plane at all.
    assert!(matches!(
        binding.binding.scope.provider,
        dto::Provider::Claude
    ));

    let remembered = find_login(&state.agent_logins, &ticket.attempt_id, actor.org_id, actor.identity.user_id)
        .expect("the sign-in is addressable for later polls");
    assert!(remembered.approval.is_none(), "nothing approved yet");
}

#[tokio::test]
async fn a_remembered_login_is_scoped_to_its_own_user() {
    let (state, actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![Ok(
        started(),
    )])))
    .await;
    let ticket = ticket_for(&actor);
    start_login(&state, &actor, &ticket).await.unwrap();
    // Knowing the attempt id is not authority: another account gets nothing.
    assert!(find_login(
        &state.agent_logins,
        &ticket.attempt_id,
        actor.org_id,
        actor.identity.user_id + 1,
    ).is_none());
    assert!(find_login(
        &state.agent_logins,
        &ticket.attempt_id,
        actor.org_id + 1,
        actor.identity.user_id,
    ).is_none());
}

#[tokio::test]
async fn observing_surfaces_the_claude_authorize_url_and_no_invented_user_code() {
    let (state, actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![
        Ok(started()),
        Ok(awaiting_claude()),
    ])))
    .await;
    let ticket = ticket_for(&actor);
    start_login(&state, &actor, &ticket).await.unwrap();
    let session = find_login(&state.agent_logins, &ticket.attempt_id, actor.org_id, actor.identity.user_id).unwrap();

    let snapshot = observe_login(&state, &session).await.unwrap();
    let dto::LoginSnapshot::AwaitingUser { challenge, .. } = snapshot else {
        panic!("expected an awaiting-user snapshot");
    };
    assert!(challenge
        .verification_url()
        .starts_with("https://claude.com/cai/oauth/authorize"));
    // Claude has no device code, and we do not manufacture one.
    assert!(challenge.user_code().is_none());

    // The approval is cached so the next poll can answer without another trip.
    let updated = find_login(&state.agent_logins, &ticket.attempt_id, actor.org_id, actor.identity.user_id).unwrap();
    assert!(updated.approval.is_some());
}

#[tokio::test]
async fn a_submitted_code_travels_bound_to_its_own_login() {
    let (state, actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![
        Ok(started()),
        Ok(response(
            dto::Op::SubmitCode,
            dto::ResultBody::SubmitCode(dto::LoginSnapshot::Verifying {
                expires_at: dto::UnixSec::new(NOW + 600).unwrap(),
                poll_after_ms: dto::RetryMs::new(500).unwrap(),
            }),
        )),
    ])))
    .await;
    let ticket = ticket_for(&actor);
    start_login(&state, &actor, &ticket).await.unwrap();
    let session = find_login(&state.agent_logins, &ticket.attempt_id, actor.org_id, actor.identity.user_id).unwrap();

    let snapshot = submit_code(
        &state,
        &session,
        dto::AuthorizationCode::new("PASTED-CODE".into()).unwrap(),
    )
    .await
    .unwrap();
    assert!(matches!(snapshot, dto::LoginSnapshot::Verifying { .. }));

    let Supervisor::Scripted(scripted) = state.supervisor.as_ref() else {
        panic!("scripted supervisor expected");
    };
    let sent = scripted.requests();
    let submit = sent.last().unwrap();
    assert!(matches!(submit.op(), dto::Op::SubmitCode));
    // The binding travels with the code, so a leaked code cannot be applied to
    // a different user's attempt.
    let binding = submit.login_binding().expect("submit carries its binding");
    assert_eq!(binding.attempt_id.as_str(), ticket.attempt_id);
    assert_eq!(binding.scope.user_id.get(), i64::from(actor.identity.user_id));
}

#[tokio::test]
async fn a_refused_start_leaves_no_addressable_login() {
    let (state, actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![Err(
        SupervisorError::Unavailable,
    )])))
    .await;
    let ticket = ticket_for(&actor);
    assert_eq!(
        start_login(&state, &actor, &ticket).await,
        Err(SupervisorError::Unavailable)
    );
    assert!(find_login(&state.agent_logins, &ticket.attempt_id, actor.org_id, actor.identity.user_id).is_none());
}

#[tokio::test]
async fn an_answer_for_the_wrong_operation_is_refused_rather_than_coerced() {
    let (state, actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![
        Ok(started()),
        // A supervisor answering an observe with a start result is out of
        // contract; we refuse rather than guessing what it meant.
        Ok(started()),
    ])))
    .await;
    let ticket = ticket_for(&actor);
    start_login(&state, &actor, &ticket).await.unwrap();
    let session = find_login(&state.agent_logins, &ticket.attempt_id, actor.org_id, actor.identity.user_id).unwrap();
    assert!(matches!(
        observe_login(&state, &session).await,
        Err(SupervisorError::Invalid)
    ));
}

#[tokio::test]
async fn the_live_login_registry_is_bounded() {
    let (_state, actor) = state_with(Supervisor::Unavailable).await;
    let registry = new_login_registry();
    let session = |expires_at: i64| LoginSession {
        org_id: actor.org_id,
        user_id: actor.identity.user_id,
        provider: Provider::Claude,
        binding: LoginBinding {
            scope: Scope {
                org_id: dto::OwnerId::new(3).unwrap(),
                user_id: dto::OwnerId::new(7).unwrap(),
                provider: dto::Provider::Claude,
            },
            connection_id: dto::Id::new("a".repeat(32)).unwrap(),
            generation: dto::Counter::new("1".into()).unwrap(),
            session_id: dto::SessionId::new(
                "11111111-1111-4111-8111-111111111111".to_owned(),
            )
            .unwrap(),
            attempt_id: dto::Id::new("b".repeat(32)).unwrap(),
            operation_id: dto::Id::new("c".repeat(32)).unwrap(),
        },
        expires_at,
        approval: None,
        actor: actor.clone(),
        code_attempts: 0,
    };
    for index in 0..(MAX_LIVE_LOGINS + 50) {
        remember_login(&registry, format!("attempt-{index}"), session(index as i64));
    }
    // Abandoned sign-ins cannot grow the process without bound; the newest is
    // always retained and the stalest is dropped.
    assert!(registry.lock().unwrap().len() <= MAX_LIVE_LOGINS);
    assert!(registry
        .lock()
        .unwrap()
        .contains_key(&format!("attempt-{}", MAX_LIVE_LOGINS + 49)));
}
