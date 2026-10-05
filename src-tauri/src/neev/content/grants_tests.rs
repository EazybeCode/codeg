//! Sharing with members and teams: the one rule (`grants::level`) and its SQL
//! form (`Audience::readable`) agree, View is read-only everywhere, private is
//! a real exception, revocation is immediate and permanent, and nothing shared
//! ever reaches org knowledge. Every scenario runs on SQLite and on the guarded
//! PostgreSQL.

use super::{
    access,
    grants::{self, Audience, Level, Principal, SharingError},
    mentions, messages, resources, sharing,
    types::*,
};
use crate::neev::{
    agents::{session_fork, sessions},
    catalog::nodes,
    db,
    org::{self, teams},
    startup::ServerState,
    test_support as h,
    types::{NodeInput, NodeKind, OrgContext},
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use std::collections::BTreeSet;
use std::sync::Arc;

const NOW: i64 = 1_700_000_000;

struct World {
    state: Arc<ServerState>,
    db: DatabaseConnection,
    org: i32,
    admin: OrgContext,
    creator: OrgContext,
    viewer: OrgContext,
    editor: OrgContext,
    mate: OrgContext,
    mate_member: i32,
    outsider: OrgContext,
    foreign: OrgContext,
    project: i32,
    folder: i32,
    deep: i32,
    team: String,
    /// shared doc in `deep`, no direct grants (folder access)
    a: Resource,
    /// private doc in `deep` (the private exception)
    b: Resource,
    /// shared chat in `project`: viewer View, editor Edit
    c: Resource,
    /// organization doc in `project`
    d: Resource,
    /// document bound to `c`
    e: Resource,
    /// shared chat in `deep`, folder access only
    f: Resource,
}

async fn account(db: &DatabaseConnection, name: &str) -> i32 {
    let id = h::native_account(db, &format!("share-{name}"), &format!("{name}@example.test")).await;
    db::execute(db, "UPDATE neev_accounts SET display_name=$1 WHERE id=$2", vec![name.into(), id.into()])
        .await
        .unwrap();
    id
}

async fn node(state: &ServerState, actor: &OrgContext, parent: Option<i32>, name: &str) -> i32 {
    nodes::create(
        state,
        actor,
        NodeInput {
            parent_id: parent,
            kind: if parent.is_none() { NodeKind::Project } else { NodeKind::Folder },
            name: name.into(),
        },
    )
    .await
    .unwrap()
    .id
}

fn input(node_id: i32, kind: ResourceKind, title: &str, origin: Option<&str>) -> ResourceInput {
    ResourceInput {
        node_id,
        kind,
        title: title.into(),
        origin_chat_id: origin.map(str::to_owned),
        request_id: uuid::Uuid::new_v4().to_string(),
    }
}

async fn node_revision(w: &World, node_id: i32) -> i64 {
    grants::node_access(&w.state, &w.admin, node_id).await.unwrap().revision
}

async fn auth_revision(w: &World, id: &str) -> i64 {
    resources::get(&w.state, &w.creator, id).await.unwrap().auth_revision
}

async fn world(db: DatabaseConnection) -> World {
    let clock = Arc::new(h::TestClock::new(NOW));
    let mail = crate::neev::mail::Service::new(db.clone(), Arc::new(h::MailCapture::new()));
    let state = Arc::new(ServerState::with_mail(db.clone(), h::preview_config(), clock, mail).unwrap());
    let admin_id = account(&db, "Admin").await;
    let (org, _) = h::organization(&db, admin_id, "admin").await;
    let mut ids = Vec::new();
    for name in ["Creator", "Viewer", "Editor", "Mate", "Outsider"] {
        let id = account(&db, name).await;
        let member = h::add_membership(&db, org, id, "member").await;
        ids.push((id, member));
    }
    let foreign_id = account(&db, "Foreign").await;
    let (other, _) = h::organization(&db, foreign_id, "admin").await;
    let ctx = |user: i32| h::scoped_context(&db, user, org, NOW);
    let admin = ctx(admin_id).await;
    let creator = ctx(ids[0].0).await;
    let viewer = ctx(ids[1].0).await;
    let editor = ctx(ids[2].0).await;
    let mate = ctx(ids[3].0).await;
    let outsider = ctx(ids[4].0).await;
    let foreign = h::scoped_context(&db, foreign_id, other, NOW).await;

    let project = node(&state, &creator, None, "Plans").await;
    let folder = node(&state, &creator, Some(project), "Q4").await;
    let deep = node(&state, &creator, Some(folder), "Launch").await;

    let team = teams::create(&state, &admin, "Design", &uuid::Uuid::new_v4().to_string())
        .await
        .unwrap();
    teams::set_member(&state, &admin, &team.id, mate.identity.user_id, team.revision, true)
        .await
        .unwrap();

    let make = |node_id, kind, title: &'static str, visibility| {
        let state = state.clone();
        let creator = creator.clone();
        async move {
            resources::create_with_visibility(&state, &creator, input(node_id, kind, title, None), Some(visibility))
                .await
                .unwrap()
        }
    };
    let a = make(deep, ResourceKind::Document, "A", Visibility::Shared).await;
    let b = make(deep, ResourceKind::Document, "B", Visibility::Private).await;
    let c = make(project, ResourceKind::Chat, "C", Visibility::Shared).await;
    let d = make(project, ResourceKind::Document, "D", Visibility::Organization).await;
    let f = make(deep, ResourceKind::Chat, "F", Visibility::Shared).await;
    let e = resources::create(&state, &creator, input(project, ResourceKind::Document, "E", Some(&c.id)))
        .await
        .unwrap();

    let mut w = World {
        state,
        db,
        org,
        admin,
        creator,
        viewer,
        editor,
        mate,
        mate_member: ids[3].1,
        outsider,
        foreign,
        project,
        folder,
        deep,
        team: team.id,
        a,
        b,
        c,
        d,
        e,
        f,
    };
    // The team may view everything shared under the project; two people are
    // named on the chat itself.
    let revision = node_revision(&w, project).await;
    grants::set_node_grant(&w.state, &w.creator, project, revision, &Principal::Team(w.team.clone()), Some(Level::View))
        .await
        .unwrap();
    for (who, level) in [(w.viewer.identity.user_id, Level::View), (w.editor.identity.user_id, Level::Edit)] {
        let revision = auth_revision(&w, &w.c.id).await;
        grants::set_resource_grant(&w.state, &w.creator, &w.c.id, revision, &Principal::User(who), Some(level))
            .await
            .unwrap();
    }
    w.c = resources::get(&w.state, &w.creator, &w.c.id).await.unwrap();
    w
}

async fn level_of(w: &World, actor: &OrgContext, id: &str) -> Option<Level> {
    let tx = db::begin(&w.state.database).await.unwrap();
    let result = access::resolve(&tx, actor, id, false).await;
    tx.rollback().await.unwrap();
    match result {
        Ok((_, level)) => Some(level),
        Err(ContentError::NotFound) => None,
        Err(error) => panic!("{error:?}"),
    }
}

async fn writable(w: &World, actor: &OrgContext, id: &str) -> bool {
    let tx = db::begin(&w.state.database).await.unwrap();
    let result = access::resolve(&tx, actor, id, true).await.is_ok();
    tx.rollback().await.unwrap();
    result
}

async fn sql_readable(w: &World, actor: &OrgContext) -> BTreeSet<String> {
    let audience = Audience::load(&w.db, actor.org_id, actor.identity.user_id).await.unwrap();
    let sql = format!(
        "SELECT r.id FROM neev_resources r LEFT JOIN neev_resources s ON s.org_id=r.org_id AND s.id=r.origin_chat_id AND s.kind='chat' WHERE r.org_id=$1 AND {}",
        audience.readable("r", "s")
    );
    w.db.query_all(db::statement(&w.db, &sql, vec![actor.org_id.into()]))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<String>("", "id").unwrap())
        .collect()
}

async fn the_rules_agree(w: &World) {
    use Level::*;
    let all = [&w.a, &w.b, &w.c, &w.d, &w.e, &w.f];
    let expected: [(&OrgContext, [Option<Level>; 6]); 6] = [
        (&w.creator, [Some(Owner), Some(Owner), Some(Owner), Some(Owner), Some(Owner), Some(Owner)]),
        (&w.viewer, [None, None, Some(View), Some(Edit), Some(View), None]),
        (&w.editor, [None, None, Some(Edit), Some(Edit), Some(Edit), None]),
        // Team grant on the project: inherited two levels down, never into the
        // private exception, and the bound document follows its chat.
        (&w.mate, [Some(View), None, Some(View), Some(Edit), Some(View), Some(View)]),
        (&w.outsider, [None, None, None, Some(Edit), None, None]),
        // An admin is not a reader of other people's private or shared work.
        (&w.admin, [None, None, None, Some(Edit), None, None]),
    ];
    for (actor, levels) in expected {
        let sql = sql_readable(w, actor).await;
        for (resource, want) in all.iter().zip(levels) {
            let got = level_of(w, actor, &resource.id).await;
            assert_eq!(got, want, "user {} on {}", actor.identity.user_id, resource.title);
            assert_eq!(sql.contains(&resource.id), got.is_some(), "SQL disagrees: user {} on {}", actor.identity.user_id, resource.title);
            assert_eq!(writable(w, actor, &resource.id).await, matches!(got, Some(Edit | Owner)));
        }
    }
    for resource in all {
        assert_eq!(level_of(w, &w.foreign, &resource.id).await, None);
    }
    assert!(sql_readable(w, &w.foreign).await.is_empty());
    // The DTO says shared; storage says private, so anything ignoring the mark narrows.
    assert_eq!(w.a.visibility, Visibility::Shared);
    assert_eq!(resources::get(&w.state, &w.creator, &w.e.id).await.unwrap().visibility, Visibility::Shared);
    let stored: String = db::one(&w.db, "SELECT visibility FROM neev_resources WHERE id=$1", vec![w.a.id.clone().into()])
        .await
        .unwrap()
        .unwrap()
        .try_get("", "visibility")
        .unwrap();
    assert_eq!(stored, "private");
}

async fn view_is_read_only(w: &World) {
    // Messages, renames and grants: the viewer is refused as a non-reader would be.
    assert_eq!(
        messages::append(&w.state, &w.viewer, &w.c.id, &uuid::Uuid::new_v4().to_string(), w.c.auth_revision, "hi").await.err(),
        Some(ContentError::NotFound)
    );
    messages::append(&w.state, &w.editor, &w.c.id, &uuid::Uuid::new_v4().to_string(), w.c.auth_revision, "hi")
        .await
        .unwrap();
    let mut patch = super::test_support::patch(&w.c);
    patch.title = "Renamed".into();
    assert_eq!(resources::update(&w.state, &w.viewer, &w.c.id, patch.clone()).await.err(), Some(ContentError::NotFound));
    // An editor renames but never re-shares, moves or archives.
    resources::update(&w.state, &w.editor, &w.c.id, patch.clone()).await.unwrap();
    let c = resources::get(&w.state, &w.creator, &w.c.id).await.unwrap();
    let mut widen = super::test_support::patch(&c);
    widen.visibility = Visibility::Organization;
    assert_eq!(resources::update(&w.state, &w.editor, &c.id, widen).await.err(), Some(ContentError::NotFound));
    // Readers who are not the creator are forbidden; anyone else cannot see it.
    for (actor, refusal) in [
        (&w.viewer, ContentError::Auth(crate::neev::error::AuthError::Forbidden)),
        (&w.editor, ContentError::Auth(crate::neev::error::AuthError::Forbidden)),
        (&w.admin, ContentError::NotFound),
    ] {
        assert_eq!(
            grants::set_resource_grant(&w.state, actor, &c.id, c.auth_revision, &Principal::User(w.outsider.identity.user_id), Some(Level::View)).await.err(),
            Some(SharingError::Content(refusal))
        );
    }
    // A node grant is the node creator's or an admin's to change.
    let revision = node_revision(w, w.folder).await;
    assert!(matches!(
        grants::set_node_grant(&w.state, &w.mate, w.folder, revision, &Principal::User(w.mate.identity.user_id), Some(Level::Edit)).await,
        Err(ContentError::Auth(crate::neev::error::AuthError::Forbidden))
    ));
    // Writes to a view-only document fail too.
    assert!(!writable(w, &w.mate, &w.a.id).await);
}

async fn grants_lifecycle(w: &World) {
    // Only a shared item takes direct grants.
    let b = auth_revision(w, &w.b.id).await;
    assert_eq!(
        grants::set_resource_grant(&w.state, &w.creator, &w.b.id, b, &Principal::User(w.viewer.identity.user_id), Some(Level::View)).await.err(),
        Some(SharingError::NotShared)
    );
    // A stale revision, a stranger, a foreign team, the creator themself.
    let a = auth_revision(w, &w.a.id).await;
    assert_eq!(
        grants::set_resource_grant(&w.state, &w.creator, &w.a.id, a + 7, &Principal::User(w.viewer.identity.user_id), Some(Level::View)).await.err(),
        Some(SharingError::Content(ContentError::Conflict))
    );
    assert_eq!(
        grants::set_resource_grant(&w.state, &w.creator, &w.a.id, a, &Principal::User(w.foreign.identity.user_id), Some(Level::View)).await.err(),
        Some(SharingError::Content(ContentError::NotFound))
    );
    assert_eq!(
        grants::set_resource_grant(&w.state, &w.creator, &w.a.id, a, &Principal::Team(uuid::Uuid::new_v4().to_string()), Some(Level::View)).await.err(),
        Some(SharingError::Content(ContentError::NotFound))
    );
    assert_eq!(
        grants::set_resource_grant(&w.state, &w.creator, &w.a.id, a, &Principal::User(w.creator.identity.user_id), Some(Level::View)).await.err(),
        Some(SharingError::Content(ContentError::Invalid))
    );
    // A direct Edit on top of the inherited View; removing it leaves the View.
    let access = grants::set_resource_grant(&w.state, &w.creator, &w.a.id, a, &Principal::User(w.mate.identity.user_id), Some(Level::Edit))
        .await
        .unwrap();
    assert_eq!(access.auth_revision, a + 1, "a grant change moves the auth revision");
    assert_eq!(access.direct.len(), 1);
    assert_eq!(access.inherited.len(), 1);
    assert_eq!(access.inherited[0].node_id, w.project);
    assert_eq!(level_of(w, &w.mate, &w.a.id).await, Some(Level::Edit));
    let access = grants::set_resource_grant(&w.state, &w.creator, &w.a.id, a + 1, &Principal::User(w.mate.identity.user_id), None)
        .await
        .unwrap();
    assert!(access.direct.is_empty());
    assert_eq!(level_of(w, &w.mate, &w.a.id).await, Some(Level::View));
    // Any reader may see the sharing; the level is their own.
    let seen = grants::resource_access(&w.state, &w.viewer, &w.c.id).await.unwrap();
    assert_eq!((seen.level, seen.can_manage, seen.direct.len()), (Level::View, false, 2));
    assert!(grants::resource_access(&w.state, &w.outsider, &w.c.id).await.is_err());
    // A bound document reports its chat and refuses grants of its own.
    let bound = grants::resource_access(&w.state, &w.creator, &w.e.id).await.unwrap();
    assert_eq!(bound.bound_to.as_deref(), Some(w.c.id.as_str()));
    assert!(!bound.can_manage);
    let e = auth_revision(w, &w.e.id).await;
    assert_eq!(
        grants::set_resource_grant(&w.state, &w.creator, &w.e.id, e, &Principal::User(w.outsider.identity.user_id), Some(Level::View)).await.err(),
        Some(SharingError::Content(ContentError::Invalid))
    );
    // Leaving shared ends the direct grants; returning starts from nobody but
    // the folder.
    let c = resources::get(&w.state, &w.creator, &w.c.id).await.unwrap();
    let mut private = super::test_support::patch(&c);
    private.visibility = Visibility::Private;
    let c = resources::update(&w.state, &w.creator, &c.id, private).await.unwrap();
    assert_eq!(level_of(w, &w.viewer, &c.id).await, None);
    assert_eq!(level_of(w, &w.mate, &c.id).await, None, "private ignores folder grants");
    assert_eq!(h::count(&w.db, "neev_grants WHERE target_kind='resource'").await, 0);
    let mut shared = super::test_support::patch(&c);
    shared.visibility = Visibility::Shared;
    let c = resources::update(&w.state, &w.creator, &c.id, shared).await.unwrap();
    assert_eq!(level_of(w, &w.viewer, &c.id).await, None);
    assert_eq!(level_of(w, &w.mate, &c.id).await, Some(Level::View));
    // Moving it out from under the granted project ends the inherited access.
    let other = node(&w.state, &w.creator, None, "Elsewhere").await;
    let mut moved = super::test_support::patch(&c);
    moved.node_id = other;
    resources::update(&w.state, &w.creator, &c.id, moved).await.unwrap();
    assert_eq!(level_of(w, &w.mate, &c.id).await, None);
    // Narrow-only rollback: without its mark a shared item is private.
    db::execute(&w.db, "DELETE FROM neev_shared_marks WHERE target_id=$1", vec![w.a.id.clone().into()]).await.unwrap();
    assert_eq!(level_of(w, &w.mate, &w.a.id).await, None);
    assert_eq!(level_of(w, &w.creator, &w.a.id).await, Some(Level::Owner));
}

async fn revocation_is_immediate_and_permanent(w: &World) {
    assert_eq!(level_of(w, &w.mate, &w.a.id).await, Some(Level::View));
    // Out of the team: gone at the next request.
    let team = teams::members(&w.state, &w.admin, &w.team).await.unwrap();
    teams::set_member(&w.state, &w.admin, &w.team, w.mate.identity.user_id, team.revision, false).await.unwrap();
    assert_eq!(level_of(w, &w.mate, &w.a.id).await, None);
    assert!(!sql_readable(w, &w.mate).await.contains(&w.a.id));
    // Back in, then the team is deleted: its grants go with it.
    let team = teams::members(&w.state, &w.admin, &w.team).await.unwrap();
    teams::set_member(&w.state, &w.admin, &w.team, w.mate.identity.user_id, team.revision, true).await.unwrap();
    assert_eq!(level_of(w, &w.mate, &w.a.id).await, Some(Level::View));
    let team = teams::members(&w.state, &w.admin, &w.team).await.unwrap();
    teams::delete(&w.state, &w.admin, &w.team, team.revision).await.unwrap();
    assert_eq!(level_of(w, &w.mate, &w.a.id).await, None);
    assert_eq!(h::count(&w.db, "neev_grants WHERE principal_kind='team'").await, 0);
    // A direct grant, then removal from the organization: the grant and team
    // places are deleted with the membership, and a re-invite brings nothing back.
    let a = auth_revision(w, &w.a.id).await;
    grants::set_resource_grant(&w.state, &w.creator, &w.a.id, a, &Principal::User(w.mate.identity.user_id), Some(Level::Edit))
        .await
        .unwrap();
    let again = teams::create(&w.state, &w.admin, "Ops", &uuid::Uuid::new_v4().to_string()).await.unwrap();
    teams::set_member(&w.state, &w.admin, &again.id, w.mate.identity.user_id, again.revision, true).await.unwrap();
    org::remove(&w.state, &w.admin, w.mate_member).await.unwrap();
    assert_eq!(h::count(&w.db, &format!("neev_grants WHERE principal_id='{}'", w.mate.identity.user_id)).await, 0);
    assert_eq!(h::count(&w.db, &format!("neev_team_members WHERE user_id={}", w.mate.identity.user_id)).await, 0);
    db::execute(&w.db, "UPDATE neev_memberships SET active=true,revision=revision+1 WHERE id=$1", vec![w.mate_member.into()]).await.unwrap();
    let back = h::scoped_context(&w.db, w.mate.identity.user_id, w.org, NOW).await;
    assert_eq!(level_of(w, &back, &w.a.id).await, None);
    assert!(!sql_readable(w, &back).await.contains(&w.a.id));
}

async fn lists_mentions_knowledge_and_defaults(w: &World) {
    // Lists apply the same rule before the page limit.
    let docs = resources::list(&w.state, &w.mate, w.deep, ResourceKind::Document, false, None).await.unwrap();
    assert_eq!(docs.items.iter().map(|r| r.id.clone()).collect::<Vec<_>>(), vec![w.a.id.clone()]);
    let chats = resources::list(&w.state, &w.mate, w.deep, ResourceKind::Chat, false, None).await.unwrap();
    assert_eq!(chats.items.len(), 1);
    assert!(resources::list(&w.state, &w.viewer, w.deep, ResourceKind::Document, false, None).await.unwrap().items.is_empty());
    // Mentions in a shared chat: only people who can read it.
    let name = |who: &str| format!("@{who}");
    let body = format!("{} please look", name("Viewer"));
    messages::append_with_attachments(
        &w.state, &w.creator, &w.c.id, &uuid::Uuid::new_v4().to_string(), w.c.auth_revision, &body, &[],
        &[mentions::MentionInput { user_id: w.viewer.identity.user_id, start: 0, length: 7 }],
    )
    .await
    .unwrap();
    let body = format!("{} please look", name("Outsider"));
    assert_eq!(
        messages::append_with_attachments(
            &w.state, &w.creator, &w.c.id, &uuid::Uuid::new_v4().to_string(), w.c.auth_revision, &body, &[],
            &[mentions::MentionInput { user_id: w.outsider.identity.user_id, start: 0, length: 9 }],
        )
        .await
        .err(),
        Some(ContentError::Invalid)
    );
    let (notices, unread) = mentions::inbox(&w.state, &w.viewer).await.unwrap();
    assert_eq!((notices.len(), unread), (1, 1));
    // Org knowledge: only organization items, never shared ones.
    for (resource, eligible) in [(&w.a, false), (&w.b, false), (&w.c, false), (&w.d, true), (&w.e, false), (&w.f, false)] {
        assert_eq!(
            crate::neev::knowledge::access::is_eligible(&w.db, w.org, &resource.id).await.unwrap().is_some(),
            eligible,
            "{}",
            resource.title
        );
    }
    // Organization → shared leaves knowledge.
    let d = resources::get(&w.state, &w.creator, &w.d.id).await.unwrap();
    let mut shared = super::test_support::patch(&d);
    shared.visibility = Visibility::Shared;
    let d = resources::update(&w.state, &w.creator, &d.id, shared).await.unwrap();
    assert!(crate::neev::knowledge::access::is_eligible(&w.db, w.org, &d.id).await.unwrap().is_none());
    assert_eq!(level_of(w, &w.outsider, &d.id).await, None);
    // A "folder access" default: new items there are shared, old ones untouched.
    let revision = node_revision(w, w.folder).await;
    let set = sharing::set(&w.state, &w.creator, w.folder, revision, sharing::DefaultVisibility::Shared).await.unwrap();
    assert_eq!(set.effective_visibility, Visibility::Shared);
    let inherited = sharing::get(&w.state, &w.creator, w.deep).await.unwrap();
    assert_eq!((inherited.effective_visibility, inherited.inherited_from), (Visibility::Shared, Some(w.folder)));
    let fresh = resources::create(&w.state, &w.creator, input(w.deep, ResourceKind::Document, "Fresh", None)).await.unwrap();
    assert_eq!(fresh.visibility, Visibility::Shared);
    assert_eq!(level_of(w, &w.mate, &fresh.id).await, Some(Level::View));
    assert_eq!(level_of(w, &w.mate, &w.b.id).await, None, "an existing private item stays private");
    let revision = node_revision(w, w.folder).await;
    sharing::set(&w.state, &w.creator, w.folder, revision, sharing::DefaultVisibility::Inherit).await.unwrap();
    assert_eq!(h::count(&w.db, "neev_shared_marks WHERE mark_kind='node_default'").await, 0);
    crate::neev::db::validate_content_snapshot(&w.db).await.unwrap();
}

async fn agent_chats_are_view_only_and_fork_privately(w: &World) {
    let session = uuid::Uuid::new_v4().to_string();
    let turn = uuid::Uuid::new_v4().to_string();
    db::execute(&w.db, "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,effort,fast,visibility,kind,bound_chat_id,bound_kind,node_id,archived,revision,created_at,updated_at) VALUES($1,$2,$3,'Research','claude','default',NULL,false,'private','chat',NULL,NULL,NULL,false,1,1,1)", vec![w.org.into(), session.clone().into(), w.creator.identity.user_id.into()]).await.unwrap();
    let trace = r#"[{"seq":1,"offset":0,"id":"t1","kind":"tool_call","name":"personal_mail","input":"{}","inputTruncated":false}]"#;
    db::execute(&w.db, "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,request_id,prompt,response,activity,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,1,$3,$4,NULL,'Ask','Answer',$5,'completed',NULL,'claude','default',2,2,2)", vec![w.org.into(), session.clone().into(), turn.clone().into(), "0123456789abcdef0123456789abcdef".into(), trace.into()]).await.unwrap();
    assert_eq!(sessions::view(&w.state, &w.viewer, &session).await.err(), Some(ContentError::NotFound));
    assert_eq!(
        grants::set_session_grant(&w.state, &w.creator, &session, 1, &Principal::User(w.viewer.identity.user_id), Some(Level::Edit)).await.err(),
        Some(ContentError::Invalid)
    );
    assert_eq!(
        grants::set_session_grant(&w.state, &w.viewer, &session, 1, &Principal::User(w.viewer.identity.user_id), Some(Level::View)).await.err(),
        Some(ContentError::NotFound)
    );
    let access = grants::set_session_grant(&w.state, &w.creator, &session, 1, &Principal::User(w.viewer.identity.user_id), Some(Level::View))
        .await
        .unwrap();
    assert_eq!((access.revision, access.direct.len()), (2, 1));
    // The viewer reads the conversation, never the owner's trace.
    assert_eq!(sessions::view(&w.state, &w.viewer, &session).await.unwrap().id, session);
    let turns = sessions::turns(&w.state, &w.viewer, &session, None).await.unwrap();
    assert_eq!((turns.items.len(), turns.items[0].response.as_str()), (1, "Answer"));
    assert!(turns.items[0].activity.is_empty());
    assert!(!sessions::turns(&w.state, &w.creator, &session, None).await.unwrap().items[0].activity.is_empty());
    // Every owner path stays closed to them.
    assert_eq!(sessions::get(&w.state, &w.viewer, &session).await.err(), Some(ContentError::NotFound));
    assert_eq!(sessions::view(&w.state, &w.outsider, &session).await.err(), Some(ContentError::NotFound));
    let shared = sessions::shared_with(&w.state, &w.viewer).await.unwrap();
    assert_eq!(shared.len(), 1);
    assert_eq!(shared[0].1.display_name, "Creator");
    assert!(sessions::shared_with(&w.state, &w.creator).await.unwrap().is_empty());
    let seen = grants::session_access(&w.state, &w.viewer, &session).await.unwrap();
    assert_eq!((seen.level, seen.can_manage), (Level::View, false));
    // Forking makes the viewer's own private chat, without the trace.
    let fork = session_fork::fork_session(&w.state, &w.viewer, &session, &turn, &uuid::Uuid::new_v4().to_string())
        .await
        .unwrap_or_else(|_| panic!("fork refused"));
    let copy = sessions::get(&w.state, &w.viewer, &fork.session_id).await.unwrap();
    assert_eq!(copy.owner_id, w.viewer.identity.user_id);
    let copied = sessions::turns(&w.state, &w.viewer, &fork.session_id, None).await.unwrap();
    assert!(copied.items[0].activity.is_empty());
    let stored: Option<String> = db::one(&w.db, "SELECT activity FROM neev_agent_turns WHERE session_id=$1", vec![fork.session_id.clone().into()])
        .await
        .unwrap()
        .unwrap()
        .try_get("", "activity")
        .unwrap();
    assert_eq!(stored, None);
    assert!(sessions::view(&w.state, &w.creator, &fork.session_id).await.is_err());
    // Archived or revoked: gone.
    db::execute(&w.db, "UPDATE neev_agent_sessions SET archived=true WHERE id=$1", vec![session.clone().into()]).await.unwrap();
    assert_eq!(sessions::view(&w.state, &w.viewer, &session).await.err(), Some(ContentError::NotFound));
    db::execute(&w.db, "UPDATE neev_agent_sessions SET archived=false WHERE id=$1", vec![session.clone().into()]).await.unwrap();
    grants::set_session_grant(&w.state, &w.creator, &session, 2, &Principal::User(w.viewer.identity.user_id), None).await.unwrap();
    assert_eq!(sessions::view(&w.state, &w.viewer, &session).await.err(), Some(ContentError::NotFound));
    assert!(session_fork::fork_session(&w.state, &w.viewer, &session, &turn, &uuid::Uuid::new_v4().to_string()).await.is_err());
}

/// S-L1: a grant names a reader only while they hold an ACTIVE membership on
/// an account that is not disabled — on the user and the team branch alike,
/// in the Rust rule and the SQL rule. (Removal through the API deletes the
/// rows anyway; this holds even for a row left behind.)
async fn inactive_or_disabled_principals_read_nothing(w: &World) {
    let rust = |user: i32, resource: Resource| {
        let db = w.db.clone();
        let org = w.org;
        async move { grants::level(&db, org, user, &resource).await.unwrap() }
    };
    let sql = |user: i32, id: String| {
        let db = w.db.clone();
        let org = w.org;
        async move {
            let audience = Audience::load(&db, org, user).await.unwrap();
            let statement = format!(
                "SELECT r.id FROM neev_resources r LEFT JOIN neev_resources s ON s.org_id=r.org_id AND s.id=r.origin_chat_id AND s.kind='chat' WHERE r.org_id=$1 AND r.id=$2 AND {}",
                audience.readable("r", "s")
            );
            db::one(&db, &statement, vec![org.into(), id.into()]).await.unwrap().is_some()
        }
    };
    let c = resources::get(&w.state, &w.creator, &w.c.id).await.unwrap();
    let a = resources::get(&w.state, &w.creator, &w.a.id).await.unwrap();
    let viewer = w.viewer.identity.user_id;
    let mate = w.mate.identity.user_id;
    assert_eq!(rust(viewer, c.clone()).await, Some(Level::View));
    assert_eq!(rust(mate, a.clone()).await, Some(Level::View));
    assert!(sql(viewer, c.id.clone()).await && sql(mate, a.id.clone()).await);
    // A stale direct-grant row for a member made inactive behind the API.
    db::execute(&w.db, "UPDATE neev_memberships SET active=false WHERE org_id=$1 AND user_id=$2", vec![w.org.into(), viewer.into()]).await.unwrap();
    assert_eq!(h::count(&w.db, &format!("neev_grants WHERE principal_id='{viewer}'")).await, 1, "the row is still there");
    assert_eq!(rust(viewer, c.clone()).await, None);
    assert!(!sql(viewer, c.id.clone()).await);
    db::execute(&w.db, "UPDATE neev_memberships SET active=true WHERE org_id=$1 AND user_id=$2", vec![w.org.into(), viewer.into()]).await.unwrap();
    assert_eq!(rust(viewer, c.clone()).await, Some(Level::View));
    // A disabled account with an active membership: neither branch names it.
    for user in [viewer, mate] {
        db::execute(&w.db, "UPDATE neev_accounts SET disabled=true WHERE id=$1", vec![user.into()]).await.unwrap();
    }
    assert_eq!(rust(viewer, c.clone()).await, None, "user branch");
    assert_eq!(rust(mate, a.clone()).await, None, "team branch");
    assert_eq!(rust(mate, c.clone()).await, None, "team branch, inherited");
    assert!(!sql(viewer, c.id.clone()).await && !sql(mate, a.id.clone()).await);
    // Disabled accounts are refused as new principals too.
    let revision = c.auth_revision;
    assert_eq!(
        grants::set_resource_grant(&w.state, &w.creator, &c.id, revision, &Principal::User(viewer), Some(Level::Edit)).await.err(),
        Some(SharingError::Content(ContentError::NotFound))
    );
}

#[tokio::test]
async fn inactive_or_disabled_principals_are_named_by_no_grant() {
    inactive_or_disabled_principals_read_nothing(&world(h::database().await).await).await;
}

async fn node_ids(w: &World, actor: &OrgContext) -> BTreeSet<i32> {
    nodes::list(&w.state, actor, None).await.unwrap().into_iter().map(|n| n.id).collect()
}

async fn restrict(w: &World, actor: &OrgContext, node_id: i32, on: bool) -> Result<grants::NodeAccess, ContentError> {
    let revision = node_revision_for(w, actor, node_id).await;
    grants::set_node_restricted(&w.state, actor, node_id, revision, on).await
}

async fn node_revision_for(w: &World, actor: &OrgContext, node_id: i32) -> i64 {
    grants::node_access(&w.state, actor, node_id).await.unwrap().revision
}

/// v2: a restricted folder and everything in it is hidden from everyone but
/// its creator and its recipients — in the tree, lists, counts, the wiki and
/// direct lookups — while explicit child and item shares still work with the
/// parent redacted, and placement/moves/forks never write into or widen it.
async fn restricted_folders_hide_and_redact(w: &World) {
    use crate::neev::error::AuthError;
    // Before: everything visible; an org doc deep inside is org knowledge.
    let deep_doc = resources::create_with_visibility(&w.state, &w.creator, input(w.deep, ResourceKind::Document, "Deep", None), Some(Visibility::Organization))
        .await
        .unwrap();
    assert!(crate::neev::knowledge::access::is_eligible(&w.db, w.org, &deep_doc.id).await.unwrap().is_some());
    assert!(node_ids(w, &w.outsider).await.contains(&w.deep));

    // Only the folder's creator restricts it.
    let revision = node_revision(w, w.folder).await;
    assert_eq!(grants::set_node_restricted(&w.state, &w.outsider, w.folder, revision, true).await.err(), Some(ContentError::Auth(AuthError::Forbidden)));
    assert_eq!(grants::set_node_restricted(&w.state, &w.admin, w.folder, revision, true).await.err(), Some(ContentError::Auth(AuthError::Forbidden)));
    let restricted = restrict(w, &w.creator, w.folder, true).await.unwrap();
    assert!(restricted.restricted);
    assert_eq!(restricted.level, Level::Owner);
    crate::neev::db::validate_content_snapshot(&w.db).await.unwrap();
    assert_eq!(h::count(&w.db, "neev_restricted_nodes").await, 2, "the folder and its child");

    // Hidden from everyone else — admins included — in the tree and direct lookups.
    for actor in [&w.outsider, &w.admin, &w.viewer, &w.mate] {
        let seen = node_ids(w, actor).await;
        assert!(seen.contains(&w.project) && !seen.contains(&w.folder) && !seen.contains(&w.deep));
        assert!(nodes::get(&w.state, actor, w.deep).await.unwrap().is_none());
        assert_eq!(grants::node_access(&w.state, actor, w.folder).await.err(), Some(ContentError::NotFound));
        assert_eq!(resources::list(&w.state, actor, w.deep, ResourceKind::Document, false, None).await.err(), Some(ContentError::NotFound));
        assert_eq!(sharing::get(&w.state, actor, w.folder).await.err(), Some(ContentError::NotFound));
    }
    assert!(node_ids(w, &w.creator).await.contains(&w.deep));
    // A legacy org-visible item inside does not defeat the restriction; nor do
    // grants from ABOVE it (the team's project grant); the wiki drops it.
    assert_eq!(level_of(w, &w.outsider, &deep_doc.id).await, None);
    assert_eq!(level_of(w, &w.mate, &w.a.id).await, None, "a project grant does not reach into a restricted folder");
    assert!(crate::neev::knowledge::access::is_eligible(&w.db, w.org, &deep_doc.id).await.unwrap().is_none());
    assert!(crate::neev::knowledge::access::is_eligible(&w.db, w.org, &w.d.id).await.unwrap().is_some(), "outside it nothing changed");

    // A View recipient of the folder sees it all, read-only.
    let revision = node_revision_for(w, &w.creator, w.folder).await;
    grants::set_node_grant(&w.state, &w.creator, w.folder, revision, &Principal::User(w.mate.identity.user_id), Some(Level::View)).await.unwrap();
    assert!(node_ids(w, &w.mate).await.contains(&w.deep));
    assert_eq!(level_of(w, &w.mate, &deep_doc.id).await, Some(Level::View), "org-visible in a restricted folder is the folder level");
    assert!(!writable(w, &w.mate, &deep_doc.id).await);
    assert_eq!(level_of(w, &w.mate, &w.a.id).await, Some(Level::View));
    assert_eq!(level_of(w, &w.mate, &w.b.id).await, None, "the private exception holds");
    assert_eq!(
        resources::create(&w.state, &w.mate, input(w.deep, ResourceKind::Document, "No", None)).await.err(),
        Some(ContentError::Auth(AuthError::Forbidden))
    );
    assert_eq!(
        nodes::create(&w.state, &w.mate, NodeInput { parent_id: Some(w.deep), kind: NodeKind::Folder, name: "No".into() }).await.err(),
        Some(AuthError::Forbidden)
    );
    let mate_view = grants::node_access(&w.state, &w.mate, w.deep).await.unwrap();
    assert_eq!((mate_view.level, mate_view.can_manage), (Level::View, false));
    assert_eq!(grants::set_node_restricted(&w.state, &w.mate, w.folder, revision + 1, false).await.err(), Some(ContentError::Auth(AuthError::Forbidden)));

    // An explicit child share: Edit on the inner folder only. Its parent stays
    // hidden (id, never name); work inside is allowed; leaving is not.
    let revision = node_revision_for(w, &w.creator, w.deep).await;
    grants::set_node_grant(&w.state, &w.creator, w.deep, revision, &Principal::User(w.editor.identity.user_id), Some(Level::Edit)).await.unwrap();
    let seen = node_ids(w, &w.editor).await;
    assert!(seen.contains(&w.deep) && !seen.contains(&w.folder));
    let child = nodes::get(&w.state, &w.editor, w.deep).await.unwrap().unwrap();
    assert_eq!(child.parent_id, Some(w.folder), "a shared root keeps its parent id");
    assert!(nodes::get(&w.state, &w.editor, w.folder).await.unwrap().is_none());
    let access = grants::resource_access(&w.state, &w.editor, &deep_doc.id).await.unwrap();
    assert!(access.inherited.iter().all(|g| g.node_id == w.deep), "no hidden ancestor's name: {:?}", access.inherited);
    assert_eq!(access.level, Level::Edit);
    let own = resources::create(&w.state, &w.editor, input(w.deep, ResourceKind::Document, "Mine", None)).await.unwrap();
    let mut out = super::test_support::patch(&own);
    out.node_id = w.project;
    assert_eq!(resources::update(&w.state, &w.editor, &own.id, out).await.err(), Some(ContentError::Auth(AuthError::Forbidden)), "only the restricted root's creator may move things out");
    let deep_node = nodes::get(&w.state, &w.editor, w.deep).await.unwrap().unwrap();
    assert_eq!(
        nodes::update(&w.state, &w.editor, w.deep, deep_node.revision, NodeInput { parent_id: Some(w.project), kind: NodeKind::Folder, name: deep_node.name.clone() }).await.err(),
        Some(AuthError::Forbidden)
    );
    // Renaming in place is fine with Edit.
    nodes::update(&w.state, &w.editor, w.deep, deep_node.revision, NodeInput { parent_id: Some(w.folder), kind: NodeKind::Folder, name: "Renamed".into() }).await.unwrap();

    // An item shared directly with someone who cannot see its folder.
    let a = auth_revision(w, &w.a.id).await;
    grants::set_resource_grant(&w.state, &w.creator, &w.a.id, a, &Principal::User(w.viewer.identity.user_id), Some(Level::View)).await.unwrap();
    assert_eq!(level_of(w, &w.viewer, &w.a.id).await, Some(Level::View));
    let shared = grants::shared_in_hidden(&w.state, &w.viewer).await.unwrap();
    assert_eq!(shared.iter().map(|r| r.id.clone()).collect::<Vec<_>>(), vec![w.a.id.clone()]);
    assert!(grants::resource_access(&w.state, &w.viewer, &w.a.id).await.unwrap().inherited.is_empty());
    // A fork from a hidden folder lands where the forker may place a chat.
    let f = resources::get(&w.state, &w.creator, &w.f.id).await.unwrap();
    grants::set_resource_grant(&w.state, &w.creator, &f.id, f.auth_revision, &Principal::User(w.viewer.identity.user_id), Some(Level::View)).await.unwrap();
    messages::append(&w.state, &w.creator, &f.id, &uuid::Uuid::new_v4().to_string(), f.auth_revision + 1, "Ask").await.unwrap();
    let reply = messages::publish_agent_reply(&w.state, &w.creator, &f.id, "Answer", "claude", &uuid::Uuid::new_v4().to_string()).await.unwrap();
    // The walk stops at the hidden folder (review P-2): name a target.
    assert!(matches!(
        super::fork::fork_chat(&w.state, &w.viewer, &f.id, &reply.id, &uuid::Uuid::new_v4().to_string()).await,
        Err(super::fork::ForkError::NoLocation)
    ));
    let fork = super::fork::fork_chat_into(&w.state, &w.viewer, &f.id, &reply.id, &uuid::Uuid::new_v4().to_string(), Some(w.project))
        .await
        .unwrap_or_else(|_| panic!("fork refused"));
    assert_eq!(fork.node_id, w.project);
    assert_eq!(level_of(w, &w.viewer, &fork.chat_id).await, Some(Level::Owner));

    // Losing the folder keeps your own items (location redacted).
    let revision = node_revision_for(w, &w.creator, w.deep).await;
    grants::set_node_grant(&w.state, &w.creator, w.deep, revision, &Principal::User(w.editor.identity.user_id), None).await.unwrap();
    assert!(!node_ids(w, &w.editor).await.contains(&w.deep));
    assert_eq!(level_of(w, &w.editor, &own.id).await, Some(Level::Owner));
    assert_eq!(level_of(w, &w.editor, &deep_doc.id).await, None);
    assert!(grants::shared_in_hidden(&w.state, &w.editor).await.unwrap().iter().any(|r| r.id == own.id));

    // Trashing the open project would hide what others cannot see: refused.
    let preview = crate::neev::catalog::trash::preview(&w.state, &w.outsider, w.project).await.unwrap_or_else(|_| panic!("preview"));
    assert!(preview.private_others);
    assert!(crate::neev::catalog::trash::preview(&w.state, &w.outsider, w.folder).await.is_err());

    // The Rust and SQL forms still agree for every reader, in this state.
    let all = [&w.a, &w.b, &w.c, &w.d, &w.e, &w.f, &deep_doc, &own];
    for actor in [&w.creator, &w.viewer, &w.editor, &w.mate, &w.outsider, &w.admin] {
        let sql = sql_readable(w, actor).await;
        for resource in all {
            let got = level_of(w, actor, &resource.id).await;
            assert_eq!(sql.contains(&resource.id), got.is_some(), "SQL disagrees: user {} on {}", actor.identity.user_id, resource.title);
        }
    }

    // Reopening widens back exactly to the open rule; the region empties.
    restrict(w, &w.creator, w.folder, false).await.unwrap();
    assert_eq!(h::count(&w.db, "neev_restricted_nodes").await, 0);
    assert!(node_ids(w, &w.outsider).await.contains(&w.deep));
    assert_eq!(level_of(w, &w.outsider, &deep_doc.id).await, Some(Level::Edit));
    assert!(crate::neev::knowledge::access::is_eligible(&w.db, w.org, &deep_doc.id).await.unwrap().is_some());
    // Downgrade refuses while a restriction (or any share) exists.
    restrict(w, &w.creator, w.folder, true).await.unwrap();
    assert!(matches!(
        crate::neev::db::downgrade_member_sharing(&w.db).await,
        Ok(crate::neev::db::member_sharing_migration::Downgrade::SharingExists { .. })
    ));
    crate::neev::db::validate_content_snapshot(&w.db).await.unwrap();
    // A new folder inside joins the region in the same transaction.
    nodes::create(&w.state, &w.creator, NodeInput { parent_id: Some(w.folder), kind: NodeKind::Folder, name: "New".into() }).await.unwrap();
    assert_eq!(h::count(&w.db, "neev_restricted_nodes").await, 3);
    crate::neev::db::validate_content_snapshot(&w.db).await.unwrap();
}

/// The derived region equals the marks after every shape change (P-5).
async fn region_agrees(w: &World) {
    crate::neev::db::validate_content_snapshot(&w.db).await.unwrap();
}

/// Review P-4 / P-1: nobody hides someone else's folder by moving it into a
/// restriction, and an Edit recipient never re-shares inside one.
async fn moves_and_resharing_respect_restriction(w: &World) {
    use crate::neev::error::AuthError;
    restrict(w, &w.creator, w.folder, true).await.unwrap();
    region_agrees(w).await;
    // Another member's open folder cannot be moved into the creator's restriction.
    let theirs = node(&w.state, &w.outsider, None, "Theirs").await;
    let their_folder = node(&w.state, &w.outsider, Some(theirs), "Their folder").await;
    let current = nodes::get(&w.state, &w.creator, their_folder).await.unwrap().unwrap();
    assert_eq!(
        nodes::update(&w.state, &w.creator, their_folder, current.revision, NodeInput { parent_id: Some(w.folder), kind: NodeKind::Folder, name: current.name.clone() }).await.err(),
        Some(AuthError::Forbidden)
    );
    assert!(node_ids(w, &w.outsider).await.contains(&their_folder), "unchanged for its owner");
    region_agrees(w).await;
    // Your own folder: allowed, and the region grows in the same transaction.
    let mine = node(&w.state, &w.creator, Some(w.project), "Mine").await;
    let current = nodes::get(&w.state, &w.creator, mine).await.unwrap().unwrap();
    nodes::update(&w.state, &w.creator, mine, current.revision, NodeInput { parent_id: Some(w.folder), kind: NodeKind::Folder, name: "Mine".into() }).await.unwrap();
    assert!(!node_ids(w, &w.outsider).await.contains(&mine));
    region_agrees(w).await;
    // Out again (the root's creator), then nested restrict / unrestrict.
    let current = nodes::get(&w.state, &w.creator, mine).await.unwrap().unwrap();
    nodes::update(&w.state, &w.creator, mine, current.revision, NodeInput { parent_id: Some(w.project), kind: NodeKind::Folder, name: "Mine".into() }).await.unwrap();
    assert!(node_ids(w, &w.outsider).await.contains(&mine));
    region_agrees(w).await;

    // An Edit recipient of the restricted folder.
    let revision = node_revision_for(w, &w.creator, w.folder).await;
    grants::set_node_grant(&w.state, &w.creator, w.folder, revision, &Principal::User(w.editor.identity.user_id), Some(Level::Edit)).await.unwrap();
    let sub = node(&w.state, &w.editor, Some(w.folder), "Sub").await;
    region_agrees(w).await;
    // P-1: they made the subfolder, but may not share it (nor anything) inside the restriction.
    let revision = node_revision_for(w, &w.editor, sub).await;
    assert_eq!(
        grants::set_node_grant(&w.state, &w.editor, sub, revision, &Principal::User(w.outsider.identity.user_id), Some(Level::View)).await.err(),
        Some(ContentError::Auth(AuthError::Forbidden))
    );
    assert!(!grants::node_access(&w.state, &w.editor, sub).await.unwrap().can_manage);
    let revision = node_revision_for(w, &w.editor, w.deep).await;
    assert_eq!(
        grants::set_node_grant(&w.state, &w.editor, w.deep, revision, &Principal::User(w.outsider.identity.user_id), Some(Level::View)).await.err(),
        Some(ContentError::Auth(AuthError::Forbidden))
    );
    assert!(grants::node_access(&w.state, &w.creator, sub).await.unwrap().can_manage, "the root's creator manages all of it");
    // P-8: an Edit recipient cannot create a nested restriction (it could
    // strand content); the region's controller can, and still sees it.
    let revision = node_revision_for(w, &w.editor, sub).await;
    assert_eq!(
        grants::set_node_restricted(&w.state, &w.editor, sub, revision, true).await.err(),
        Some(ContentError::Auth(AuthError::Forbidden))
    );
    assert!(!grants::node_access(&w.state, &w.editor, sub).await.unwrap().can_restrict);
    assert!(grants::node_access(&w.state, &w.creator, sub).await.unwrap().can_restrict);
    restrict(w, &w.creator, sub, true).await.unwrap();
    region_agrees(w).await;
    assert!(node_ids(w, &w.creator).await.contains(&sub), "the controller always sees nested restrictions");
    // P-4 nested: nobody pulls another's folder into the nested restriction.
    let deep = nodes::get(&w.state, &w.editor, w.deep).await.unwrap().unwrap();
    assert_eq!(
        nodes::update(&w.state, &w.editor, w.deep, deep.revision, NodeInput { parent_id: Some(sub), kind: NodeKind::Folder, name: deep.name.clone() }).await.err(),
        Some(AuthError::Forbidden)
    );
    // Removing the recipient strands nothing: the controller reopens it.
    let revision = node_revision_for(w, &w.creator, w.folder).await;
    grants::set_node_grant(&w.state, &w.creator, w.folder, revision, &Principal::User(w.editor.identity.user_id), None).await.unwrap();
    assert!(!node_ids(w, &w.editor).await.contains(&sub));
    restrict(w, &w.creator, sub, false).await.unwrap();
    assert!(node_ids(w, &w.creator).await.contains(&sub));
    region_agrees(w).await;
    // An admin has no restriction bypass on an open folder they may manage.
    let open = grants::node_access(&w.state, &w.admin, w.project).await.unwrap();
    assert!(open.can_manage && !open.can_restrict);
    // Trash and restore inside the region keep it consistent.
    crate::neev::catalog::trash::trash(&w.state, &w.creator, sub, node_revision_for(w, &w.creator, sub).await)
        .await
        .unwrap_or_else(|_| panic!("trash"));
    region_agrees(w).await;
    crate::neev::catalog::trash::restore(&w.state, &w.creator, sub).await.unwrap_or_else(|_| panic!("restore"));
    region_agrees(w).await;
    // A fork from a hidden folder never walks through it: refused until a target is named.
    let f = resources::get(&w.state, &w.creator, &w.f.id).await.unwrap();
    grants::set_resource_grant(&w.state, &w.creator, &f.id, f.auth_revision, &Principal::User(w.viewer.identity.user_id), Some(Level::View)).await.unwrap();
    messages::append(&w.state, &w.creator, &f.id, &uuid::Uuid::new_v4().to_string(), f.auth_revision + 1, "Ask").await.unwrap();
    let reply = messages::publish_agent_reply(&w.state, &w.creator, &f.id, "Answer", "claude", &uuid::Uuid::new_v4().to_string()).await.unwrap();
    assert!(matches!(
        super::fork::fork_chat(&w.state, &w.viewer, &f.id, &reply.id, &uuid::Uuid::new_v4().to_string()).await,
        Err(super::fork::ForkError::NoLocation)
    ));
    assert!(matches!(
        super::fork::fork_chat_into(&w.state, &w.viewer, &f.id, &reply.id, &uuid::Uuid::new_v4().to_string(), Some(w.deep)).await,
        Err(super::fork::ForkError::Content(ContentError::NotFound))
    ));
    let fork = super::fork::fork_chat_into(&w.state, &w.viewer, &f.id, &reply.id, &uuid::Uuid::new_v4().to_string(), Some(w.project))
        .await
        .unwrap_or_else(|_| panic!("fork refused"));
    assert_eq!(fork.node_id, w.project);
}

/// Review T-1: nobody takes control of someone else's restriction by
/// restricting an ancestor of it or moving it across a restriction boundary.
async fn ancestor_takeover_is_refused(w: &World) {
    use crate::neev::error::AuthError;
    // M (outsider) owns an open project and folder; A (mate) keeps a private
    // restricted area inside it.
    let mp = node(&w.state, &w.outsider, None, "M project").await;
    let mf = node(&w.state, &w.outsider, Some(mp), "M folder").await;
    let ra = node(&w.state, &w.mate, Some(mf), "A private").await;
    restrict(w, &w.mate, ra, true).await.unwrap();
    assert!(!node_ids(w, &w.outsider).await.contains(&ra));
    // M restricting the ancestor would make M RA's controller: refused.
    let revision = node_revision_for(w, &w.outsider, mf).await;
    assert_eq!(
        grants::set_node_restricted(&w.state, &w.outsider, mf, revision, true).await.err(),
        Some(ContentError::Auth(AuthError::Forbidden))
    );
    assert!(!node_ids(w, &w.outsider).await.contains(&ra));
    assert!(!grants::node_access(&w.state, &w.outsider, mf).await.unwrap().restricted);
    // Moving it into M's own restriction: refused.
    let mr = node(&w.state, &w.outsider, Some(mp), "M restricted").await;
    restrict(w, &w.outsider, mr, true).await.unwrap();
    let current = nodes::get(&w.state, &w.outsider, mf).await.unwrap().unwrap();
    assert_eq!(
        nodes::update(&w.state, &w.outsider, mf, current.revision, NodeInput { parent_id: Some(mr), kind: NodeKind::Folder, name: current.name.clone() }).await.err(),
        Some(AuthError::Forbidden)
    );
    // Moving it into someone else's region where M has Edit: refused too.
    restrict(w, &w.creator, w.folder, true).await.unwrap();
    let revision = node_revision_for(w, &w.creator, w.folder).await;
    grants::set_node_grant(&w.state, &w.creator, w.folder, revision, &Principal::User(w.outsider.identity.user_id), Some(Level::Edit)).await.unwrap();
    assert_eq!(
        nodes::update(&w.state, &w.outsider, mf, current.revision, NodeInput { parent_id: Some(w.folder), kind: NodeKind::Folder, name: current.name.clone() }).await.err(),
        Some(AuthError::Forbidden)
    );
    // A's area is untouched and still A's alone.
    assert!(node_ids(w, &w.mate).await.contains(&ra));
    assert!(grants::node_access(&w.state, &w.mate, ra).await.unwrap().can_restrict);
    region_agrees(w).await;
    // Control: A restricts an ancestor holding only A's own restriction.
    let af = node(&w.state, &w.mate, Some(mp), "A folder").await;
    let ar = node(&w.state, &w.mate, Some(af), "A inner").await;
    restrict(w, &w.mate, ar, true).await.unwrap();
    restrict(w, &w.mate, af, true).await.unwrap();
    assert!(node_ids(w, &w.mate).await.contains(&ar));
    // ...and reopens it again.
    restrict(w, &w.mate, af, false).await.unwrap();
    region_agrees(w).await;
    // Without M's restricted subfolder in the way, M may restrict a folder of
    // M's own that holds only open content.
    let plain = node(&w.state, &w.outsider, Some(mp), "Plain").await;
    node(&w.state, &w.outsider, Some(plain), "Plain child").await;
    restrict(w, &w.outsider, plain, true).await.unwrap();
    region_agrees(w).await;
}

#[tokio::test]
async fn restricting_or_moving_an_ancestor_never_takes_over_a_restriction() {
    ancestor_takeover_is_refused(&world(h::database().await).await).await;
}

#[tokio::test]
async fn moves_and_resharing_cannot_hide_or_widen_a_restriction() {
    moves_and_resharing_respect_restriction(&world(h::database().await).await).await;
}

#[tokio::test]
async fn restricted_folders_hide_contents_and_redact_parents() {
    restricted_folders_hide_and_redact(&world(h::database().await).await).await;
}

#[tokio::test]
async fn the_rust_rule_and_the_sql_rule_agree() {
    the_rules_agree(&world(h::database().await).await).await;
}

#[tokio::test]
async fn view_is_read_only_everywhere_a_write_exists() {
    view_is_read_only(&world(h::database().await).await).await;
}

#[tokio::test]
async fn grants_follow_shared_and_private_stays_an_exception() {
    grants_lifecycle(&world(h::database().await).await).await;
}

#[tokio::test]
async fn revocation_is_immediate_and_a_reinvite_resurrects_nothing() {
    revocation_is_immediate_and_permanent(&world(h::database().await).await).await;
}

#[tokio::test]
async fn lists_mentions_knowledge_and_folder_defaults_follow_the_rule() {
    lists_mentions_knowledge_and_defaults(&world(h::database().await).await).await;
}

#[tokio::test]
async fn shared_agent_chats_are_view_only_and_fork_privately() {
    agent_chats_are_view_only_and_fork_privately(&world(h::database().await).await).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_sharing_rules_hold() {
    for scenario in 0..10 {
        let (db, admin, schema) = h::postgres_database().await;
        let w = world(db.clone()).await;
        match scenario {
            0 => the_rules_agree(&w).await,
            1 => view_is_read_only(&w).await,
            2 => grants_lifecycle(&w).await,
            3 => revocation_is_immediate_and_permanent(&w).await,
            4 => lists_mentions_knowledge_and_defaults(&w).await,
            5 => inactive_or_disabled_principals_read_nothing(&w).await,
            6 => restricted_folders_hide_and_redact(&w).await,
            7 => moves_and_resharing_respect_restriction(&w).await,
            8 => ancestor_takeover_is_refused(&w).await,
            _ => agent_chats_are_view_only_and_fork_privately(&w).await,
        }
        drop(w);
        h::cleanup_postgres(db, admin, schema).await;
    }
}
