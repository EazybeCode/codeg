use std::sync::{atomic::Ordering, Arc};

use super::{
    intents,
    types::{BeginOutcome, LoginTicket, Provider, VerifiedSubscription},
};
use crate::neev::db;
use sea_orm::DatabaseConnection;

use crate::neev::{startup::ServerState, test_support as h, types::OrgContext};

pub(super) struct Harness {
    pub(super) state: Arc<ServerState>,
    pub(super) clock: Arc<h::TestClock>,
    pub(super) alice_a: OrgContext,
    pub(super) alice_b: OrgContext,
    pub(super) bob_a: OrgContext,
    pub(super) admin_a: OrgContext,
    pub(super) alice_second_session: OrgContext,
    pub(super) org_a: i32,
    pub(super) org_b: i32,
    pub(super) alice_id: i32,
    pub(super) _document_root: tempfile::TempDir,
}

impl Harness {
    pub(super) async fn new(database: DatabaseConnection) -> Self {
        let clock = Arc::new(h::TestClock::new(1_700_000_000));
        let document_root = h::owned_document_root();
        let mut config = h::preview_config();
        config.document_root = document_root.path().canonicalize().unwrap();
        let state = Arc::new(
            ServerState::new(database.clone(), config, clock.clone()).expect("agent test state"),
        );
        let admin_id = h::native_account(&database, "agent-admin", "admin@example.test").await;
        let alice_id = h::native_account(&database, "agent-alice", "alice@example.test").await;
        let bob_id = h::native_account(&database, "agent-bob", "bob@example.test").await;
        let (org_a, _) = h::organization(&database, admin_id, "admin").await;
        h::add_membership(&database, org_a, alice_id, "member").await;
        h::add_membership(&database, org_a, bob_id, "member").await;
        let (org_b, _) = h::organization(&database, alice_id, "admin").await;
        Self {
            alice_a: h::scoped_context(&database, alice_id, org_a, clock.now()).await,
            alice_b: h::scoped_context(&database, alice_id, org_b, clock.now()).await,
            bob_a: h::scoped_context(&database, bob_id, org_a, clock.now()).await,
            admin_a: h::scoped_context(&database, admin_id, org_a, clock.now()).await,
            alice_second_session: h::scoped_context(&database, alice_id, org_a, clock.now()).await,
            state,
            clock,
            org_a,
            org_b,
            alice_id,
            _document_root: document_root,
        }
    }

    pub(super) fn set_now(&self, now: i64) {
        self.clock.0.store(now, Ordering::SeqCst);
    }
}

trait TestClockNow {
    fn now(&self) -> i64;
}

impl TestClockNow for h::TestClock {
    fn now(&self) -> i64 {
        use crate::neev::clock::Clock;
        self.now_unix()
    }
}

pub(super) async fn begin(
    h: &Harness,
    actor: &OrgContext,
    provider: Provider,
    key: &str,
) -> LoginTicket {
    match intents::begin(&h.state, actor, provider, key)
        .await
        .unwrap()
    {
        BeginOutcome::Pending(ticket) => ticket,
        _ => panic!("expected pending login"),
    }
}

pub(super) fn account() -> VerifiedSubscription {
    VerifiedSubscription {
        account_label: "synthetic account".into(),
        storage_handle: uuid::Uuid::new_v4().simple().to_string(),
    }
}

pub(super) async fn cleanup_id(h: &Harness, ticket: &LoginTicket) -> String {
    db::one(
        &h.state.database,
        "SELECT id FROM neev_agent_operations WHERE connection_id=$1 AND kind='disconnect'",
        vec![ticket.connection_id.clone().into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "id")
    .unwrap()
}

// Only selected lifecycle fields are inspected; never print native account entities.
pub(super) async fn snapshot(
    h: &Harness,
    ticket: &LoginTicket,
) -> (
    i64,
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<i64>,
) {
    let row = db::one(&h.state.database, "SELECT c.generation,c.state,c.account_label,c.checked_at,c.error_code,c.storage_handle,a.state AS attempt_state,o.state AS operation_state,o.lease_owner,o.lease_expires_at FROM neev_agent_connections c JOIN neev_agent_attempts a ON a.connection_id=c.id JOIN neev_agent_operations o ON o.connection_id=c.id WHERE c.id=$1 AND a.id=$2 AND o.id=$3", vec![ticket.connection_id.clone().into(),ticket.attempt_id.clone().into(),ticket.operation_id.clone().into()]).await.unwrap().unwrap();
    (
        row.try_get("", "generation").unwrap(),
        row.try_get("", "state").unwrap(),
        row.try_get("", "account_label").unwrap(),
        row.try_get("", "checked_at").unwrap(),
        row.try_get("", "error_code").unwrap(),
        row.try_get("", "storage_handle").unwrap(),
        row.try_get("", "attempt_state").unwrap(),
        row.try_get("", "operation_state").unwrap(),
        row.try_get("", "lease_owner").unwrap(),
        row.try_get("", "lease_expires_at").unwrap(),
    )
}

pub(super) async fn lock_connection(
    h: &Harness,
    ticket: &LoginTicket,
) -> sea_orm::DatabaseTransaction {
    let tx = db::begin(&h.state.database).await.unwrap();
    db::one(
        &tx,
        "SELECT id FROM neev_agent_connections WHERE id=$1 FOR UPDATE",
        vec![ticket.connection_id.clone().into()],
    )
    .await
    .unwrap();
    tx
}
