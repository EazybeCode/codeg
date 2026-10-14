//! Database-backed Teams behaviour: admin bindings, fail-closed access
//! resolution, and shared channel threads (inbox dedupe, one live run per
//! thread, attributed context, per-member execution identity).

use std::sync::Arc;

use sea_orm::TransactionTrait;

use super::access::{self, ChannelAccess};
use super::bindings::{self, BindingError};
use super::directory::{Directory, FakeDirectory};
use super::graph::Membership;
use super::codes::Refusal;
use super::threads::{self, Admission, ThreadKey};
use crate::neev::agents::sessions::{self, SessionInput, TurnStatus};
use crate::neev::content::types::ContentError;
use crate::neev::error::AuthError;
use crate::neev::startup::ServerState;
use crate::neev::test_support as h;
use crate::neev::types::OrgContext;

const NOW: i64 = 1_000;
const TENANT: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OTHER_TENANT: &str = "cccccccc-bbbb-cccc-dddd-eeeeeeeeeeee";
const ASHA_OID: &str = "0f0f0f0f-1111-2222-3333-444444444444";
const BOB_OID: &str = "0e0e0e0e-1111-2222-3333-444444444444";
const TEAM: &str = "19:team@thread.tacv2";
const CHANNEL: &str = "19:proposals@thread.tacv2";
const OTHER_CHANNEL: &str = "19:research@thread.tacv2";

struct World {
    db: sea_orm::DatabaseConnection,
    state: Arc<ServerState>,
    org: i32,
    admin: OrgContext,
    asha: OrgContext,
    bob: OrgContext,
    project: i32,
    outsider: OrgContext,
    outsider_project: i32,
}

async fn new_project(db: &sea_orm::DatabaseConnection, org: i32, by: i32, name: &str) -> i32 {
    crate::neev::db::one(db, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,NULL,'project',$2,1,$3) RETURNING id", vec![org.into(), name.into(), by.into()])
        .await
        .unwrap()
        .unwrap()
        .try_get("", "id")
        .unwrap()
}

async fn world() -> World {
    let db = h::database().await;
    let admin_id = h::native_account(&db, "teams-admin", "admin@example.test").await;
    let (org, _) = h::organization(&db, admin_id, "admin").await;
    let asha_id = h::native_account(&db, "asha", "asha@example.test").await;
    h::add_membership(&db, org, asha_id, "member").await;
    let bob_id = h::native_account(&db, "bob", "bob@example.test").await;
    h::add_membership(&db, org, bob_id, "member").await;
    let outsider_id = h::native_account(&db, "outsider", "outsider@example.test").await;
    let (outsider_org, _) = h::organization(&db, outsider_id, "admin").await;
    let project = new_project(&db, org, admin_id, "Sales").await;
    let outsider_project = new_project(&db, outsider_org, outsider_id, "Theirs").await;
    let state = Arc::new(
        ServerState::new(db.clone(), h::preview_config(), Arc::new(h::TestClock::new(NOW))).unwrap(),
    );
    World {
        admin: h::scoped_context(&db, admin_id, org, NOW).await,
        asha: h::scoped_context(&db, asha_id, org, NOW).await,
        bob: h::scoped_context(&db, bob_id, org, NOW).await,
        outsider: h::scoped_context(&db, outsider_id, outsider_org, NOW).await,
        db,
        state,
        org,
        project,
        outsider_project,
    }
}

const ADMIN_OID: &str = "0a0a0a0a-1111-2222-3333-444444444444";
const SERVICE_URL: &str = "https://smba.trafficmanager.net/amer/";

/// A Graph directory where the admin owns both Teams and every channel is a
/// standard, guest-free channel (the Teams-side checks are covered in the
/// HTTP flow tests; these tests cover the Neev-side rules).
fn directory() -> Directory {
    let fake = FakeDirectory {
        graph_configured: true,
        ..FakeDirectory::default()
    };
    {
        let mut groups = fake.groups.try_lock().unwrap();
        groups.insert(TEAM.to_owned(), "12345678-1234-1234-1234-123456789abc".to_owned());
        groups.insert("19:team2@thread.tacv2".to_owned(), "22345678-1234-1234-1234-123456789abc".to_owned());
    }
    {
        let mut members = fake.members.try_lock().unwrap();
        members.insert(("12345678-1234-1234-1234-123456789abc".to_owned(), ADMIN_OID.to_owned()), Membership::Owner);
        members.insert(("22345678-1234-1234-1234-123456789abc".to_owned(), ADMIN_OID.to_owned()), Membership::Owner);
    }
    {
        let mut channels = fake.channels.try_lock().unwrap();
        channels.insert(CHANNEL.to_owned(), FakeDirectory::standard());
        channels.insert(OTHER_CHANNEL.to_owned(), FakeDirectory::standard());
    }
    Directory::Fake(Arc::new(fake))
}

fn content<T>(result: Result<T, BindingError>) -> Result<T, ContentError> {
    match result {
        Ok(found) => Ok(found),
        Err(BindingError::Content(error)) => Err(error),
        Err(BindingError::Refused(reason)) => panic!("unexpected Teams refusal {reason:?}"),
    }
}

/// The admin's Teams side of a binding write: the service URL a signed
/// activity would have recorded, and the admin's own linked identity.
async fn teams_side(state: &ServerState, actor: &OrgContext, tenant: &str) {
    let _ = bindings::record_service_url(&state.database, tenant, SERVICE_URL, NOW).await;
    if actor.role == crate::neev::types::Role::Admin {
        let _ = bindings::link_principal(state, actor, tenant, ADMIN_OID).await;
    }
}

async fn link_team(state: &ServerState, actor: &OrgContext, tenant: &str, team: &str, node: i32) -> Result<bindings::ProjectLink, ContentError> {
    teams_side(state, actor, tenant).await;
    content(bindings::link_team(state, &directory(), actor, tenant, team, node).await)
}

async fn set_channel(state: &ServerState, actor: &OrgContext, tenant: &str, team: &str, channel: &str, enabled: bool) -> Result<bindings::ChannelLink, ContentError> {
    teams_side(state, actor, tenant).await;
    content(bindings::set_channel(state, &directory(), actor, tenant, team, channel, enabled).await)
}

/// Tenant -> org, Team -> Sales, Proposals channel on, Asha and Bob linked.
async fn bound(w: &World) {
    bindings::connect_tenant(&w.state, &w.admin, TENANT, "claude", "default").await.unwrap();
    link_team(&w.state, &w.admin, TENANT, TEAM, w.project).await.unwrap();
    set_channel(&w.state, &w.admin, TENANT, TEAM, CHANNEL, true).await.unwrap();
    bindings::link_principal(&w.state, &w.asha, TENANT, ASHA_OID).await.unwrap();
    bindings::link_principal(&w.state, &w.bob, TENANT, BOB_OID).await.unwrap();
}

fn forbidden<T: std::fmt::Debug>(result: Result<T, ContentError>) {
    assert!(
        matches!(result, Err(ContentError::Auth(AuthError::Forbidden))),
        "{result:?}"
    );
}

// ---------------------------------------------------------------- bindings

#[tokio::test]
async fn connecting_is_admin_only_idempotent_and_one_org_per_tenant() {
    let w = world().await;
    forbidden(bindings::connect_tenant(&w.state, &w.asha, TENANT, "claude", "default").await);
    let first = bindings::connect_tenant(&w.state, &w.admin, &TENANT.to_uppercase(), "claude", "default").await.unwrap();
    assert_eq!((first.tenant_id.as_str(), first.org_id, first.revision), (TENANT, w.org, 1));
    // Repeating the same connect changes nothing.
    let again = bindings::connect_tenant(&w.state, &w.admin, TENANT, "claude", "default").await.unwrap();
    assert_eq!(again, first);
    // A different agent is an explicit update.
    let changed = bindings::connect_tenant(&w.state, &w.admin, TENANT, "claude", "opus").await.unwrap();
    assert_eq!((changed.model.as_str(), changed.revision), ("opus", 2));
    // Another org cannot claim the tenant, and learns nothing about the owner.
    assert_eq!(
        bindings::connect_tenant(&w.state, &w.outsider, TENANT, "claude", "default").await,
        Err(ContentError::Conflict)
    );
    // Malformed input.
    for (tenant, provider, model) in [("nope", "claude", "default"), (TENANT, "unknown", "default"), (TENANT, "claude", "-rf")] {
        assert_eq!(
            bindings::connect_tenant(&w.state, &w.admin, tenant, provider, model).await,
            Err(ContentError::Invalid)
        );
    }
}

#[tokio::test]
async fn teams_link_only_to_live_projects_of_the_admins_org() {
    let w = world().await;
    // No installation yet.
    assert_eq!(
        link_team(&w.state, &w.admin, TENANT, TEAM, w.project).await,
        Err(ContentError::NotFound)
    );
    bindings::connect_tenant(&w.state, &w.admin, TENANT, "claude", "default").await.unwrap();
    forbidden(link_team(&w.state, &w.asha, TENANT, TEAM, w.project).await);
    // Another org's project, a folder, and a missing node all look the same.
    let folder: i32 = crate::neev::db::one(&w.db, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,$2,'folder','F',1,$3) RETURNING id", vec![w.org.into(), w.project.into(), w.admin.identity.user_id.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    for node in [w.outsider_project, folder, 999_999] {
        assert_eq!(
            link_team(&w.state, &w.admin, TENANT, TEAM, node).await,
            Err(ContentError::NotFound),
            "{node}"
        );
    }
    let link = link_team(&w.state, &w.admin, TENANT, TEAM, w.project).await.unwrap();
    assert_eq!((link.node_id, link.revision), (w.project, 1));
    assert_eq!(link_team(&w.state, &w.admin, TENANT, TEAM, w.project).await.unwrap(), link);
    // The outsider org cannot touch this tenant's Team.
    assert_eq!(
        link_team(&w.state, &w.outsider, TENANT, TEAM, w.outsider_project).await,
        Err(ContentError::NotFound)
    );
    // Channels only under a linked Team; one Team per channel id.
    assert_eq!(
        set_channel(&w.state, &w.admin, TENANT, "19:unlinked@thread.tacv2", CHANNEL, true).await,
        Err(ContentError::NotFound)
    );
    set_channel(&w.state, &w.admin, TENANT, TEAM, CHANNEL, true).await.unwrap();
    let second = new_project(&w.db, w.org, w.admin.identity.user_id, "Support").await;
    link_team(&w.state, &w.admin, TENANT, "19:team2@thread.tacv2", second).await.unwrap();
    assert_eq!(
        set_channel(&w.state, &w.admin, TENANT, "19:team2@thread.tacv2", CHANNEL, true).await,
        Err(ContentError::Conflict)
    );
}

#[tokio::test]
async fn a_principal_link_is_one_to_one_and_needs_a_connected_tenant() {
    let w = world().await;
    assert_eq!(
        bindings::link_principal(&w.state, &w.asha, TENANT, ASHA_OID).await,
        Err(ContentError::NotFound)
    );
    bindings::connect_tenant(&w.state, &w.admin, TENANT, "claude", "default").await.unwrap();
    let linked = bindings::link_principal(&w.state, &w.asha, TENANT, &ASHA_OID.to_uppercase()).await.unwrap();
    assert_eq!((linked.aad_object_id.as_str(), linked.user_id), (ASHA_OID, w.asha.identity.user_id));
    // Idempotent.
    assert_eq!(bindings::link_principal(&w.state, &w.asha, TENANT, ASHA_OID).await.unwrap(), linked);
    // Bob cannot take Asha's Teams identity, and Asha cannot add a second one.
    assert_eq!(bindings::link_principal(&w.state, &w.bob, TENANT, ASHA_OID).await, Err(ContentError::Conflict));
    assert_eq!(bindings::link_principal(&w.state, &w.asha, TENANT, BOB_OID).await, Err(ContentError::Conflict));
    // Another org's member cannot link into this tenant.
    assert_eq!(bindings::link_principal(&w.state, &w.outsider, TENANT, BOB_OID).await, Err(ContentError::NotFound));
    // Only an admin unlinks someone else.
    forbidden(bindings::unlink_principal(&w.state, &w.bob, TENANT, w.asha.identity.user_id).await);
    bindings::unlink_principal(&w.state, &w.asha, TENANT, w.asha.identity.user_id).await.unwrap();
    assert_eq!(
        access::principal(&w.db, TENANT, w.org, Some(ASHA_OID)).await,
        Err(Refusal::UnmappedUser)
    );
}

// ---------------------------------------------------------------- access

#[tokio::test]
async fn access_fails_closed_without_naming_anything() {
    let w = world().await;
    assert_eq!(access::tenant(&w.db, TENANT).await, Err(Refusal::UnmappedTenant));
    bound(&w).await;
    let found = access::channel(&w.db, TENANT, Some(TEAM), CHANNEL).await.unwrap();
    assert_eq!((found.node_id, found.tenant.org_id), (w.project, w.org));
    // Team id omitted by Teams is fine; a different Team is not.
    assert!(access::channel(&w.db, &TENANT.to_uppercase(), None, CHANNEL).await.is_ok());
    assert_eq!(access::channel(&w.db, TENANT, Some("19:x@thread.tacv2"), CHANNEL).await, Err(Refusal::UnmappedChannel));
    assert_eq!(access::channel(&w.db, TENANT, Some(TEAM), OTHER_CHANNEL).await, Err(Refusal::UnmappedChannel));
    assert_eq!(access::channel(&w.db, OTHER_TENANT, Some(TEAM), CHANNEL).await, Err(Refusal::UnmappedTenant));
    assert_eq!(access::principal(&w.db, TENANT, w.org, None).await, Err(Refusal::GuestSender));
    assert_eq!(
        access::principal(&w.db, TENANT, w.org, Some("12345678-1234-1234-1234-123456789012")).await,
        Err(Refusal::UnmappedUser)
    );
    assert_eq!(access::principal(&w.db, TENANT, w.org, Some(&BOB_OID.to_uppercase())).await, Ok(w.bob.identity.user_id));
    // Each level of suspension stops the channel, and resuming restores it.
    set_channel(&w.state, &w.admin, TENANT, TEAM, CHANNEL, false).await.unwrap();
    assert_eq!(access::channel(&w.db, TENANT, Some(TEAM), CHANNEL).await, Err(Refusal::Suspended));
    set_channel(&w.state, &w.admin, TENANT, TEAM, CHANNEL, true).await.unwrap();
    bindings::set_team_enabled(&w.state, &w.admin, TENANT, TEAM, false).await.unwrap();
    assert_eq!(access::channel(&w.db, TENANT, Some(TEAM), CHANNEL).await, Err(Refusal::Suspended));
    bindings::set_team_enabled(&w.state, &w.admin, TENANT, TEAM, true).await.unwrap();
    bindings::set_tenant_enabled(&w.state, &w.admin, TENANT, false).await.unwrap();
    assert_eq!(access::channel(&w.db, TENANT, Some(TEAM), CHANNEL).await, Err(Refusal::Suspended));
    assert_eq!(access::tenant(&w.db, TENANT).await, Err(Refusal::Suspended));
    bindings::set_tenant_enabled(&w.state, &w.admin, TENANT, true).await.unwrap();
    assert!(access::channel(&w.db, TENANT, Some(TEAM), CHANNEL).await.is_ok());
}

#[tokio::test]
async fn a_removed_member_cannot_act_even_while_still_linked() {
    let w = world().await;
    bound(&w).await;
    // The link alone is not authority: the delegated org session is.
    crate::neev::db::execute(&w.db, "UPDATE neev_memberships SET active=false,revision=revision+1 WHERE org_id=$1 AND user_id=$2", vec![w.org.into(), w.bob.identity.user_id.into()]).await.unwrap();
    assert_eq!(access::principal(&w.db, TENANT, w.org, Some(BOB_OID)).await, Ok(w.bob.identity.user_id));
    assert!(crate::neev::auth::session::issue_delegated(&w.db, w.bob.identity.user_id, w.org, NOW, 600).await.is_err());
    assert!(crate::neev::auth::session::issue_delegated(&w.db, w.asha.identity.user_id, w.org, NOW, 600).await.is_ok());
}

// ---------------------------------------------------------------- threads

fn key(root: &str) -> ThreadKey {
    ThreadKey {
        tenant_id: TENANT.to_owned(),
        channel_id: CHANNEL.to_owned(),
        root_id: root.to_owned(),
    }
}

async fn say(
    w: &World,
    key: &ThreadKey,
    who: &OrgContext,
    name: &str,
    activity: &str,
    body: &str,
    now: i64,
) -> Admission {
    threads::admit(
        &w.db,
        key,
        &threads::Inbound {
            org_id: w.org,
            team_id: TEAM,
            conversation_id: &format!("{};messageid={}", key.channel_id, key.root_id),
            activity_id: activity,
            service_url: SERVICE_URL,
            author_user_id: who.identity.user_id,
            author_object_id: if who.identity.user_id == w.bob.identity.user_id { BOB_OID } else { ASHA_OID },
            author_name: name,
            body,
        },
        now,
    )
    .await
    .unwrap()
}

/// A real running turn in `who`'s own session, as the launcher would make.
async fn running_turn(w: &World, who: &OrgContext, access: &ChannelAccess, text: &str) -> String {
    let session = sessions::create(
        &w.state,
        who,
        SessionInput {
            title: "Teams · channel thread".into(),
            provider: access.tenant.provider.clone(),
            model: access.tenant.model.clone(),
            bound_chat_id: None,
            node_id: Some(access.node_id),
            effort: None,
            fast: false,
            request_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    let (turn, created) = sessions::begin_turn(&w.state, who, &session.id, text, None).await.unwrap();
    assert!(created);
    // The session is the submitter's own: execution identity never borrows
    // the thread starter's.
    assert_eq!(session.owner_id, who.identity.user_id);
    turn.run_id
}

#[tokio::test]
async fn one_root_is_one_shared_thread_and_roots_stay_isolated() {
    let w = world().await;
    bound(&w).await;
    let access = access::channel(&w.db, TENANT, Some(TEAM), CHANNEL).await.unwrap();
    let a = key("1700000000001");
    // Asha starts; her run is live.
    let Admission::Start { sequence } = say(&w, &a, &w.asha, "Asha", "1700000000001", "draft a proposal", NOW).await else {
        panic!("first message starts");
    };
    assert_eq!(sequence, 1);
    let run = running_turn(&w, &w.asha, &access, "draft a proposal").await;
    threads::started(&w.db, &a, 1, &run, w.asha.identity.user_id).await.unwrap();
    // A Bot Connector retry of the same activity is a replay: no second run.
    assert_eq!(say(&w, &a, &w.asha, "Asha", "1700000000001", "draft a proposal", NOW).await, Admission::Replay);
    // Bob replies during the run: recorded, queued, not run.
    assert_eq!(
        say(&w, &a, &w.bob, "Bob", "1700000000002", "include annual pricing", NOW + 5).await,
        Admission::Queued { sequence: 2 }
    );
    // A second root in the same channel is independent and may start at once.
    let b = key("1700000000099");
    assert_eq!(
        say(&w, &b, &w.bob, "Bob", "1700000000099", "account research", NOW + 6).await,
        Admission::Start { sequence: 1 }
    );
    assert!(threads::context(&w.db, &b, 1).await.unwrap().is_empty());
    // Asha's run settles and its answer is posted into the thread.
    sessions::finish(&w.db, &run, "Draft v1", None, TurnStatus::Completed, None, NOW + 10).await.unwrap();
    threads::record_answer(&w.db, &a, w.org, &format!("{CHANNEL};messageid=1700000000001"), &run, "Draft v1", NOW + 11).await.unwrap();
    threads::record_answer(&w.db, &a, w.org, &format!("{CHANNEL};messageid=1700000000001"), &run, "Draft v1", NOW + 12).await.unwrap();
    // The worker settles Asha's run: the claim passes to Bob's queued note.
    let tx = w.db.begin().await.unwrap();
    assert_eq!(threads::release(&tx, &a, 1, NOW + 13).await.unwrap(), Some(2));
    tx.commit().await.unwrap();
    // Bob's next mention waits behind his note, with the whole thread as
    // context, attributed, including the posted answer once.
    let Admission::Queued { sequence } = say(&w, &a, &w.bob, "Bob", "1700000000003", "now tighten it", NOW + 20).await else {
        panic!("waits behind the earlier note");
    };
    let context = threads::context(&w.db, &a, sequence).await.unwrap();
    let lines: Vec<(Option<&str>, &str)> = context.iter().map(|e| (e.author.as_deref(), e.body.as_str())).collect();
    assert_eq!(
        lines,
        vec![
            (Some("Asha"), "draft a proposal"),
            (Some("Bob"), "include annual pricing"),
            (None, "Draft v1"),
        ]
    );
    // Bob's run executes in Bob's own session.
    let bob_run = running_turn(&w, &w.bob, &access, &threads::compose(&context, "Bob", "now tighten it")).await;
    threads::started(&w.db, &a, sequence, &bob_run, w.bob.identity.user_id).await.unwrap();
    let row = crate::neev::db::one(&w.db, "SELECT author_user_id,executed_as FROM neev_msteams_thread_messages WHERE run_id=$1", vec![bob_run.clone().into()]).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i32>("", "author_user_id").unwrap(), w.bob.identity.user_id);
    assert_eq!(row.try_get::<i32>("", "executed_as").unwrap(), w.bob.identity.user_id);
    // Asha cannot read Bob's run (owner-scoped), and vice versa.
    assert_eq!(sessions::turn_by_run(&w.state, &w.asha, &bob_run).await, Err(ContentError::NotFound));
    assert_eq!(sessions::turn_by_run(&w.state, &w.bob, &run).await, Err(ContentError::NotFound));
    assert_eq!(crate::neev::db::validate_content_snapshot(&w.db).await, Ok(25));
}

#[tokio::test]
async fn a_refused_launch_passes_the_thread_to_the_next_message_in_order() {
    let w = world().await;
    bound(&w).await;
    let a = key("1700000000001");
    assert_eq!(say(&w, &a, &w.asha, "Asha", "1", "first", NOW).await, Admission::Start { sequence: 1 });
    // While the claim is held (launch in progress), others queue, in order.
    assert_eq!(say(&w, &a, &w.bob, "Bob", "2", "second", NOW + 1).await, Admission::Queued { sequence: 2 });
    assert_eq!(say(&w, &a, &w.asha, "Asha", "3", "third", NOW + 2).await, Admission::Queued { sequence: 3 });
    // The launch was refused: its text is dropped and the claim goes to the
    // OLDEST waiting message, which becomes due to launch.
    let tx = w.db.begin().await.unwrap();
    assert_eq!(threads::refused(&tx, &a, 1, NOW + 3).await.unwrap(), Some(2));
    tx.commit().await.unwrap();
    let state: String = crate::neev::db::one(&w.db, "SELECT state FROM neev_msteams_inbox WHERE activity_id='2'", vec![]).await.unwrap().unwrap().try_get("", "state").unwrap();
    assert_eq!(state, "pending");
    // Releasing a claim that is no longer held changes nothing.
    let tx = w.db.begin().await.unwrap();
    assert_eq!(threads::release(&tx, &a, 1, NOW + 4).await.unwrap(), None);
    tx.commit().await.unwrap();
    let context = threads::context(&w.db, &a, 4).await.unwrap();
    assert!(context.iter().all(|entry| entry.body != "first"), "a refused input is not context");
    assert_eq!(crate::neev::db::validate_content_snapshot(&w.db).await, Ok(25));
}

#[tokio::test]
async fn concurrent_first_mentions_start_exactly_one_run() {
    let w = world().await;
    bound(&w).await;
    let a = key("1700000000001");
    let (first, second) = tokio::join!(
        say(&w, &a, &w.asha, "Asha", "10", "one", NOW),
        say(&w, &a, &w.bob, "Bob", "11", "two", NOW),
    );
    let starts = [first, second].iter().filter(|admission| matches!(admission, Admission::Start { .. })).count();
    assert_eq!(starts, 1, "{first:?} {second:?}");
}
