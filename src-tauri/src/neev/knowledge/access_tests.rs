//! Task 3: eligibility parity with `content::access::authorized` and
//! dependency readability (Amendment 1 §A1.3 matrix).

use super::access::*;
use super::test_support::{database, K};
use super::types::*;
use crate::neev::content::{access as content_access, resources, types::*};
use crate::neev::db;

/// Insert a raw resource the API cannot create (nested origins). `None` when
/// the database itself refuses the shape — then it can never exist.
async fn raw(k: &K, kind: &str, origin: &str, origin_kind: &str) -> Option<String> {
    let id = uuid::Uuid::new_v4().to_string();
    db::execute(
        &k.state.database,
        "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,$3,$4,$5,'Raw','organization',$6,$7,1,0,false,1,1)",
        vec![k.admin.org_id.into(), id.clone().into(), kind.into(), k.node.into(), k.member.identity.user_id.into(), origin.into(), origin_kind.into()],
    )
    .await
    .ok()
    .map(|_| id)
}

async fn assert_parity(k: &K, id: &str, label: &str) {
    let tx = resources::begin(&k.state, &k.admin).await.unwrap();
    let authorized = content_access::authorized(&tx, &k.admin, id, false).await;
    drop(tx);
    let readable = authorized.is_ok();
    let archived = match &authorized {
        Ok(resource) => {
            let chat_archived = match &resource.origin_chat_id {
                Some(chat) => k.current(chat).await.archived,
                None => false,
            };
            resource.archived || chat_archived
        }
        Err(_) => false,
    };
    let eligible = is_eligible(&k.state.database, k.admin.org_id, id).await.unwrap().is_some();
    // Implication: eligible ⇒ a non-creator member can read it.
    assert!(!eligible || readable, "eligible but unreadable: {label}");
    // Equality with the archived tightening.
    assert_eq!(eligible, readable && !archived, "parity broken: {label}");
    let listed = eligible_sources(&k.state.database, k.admin.org_id)
        .await
        .unwrap()
        .iter()
        .any(|s| s.id == id);
    assert_eq!(listed, eligible, "eligible_sources disagrees: {label}");
}

#[tokio::test]
async fn eligibility_matches_access_for_a_non_creator_member() {
    let k = K::new(database().await).await;
    let shared_doc = k.shared_doc("Doc").await;
    let private_doc = k.resource(ResourceKind::Document, "Mine", Visibility::Private).await;
    let shared_chat = k.shared_chat("Team").await;
    let private_chat = k.resource(ResourceKind::Chat, "Solo", Visibility::Private).await;
    let archived = k.shared_doc("Old").await;
    k.archive(&archived.id, true).await;
    let bound_private = k.bound_document(&private_chat.id).await;
    let bound_shared = k.bound_document(&shared_chat.id).await;
    let archived_chat = k.shared_chat("Gone").await;
    let bound_archived_chat = k.bound_document(&archived_chat.id).await;
    k.archive(&archived_chat.id, true).await;
    let archived_bound = k.bound_document(&shared_chat.id).await;
    k.archive(&archived_bound.id, true).await;
    let mut cases = vec![
        (shared_doc.id.clone(), "shared doc"),
        (private_doc.id.clone(), "private doc"),
        (shared_chat.id.clone(), "shared chat"),
        (private_chat.id.clone(), "private chat"),
        (archived.id.clone(), "archived shared doc"),
        (bound_private.id.clone(), "bound to private chat"),
        (bound_shared.id.clone(), "bound to shared chat"),
        (archived_chat.id.clone(), "archived shared chat"),
        (bound_archived_chat.id.clone(), "bound to archived chat"),
        (archived_bound.id.clone(), "archived bound doc"),
    ];
    // Shapes the API cannot make; the database may refuse them outright.
    if let Some(id) = raw(&k, "document", &shared_doc.id, "document").await {
        cases.push((id, "document whose origin is a document"));
    }
    if let Some(nested_chat) = raw(&k, "chat", &shared_chat.id, "chat").await {
        cases.push((nested_chat.clone(), "chat with an origin"));
        if let Some(id) = raw(&k, "document", &nested_chat, "chat").await {
            cases.push((id, "document bound to a nested chat"));
        }
    }
    let foreign = resources::create_with_visibility(
        &k.state,
        &k.foreign,
        ResourceInput { node_id: k.foreign_node, kind: ResourceKind::Document, title: "F".into(), origin_chat_id: None, request_id: uuid::Uuid::new_v4().to_string() },
        Some(Visibility::Organization),
    )
    .await
    .unwrap();
    for (id, label) in &cases {
        assert_parity(&k, id, label).await;
    }
    // Another org's shared document: never eligible here.
    assert!(is_eligible(&k.state.database, k.admin.org_id, &foreign.id).await.unwrap().is_none());
    // Sanity on the matrix itself: both answers occur.
    let eligible = eligible_sources(&k.state.database, k.admin.org_id).await.unwrap();
    let ids: Vec<&str> = eligible.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&shared_doc.id.as_str()) && ids.contains(&bound_shared.id.as_str()));
    assert!(!ids.contains(&private_doc.id.as_str()) && !ids.contains(&bound_archived_chat.id.as_str()));
}

#[tokio::test]
async fn a_revision_is_withheld_while_any_dependency_is_revoked() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let a = k.shared_doc("A").await;
    k.save_markdown(&a.id, "# A\n\nalpha").await;
    let b = k.shared_chat("B").await;
    k.post(&b.id, "beta").await;
    k.capture().await;
    let page = k.seed_page("p", "P", "# P\n\nbody", &[&a.id, &b.id]).await;
    let heads = || readable_heads(&k.state.database, k.admin.org_id);
    assert_eq!(revision_readable(&k.state.database, k.admin.org_id, &page, 1).await.unwrap(), Readable::Readable);
    assert_eq!(heads().await.unwrap().get(&page), Some(&Readable::Readable));
    k.set_visibility(&b.id, Visibility::Private).await;
    let withheld = Readable::Withheld { unreadable_sources: 1 };
    assert_eq!(revision_readable(&k.state.database, k.admin.org_id, &page, 1).await.unwrap(), withheld);
    assert_eq!(heads().await.unwrap().get(&page), Some(&withheld));
    // Archiving the other source too: two distinct sources now.
    k.archive(&a.id, true).await;
    assert_eq!(
        revision_readable(&k.state.database, k.admin.org_id, &page, 1).await.unwrap(),
        Readable::Withheld { unreadable_sources: 2 }
    );
    // Re-share and restore: readable again, automatically.
    k.set_visibility(&b.id, Visibility::Organization).await;
    k.archive(&a.id, false).await;
    assert_eq!(revision_readable(&k.state.database, k.admin.org_id, &page, 1).await.unwrap(), Readable::Readable);
}

/// Restricted folders (schema 24): a page is withheld at read the moment any
/// dependency's folder is restricted — no recompile — and readable again when
/// it reopens; a document whose own folder is open but whose BOUND CHAT sits in
/// a restricted folder is never eligible.
#[tokio::test]
async fn a_restricted_folder_withholds_its_sources_live_and_reopening_restores_them() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let a = k.shared_doc("A").await;
    k.save_markdown(&a.id, "# A\n\nalpha").await;
    let b = k.shared_chat("B").await;
    k.post(&b.id, "beta").await;
    let open = crate::neev::catalog::nodes::create(
        &k.state,
        &k.member,
        crate::neev::types::NodeInput {
            parent_id: None,
            kind: crate::neev::types::NodeKind::Project,
            name: "Open".into(),
        },
    )
    .await
    .unwrap()
    .id;
    let bound = resources::create(
        &k.state,
        &k.member,
        ResourceInput {
            node_id: open,
            kind: ResourceKind::Document,
            title: "Notes".into(),
            origin_chat_id: Some(b.id.clone()),
            request_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    k.capture().await;
    let page = k.seed_page("p", "P", "# P\n\nbody", &[&a.id, &b.id]).await;
    let db = &k.state.database;
    let org = k.admin.org_id;
    assert_eq!(revision_readable(db, org, &page, 1).await.unwrap(), Readable::Readable);
    assert!(is_eligible(db, org, &bound.id).await.unwrap().is_some());

    let revision = crate::neev::content::grants::node_access(&k.state, &k.member, k.node).await.unwrap().revision;
    crate::neev::content::grants::set_node_restricted(&k.state, &k.member, k.node, revision, true).await.unwrap();
    let withheld = Readable::Withheld { unreadable_sources: 2 };
    assert_eq!(revision_readable(db, org, &page, 1).await.unwrap(), withheld);
    assert_eq!(readable_heads(db, org).await.unwrap().get(&page), Some(&withheld));
    assert!(is_eligible(db, org, &a.id).await.unwrap().is_none());
    assert!(is_eligible(db, org, &bound.id).await.unwrap().is_none(), "its chat is in a restricted folder");
    let eligible = eligible_sources(db, org).await.unwrap();
    assert!(eligible.iter().all(|s| s.id != a.id && s.id != b.id && s.id != bound.id));

    let revision = crate::neev::content::grants::node_access(&k.state, &k.member, k.node).await.unwrap().revision;
    crate::neev::content::grants::set_node_restricted(&k.state, &k.member, k.node, revision, false).await.unwrap();
    assert_eq!(revision_readable(db, org, &page, 1).await.unwrap(), Readable::Readable);
    assert!(is_eligible(db, org, &bound.id).await.unwrap().is_some());
}
