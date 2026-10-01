//! Task 10 Rust handoff: `content::backup` verifies knowledge refs (schema 22).

use super::store;
use super::test_support::{database, K};
use crate::neev::content::{backup::verify_snapshot_with, types::ContentError};
use crate::neev::db;

async fn captured() -> K {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let chat = k.shared_chat("Team").await;
    k.post(&chat.id, "fact").await;
    k.capture().await;
    k.seed_page("p", "P", "# P\n\nbody", &[&chat.id]).await;
    k
}

#[tokio::test]
async fn schema_22_knowledge_refs_are_verified_and_in_the_manifest() {
    let k = captured().await;
    let knowledge = store::store(&k.state).unwrap();
    let manifest = verify_snapshot_with(&k.state.database, &k.state.blobs, Some(knowledge)).await.unwrap();
    assert_eq!(manifest.schema_version, 22);
    // One snapshot and one page revision.
    assert_eq!(manifest.knowledge.len(), 2);
    let mut sorted = manifest.knowledge.clone();
    sorted.sort_by(|a, b| (a.org_id, &a.key).cmp(&(b.org_id, &b.key)));
    assert_eq!(manifest.knowledge, sorted);
    let encoded = serde_json::to_string(&manifest).unwrap();
    assert!(encoded.contains("\"knowledge\""));
    // Rows but no knowledge root: refused, never skipped.
    assert_eq!(
        verify_snapshot_with(&k.state.database, &k.state.blobs, None).await.err(),
        Some(ContentError::Invalid)
    );
}

#[tokio::test]
async fn no_knowledge_rows_need_no_root_and_hash_as_before() {
    let k = K::new(database().await).await;
    let manifest = verify_snapshot_with(&k.state.database, &k.state.blobs, None).await.unwrap();
    assert!(manifest.knowledge.is_empty());
    assert!(!serde_json::to_string(&manifest).unwrap().contains("knowledge"));
}

#[tokio::test]
async fn a_missing_rewritten_or_foreign_knowledge_blob_fails() {
    let k = captured().await;
    let knowledge = store::store(&k.state).unwrap();
    let org = k.admin.org_id;
    let row = db::one(&k.state.database, "SELECT blob_key,sha256 FROM neev_knowledge_revisions WHERE org_id=$1", vec![org.into()]).await.unwrap().unwrap();
    let key: String = row.try_get("", "blob_key").unwrap();
    let sha: String = row.try_get("", "sha256").unwrap();
    // A wrong digest in the row.
    db::execute(&k.state.database, "UPDATE neev_knowledge_revisions SET sha256=$1 WHERE blob_key=$2", vec!["0".repeat(64).into(), key.clone().into()]).await.unwrap();
    assert!(verify_snapshot_with(&k.state.database, &k.state.blobs, Some(knowledge)).await.is_err());
    db::execute(&k.state.database, "UPDATE neev_knowledge_revisions SET sha256=$1 WHERE blob_key=$2", vec![sha.into(), key.clone().into()]).await.unwrap();
    assert!(verify_snapshot_with(&k.state.database, &k.state.blobs, Some(knowledge)).await.is_ok());
    // The blob moved under another org's directory only.
    let root = k.state.config.knowledge_root.clone().unwrap();
    let elsewhere = root.join((org + 1000).to_string());
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::rename(root.join(org.to_string()).join(format!("{key}.md")), elsewhere.join(format!("{key}.md"))).unwrap();
    assert!(verify_snapshot_with(&k.state.database, &k.state.blobs, Some(knowledge)).await.is_err());
}

/// Seeds a REAL schema-22 source for the end-to-end v3 proof: backup script →
/// pg_restore → `restore-blobs` → `neev-content-verify`. Driven by
/// `NEEV_E2E_SOURCE_URL`, `NEEV_E2E_DOCUMENT_ROOT`, `NEEV_E2E_KNOWLEDGE_ROOT`.
#[tokio::test]
#[ignore = "end-to-end v3 seeding; needs NEEV_E2E_* environment"]
async fn e2e_seed_schema_22_source() {
    let url = std::env::var("NEEV_E2E_SOURCE_URL").expect("NEEV_E2E_SOURCE_URL");
    let mut options = sea_orm::ConnectOptions::new(url);
    options.max_connections(1).sqlx_logging(false);
    let database = sea_orm::Database::connect(options).await.unwrap();
    crate::neev::db::initialize(&database).await.unwrap();
    let mut config = crate::neev::test_support::preview_config();
    config.document_root = std::env::var("NEEV_E2E_DOCUMENT_ROOT").unwrap().into();
    config.knowledge_root = Some(std::env::var("NEEV_E2E_KNOWLEDGE_ROOT").unwrap().into());
    let k = K::with_config(database, config).await;
    k.enable_capture().await;
    let doc = k.shared_doc("Runbook").await;
    k.save_markdown(&doc.id, "# Runbook\n\nRestart the worker.").await;
    let chat = k.shared_chat("Ops").await;
    k.post(&chat.id, "the backup ran").await;
    k.capture().await;
    k.seed_page("ops", "Ops", "# Ops\n\nNotes.", &[&doc.id, &chat.id]).await;
    let rows = k.scalar_i64("SELECT (SELECT COUNT(*) FROM neev_knowledge_snapshots)+(SELECT COUNT(*) FROM neev_knowledge_revisions) AS n", vec![]).await;
    assert_eq!(rows, 3);
    println!("E2E_SEEDED knowledge_rows={rows}");
}
