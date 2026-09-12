use std::sync::{atomic::Ordering, Arc};

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
