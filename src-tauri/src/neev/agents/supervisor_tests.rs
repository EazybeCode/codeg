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
        start_login(&state, &actor, &ticket_for(&actor), None).await,
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
    start_login(&state, &actor, &ticket, None).await.unwrap();

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
    start_login(&state, &actor, &ticket, None).await.unwrap();
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
    start_login(&state, &actor, &ticket, None).await.unwrap();
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
    start_login(&state, &actor, &ticket, None).await.unwrap();
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
        start_login(&state, &actor, &ticket, None).await,
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
    start_login(&state, &actor, &ticket, None).await.unwrap();
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
        lease_owner: "d".repeat(32),
        approval: None,
        actor: actor.clone(),
        code_attempts: 0,
        callback_key: None,
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

/// A check ticket for a real actor. `StatusTicket::for_test` exists so the
/// dispatch under test can be exercised without standing up a connected
/// connection and an enabled epoch first.
fn check_ticket(actor: &crate::neev::types::OrgContext) -> crate::neev::agents::status::StatusTicket {
    crate::neev::agents::status::StatusTicket::for_test(
        1,
        "1".repeat(32),
        actor.org_id,
        actor.identity.user_id,
        Provider::Claude,
        "4".repeat(32),
        1,
        actor.identity.session_id.clone(),
        "8".repeat(32),
        "7".repeat(32),
        NOW,
        NOW + 300,
    )
}

fn reconcile_page(entries: usize, revision: &str, more: bool) -> dto::Response {
    let page = dto::ReconcilePage {
        snapshot_revision: dto::Counter::new(revision.to_owned()).unwrap(),
        next_after_entry_id: more
            .then(|| dto::Id::new(format!("{:032x}", entries.saturating_sub(1))).unwrap()),
        entries: (0..entries)
            .map(|index| dto::ReconcileEntry {
                entry_id: dto::Id::new(format!("{index:032x}")).unwrap(),
                binding: dto::ConnectionBinding {
                    scope: dto::Scope {
                        org_id: dto::OwnerId::new(1).unwrap(),
                        user_id: dto::OwnerId::new(2).unwrap(),
                        provider: dto::Provider::Claude,
                    },
                    connection_id: dto::Id::new("4".repeat(32)).unwrap(),
                    generation: dto::Counter::new("1".into()).unwrap(),
                },
                attempt_id: dto::Id::new("5".repeat(32)).unwrap(),
                operation_id: dto::Id::new("6".repeat(32)).unwrap(),
                phase: dto::RegistryPhase::PendingLogin,
                storage_handle: None,
                receipt_id: None,
                cleanup_operation_id: None,
            })
            .collect(),
    };
    response(dto::Op::ReconcilePage, dto::ResultBody::ReconcilePage(page))
}

#[tokio::test]
async fn reconciling_walks_pages_until_a_short_one() {
    // A page that names a continuation is followed; one that does not ends the
    // walk. The page states this itself rather than us inferring it from length.
    let (state, _actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![
        Ok(reconcile_page(RECONCILE_LIMIT as usize, "7", true)),
        Ok(reconcile_page(3, "7", false)),
    ])))
    .await;
    assert_eq!(reconcile(&state).await, Ok(RECONCILE_LIMIT as usize + 3));

    let Supervisor::Scripted(scripted) = state.supervisor.as_ref() else {
        panic!("scripted supervisor expected");
    };
    let sent = scripted.requests();
    assert_eq!(sent.len(), 2, "one page too few or too many");
    // The second request must carry BOTH cursor fields or neither — the DTO
    // rejects a half-specified cursor.
    match &sent[1].body {
        dto::RequestBody::ReconcilePage(body) => {
            assert!(body.snapshot_revision.is_some());
            assert!(body.after_entry_id.is_some());
        }
        _ => panic!("expected a reconcile page request"),
    }
}

#[tokio::test]
async fn a_supervisor_that_never_ends_its_pages_is_not_reconciled() {
    // Every page points at another, forever. Readiness must NOT follow from a walk that
    // never terminates, so this is a failure rather than a silent give-up.
    let answers = (0..(RECONCILE_MAX_PAGES + 5))
        .map(|_| Ok(reconcile_page(RECONCILE_LIMIT as usize, "7", true)))
        .collect();
    let (state, _actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(answers))).await;
    assert_eq!(reconcile(&state).await, Err(SupervisorError::Invalid));
}

#[tokio::test]
async fn readiness_cannot_be_reached_without_a_successful_reconcile() {
    // The supervisor refuses to reconcile: adoption fails, so checks stay
    // disabled. This is the property that stops a coordinator asserting a
    // readiness it never established.
    let (state, _actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![Err(
        SupervisorError::Unavailable,
    )])))
    .await;
    assert!(adopt_status_epoch(&state).await.is_none());
}

#[tokio::test]
async fn adopting_an_epoch_enables_checks_after_reconciling() {
    let (state, _actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![Ok(
        reconcile_page(0, "7", false),
    )])))
    .await;
    let epoch = adopt_status_epoch(&state)
        .await
        .expect("a reconciled epoch is adopted");
    assert_eq!(epoch.epoch(), 1);
    // Reconcile happened BEFORE enablement, which is the ordering the protocol
    // requires: activation clears the previous owner's records.
    let Supervisor::Scripted(scripted) = state.supervisor.as_ref() else {
        panic!("scripted supervisor expected");
    };
    assert!(matches!(
        scripted.requests().first().map(|request| request.op()),
        Some(dto::Op::ReconcilePage)
    ));
}

#[tokio::test]
async fn with_no_supervisor_no_epoch_is_adopted() {
    let (state, _actor) = state_with(Supervisor::Unavailable).await;
    assert!(adopt_status_epoch(&state).await.is_none());
}

#[tokio::test]
async fn a_check_maps_every_supervisor_outcome_without_inventing_one() {
    use crate::neev::agents::status::{CheckUnavailable, CheckedAccountOutcome};
    let observed = dto::UnixSec::new(NOW + 5).unwrap();
    let cases: Vec<(dto::ResultBody, &str)> = vec![
        (
            dto::ResultBody::CheckConnection(dto::CheckResult::Queued {
                check_id: dto::Id::new("7".repeat(32)).unwrap(),
                poll_after_ms: dto::RetryMs::new(250).unwrap(),
            }),
            "pending",
        ),
        (
            dto::ResultBody::CheckConnection(dto::CheckResult::Finished {
                check_id: dto::Id::new("7".repeat(32)).unwrap(),
                observed_at: observed.clone(),
                account: dto::AccountStatus::Supported {
                    account_label: dto::Label::new("work@example.test".into()).unwrap(),
                },
            }),
            "supported",
        ),
        (
            dto::ResultBody::CheckConnection(dto::CheckResult::Finished {
                check_id: dto::Id::new("7".repeat(32)).unwrap(),
                observed_at: observed.clone(),
                account: dto::AccountStatus::Disconnected,
            }),
            "disconnected",
        ),
        (
            dto::ResultBody::CheckConnection(dto::CheckResult::Unavailable {
                check_id: dto::Id::new("7".repeat(32)).unwrap(),
                reason: dto::CheckFailure::ProviderUnavailable,
            }),
            "unavailable",
        ),
    ];
    for (body, expected) in cases {
        let (state, actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![Ok(
            response(dto::Op::CheckConnection, body),
        )])))
        .await;
        let ticket = check_ticket(&actor);
        let progress = check_connection(&state, &ticket).await.unwrap();
        let actual = match progress {
            CheckProgress::Pending => "pending",
            CheckProgress::Observed(CheckedAccountOutcome::Supported { .. }) => "supported",
            CheckProgress::Observed(CheckedAccountOutcome::Disconnected { .. }) => "disconnected",
            CheckProgress::Observed(CheckedAccountOutcome::Unsupported { .. }) => "unsupported",
            CheckProgress::Observed(CheckedAccountOutcome::Unavailable(
                CheckUnavailable::ProviderUnavailable,
            )) => "unavailable",
            CheckProgress::Observed(CheckedAccountOutcome::Unavailable(_)) => "other",
        };
        assert_eq!(actual, expected);
    }
}

#[tokio::test]
async fn a_check_answered_with_the_wrong_operation_is_refused() {
    let (state, actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![Ok(
        started(),
    )])))
    .await;
    assert!(matches!(
        check_connection(&state, &check_ticket(&actor)).await,
        Err(SupervisorError::Invalid)
    ));
}

/// The credential handle a `Prepared` supervisor snapshot carries.
const HANDLE: &str = "fedcba9876543210fedcba9876543210";

fn prepared() -> dto::Response {
    response(
        dto::Op::ObserveLogin,
        dto::ResultBody::ObserveLogin(dto::LoginSnapshot::Prepared {
            receipt_id: dto::Id::new("7".repeat(32)).unwrap(),
            storage_handle: dto::Id::new(HANDLE.to_owned()).unwrap(),
            account_label: Some(dto::Label::new("Claude".to_owned()).unwrap()),
        }),
    )
}

fn settle_ack() -> dto::Response {
    response(
        dto::Op::SettleLogin,
        dto::ResultBody::SettleLogin(dto::SettleLoginResult {
            state: dto::CommittedState::Committed,
            receipt_id: dto::Id::new("7".repeat(32)).unwrap(),
            storage_handle: dto::Id::new(HANDLE.to_owned()).unwrap(),
        }),
    )
}

/// Start a REAL connect, so the attempt and the leased connect operation exist
/// in the database exactly as production writes them.
///
/// The lease owner is deliberately not chosen by the test: it is whatever
/// `intents::begin` minted and stored, which is the only value
/// `completion::lock_ticket` accepts. A test that supplied its own would agree
/// with a bug that invented one.
async fn real_connect(
    state: &ServerState,
    actor: &crate::neev::types::OrgContext,
) -> LoginTicket {
    match crate::neev::agents::intents::begin(state, actor, Provider::Claude, "regression-connect")
        .await
        .unwrap()
    {
        crate::neev::agents::types::BeginOutcome::Pending(ticket) => ticket,
        crate::neev::agents::types::BeginOutcome::Settled(_) => {
            panic!("a first connect is pending")
        }
    }
}

/// The end of a sign-in, driven through the real completion gate.
///
/// Regression for a commit that failed silently and totally: the supervisor had
/// signed the user in and stored their credential, but the ticket we rebuilt to
/// publish it carried no lease owner, so `lock_ticket` refused it and every
/// poll kept answering `awaiting` until the attempt expired. Asserting
/// `Connected` alone would not be enough of a guard — both callers of
/// `advance_login` discard its error — so this asserts the credential is
/// actually readable afterwards.
#[tokio::test]
async fn a_prepared_login_publishes_the_credential_through_the_real_completion_gate() {
    let (state, actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![
        Ok(started()),
        Ok(prepared()),
        Ok(settle_ack()),
    ])))
    .await;
    let ticket = real_connect(&state, &actor).await;
    start_login(&state, &actor, &ticket, None).await.unwrap();
    let session = find_login(
        &state.agent_logins,
        &ticket.attempt_id,
        actor.org_id,
        actor.identity.user_id,
    )
    .unwrap();

    assert_eq!(
        advance_login(&state, &actor, &session).await,
        Ok(LoginProgress::Connected)
    );

    // The product outcome rather than a label: the handle the launcher mounts is
    // now deliverable, which `intents::credential` reports only for a CONNECTED
    // connection.
    assert_eq!(
        crate::neev::agents::intents::credential(&state, &actor, Provider::Claude)
            .await
            .unwrap()
            .as_deref(),
        Some(HANDLE)
    );
    // The attempt is finished, so polls stop asking for an approval the user has
    // already given.
    assert_ne!(
        crate::neev::agents::intents::attempt(&state, &actor, Provider::Claude, &ticket.attempt_id)
            .await
            .unwrap()
            .state,
        "pending"
    );
    assert!(find_login(
        &state.agent_logins,
        &ticket.attempt_id,
        actor.org_id,
        actor.identity.user_id
    )
    .is_none());
}

/// The gate the fix has to keep: holding the attempt is not enough, the commit
/// must hold the operation's lease. This is what stops a coordinator that lost
/// the lease from publishing a credential for a login it no longer owns.
#[tokio::test]
async fn a_login_without_the_operations_lease_still_cannot_publish_a_credential() {
    let (state, actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![
        Ok(started()),
        Ok(prepared()),
    ])))
    .await;
    let ticket = real_connect(&state, &actor).await;
    start_login(&state, &actor, &ticket, None).await.unwrap();
    let mut session = find_login(
        &state.agent_logins,
        &ticket.attempt_id,
        actor.org_id,
        actor.identity.user_id,
    )
    .unwrap();
    // Everything else about this login is genuine; only the lease is not ours.
    session.lease_owner = "9".repeat(32);

    assert!(matches!(
        advance_login(&state, &actor, &session).await,
        Err(SupervisorError::Invalid)
    ));
    // Nothing was published, so no credential can be delivered.
    assert_eq!(
        crate::neev::agents::intents::credential(&state, &actor, Provider::Claude)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_relayed_callback_travels_bound_and_a_rejection_is_not_success() {
    let mut rejected = response(
        dto::Op::DeliverCallback,
        dto::ResultBody::DeliverCallback(dto::LoginSnapshot::Verifying {
            expires_at: dto::UnixSec::new(NOW + 600).unwrap(),
            poll_after_ms: dto::RetryMs::new(500).unwrap(),
        }),
    );
    rejected.outcome = dto::Outcome::Error;
    rejected.body = None;
    rejected.error = Some(dto::AppError {
        code: dto::ErrorCode::Rejected,
        retry_after_ms: None,
    });
    let (state, actor) = state_with(Supervisor::Scripted(ScriptedSupervisor::new(vec![
        Ok(started()),
        Ok(rejected),
        Ok(response(
            dto::Op::DeliverCallback,
            dto::ResultBody::DeliverCallback(dto::LoginSnapshot::Verifying {
                expires_at: dto::UnixSec::new(NOW + 600).unwrap(),
                poll_after_ms: dto::RetryMs::new(500).unwrap(),
            }),
        )),
    ])))
    .await;
    let ticket = ticket_for(&actor);
    start_login(&state, &actor, &ticket, Some(dto::LoginMethod::Browser))
        .await
        .unwrap();
    let session = find_login(
        &state.agent_logins,
        &ticket.attempt_id,
        actor.org_id,
        actor.identity.user_id,
    )
    .unwrap();
    let query = || dto::CallbackQuery::new("code=c&state=s".into()).unwrap();
    // A mismatch the supervisor refused is an input error, never "connected".
    assert!(matches!(
        deliver_callback(&state, &session, query()).await,
        Err(SupervisorError::Rejected)
    ));
    assert!(matches!(
        AgentError::from(SupervisorError::Rejected),
        AgentError::InvalidInput
    ));
    let snapshot = deliver_callback(&state, &session, query()).await.unwrap();
    assert!(matches!(snapshot, dto::LoginSnapshot::Verifying { .. }));

    let Supervisor::Scripted(scripted) = state.supervisor.as_ref() else {
        panic!("scripted supervisor expected");
    };
    let sent = scripted.requests();
    // The start asked for direct sign-in explicitly.
    match &sent[0].body {
        dto::RequestBody::StartLogin(body) => {
            assert!(body.method == Some(dto::LoginMethod::Browser))
        }
        _ => panic!("first request is not a start"),
    }
    let deliver = sent.last().unwrap();
    assert!(matches!(deliver.op(), dto::Op::DeliverCallback));
    let binding = deliver.login_binding().expect("deliver carries its binding");
    assert_eq!(binding.attempt_id.as_str(), ticket.attempt_id);
    assert_eq!(
        binding.scope.user_id.get(),
        i64::from(actor.identity.user_id)
    );
}
