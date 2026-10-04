//! Move to Trash / restore for projects and folders: two members (A = owner,
//! B = member) of one org plus a foreign org (C).

use crate::neev::{
    content::{
        resources,
        types::{ContentError, Resource, ResourceInput, ResourceKind, ResourcePatch, Visibility},
    },
    db,
    error::AuthError,
    knowledge::access::eligible_sources,
    test_support::{self as support, OrgHarness},
    types::{Node, NodeInput, NodeKind, OrgContext},
};

use super::{
    nodes,
    trash::{self, Blocker, TrashError},
};

async fn node(h: &OrgHarness, actor: &OrgContext, parent: Option<&Node>, name: &str) -> Node {
    nodes::create(
        h.state(),
        actor,
        NodeInput {
            parent_id: parent.map(|p| p.id),
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

async fn resource(
    h: &OrgHarness,
    actor: &OrgContext,
    at: &Node,
    kind: ResourceKind,
    visibility: Visibility,
) -> Resource {
    resources::create_with_visibility(
        h.state(),
        actor,
        ResourceInput {
            node_id: at.id,
            kind,
            title: "Item".into(),
            origin_chat_id: None,
            request_id: uuid::Uuid::new_v4().to_string(),
        },
        Some(visibility),
    )
    .await
    .unwrap()
}

async fn current(h: &OrgHarness, id: i32) -> Node {
    nodes::list(h.state(), h.owner_context(), None)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.id == id)
        .expect("node is live")
}

async fn visible_ids(h: &OrgHarness, actor: &OrgContext) -> Vec<i32> {
    nodes::list(h.state(), actor, None)
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.id)
        .collect()
}

async fn trash_node(
    h: &OrgHarness,
    actor: &OrgContext,
    id: i32,
) -> Result<trash::Trashed, TrashError> {
    let revision = current(h, id).await.revision;
    trash::trash(h.state(), actor, id, revision).await
}

async fn archived(h: &OrgHarness, id: &str) -> bool {
    db::one(
        &h.database,
        "SELECT archived FROM neev_resources WHERE id=$1",
        vec![id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "archived")
    .unwrap()
}

async fn visibility(h: &OrgHarness, id: &str) -> String {
    db::one(
        &h.database,
        "SELECT visibility FROM neev_resources WHERE id=$1",
        vec![id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "visibility")
    .unwrap()
}

async fn eligible(h: &OrgHarness) -> Vec<String> {
    eligible_sources(&h.database, h.org_id())
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect()
}

fn archive_patch(r: &Resource, archived: bool) -> ResourcePatch {
    ResourcePatch {
        expected_auth_revision: r.auth_revision,
        title: r.title.clone(),
        node_id: r.node_id,
        visibility: r.visibility,
        archived,
    }
}

#[tokio::test]
async fn trashed_subtree_is_hidden_from_both_members_and_restores_in_place() {
    let h = OrgHarness::new().await;
    let (a, b) = (h.owner(), h.member());
    let project = node(&h, &a, None, "Project").await;
    let folder = node(&h, &a, Some(&project), "Folder").await;
    let nested = node(&h, &b, Some(&folder), "Nested").await;
    let doc = resource(
        &h,
        &b,
        &nested,
        ResourceKind::Document,
        Visibility::Organization,
    )
    .await;

    let preview = trash::preview(h.state(), &a, folder.id).await.unwrap();
    assert_eq!(
        (preview.folders, preview.documents, preview.chats),
        (1, 1, 0)
    );
    assert_eq!(
        (preview.agent_active, preview.in_use, preview.private_others),
        (false, false, false)
    );

    let done = trash_node(&h, &a, folder.id).await.unwrap();
    assert_eq!((done.nodes, done.resources), (2, 1));
    for actor in [&a, &b] {
        let ids = visible_ids(&h, actor).await;
        assert!(ids.contains(&project.id));
        assert!(!ids.contains(&folder.id) && !ids.contains(&nested.id));
    }
    assert!(nodes::get(h.state(), &b, nested.id)
        .await
        .unwrap()
        .is_none());
    assert!(archived(&h, &doc.id).await);

    // B (not the trasher) sees it in the trash and restores it.
    let items = trash::list(h.state(), &b).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].node_id, folder.id);
    assert_eq!(items[0].parent_name.as_deref(), Some("Project"));
    assert_eq!((items[0].folders, items[0].documents), (1, 1));
    let restored = trash::restore(h.state(), &b, folder.id).await.unwrap();
    assert_eq!((restored.nodes, restored.resources), (2, 1));
    let back = current(&h, folder.id).await;
    assert_eq!(
        back.parent_id,
        Some(project.id),
        "restored to its original location"
    );
    assert_eq!(current(&h, nested.id).await.parent_id, Some(folder.id));
    assert!(!archived(&h, &doc.id).await);
    assert!(trash::list(h.state(), &a).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_independently_trashed_descendant_does_not_come_back_with_its_parent() {
    let h = OrgHarness::new().await;
    let a = h.owner();
    let project = node(&h, &a, None, "Project").await;
    let child = node(&h, &a, Some(&project), "Child").await;
    let grandchild = node(&h, &a, Some(&child), "Grandchild").await;
    let sibling = node(&h, &a, Some(&project), "Sibling").await;
    let child_doc = resource(
        &h,
        &a,
        &grandchild,
        ResourceKind::Document,
        Visibility::Organization,
    )
    .await;
    let project_doc = resource(
        &h,
        &a,
        &project,
        ResourceKind::Chat,
        Visibility::Organization,
    )
    .await;
    // A document the user archived on its own, before any folder trash.
    let loose = resource(
        &h,
        &a,
        &sibling,
        ResourceKind::Document,
        Visibility::Organization,
    )
    .await;
    resources::update(h.state(), &a, &loose.id, archive_patch(&loose, true))
        .await
        .unwrap();

    trash_node(&h, &a, child.id).await.unwrap();
    let parent_batch = trash_node(&h, &a, project.id).await.unwrap();
    assert_eq!(
        parent_batch.nodes, 2,
        "project + sibling; the child is in its own batch"
    );
    assert_eq!(
        parent_batch.resources, 1,
        "only the project chat; loose was already archived"
    );
    assert_eq!(trash::list(h.state(), &a).await.unwrap().len(), 2);

    // The child cannot come back while its parent is trashed.
    assert_eq!(
        trash::restore(h.state(), &a, child.id).await,
        Err(TrashError::ParentTrashed)
    );
    // Only a batch root restores.
    assert_eq!(
        trash::restore(h.state(), &a, grandchild.id).await,
        Err(TrashError::NotFound)
    );

    trash::restore(h.state(), &a, project.id).await.unwrap();
    let ids = visible_ids(&h, &a).await;
    assert!(ids.contains(&project.id) && ids.contains(&sibling.id));
    assert!(!ids.contains(&child.id) && !ids.contains(&grandchild.id));
    assert!(!archived(&h, &project_doc.id).await);
    assert!(
        archived(&h, &child_doc.id).await,
        "still in the child's batch"
    );
    assert!(
        archived(&h, &loose.id).await,
        "archived on its own; restore never touches it"
    );

    trash::restore(h.state(), &a, child.id).await.unwrap();
    assert_eq!(current(&h, grandchild.id).await.parent_id, Some(child.id));
    assert!(!archived(&h, &child_doc.id).await);
    assert!(archived(&h, &loose.id).await);
}

#[tokio::test]
async fn another_members_private_content_blocks_the_trash_without_writing() {
    let h = OrgHarness::new().await;
    let (a, b) = (h.owner(), h.member());
    let project = node(&h, &a, None, "Project").await;
    let folder = node(&h, &a, Some(&project), "Folder").await;
    let mine = resource(&h, &a, &folder, ResourceKind::Document, Visibility::Private).await;
    let shared = resource(
        &h,
        &b,
        &folder,
        ResourceKind::Chat,
        Visibility::Organization,
    )
    .await;
    let theirs = resource(&h, &b, &folder, ResourceKind::Document, Visibility::Private).await;

    let preview = trash::preview(h.state(), &a, folder.id).await.unwrap();
    // A sees counts only for what A can read; B's private doc is a yes/no
    // blocker, never a count.
    assert_eq!(
        (preview.documents, preview.chats, preview.private_others),
        (1, 1, true)
    );
    let revision = current(&h, folder.id).await.revision;
    assert_eq!(
        trash::trash(h.state(), &a, folder.id, revision).await,
        Err(TrashError::Blocked {
            blocker: Blocker::PrivateContent
        })
    );
    assert!(visible_ids(&h, &a).await.contains(&folder.id));
    for id in [&mine.id, &shared.id, &theirs.id] {
        assert!(!archived(&h, id).await, "a refused trash writes nothing");
    }
    assert_eq!(current(&h, folder.id).await.revision, revision);

    // B owns the private doc, so B may trash the folder; A's private doc then blocks B.
    assert_eq!(
        trash_node(&h, &b, folder.id).await,
        Err(TrashError::Blocked {
            blocker: Blocker::PrivateContent
        })
    );
    // Once B archives their own private doc, A can trash; B's archived doc is not in A's batch.
    resources::update(h.state(), &b, &theirs.id, archive_patch(&theirs, true))
        .await
        .unwrap();
    let done = trash_node(&h, &a, folder.id).await.unwrap();
    assert_eq!(done.resources, 2);
    trash::restore(h.state(), &a, folder.id).await.unwrap();
    assert!(archived(&h, &theirs.id).await);
}

#[tokio::test]
async fn a_running_agent_in_the_subtree_blocks_the_trash() {
    let h = OrgHarness::new().await;
    let a = h.owner();
    let project = node(&h, &a, None, "Project").await;
    let folder = node(&h, &a, Some(&project), "Folder").await;
    let session = uuid::Uuid::new_v4().to_string();
    db::execute(
        &h.database,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,visibility,kind,bound_chat_id,bound_kind,node_id,archived,revision,created_at,updated_at) VALUES($1,$2,$3,'S','claude','default','private','chat',NULL,NULL,$4,false,1,0,0)",
        vec![h.org_id().into(), session.clone().into(), a.identity.user_id.into(), folder.id.into()],
    )
    .await
    .unwrap();
    let turn = uuid::Uuid::new_v4().to_string();
    db::execute(
        &h.database,
        "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,request_id,prompt,response,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,1,$3,$4,NULL,'p','','running',NULL,'claude','default',0,0,NULL)",
        vec![h.org_id().into(), session.clone().into(), turn.clone().into(), uuid::Uuid::new_v4().to_string().into()],
    )
    .await
    .unwrap();

    assert_eq!(
        trash::preview(h.state(), &a, project.id)
            .await
            .unwrap()
            .agent_active,
        true
    );
    assert_eq!(
        trash_node(&h, &a, project.id).await,
        Err(TrashError::Blocked {
            blocker: Blocker::AgentActive
        })
    );
    db::execute(
        &h.database,
        "UPDATE neev_agent_turns SET status='completed',completed_at=1 WHERE id=$1",
        vec![turn.into()],
    )
    .await
    .unwrap();
    trash_node(&h, &a, project.id).await.unwrap();
    // The session and its history are untouched by trash.
    assert_eq!(support::count(&h.database, "neev_agent_turns").await, 1);
    assert_eq!(support::count(&h.database, "neev_agent_sessions").await, 1);
}

#[tokio::test]
async fn knowledge_eligibility_follows_the_trash_and_private_stays_private() {
    let h = OrgHarness::new().await;
    let (a, b) = (h.owner(), h.member());
    let project = node(&h, &a, None, "Project").await;
    let folder = node(&h, &a, Some(&project), "Folder").await;
    let shared = resource(
        &h,
        &a,
        &folder,
        ResourceKind::Document,
        Visibility::Organization,
    )
    .await;
    let private = resource(&h, &a, &folder, ResourceKind::Document, Visibility::Private).await;
    let shared_chat = resource(
        &h,
        &b,
        &folder,
        ResourceKind::Chat,
        Visibility::Organization,
    )
    .await;
    let before = eligible(&h).await;
    assert!(before.contains(&shared.id) && before.contains(&shared_chat.id));
    assert!(!before.contains(&private.id));

    trash_node(&h, &a, project.id).await.unwrap();
    let during = eligible(&h).await;
    for id in [&shared.id, &private.id, &shared_chat.id] {
        assert!(!during.contains(id), "trashed content left knowledge");
    }

    trash::restore(h.state(), &b, project.id).await.unwrap();
    let after = eligible(&h).await;
    assert!(after.contains(&shared.id) && after.contains(&shared_chat.id));
    assert!(
        !after.contains(&private.id),
        "a private doc is never restored public"
    );
    assert_eq!(visibility(&h, &private.id).await, "private");
    assert_eq!(visibility(&h, &shared.id).await, "organization");
    // B still cannot read A's private doc after B restored the folder.
    assert!(resources::get(h.state(), &b, &private.id).await.is_err());
}

#[tokio::test]
async fn another_org_cannot_see_trash_or_restore() {
    let h = OrgHarness::new().await;
    let (a, c) = (h.owner(), h.other_org_owner());
    let project = node(&h, &a, None, "Project").await;
    let revision = project.revision;
    assert_eq!(
        trash::preview(h.state(), &c, project.id).await,
        Err(TrashError::NotFound)
    );
    assert_eq!(
        trash::trash(h.state(), &c, project.id, revision).await,
        Err(TrashError::NotFound)
    );
    trash_node(&h, &a, project.id).await.unwrap();
    assert!(trash::list(h.state(), &c).await.unwrap().is_empty());
    assert_eq!(
        trash::restore(h.state(), &c, project.id).await,
        Err(TrashError::NotFound)
    );
    assert_eq!(trash::list(h.state(), &a).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_trashed_node_takes_no_writes_and_its_resources_are_frozen() {
    let h = OrgHarness::new().await;
    let a = h.owner();
    let project = node(&h, &a, None, "Project").await;
    let folder = node(&h, &a, Some(&project), "Folder").await;
    let other = node(&h, &a, None, "Other").await;
    let doc = resource(
        &h,
        &a,
        &folder,
        ResourceKind::Document,
        Visibility::Organization,
    )
    .await;
    trash_node(&h, &a, folder.id).await.unwrap();

    // No child under it, no rename of it, no move into it.
    let child = nodes::create(
        h.state(),
        &a,
        NodeInput {
            parent_id: Some(folder.id),
            kind: NodeKind::Folder,
            name: "X".into(),
        },
    )
    .await;
    assert_eq!(child.err(), Some(AuthError::Forbidden));
    let renamed = nodes::update(
        h.state(),
        &a,
        folder.id,
        folder.revision + 1,
        NodeInput {
            parent_id: Some(project.id),
            kind: NodeKind::Folder,
            name: "Renamed".into(),
        },
    )
    .await;
    assert_eq!(renamed.err(), Some(AuthError::Forbidden));
    let moved = nodes::update(
        h.state(),
        &a,
        other.id,
        other.revision,
        NodeInput {
            parent_id: Some(folder.id),
            kind: NodeKind::Folder,
            name: "Other".into(),
        },
    )
    .await;
    assert_eq!(moved.err(), Some(AuthError::Forbidden));
    // Trashing it again is not found (it is already in the trash).
    assert_eq!(
        trash::trash(h.state(), &a, folder.id, folder.revision + 1).await,
        Err(TrashError::NotFound)
    );

    // No new resource in it; the batch resource cannot be unarchived or moved out.
    let created = resources::create(
        h.state(),
        &a,
        ResourceInput {
            node_id: folder.id,
            kind: ResourceKind::Document,
            title: "New".into(),
            origin_chat_id: None,
            request_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .await;
    assert!(matches!(created, Err(ContentError::NotFound)));
    let doc = resources::get(h.state(), &a, &doc.id).await.unwrap();
    assert!(doc.archived);
    assert!(matches!(
        resources::update(h.state(), &a, &doc.id, archive_patch(&doc, false)).await,
        Err(ContentError::Archived)
    ));
    let mut out = archive_patch(&doc, true);
    out.node_id = other.id;
    assert!(matches!(
        resources::update(h.state(), &a, &doc.id, out).await,
        Err(ContentError::Archived)
    ));
}

#[tokio::test]
async fn a_stale_revision_conflicts_and_writes_nothing() {
    let h = OrgHarness::new().await;
    let a = h.owner();
    let project = node(&h, &a, None, "Project").await;
    nodes::update(
        h.state(),
        &a,
        project.id,
        project.revision,
        NodeInput {
            parent_id: None,
            kind: NodeKind::Project,
            name: "Renamed".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        trash::trash(h.state(), &a, project.id, project.revision).await,
        Err(TrashError::Auth(AuthError::Conflict))
    );
    assert!(trash::list(h.state(), &a).await.unwrap().is_empty());
}

#[tokio::test]
async fn trash_counts_never_reveal_another_members_private_resources() {
    let h = OrgHarness::new().await;
    let (a, b) = (h.owner(), h.member());
    let project = node(&h, &a, None, "Project").await;
    resource(
        &h,
        &a,
        &project,
        ResourceKind::Document,
        Visibility::Private,
    )
    .await;
    resource(
        &h,
        &a,
        &project,
        ResourceKind::Document,
        Visibility::Organization,
    )
    .await;
    trash_node(&h, &a, project.id).await.unwrap();
    assert_eq!(trash::list(h.state(), &a).await.unwrap()[0].documents, 2);
    assert_eq!(trash::list(h.state(), &b).await.unwrap()[0].documents, 1);
}

// ---- Agent Chats (standalone sessions filed on a node; root review T1) ----

use crate::neev::agents::sessions::{
    self as agent_sessions, AgentSession, SessionInput, TurnStatus,
};

async fn agent_chat(
    h: &OrgHarness,
    actor: &OrgContext,
    at: &Node,
    bound: Option<&str>,
) -> AgentSession {
    agent_sessions::create(
        h.state(),
        actor,
        SessionInput {
            title: "Agent chat".into(),
            provider: "claude".into(),
            model: "default".into(),
            bound_chat_id: bound.map(str::to_owned),
            node_id: Some(at.id),
            effort: None,
            fast: false,
            request_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap()
}

async fn session_now(h: &OrgHarness, actor: &OrgContext, id: &str) -> AgentSession {
    agent_sessions::get(h.state(), actor, id).await.unwrap()
}

#[tokio::test]
async fn own_agent_chats_are_counted_trashed_and_restored_with_their_history() {
    let h = OrgHarness::new().await;
    let (a, b) = (h.owner(), h.member());
    let project = node(&h, &a, None, "Project").await;
    let folder = node(&h, &a, Some(&project), "Folder").await;
    let chat = resource(
        &h,
        &a,
        &folder,
        ResourceKind::Chat,
        Visibility::Organization,
    )
    .await;
    let standalone = agent_chat(&h, &a, &folder, None).await;
    let bound = agent_chat(&h, &a, &folder, Some(&chat.id)).await;
    let earlier = agent_chat(&h, &a, &folder, None).await;
    // Archived on its own before any folder trash.
    agent_sessions::set_archived(h.state(), &a, &earlier.id, true, earlier.revision)
        .await
        .unwrap();
    // History that must survive the round trip.
    let (turn, _) = agent_sessions::begin_turn(h.state(), &a, &standalone.id, "hello", None)
        .await
        .unwrap();
    agent_sessions::finish(
        &h.database,
        &turn.run_id,
        "hi",
        None,
        TurnStatus::Completed,
        None,
        1_700_000_000,
    )
    .await
    .unwrap();

    let preview = trash::preview(h.state(), &a, folder.id).await.unwrap();
    // The bound session belongs to its chat, which is already one of `chats`.
    assert_eq!(
        (preview.chats, preview.agent_chats, preview.private_others),
        (1, 1, false)
    );

    let done = trash_node(&h, &a, folder.id).await.unwrap();
    assert_eq!(
        (done.resources, done.sessions),
        (1, 2),
        "standalone + bound; not the earlier one"
    );
    assert!(session_now(&h, &a, &standalone.id).await.archived);
    assert!(session_now(&h, &a, &bound.id).await.archived);
    // Frozen while trashed: neither unarchived nor re-filed on its own.
    let frozen = session_now(&h, &a, &standalone.id).await;
    assert_eq!(
        agent_sessions::set_archived(h.state(), &a, &frozen.id, false, frozen.revision)
            .await
            .err(),
        Some(ContentError::Archived)
    );
    assert_eq!(
        agent_sessions::move_to_node(h.state(), &a, &frozen.id, Some(project.id), frozen.revision)
            .await
            .err(),
        Some(ContentError::Archived)
    );
    // Counts are per viewer: B owns none of these sessions.
    assert_eq!(trash::list(h.state(), &a).await.unwrap()[0].agent_chats, 1);
    assert_eq!(trash::list(h.state(), &b).await.unwrap()[0].agent_chats, 0);

    let restored = trash::restore(h.state(), &a, folder.id).await.unwrap();
    assert_eq!(restored.sessions, 2);
    let back = session_now(&h, &a, &standalone.id).await;
    assert!(!back.archived);
    assert_eq!(back.node_id, Some(folder.id), "filed where it was");
    assert!(!session_now(&h, &a, &bound.id).await.archived);
    assert!(
        session_now(&h, &a, &earlier.id).await.archived,
        "archived on its own; stays"
    );
    let history = agent_sessions::turns(h.state(), &a, &standalone.id, None)
        .await
        .unwrap();
    assert_eq!(history.items.len(), 1);
    assert_eq!(history.items[0].response, "hi");
}

#[tokio::test]
async fn another_members_agent_chat_blocks_the_trash_and_stays_theirs() {
    let h = OrgHarness::new().await;
    let (a, b, c) = (h.owner(), h.member(), h.other_org_owner());
    let project = node(&h, &a, None, "Project").await;
    let folder = node(&h, &a, Some(&project), "Folder").await;
    // Sessions have no sharing: B's Agent Chat is B's alone wherever it is filed.
    let theirs = agent_chat(&h, &b, &folder, None).await;

    let preview = trash::preview(h.state(), &a, folder.id).await.unwrap();
    assert_eq!((preview.agent_chats, preview.private_others), (0, true));
    let revision = current(&h, folder.id).await.revision;
    assert_eq!(
        trash::trash(h.state(), &a, folder.id, revision).await,
        Err(TrashError::Blocked {
            blocker: Blocker::PrivateContent
        })
    );
    assert!(
        !session_now(&h, &b, &theirs.id).await.archived,
        "a refusal writes nothing"
    );
    // The owner may trash it, and gets it back on restore.
    assert_eq!(
        trash::preview(h.state(), &b, folder.id)
            .await
            .unwrap()
            .agent_chats,
        1
    );
    let done = trash_node(&h, &b, folder.id).await.unwrap();
    assert_eq!(done.sessions, 1);
    assert!(session_now(&h, &b, &theirs.id).await.archived);
    // A, another member, restores the folder; B's session comes back private to B.
    trash::restore(h.state(), &a, folder.id).await.unwrap();
    let back = session_now(&h, &b, &theirs.id).await;
    assert!(!back.archived);
    assert!(agent_sessions::get(h.state(), &a, &theirs.id)
        .await
        .is_err());
    // Another org sees neither the folder nor anything about the session.
    assert_eq!(
        trash::preview(h.state(), &c, folder.id).await,
        Err(TrashError::NotFound)
    );
    assert!(agent_sessions::get(h.state(), &c, &theirs.id)
        .await
        .is_err());
}

#[tokio::test]
async fn a_running_standalone_agent_chat_blocks_and_its_owner_sees_it() {
    let h = OrgHarness::new().await;
    let a = h.owner();
    let project = node(&h, &a, None, "Project").await;
    let session = agent_chat(&h, &a, &project, None).await;
    let (turn, _) = agent_sessions::begin_turn(h.state(), &a, &session.id, "go", None)
        .await
        .unwrap();
    let preview = trash::preview(h.state(), &a, project.id).await.unwrap();
    assert_eq!((preview.agent_chats, preview.agent_active), (1, true));
    assert_eq!(
        trash_node(&h, &a, project.id).await,
        Err(TrashError::Blocked {
            blocker: Blocker::AgentActive
        })
    );
    assert!(!session_now(&h, &a, &session.id).await.archived);
    agent_sessions::finish(
        &h.database,
        &turn.run_id,
        "",
        None,
        TurnStatus::Completed,
        None,
        1_700_000_000,
    )
    .await
    .unwrap();
    assert_eq!(trash_node(&h, &a, project.id).await.unwrap().sessions, 1);
}

#[tokio::test]
async fn how_much_private_content_other_members_hold_is_never_revealed() {
    let h = OrgHarness::new().await;
    let (a, b) = (h.owner(), h.member());
    let one = node(&h, &a, None, "One").await;
    let many = node(&h, &a, None, "Many").await;
    resource(&h, &b, &one, ResourceKind::Document, Visibility::Private).await;
    for _ in 0..3 {
        resource(&h, &b, &many, ResourceKind::Document, Visibility::Private).await;
        agent_chat(&h, &b, &many, None).await;
    }
    let p1 = trash::preview(h.state(), &a, one.id).await.unwrap();
    let p2 = trash::preview(h.state(), &a, many.id).await.unwrap();
    // Identical answers (apart from the node itself): one private item and six
    // are indistinguishable to A.
    assert_eq!(p1, p2);
    assert!(p1.private_others);
    assert_eq!(
        trash_node(&h, &a, one.id).await,
        trash_node(&h, &a, many.id).await
    );
    // A's own counts are still exact.
    resource(&h, &a, &many, ResourceKind::Document, Visibility::Private).await;
    agent_chat(&h, &a, &many, None).await;
    let mine = trash::preview(h.state(), &a, many.id).await.unwrap();
    assert_eq!((mine.documents, mine.agent_chats), (1, 1));
}

#[tokio::test]
async fn a_session_archived_on_its_own_stays_archived_while_its_folder_is_trashed() {
    let h = OrgHarness::new().await;
    let a = h.owner();
    let project = node(&h, &a, None, "Project").await;
    let folder = node(&h, &a, Some(&project), "Folder").await;
    let session = agent_chat(&h, &a, &folder, None).await;
    let archived_session =
        agent_sessions::set_archived(h.state(), &a, &session.id, true, session.revision)
            .await
            .unwrap();
    trash_node(&h, &a, folder.id).await.unwrap();
    // Not in the batch, but filed under a hidden node: it may not come back alone.
    assert_eq!(
        agent_sessions::set_archived(h.state(), &a, &session.id, false, archived_session.revision)
            .await
            .err(),
        Some(ContentError::Archived)
    );
    trash::restore(h.state(), &a, folder.id).await.unwrap();
    let still = session_now(&h, &a, &session.id).await;
    assert!(
        still.archived,
        "restore never touches what was archived on its own"
    );
    agent_sessions::set_archived(h.state(), &a, &session.id, false, still.revision)
        .await
        .unwrap();
}

/// TR1 on SQLite: one writer at a time, so a move and a trash are ordered; the
/// two orders must each leave a coherent tree.
#[tokio::test]
async fn a_move_and_a_trash_never_leave_a_live_folder_holding_a_hidden_node() {
    let h = OrgHarness::new().await;
    let a = h.owner();
    let project = node(&h, &a, None, "Project").await;
    let doomed = node(&h, &a, Some(&project), "Doomed").await;
    let moving = node(&h, &a, Some(&doomed), "Moving").await;
    let safe = node(&h, &a, None, "Safe").await;
    // Move first: the moved folder is no longer in the subtree and stays live.
    let moved = nodes::update(
        h.state(),
        &a,
        moving.id,
        moving.revision,
        NodeInput {
            parent_id: Some(safe.id),
            kind: NodeKind::Folder,
            name: "Moving".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(trash_node(&h, &a, doomed.id).await.unwrap().nodes, 1);
    assert!(visible_ids(&h, &a).await.contains(&moved.id));
    // Trash first: the move of a now-hidden folder is refused.
    trash::restore(h.state(), &a, doomed.id).await.unwrap();
    let back = node(&h, &a, Some(&doomed), "Back").await;
    trash_node(&h, &a, doomed.id).await.unwrap();
    let late = nodes::update(
        h.state(),
        &a,
        back.id,
        back.revision,
        NodeInput {
            parent_id: Some(safe.id),
            kind: NodeKind::Folder,
            name: "Back".into(),
        },
    )
    .await;
    assert_eq!(late.err(), Some(AuthError::Forbidden));
}

/// TR1 on PostgreSQL: a move racing an in-flight trash must wait for it (the
/// shared admission guard), then see the folder hidden and refuse — never move
/// a node the trash has already recorded.
#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_a_move_waits_for_an_in_flight_trash_and_is_refused() {
    use sea_orm::TransactionTrait;
    let (database, admin, schema) = support::postgres_database().await;
    let h = OrgHarness::new_with_gateway_events_on(database.clone()).await;
    let a = h.owner();
    let project = node(&h, &a, None, "Project").await;
    let doomed = node(&h, &a, Some(&project), "Doomed").await;
    let moving = node(&h, &a, Some(&doomed), "Moving").await;
    let safe = node(&h, &a, None, "Safe").await;

    // Connection A stands in for a trash between its plan and its writes: it
    // holds the admission guard, exactly as `catalog::trash::trash` does.
    let held = database.begin().await.unwrap();
    db::one(
        &held,
        "SELECT version FROM neev_schema_version WHERE id=1 FOR UPDATE",
        vec![],
    )
    .await
    .unwrap();
    // Connection B: the move. It must block on the guard, not read the old tree.
    let state = h.state.clone();
    let actor = a.clone();
    let (moving_id, moving_revision, safe_id) = (moving.id, moving.revision, safe.id);
    let mover = tokio::spawn(async move {
        nodes::update(
            &state,
            &actor,
            moving_id,
            moving_revision,
            NodeInput {
                parent_id: Some(safe_id),
                kind: NodeKind::Folder,
                name: "Moving".into(),
            },
        )
        .await
    });
    support::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
    // The "trash" records the moving folder, then commits.
    db::execute(
        &held,
        "INSERT INTO neev_node_trash(org_id,batch_id,item_kind,item_id,root_node_id,trashed_by,trashed_at) VALUES($1,'race','node',$2,$3,$4,1),($1,'race','node',$5,$3,$4,1)",
        vec![
            h.org_id().into(),
            doomed.id.to_string().into(),
            doomed.id.into(),
            a.identity.user_id.into(),
            moving.id.to_string().into(),
        ],
    )
    .await
    .unwrap();
    held.commit().await.unwrap();
    // The move resumes after the trash and refuses the now-hidden folder.
    assert_eq!(mover.await.unwrap().err(), Some(AuthError::Forbidden));
    let parent: Option<i32> = db::one(
        &database,
        "SELECT parent_id FROM neev_nodes WHERE id=$1",
        vec![moving.id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "parent_id")
    .unwrap();
    assert_eq!(parent, Some(doomed.id), "a recorded node never moved");

    // And the full functions racing for real: whichever wins, the tree is coherent.
    let other = node(&h, &a, Some(&project), "Other").await;
    let child = node(&h, &a, Some(&other), "Child").await;
    let revision = current(&h, other.id).await.revision;
    let (s1, s2, a1, a2) = (h.state.clone(), h.state.clone(), a.clone(), a.clone());
    let (other_id, child_id, child_revision) = (other.id, child.id, child.revision);
    let trashing = tokio::spawn(async move { trash::trash(&s1, &a1, other_id, revision).await });
    let moving = tokio::spawn(async move {
        nodes::update(
            &s2,
            &a2,
            child_id,
            child_revision,
            NodeInput {
                parent_id: Some(safe_id),
                kind: NodeKind::Folder,
                name: "Child".into(),
            },
        )
        .await
    });
    let (trashed, moved) = (trashing.await.unwrap(), moving.await.unwrap());
    let hidden = trash::node_trashed(&database, h.org_id(), child.id)
        .await
        .unwrap();
    let live_parent = db::one(
        &database,
        "SELECT parent_id FROM neev_nodes WHERE id=$1",
        vec![child.id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<Option<i32>>("", "parent_id")
    .unwrap();
    match moved {
        // Moved first: it is under Safe and not hidden.
        Ok(_) => assert!(!hidden && live_parent == Some(safe_id) && trashed.is_ok()),
        // Trashed first: it is hidden and still under Other.
        Err(_) => assert!(hidden && live_parent == Some(other_id) && trashed.is_ok()),
    }
    drop(h);
    support::cleanup_postgres(database, admin, schema).await;
}
