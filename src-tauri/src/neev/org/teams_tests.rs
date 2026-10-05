//! The member directory and admin-managed teams.

use super::teams;
use crate::neev::{
    content::{grants::SharingError, types::ContentError},
    db,
    error::AuthError,
    startup::ServerState,
    test_support as h,
    types::OrgContext,
};
use sea_orm::DatabaseConnection;
use std::sync::Arc;

const NOW: i64 = 1_700_000_000;

struct Fixture {
    state: Arc<ServerState>,
    db: DatabaseConnection,
    admin: OrgContext,
    member: OrgContext,
    foreign: OrgContext,
    removed: i32,
    names: Vec<(String, i32)>,
}

async fn fixture(db: DatabaseConnection) -> Fixture {
    let clock = Arc::new(h::TestClock::new(NOW));
    let mail = crate::neev::mail::Service::new(db.clone(), Arc::new(h::MailCapture::new()));
    let state = Arc::new(ServerState::with_mail(db.clone(), h::preview_config(), clock, mail).unwrap());
    let named = |subject: &'static str, name: &'static str| {
        let db = db.clone();
        async move {
            let id = h::native_account(&db, subject, &format!("{subject}@example.test")).await;
            db::execute(&db, "UPDATE neev_accounts SET display_name=$1 WHERE id=$2", vec![name.into(), id.into()])
                .await
                .unwrap();
            id
        }
    };
    let admin_id = named("dir-admin", "Zed Admin").await;
    let (org, _) = h::organization(&db, admin_id, "admin").await;
    let mut names = vec![("Zed Admin".to_owned(), admin_id)];
    // Two of the same name: the id breaks the tie, so paging never skips one.
    for (subject, name) in [("dir-a", "Ana"), ("dir-b", "ana"), ("dir-c", "Bo_b"), ("dir-d", "Ana")] {
        let id = named(subject, name).await;
        h::add_membership(&db, org, id, "member").await;
        names.push((name.to_owned(), id));
    }
    let removed = named("dir-gone", "Ana Gone").await;
    let membership = h::add_membership(&db, org, removed, "member").await;
    db::execute(&db, "UPDATE neev_memberships SET active=false WHERE id=$1", vec![membership.into()]).await.unwrap();
    let foreign_id = named("dir-foreign", "Ana Foreign").await;
    let (other, _) = h::organization(&db, foreign_id, "admin").await;
    let admin = h::scoped_context(&db, admin_id, org, NOW).await;
    let member = h::scoped_context(&db, names[1].1, org, NOW).await;
    let foreign = h::scoped_context(&db, foreign_id, other, NOW).await;
    Fixture { state, db, admin, member, foreign, removed, names }
}

async fn directory_pages_active_members_by_name(f: &Fixture) {
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = teams::directory(&f.state, &f.member, None, 2, after.as_deref()).await.unwrap();
        assert!(page.members.len() <= 2);
        seen.extend(page.members.iter().map(|m| m.user_id));
        match page.next_after {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 5, "every active member exactly once");
    assert!(!seen.contains(&f.removed), "an inactive member is not listed");
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 5);
    // Case-insensitive substring; `_` is literal, not a wildcard.
    let ana = teams::directory(&f.state, &f.member, Some("AN"), 50, None).await.unwrap();
    assert_eq!(ana.members.len(), 3);
    let literal = teams::directory(&f.state, &f.member, Some("o_b"), 50, None).await.unwrap();
    assert_eq!(literal.members.len(), 1);
    assert_eq!(teams::directory(&f.state, &f.member, Some("_"), 50, None).await.unwrap().members.len(), 1);
    // The other org's members never appear, and a cursor is bound to its org and query.
    assert!(teams::directory(&f.state, &f.foreign, Some("Ana"), 50, None).await.unwrap().members.iter().all(|m| m.display_name == "Ana Foreign"));
    let page = teams::directory(&f.state, &f.member, None, 1, None).await.unwrap();
    let cursor = page.next_after.unwrap();
    assert_eq!(teams::directory(&f.state, &f.member, Some("a"), 1, Some(&cursor)).await.err(), Some(ContentError::Invalid));
    assert_eq!(teams::directory(&f.state, &f.foreign, None, 1, Some(&cursor)).await.err(), Some(ContentError::Invalid));
    for limit in [0, 101] {
        assert_eq!(teams::directory(&f.state, &f.member, None, limit, None).await.err(), Some(ContentError::Invalid));
    }
}

async fn teams_are_admin_managed(f: &Fixture) {
    let forbidden = Some(SharingError::Content(ContentError::Auth(AuthError::Forbidden)));
    let request = uuid::Uuid::new_v4().to_string();
    assert_eq!(teams::create(&f.state, &f.member, "Design", &request).await.err(), forbidden);
    let team = teams::create(&f.state, &f.admin, "  Design ", &request).await.unwrap();
    assert_eq!((team.name.as_str(), team.revision, team.member_count, team.can_manage), ("Design", 1, 0, true));
    // A retry is the same team; the same request with another name conflicts.
    assert_eq!(teams::create(&f.state, &f.admin, "Design", &request).await.unwrap().id, team.id);
    assert_eq!(teams::create(&f.state, &f.admin, "Other", &request).await.err(), Some(SharingError::Content(ContentError::Conflict)));
    assert_eq!(teams::create(&f.state, &f.admin, "DESIGN", &uuid::Uuid::new_v4().to_string()).await.err(), Some(SharingError::NameTaken));
    for bad in ["", "   ", &"x".repeat(81), "a\u{7}b"] {
        assert_eq!(teams::create(&f.state, &f.admin, bad, &uuid::Uuid::new_v4().to_string()).await.err(), Some(SharingError::Content(ContentError::Invalid)));
    }
    // Members read; only admins change.
    let listed = teams::list(&f.state, &f.member).await.unwrap();
    assert_eq!((listed.len(), listed[0].can_manage), (1, false));
    assert!(matches!(teams::rename(&f.state, &f.member, &team.id, "X", 1).await, Err(SharingError::Content(ContentError::Auth(AuthError::Forbidden)))));
    let renamed = teams::rename(&f.state, &f.admin, &team.id, "Product", 1).await.unwrap();
    assert_eq!(renamed.revision, 2);
    assert_eq!(teams::rename(&f.state, &f.admin, &team.id, "Late", 1).await.err(), Some(SharingError::Content(ContentError::Conflict)));
    let other = teams::create(&f.state, &f.admin, "Ops", &uuid::Uuid::new_v4().to_string()).await.unwrap();
    assert_eq!(teams::rename(&f.state, &f.admin, &other.id, "product", other.revision).await.err(), Some(SharingError::NameTaken));
    // Keeping its own name (case change) is allowed.
    teams::rename(&f.state, &f.admin, &team.id, "PRODUCT", 2).await.unwrap();
    // Membership: active members of this org only.
    let added = teams::set_member(&f.state, &f.admin, &team.id, f.names[1].1, 3, true).await.unwrap();
    assert_eq!((added.revision, added.members.len()), (4, 1));
    assert_eq!(teams::set_member(&f.state, &f.admin, &team.id, f.names[1].1, 4, true).await.unwrap().revision, 4, "no-op");
    for user in [f.removed, f.foreign.identity.user_id] {
        assert_eq!(teams::set_member(&f.state, &f.admin, &team.id, user, 4, true).await.err(), Some(ContentError::NotFound));
    }
    assert_eq!(teams::set_member(&f.state, &f.member, &team.id, f.names[2].1, 4, true).await.err(), Some(ContentError::Auth(AuthError::Forbidden)));
    assert_eq!(teams::members(&f.state, &f.member, &team.id).await.unwrap().members[0].display_name, "Ana");
    // Another org cannot see, use or change it.
    assert_eq!(teams::members(&f.state, &f.foreign, &team.id).await.err(), Some(ContentError::NotFound));
    assert!(teams::list(&f.state, &f.foreign).await.unwrap().is_empty());
    assert_eq!(teams::delete(&f.state, &f.foreign, &team.id, 4).await.err(), Some(ContentError::NotFound));
    let removed = teams::set_member(&f.state, &f.admin, &team.id, f.names[1].1, 4, false).await.unwrap();
    assert!(removed.members.is_empty());
    assert_eq!(teams::delete(&f.state, &f.admin, &team.id, 4).await.err(), Some(ContentError::Conflict));
    teams::delete(&f.state, &f.admin, &team.id, removed.revision).await.unwrap();
    assert_eq!(teams::members(&f.state, &f.admin, &team.id).await.err(), Some(ContentError::NotFound));
    let _ = &f.db;
}

#[tokio::test]
async fn the_directory_pages_active_members_by_name() {
    directory_pages_active_members_by_name(&fixture(h::database().await).await).await;
}

#[tokio::test]
async fn teams_are_admin_managed_and_org_scoped() {
    teams_are_admin_managed(&fixture(h::database().await).await).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_directory_and_teams() {
    for scenario in 0..2 {
        let (db, admin, schema) = h::postgres_database().await;
        let f = fixture(db.clone()).await;
        if scenario == 0 {
            directory_pages_active_members_by_name(&f).await;
        } else {
            teams_are_admin_managed(&f).await;
        }
        drop(f);
        h::cleanup_postgres(db, admin, schema).await;
    }
}
