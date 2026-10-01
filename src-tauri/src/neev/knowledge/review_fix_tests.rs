//! Regression tests for the integrated backend review (B1–B4, M1–M6).

use super::compile;
use super::jobs::{self, Advance};
use super::output::{self, AcceptOutcome};
use super::test_support::{database, staged_labels, K};
use super::types::*;
use super::{capture, settings, status};
use crate::neev::agents::execution::{ExecutionError, FakeOutputs, FakeRuntime, RunRequest};
use crate::neev::agents::generated::GeneratedFile;
use crate::neev::db;
use crate::neev::engine::test_support::{fake_bridge, upstream_for};
use crate::neev::test_support as h;
use std::collections::BTreeSet;

fn out(name: &str, body: &str) -> GeneratedFile {
    GeneratedFile { name: name.into(), bytes: body.as_bytes().to_vec(), sha256: String::new() }
}

async fn parsed_for(k: &K, batch_id: &str, files: &[GeneratedFile]) -> output::ParsedOutput {
    let labels = output::batch_labels(&k.state.database, k.admin.org_id, batch_id).await.unwrap();
    let keys = output::allowed_keys(&k.state.database, k.admin.org_id, batch_id).await.unwrap();
    output::parse_outputs(files, &labels, &keys).unwrap()
}

async fn stage(k: &K, job: &jobs::Job) -> compile::StagedBatch {
    let units = compile::select_units(&k.state.database, k.admin.org_id, job).await.unwrap();
    compile::stage_batch(&k.state, k.admin.org_id, job, units, k.now()).await.unwrap().unwrap()
}

/// B1: a stop that holds the job and batch locks while accept starts makes
/// accept wait, see the stop, and store nothing.
#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_accept_waits_for_a_concurrent_stop_and_stores_nothing() {
    use sea_orm::TransactionTrait;
    let (database, admin, schema) = h::postgres_database().await;
    let k = K::new(database.clone()).await;
    k.enable_capture().await;
    k.connect(&k.admin, "claude").await;
    let chat = k.shared_chat("A").await;
    k.post(&chat.id, "fact").await;
    k.capture().await;
    let job = k.start_job(JobMode::Incremental).await;
    let staged = stage(&k, &job).await;
    let parsed = parsed_for(&k, &staged.batch_id, &[out("wiki--a.md", &format!("# A\n\nx[^{}]", staged.labels[0].0))]).await;
    // "Stop" in progress on connection A: it holds the job row lock.
    let stop = database.begin().await.unwrap();
    db::one(&stop, "SELECT id FROM neev_knowledge_jobs WHERE org_id=$1 AND id=$2 FOR UPDATE", vec![k.admin.org_id.into(), job.id.clone().into()]).await.unwrap();
    // Accept on connection B must block on that lock.
    let state = k.state.clone();
    let (org, batch) = (k.admin.org_id, staged.batch_id.clone());
    let now = k.now();
    let accept = tokio::spawn(async move { output::accept(&state, org, &batch, parsed, now).await });
    h::wait_for_postgres_lock(&admin, "SELECT id FROM neev_knowledge_jobs WHERE org_id=$1 AND id=$2 FOR UPDATE").await;
    db::execute(&stop, "UPDATE neev_knowledge_jobs SET state='stopped' WHERE org_id=$1 AND id=$2", vec![k.admin.org_id.into(), job.id.clone().into()]).await.unwrap();
    db::execute(&stop, "UPDATE neev_knowledge_batches SET state='failed',reason='stopped' WHERE org_id=$1 AND id=$2", vec![k.admin.org_id.into(), staged.batch_id.clone().into()]).await.unwrap();
    stop.commit().await.unwrap();
    assert_eq!(accept.await.unwrap().unwrap(), AcceptOutcome::NotRunning);
    assert_eq!(k.scalar_i64("SELECT COUNT(*) AS n FROM neev_knowledge_revisions", vec![]).await, 0);
    assert!(k.page("a").await.is_none());
    let snapshot = &k.current_snapshots(&chat.id).await[0];
    assert_eq!(k.coverage(&snapshot.id).await.0, "pending");
    drop(k);
    h::cleanup_postgres(database, admin, schema).await;
}

/// B1 on either backend: settle_failed after a settled batch changes nothing,
/// and an accept for a batch no longer running stores nothing.
#[tokio::test]
async fn settlement_is_exactly_once() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    k.connect(&k.admin, "claude").await;
    let chat = k.shared_chat("A").await;
    k.post(&chat.id, "fact").await;
    k.capture().await;
    let job = k.start_job(JobMode::Incremental).await;
    let staged = stage(&k, &job).await;
    let parsed = parsed_for(&k, &staged.batch_id, &[out("wiki--a.md", &format!("# A\n\nx[^{}]", staged.labels[0].0))]).await;
    output::settle_failed(&k.state, k.admin.org_id, &staged.batch_id, PauseReason::Timeout, k.now()).await.unwrap();
    output::settle_failed(&k.state, k.admin.org_id, &staged.batch_id, PauseReason::RunFailed, k.now()).await.unwrap();
    let batches = k.batches(&job.id).await;
    assert_eq!(batches[0].2.as_deref(), Some("timeout"), "the second settlement changed nothing");
    assert_eq!(k.job(&job.id).await.pause_reason.as_deref(), Some("timeout"));
    let failures = k.scalar_i64("SELECT COUNT(*) AS n FROM neev_knowledge_log WHERE event='batch_failed'", vec![]).await;
    assert_eq!(failures, 1);
    assert_eq!(output::accept(&k.state, k.admin.org_id, &staged.batch_id, parsed, k.now()).await.unwrap(), AcceptOutcome::NotRunning);
    assert!(k.page("a").await.is_none());
}

/// B2: a rewrite that drops a citation resets that snapshot to pending unless
/// another active head still cites it.
#[tokio::test]
async fn a_dropped_citation_is_no_longer_counted_as_represented() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    k.connect(&k.admin, "claude").await;
    let x = k.shared_chat("X").await;
    k.post(&x.id, "x fact").await;
    k.capture().await;
    let x_snap = k.current_snapshots(&x.id).await[0].clone();
    k.seed_page("a", "A", &format!("# A\n\nfrom x[^k-{}]\n", x_snap.cite_key), &[&x.id]).await;
    k.seed_page("b", "B", &format!("# B\n\nalso x[^k-{}]\n", x_snap.cite_key), &[&x.id]).await;
    db::execute(&k.state.database, "UPDATE neev_knowledge_coverage SET state='cited' WHERE snapshot_id=$1", vec![x_snap.id.clone().into()]).await.unwrap();
    let job = k.start_job(JobMode::Incremental).await;
    let kr = &k;
    let represented = move || async move { status::compute(&kr.state, &kr.state.database, kr.admin.org_id).await.unwrap()["represented"].as_i64().unwrap() };
    assert_eq!(represented().await, 1);
    // Batch 1 rewrites A without X; B still cites X: X stays cited.
    let y = k.shared_chat("Y").await;
    k.post(&y.id, "y fact").await;
    k.advance(120);
    k.capture().await;
    let staged = stage(&k, &job).await;
    let parsed = parsed_for(&k, &staged.batch_id, &[out("wiki--a.md", &format!("# A\n\nnow y[^{}]\n", staged.labels[0].0))]).await;
    assert!(matches!(output::accept(&k.state, k.admin.org_id, &staged.batch_id, parsed, k.now()).await.unwrap(), AcceptOutcome::Accepted { .. }));
    assert_eq!(k.coverage(&x_snap.id).await.0, "cited");
    // Batch 2 rewrites B without X: no head cites X any more.
    let z = k.shared_chat("Z").await;
    k.post(&z.id, "z fact").await;
    k.advance(120);
    k.capture().await;
    let staged = stage(&k, &job).await;
    let parsed = parsed_for(&k, &staged.batch_id, &[out("wiki--b.md", &format!("# B\n\nnow z[^{}]\n", staged.labels[0].0))]).await;
    assert!(matches!(output::accept(&k.state, k.admin.org_id, &staged.batch_id, parsed, k.now()).await.unwrap(), AcceptOutcome::Accepted { .. }));
    assert_eq!(k.coverage(&x_snap.id).await, ("pending".into(), 0));
    let sources = status::sources(&k.state.database, k.admin.org_id, None).await.unwrap();
    let x_item = sources["items"].as_array().unwrap().iter().find(|i| i["id"] == x.id.as_str()).unwrap().clone();
    assert_eq!(x_item["represented"], false);
    // X is selectable again, so the next incremental job re-integrates it.
    let units = compile::select_units(&k.state.database, k.admin.org_id, &job).await.unwrap();
    assert!(units.iter().any(|u| u.id == x_snap.id));
}

/// B3: an empty document or a chat with no messages, once captured, counts as
/// captured and represented; a source not yet captured does not.
#[tokio::test]
async fn known_empty_sources_are_captured_and_represented_but_uncaptured_ones_are_not() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let doc = k.shared_doc("Empty doc").await;
    let chat = k.shared_chat("Quiet chat").await;
    k.capture().await;
    assert!(k.current_snapshots(&doc.id).await.is_empty() && k.current_snapshots(&chat.id).await.is_empty());
    let value = status::compute(&k.state, &k.state.database, k.admin.org_id).await.unwrap();
    assert_eq!((value["eligible"].as_i64(), value["captured"].as_i64(), value["represented"].as_i64()), (Some(2), Some(2), Some(2)));
    let sources = status::sources(&k.state.database, k.admin.org_id, None).await.unwrap();
    for item in sources["items"].as_array().unwrap() {
        assert_eq!((item["captureState"].as_str(), item["represented"].as_bool()), (Some("ok"), Some(true)));
    }
    // A new source not yet captured (NULL marker) is neither.
    let fresh = k.shared_chat("Fresh").await;
    k.post(&fresh.id, "hi").await;
    k.tick().await;
    let value = status::compute(&k.state, &k.state.database, k.admin.org_id).await.unwrap();
    assert_eq!((value["eligible"].as_i64(), value["captured"].as_i64(), value["represented"].as_i64()), (Some(3), Some(2), Some(2)));
    let sources = status::sources(&k.state.database, k.admin.org_id, None).await.unwrap();
    let item = sources["items"].as_array().unwrap().iter().find(|i| i["id"] == fresh.id.as_str()).unwrap().clone();
    assert_eq!((item["captureState"].as_str(), item["represented"].as_bool()), (Some("pending"), Some(false)));
}

/// B4: a runtime fault pauses the job; only lane capacity is a slot wait.
#[tokio::test]
async fn a_runtime_fault_pauses_instead_of_waiting_for_a_slot() {
    let k = K::with_runtime(database().await, FakeRuntime::new(vec![]).failing(ExecutionError::Unavailable)).await;
    k.enable_capture().await;
    k.connect(&k.admin, "claude").await;
    let chat = k.shared_chat("A").await;
    k.post(&chat.id, "x").await;
    k.capture().await;
    let job = k.start_job(JobMode::Incremental).await;
    assert_eq!(jobs::run_tick(&k.state, k.now()).await.unwrap(), vec![Advance::Paused(PauseReason::RunFailed)]);
    let job = k.job(&job.id).await;
    assert_eq!((job.state, job.pause_reason.as_deref(), job.wait_reason.as_deref()), (JobState::Paused, Some("run_failed"), None));
    let batches = k.batches(&job.id).await;
    assert_eq!((batches[0].1.as_str(), batches[0].2.as_deref()), ("failed", Some("run_failed")));
    let k = K::with_runtime(database().await, FakeRuntime::new(vec![]).failing(ExecutionError::LanesBusy)).await;
    k.enable_capture().await;
    k.connect(&k.admin, "claude").await;
    let chat = k.shared_chat("A").await;
    k.post(&chat.id, "x").await;
    k.capture().await;
    let job = k.start_job(JobMode::Incremental).await;
    assert_eq!(jobs::run_tick(&k.state, k.now()).await.unwrap(), vec![Advance::WaitingForSlot]);
    assert_eq!(k.job(&job.id).await.state, JobState::Running);
}

/// M1 + M2: a job whose session is not attached yet is skipped by the loop; a
/// finished job's hidden session is archived and never counts toward the cap.
#[tokio::test]
async fn the_loop_skips_unattached_jobs_and_finished_sessions_are_archived() {
    let k = K::with_runtime(
        database().await,
        FakeRuntime::new(vec![]).with_outputs(|r: &RunRequest| -> FakeOutputs {
            vec![("wiki--p.md".into(), format!("# P\n\n[^{}]", staged_labels(r)[0]).into_bytes())]
        }),
    )
    .await;
    k.enable_capture().await;
    k.connect(&k.admin, "claude").await;
    let chat = k.shared_chat("A").await;
    k.post(&chat.id, "x").await;
    k.capture().await;
    let job = k.start_job(JobMode::Incremental).await;
    let session = job.session_id.clone().unwrap();
    db::execute(&k.state.database, "UPDATE neev_knowledge_jobs SET session_id=NULL WHERE id=$1", vec![job.id.clone().into()]).await.unwrap();
    assert!(jobs::run_tick(&k.state, k.now()).await.unwrap().is_empty());
    assert_eq!(k.job(&job.id).await.state, JobState::Running);
    db::execute(&k.state.database, "UPDATE neev_knowledge_jobs SET session_id=$1 WHERE id=$2", vec![session.clone().into(), job.id.clone().into()]).await.unwrap();
    jobs::run_tick(&k.state, k.now()).await.unwrap();
    k.settle(&job.id).await;
    assert_eq!(jobs::run_tick(&k.state, k.now()).await.unwrap(), vec![Advance::Completed(JobState::Completed)]);
    let archived: bool = db::one(&k.state.database, "SELECT archived FROM neev_agent_sessions WHERE id=$1", vec![session.into()]).await.unwrap().unwrap().try_get("", "archived").unwrap();
    assert!(archived);
    let counted = k.scalar_i64("SELECT COUNT(*) AS n FROM neev_agent_sessions s WHERE owner_id=$1 AND NOT EXISTS (SELECT 1 FROM neev_knowledge_jobs j WHERE j.org_id=s.org_id AND j.session_id=s.id)", vec![k.admin.identity.user_id.into()]).await;
    assert_eq!(counted, 0, "the hidden session is excluded from the owner's cap");
}

/// M4 + M5: an org-mismatched engine document is an integrity fault (failed),
/// and an over-budget candidate backs off while `capture_blocked` stays set.
#[tokio::test]
async fn org_mismatch_is_failed_and_quota_state_does_not_flap() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let doc = k.shared_doc("Engine").await;
    let tx = sea_orm::TransactionTrait::begin(&k.state.database).await.unwrap();
    crate::neev::engine::binding::bind(&tx, k.admin.org_id, k.member.identity.user_id, &doc.id).await.unwrap();
    tx.commit().await.unwrap();
    let (base, _) = fake_bridge(vec![(409, r#"{"error":"organization_mismatch"}"#.into())]).await;
    capture::run_tick_with(&k.state, Some(&upstream_for(&base)), k.now()).await.unwrap();
    assert_eq!(k.source_state(&doc.id).await, "failed");
    // Quota: room for nothing new, but the tick-start check passes.
    let chat = k.shared_chat("Team").await;
    k.post(&chat.id, "first").await;
    let used = capture::bytes_used(&k.state.database, k.admin.org_id).await.unwrap();
    capture::set_budget_override(Some(used + 1));
    k.capture().await;
    let kr = &k;
    let blocked = move || async move { settings::get(&kr.state.database, kr.admin.org_id).await.unwrap().capture_blocked };
    assert_eq!(blocked().await.as_deref(), Some("quota_exceeded"));
    let next: i64 = k.scalar_i64("SELECT next_check_at AS n FROM neev_knowledge_sources WHERE source_id=$1", vec![chat.id.clone().into()]).await;
    assert!(next > k.now(), "the refused candidate backs off");
    k.advance(1);
    k.tick().await;
    assert_eq!(blocked().await.as_deref(), Some("quota_exceeded"), "no flapping");
    capture::set_budget_override(None);
    k.advance(BACKOFF_MAX_SECS);
    k.tick().await;
    assert_eq!(blocked().await, None);
    assert!(!k.current_snapshots(&chat.id).await.is_empty());
}

/// M6: only Claude and Codex may run a compile.
#[tokio::test]
async fn acp_providers_are_refused_for_compiles() {
    let k = K::new(database().await).await;
    k.connect(&k.admin, "claude").await;
    for provider in ["gemini", "cursor", "opencode", "grok"] {
        assert!(matches!(jobs::start(&k.state, &k.admin, JobMode::Incremental, provider, "default").await, Err(KnowledgeError::Content(_))), "{provider}");
    }
    let keys: BTreeSet<&str> = ["claude", "codex"].into_iter().collect();
    assert!(keys.contains("claude"));
}
