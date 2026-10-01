//! Task 6 (A1.6): capture over the real database and blob store.

use super::capture;
use super::test_support::{database, K};
use super::types::*;
use super::{settings, store};
use crate::neev::content::types::{ResourceKind, Visibility};
use crate::neev::db;
use crate::neev::engine::test_support::{fake_bridge, upstream_for};

#[tokio::test]
async fn disabled_org_is_untouched_and_enabled_org_captures_shared_sources_after_debounce() {
    let k = K::new(database().await).await;
    let shared = k.shared_chat("Team").await;
    let private = k.resource(ResourceKind::Chat, "Mine", Visibility::Private).await;
    k.post(&shared.id, "hello team").await;
    k.post(&private.id, "secret words").await;
    assert_eq!(k.tick().await, 0);
    assert_eq!(k.scalar_i64("SELECT COUNT(*) AS n FROM neev_knowledge_sources", vec![]).await, 0, "a disabled org is never scanned");
    k.enable_capture().await;
    assert_eq!(k.tick().await, 0, "first sighting only arms the debounce");
    k.advance(120);
    assert_eq!(k.tick().await, 1);
    assert_eq!(k.captured_sources().await, vec![shared.id.clone()]);
    let text = k.all_blob_text().await;
    assert!(text.contains("hello team") && !text.contains("secret words"));
    let snapshot = &k.current_snapshots(&shared.id).await[0];
    assert_eq!(snapshot.cite_key.len(), 12);
    assert!(snapshot.cite_key.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    assert_eq!(k.coverage(&snapshot.id).await, ("pending".into(), 0));
    assert_eq!(k.source_state(&shared.id).await, "ok");
    k.advance(120);
    assert_eq!(k.tick().await, 0, "an unchanged marker is not re-captured");
}

#[tokio::test]
async fn a_source_that_changes_every_tick_waits_until_it_is_stable_for_two() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let chat = k.shared_chat("Busy").await;
    for round in 0..4 {
        k.post(&chat.id, &format!("message {round}")).await;
        k.advance(120);
        assert_eq!(k.tick().await, 0, "round {round}");
    }
    assert!(k.captured_sources().await.is_empty());
    k.advance(120);
    assert_eq!(k.tick().await, 1, "stable for two ticks");
    assert!(k.all_blob_text().await.contains("message 3"));
}

#[tokio::test]
async fn a_hundred_changed_sources_are_captured_fairly_and_a_failing_one_delays_nobody() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let mut chats = Vec::new();
    for index in 0..100 {
        let chat = k.shared_chat(&format!("Chat {index:03}")).await;
        k.post(&chat.id, &format!("note {index}")).await;
        chats.push(chat.id);
    }
    // A permanently failing engine document (converter always 503).
    let engine_doc = k.shared_doc("Engine").await;
    let tx = sea_orm::TransactionTrait::begin(&k.state.database).await.unwrap();
    crate::neev::engine::binding::bind(&tx, k.admin.org_id, k.member.identity.user_id, &engine_doc.id).await.unwrap();
    tx.commit().await.unwrap();
    let (base, calls) = fake_bridge(vec![(503, r#"{"error":"converter_unavailable"}"#.into())]).await;
    let upstream = upstream_for(&base);
    let mut per_tick = Vec::new();
    for _ in 0..6 {
        let written = capture::run_tick_with(&k.state, Some(&upstream), k.now()).await.unwrap();
        assert!(written <= MAX_EXTRACTIONS_PER_TICK);
        per_tick.push(written);
        k.advance(120);
    }
    assert_eq!(k.captured_sources().await.len(), 100, "ticks: {per_tick:?}");
    assert_eq!(per_tick[0], 0, "debounce tick");
    assert_eq!(k.source_state(&engine_doc.id).await, "engine_unavailable");
    // It was tried, then backed off (not retried every tick).
    let attempts = calls.lock().unwrap().len();
    assert!((1..=3).contains(&attempts), "{attempts} bridge calls");
    let next: i64 = k.scalar_i64("SELECT next_check_at AS n FROM neev_knowledge_sources WHERE source_id=$1", vec![engine_doc.id.clone().into()]).await;
    assert!(next > k.now() - 120);
    assert_eq!(capture::backoff(1), BACKOFF_MIN_SECS);
    assert_eq!(capture::backoff(30), BACKOFF_MAX_SECS);
}

#[tokio::test]
async fn a_new_message_supersedes_only_the_open_chat_segment() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let chat = k.shared_chat("Team").await;
    // Two segments: messages of ~20 KB.
    for index in 0..5 {
        k.post(&chat.id, &format!("{index}{}", "z".repeat(20_000))).await;
    }
    k.capture().await;
    let before = k.current_snapshots(&chat.id).await;
    assert!(before.len() >= 2);
    k.post(&chat.id, "one more").await;
    k.advance(120);
    k.capture().await;
    let after = k.current_snapshots(&chat.id).await;
    // Closed segments keep their row (and cite key); the open one is replaced.
    assert_eq!(after[..before.len() - 1], before[..before.len() - 1]);
    assert_ne!(after[before.len() - 1].id, before[before.len() - 1].id);
    let (current, superseded) = k.snapshot_states(&chat.id).await;
    assert_eq!((current as usize, superseded), (after.len(), 1));
}

#[tokio::test]
async fn native_document_markdown_is_captured_and_a_revoked_source_is_not() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let doc = k.shared_doc("Plan").await;
    k.save_markdown(&doc.id, "# Plan\n\nShip it.").await;
    k.capture().await;
    assert!(k.all_blob_text().await.contains("Ship it."));
    let first = k.current_snapshots(&doc.id).await;
    assert_eq!(first.len(), 1);
    assert!(first[0].marker.parse::<i64>().unwrap() >= 1);
    k.set_visibility(&doc.id, Visibility::Private).await;
    k.save_markdown(&doc.id, "# Plan\n\nPrivate now.").await;
    k.advance(120);
    k.capture().await;
    assert!(!k.all_blob_text().await.contains("Private now."));
    // The old snapshot stays stored (unreadable), never deleted.
    assert_eq!(k.current_snapshots(&doc.id).await, first);
}

#[tokio::test]
async fn engine_documents_go_through_the_bridge_with_debounce_and_typed_states() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let doc = k.shared_doc("Engine").await;
    let tx = sea_orm::TransactionTrait::begin(&k.state.database).await.unwrap();
    crate::neev::engine::binding::bind(&tx, k.admin.org_id, k.member.identity.user_id, &doc.id).await.unwrap();
    tx.commit().await.unwrap();
    // Without an engine: engine_unavailable.
    k.tick().await;
    assert_eq!(k.source_state(&doc.id).await, "engine_unavailable");
    db::execute(&k.state.database, "UPDATE neev_knowledge_sources SET next_check_at=0", vec![]).await.unwrap();
    let content = r##"{"id":"x","content_sha256":"aaa","unchanged":false,"markdown":"# Spec\n\nEngine body.","updated_at":"t"}"##;
    let (base, calls) = fake_bridge(vec![
        (200, content.into()),
        (200, content.into()),
        (200, r#"{"id":"x","content_sha256":"aaa","unchanged":true}"#.into()),
        (413, r#"{"error":"too_large"}"#.into()),
    ])
    .await;
    let upstream = upstream_for(&base);
    assert_eq!(capture::run_tick_with(&k.state, Some(&upstream), k.now()).await.unwrap(), 0, "first sighting debounces");
    k.advance(1);
    assert_eq!(capture::run_tick_with(&k.state, Some(&upstream), k.now()).await.unwrap(), 1);
    assert!(k.all_blob_text().await.contains("Engine body."));
    let snapshot = &k.current_snapshots(&doc.id).await[0];
    assert!(snapshot.engine && snapshot.marker == "aaa");
    k.advance(BACKOFF_MIN_SECS + 1);
    assert_eq!(capture::run_tick_with(&k.state, Some(&upstream), k.now()).await.unwrap(), 0, "unchanged");
    let bodies = calls.lock().unwrap().clone();
    assert!(bodies[2].contains("\"if_none_match\":\"aaa\""), "{}", bodies[2]);
    k.advance(BACKOFF_MIN_SECS + 1);
    capture::run_tick_with(&k.state, Some(&upstream), k.now()).await.unwrap();
    assert_eq!(k.source_state(&doc.id).await, "too_large");
}

#[tokio::test]
async fn over_budget_blocks_capture_visibly_with_no_writes() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let chat = k.shared_chat("Team").await;
    k.post(&chat.id, "first").await;
    k.capture().await;
    let snapshots = k.scalar_i64("SELECT COUNT(*) AS n FROM neev_knowledge_snapshots", vec![]).await;
    let used = capture::bytes_used(&k.state.database, k.admin.org_id).await.unwrap();
    capture::set_budget_override(Some(used));
    k.post(&chat.id, "second").await;
    k.advance(120);
    k.capture().await;
    capture::set_budget_override(None);
    assert_eq!(k.scalar_i64("SELECT COUNT(*) AS n FROM neev_knowledge_snapshots", vec![]).await, snapshots, "no writes");
    let current = settings::get(&k.state.database, k.admin.org_id).await.unwrap();
    assert_eq!(current.capture_blocked.as_deref(), Some("quota_exceeded"));
    // Under budget again: unblocked and captured.
    k.advance(120);
    k.capture().await;
    assert_eq!(settings::get(&k.state.database, k.admin.org_id).await.unwrap().capture_blocked, None);
    assert!(k.all_blob_text().await.contains("second"));
}

#[tokio::test]
async fn the_orphan_sweep_removes_only_old_rowless_blobs() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let chat = k.shared_chat("Team").await;
    k.post(&chat.id, "kept").await;
    k.capture().await;
    let blobs = store::store(&k.state).unwrap();
    let old_orphan = store::new_key();
    let fresh_orphan = store::new_key();
    blobs.put(k.admin.org_id, &old_orphan, b"orphan").await.unwrap();
    blobs.put(k.admin.org_id, &fresh_orphan, b"orphan").await.unwrap();
    let root = k.state.config.knowledge_root.clone().unwrap().join(k.admin.org_id.to_string());
    let referenced = k.current_snapshots(&chat.id).await[0].blob_key.clone();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 3600);
    for key in [&old_orphan, &referenced] {
        std::fs::File::options().write(true).open(root.join(format!("{key}.md"))).unwrap().set_modified(old).unwrap();
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    assert_eq!(capture::sweep_orphans(&k.state, now).await.unwrap(), 1);
    assert!(!root.join(format!("{old_orphan}.md")).exists());
    assert!(root.join(format!("{fresh_orphan}.md")).exists());
    assert!(root.join(format!("{referenced}.md")).exists());
}

#[tokio::test]
async fn only_admins_change_capture_and_a_stale_revision_conflicts() {
    let k = K::new(database().await).await;
    assert!(matches!(settings::set(&k.state, &k.member, true, 0).await, Err(KnowledgeError::Auth(_))));
    let on = settings::set(&k.state, &k.admin, true, 0).await.unwrap();
    assert_eq!((on.capture_enabled, on.revision), (true, 1));
    assert!(matches!(settings::set(&k.state, &k.admin, false, 0).await, Err(KnowledgeError::Content(_))));
    let off = settings::set(&k.state, &k.admin, false, 1).await.unwrap();
    assert_eq!((off.capture_enabled, off.revision), (false, 2));
    let events = k.scalar_i64("SELECT COUNT(*) AS n FROM neev_knowledge_log WHERE event IN ('capture_enabled','capture_disabled')", vec![]).await;
    assert_eq!(events, 2);
}

