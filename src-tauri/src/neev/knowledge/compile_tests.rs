//! Task 7: unit selection and staging over real captured snapshots.

use super::compile::*;
use super::test_support::{database, K};
use super::types::*;
use crate::neev::content::types::Visibility;

async fn start(k: &K, mode: JobMode) -> super::jobs::Job {
    k.connect(&k.admin, "claude").await;
    k.start_job(mode).await
}

#[tokio::test]
async fn units_exclude_revoked_sources_and_respect_the_caps() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let mut ids = Vec::new();
    for index in 0..15 {
        let chat = k.shared_chat(&format!("c{index}")).await;
        k.post(&chat.id, "note").await;
        ids.push(chat.id);
    }
    k.capture().await;
    k.set_visibility(&ids[0], Visibility::Private).await;
    let job = start(&k, JobMode::Incremental).await;
    let units = select_units(&k.state.database, k.admin.org_id, &job).await.unwrap();
    assert_eq!(units.len(), MAX_BATCH_UNITS);
    assert!(units.iter().all(|u| u.source_id != ids[0]));
    assert_eq!(units_remaining(&k.state.database, k.admin.org_id, &job).await.unwrap(), 14);
    // Byte cap: big units stop the batch at 256 KiB.
    let k = K::new(database().await).await;
    k.enable_capture().await;
    for index in 0..6 {
        let chat = k.shared_chat(&format!("big{index}")).await;
        for part in 0..3 {
            k.post(&chat.id, &format!("{part}{}", "w".repeat(20_000))).await;
        }
    }
    k.capture().await;
    let job = start(&k, JobMode::Incremental).await;
    let units = select_units(&k.state.database, k.admin.org_id, &job).await.unwrap();
    let bytes: i64 = units.iter().map(|u| u.byte_length).sum();
    assert!(bytes <= MAX_BATCH_UNIT_BYTES as i64 && units.len() < 6, "{} units, {bytes} bytes", units.len());
}

#[tokio::test]
async fn staging_fences_units_keys_files_and_never_shows_a_withheld_page() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let a = k.shared_doc("Secret source").await;
    k.save_markdown(&a.id, "# A\n\nalpha").await;
    let open = k.shared_chat("Open chat").await;
    k.post(&open.id, "seed").await;
    k.capture().await;
    k.seed_page("withheld-page", "Hidden Title", "# Hidden Title\n\nSECRET BODY [^k-000000000000]", &[&a.id]).await;
    k.seed_page("readable-page", "Readable", "# Readable\n\nvisible body, see [[withheld-page]]", &[&open.id]).await;
    k.set_visibility(&a.id, Visibility::Private).await;
    let b = k.shared_chat("B").await;
    k.post(&b.id, "fresh unit text").await;
    k.advance(120);
    k.capture().await;
    let job = start(&k, JobMode::Incremental).await;
    let units = select_units(&k.state.database, k.admin.org_id, &job).await.unwrap();
    let staged = stage_batch(&k.state, k.admin.org_id, &job, units, k.now()).await.unwrap().unwrap();
    let keys: Vec<String> = staged.context_files.iter().map(|f| format!("{}--{}", f.key, f.file_name)).collect();
    assert!(keys.contains(&"k-index--wiki-index.md".to_owned()));
    assert!(keys.contains(&"k-u01--unit.md".to_owned()));
    assert!(keys.contains(&"k-p01--page.md".to_owned()));
    let all: String = staged.context_files.iter().map(|f| String::from_utf8_lossy(&f.bytes).into_owned()).collect();
    assert!(!all.contains("SECRET BODY") && !all.contains("Hidden Title") && !all.contains("withheld-page"));
    assert!(all.contains("visible body") && all.contains("[[readable-page]] Readable (staged)"));
    assert!(all.contains("see [[withheld page]]"), "a staged body never names a withheld page");
    let unit = staged.context_files.iter().find(|f| f.key == "k-u01").unwrap();
    let unit = String::from_utf8_lossy(&unit.bytes).into_owned();
    let nonce = unit.split("<<<DATA ").nth(1).unwrap().lines().next().unwrap().to_owned();
    assert_eq!(nonce.len(), 16);
    assert!(unit.contains(&format!("DATA {nonce}>>>")));
    assert!(staged.labels.iter().all(|(label, _)| label.len() == 3 && label.starts_with('u')));
    // Fixed briefing per mode, recorded on the batch.
    assert_eq!(staged.briefing, instructions(JobMode::Incremental));
    let recorded = k.scalar_text("SELECT instructions_sha256 AS v FROM neev_knowledge_batches WHERE id=$1", vec![staged.batch_id.clone().into()]).await;
    assert_eq!(recorded.as_deref(), Some(sha256_hex(&staged.briefing).as_str()));
    assert!(staged.briefing.contains("outputs/wiki--") && staged.briefing.contains("[^u01]") && staged.briefing.contains("neev-context/"));
    assert!(!staged.briefing.contains("fresh unit text"));
    assert_eq!(staged.prompt, format!("Compile knowledge batch {}.", staged.seq));
    assert_eq!(staged.pages.len(), 1);
    assert_eq!(staged.pages[0].role, PageRole::Staged);
}

#[tokio::test]
async fn pages_beyond_the_staging_budget_are_indexed_and_recorded() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let source = k.shared_chat("S").await;
    k.post(&source.id, "s").await;
    k.capture().await;
    for index in 0..3 {
        let body = format!("# Page {index}\n\n{}", "b".repeat(60_000));
        k.seed_page(&format!("page-{index}"), &format!("Page {index}"), &body, &[&source.id]).await;
    }
    let other = k.shared_chat("T").await;
    k.post(&other.id, "t").await;
    k.advance(120);
    k.capture().await;
    let job = start(&k, JobMode::Incremental).await;
    let units = select_units(&k.state.database, k.admin.org_id, &job).await.unwrap();
    let staged = stage_batch(&k.state, k.admin.org_id, &job, units, k.now()).await.unwrap().unwrap();
    let staged_count = staged.pages.iter().filter(|p| p.role == PageRole::Staged).count();
    let indexed_count = staged.pages.iter().filter(|p| p.role == PageRole::Indexed).count();
    assert_eq!((staged_count, indexed_count), (2, 1));
    let rows = k.scalar_i64("SELECT COUNT(*) AS n FROM neev_knowledge_batch_pages WHERE batch_id=$1 AND role='indexed'", vec![staged.batch_id.clone().into()]).await;
    assert_eq!(rows, 1);
    let index = super::test_support::staged_text(
        &super::jobs::knowledge_request(&job, &staged, "h".into()),
        "k-index",
    )
    .unwrap();
    assert!(index.contains("(not staged)"));
}

#[tokio::test]
async fn a_rebuild_stages_no_old_page_and_reselects_already_cited_units() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let a = k.shared_chat("A").await;
    k.post(&a.id, "x").await;
    k.capture().await;
    k.seed_page("old", "Old", "# Old\n\nold body", &[&a.id]).await;
    let snapshot = &k.current_snapshots(&a.id).await[0];
    crate::neev::db::execute(&k.state.database, "UPDATE neev_knowledge_coverage SET state='cited' WHERE snapshot_id=$1", vec![snapshot.id.clone().into()]).await.unwrap();
    let job = start(&k, JobMode::Rebuild).await;
    let units = select_units(&k.state.database, k.admin.org_id, &job).await.unwrap();
    assert_eq!(units.len(), 1, "a rebuild re-represents cited sources");
    let staged = stage_batch(&k.state, k.admin.org_id, &job, units, k.now()).await.unwrap().unwrap();
    assert!(staged.pages.is_empty());
    let all: String = staged.context_files.iter().map(|f| String::from_utf8_lossy(&f.bytes).into_owned()).collect();
    assert!(!all.contains("old body") && !all.contains("[[old]]"));
    assert_eq!(staged.briefing, instructions(JobMode::Rebuild));
}
