//! Tasks 7/8 (A1.6): jobs and the knowledge run plumbing, end to end on the
//! real database, blob store, run driver and a scripted runtime.

use super::compile::{self, instructions, PageRole};
use super::jobs::{self, Advance};
use super::output::{self, AcceptOutcome, ParsedOutput};
use super::test_support::{database, staged_labels, staged_text, K};
use super::types::*;
use crate::neev::agents::execution::{FakeOutputs, FakeRuntime, RunRequest, SlotPool};
use crate::neev::agents::sessions;
use crate::neev::content::types::Visibility;
use crate::neev::db;
use std::sync::{Arc, Mutex};

type Log = Arc<Mutex<Vec<RunRequest>>>;

fn page(slug: &str, body: &str) -> (String, Vec<u8>) {
    (format!("wiki--{slug}.md"), body.as_bytes().to_vec())
}

async fn setup(runtime: FakeRuntime) -> (K, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let k = K::with_runtime(database().await, runtime.recording(log.clone())).await;
    k.enable_capture().await;
    k.connect(&k.admin, "claude").await;
    (k, log)
}

async fn tick(k: &K) -> Vec<Advance> {
    jobs::run_tick(&k.state, k.now()).await.unwrap()
}

/// No 32-byte window of `secret` appears in `haystack`.
fn shares_no_32_bytes(secret: &str, haystack: &str) -> bool {
    let bytes = secret.as_bytes();
    bytes.len() < 32 || (0..=bytes.len() - 32).all(|i| match std::str::from_utf8(&bytes[i..i + 32]) {
        Ok(window) => !haystack.contains(window),
        Err(_) => true,
    })
}

#[tokio::test]
async fn start_refuses_non_admins_missing_credentials_and_a_second_active_job() {
    let k = K::new(database().await).await;
    assert!(matches!(jobs::start(&k.state, &k.member, JobMode::Incremental, "claude", "default").await, Err(KnowledgeError::Auth(_))));
    assert!(matches!(jobs::start(&k.state, &k.admin, JobMode::Incremental, "claude", "default").await, Err(KnowledgeError::CredentialMissing)));
    // Another user's credential never counts.
    k.connect(&k.member, "claude").await;
    assert!(matches!(jobs::start(&k.state, &k.admin, JobMode::Incremental, "claude", "default").await, Err(KnowledgeError::CredentialMissing)));
    k.connect(&k.admin, "claude").await;
    let job = k.start_job(JobMode::Incremental).await;
    assert_eq!(job.state, JobState::Running);
    let session_id = job.session_id.clone().unwrap();
    // The job's private session exists and is the admin's, but is not listed.
    assert!(sessions::get(&k.state, &k.admin, &session_id).await.is_ok());
    let listed = sessions::list(&k.state, &k.admin, true).await.unwrap();
    assert!(listed.iter().all(|s| s.id != session_id));
    assert!(matches!(jobs::start(&k.state, &k.admin, JobMode::Rebuild, "claude", "default").await, Err(KnowledgeError::JobActive)));
}

#[tokio::test]
async fn a_batch_runs_isolated_and_its_accept_stores_pages_coverage_and_no_content_elsewhere() {
    let secret = "The quarterly migration codeword is PERIWINKLE-SEVEN-ALPHA-ZULU";
    let runtime = FakeRuntime::new(vec![format!("I read: {secret}")]).with_outputs(|request: &RunRequest| -> FakeOutputs {
        let labels = staged_labels(request);
        let mut out = vec![page("team", &format!("# Team\n\nA fact.[^{}]\n", labels[0]))];
        if labels.len() > 1 {
            out.push(("knowledge-skipped.json".into(), format!(r#"[{{"unit":"{}","reason":"small talk"}}]"#, labels[1]).into_bytes()));
        }
        out
    });
    let (k, log) = setup(runtime).await;
    let a = k.shared_chat("A").await;
    k.post(&a.id, secret).await;
    let b = k.shared_chat("B").await;
    k.post(&b.id, "hi").await;
    k.capture().await;
    let job = k.start_job(JobMode::Incremental).await;
    let started = tick(&k).await;
    assert!(matches!(started.as_slice(), [Advance::Started { .. }]), "{started:?}");
    k.settle(&job.id).await;
    let batches = k.batches(&job.id).await;
    assert_eq!(batches[0].1, "accepted", "{batches:?}");
    // The request was built directly: fixed briefing, no library, two lanes.
    let request = log.lock().unwrap()[0].clone();
    assert!(request.library_items.is_empty() && request.attachments.is_empty());
    assert_eq!(request.min_free_lanes, MIN_FREE_LANES_FOR_KNOWLEDGE);
    assert_eq!(request.briefing.as_deref(), Some(instructions(JobMode::Incremental).as_str()));
    assert_eq!(request.prompt, "Compile knowledge batch 1.");
    assert!(request.credential_handle.is_some());
    assert!(request.context_files.iter().all(|f| f.key.starts_with("k-")));
    // Accept: [^uNN] rewritten to stable keys; coverage; deps.
    let snapshots = [k.current_snapshots(&a.id).await, k.current_snapshots(&b.id).await].concat();
    let body = k.page_body("team").await;
    let labels = staged_labels(&request);
    let cited_id = k
        .scalar_text("SELECT snapshot_id AS v FROM neev_knowledge_batch_units WHERE batch_id=$1 AND label=$2", vec![batches[0].0.clone().into(), labels[0].clone().into()])
        .await
        .unwrap();
    let cited = snapshots.iter().find(|s| s.id == cited_id).unwrap();
    assert!(body.contains(&format!("[^k-{}]", cited.cite_key)) && !body.contains("[^u"), "{body}");
    assert_eq!(k.coverage(&cited.id).await.0, "cited");
    let other = snapshots.iter().find(|s| s.id != cited.id).unwrap();
    assert_eq!(k.coverage(&other.id).await.0, "skipped_by_agent");
    let reason = k.scalar_text("SELECT skip_reason AS v FROM neev_knowledge_batch_units WHERE snapshot_id=$1", vec![other.id.clone().into()]).await;
    assert_eq!(reason.as_deref(), Some("small talk"));
    let deps = k.deps("team", 1).await;
    assert_eq!(deps, snapshots.iter().map(|s| s.id.clone()).collect());
    // The stored turn holds a status line and no unit text; no attachment.
    let turn = db::one(&k.state.database, "SELECT response,activity,status,error_code FROM neev_agent_turns WHERE session_id=$1", vec![job.session_id.clone().unwrap().into()]).await.unwrap().unwrap();
    let response: String = turn.try_get("", "response").unwrap();
    let activity: Option<String> = turn.try_get("", "activity").unwrap();
    assert!(response.starts_with("Knowledge batch 1 accepted: 1 page(s) written"), "{response}");
    assert!(shares_no_32_bytes(secret, &response) && shares_no_32_bytes(secret, activity.as_deref().unwrap_or("")));
    assert_eq!(turn.try_get::<String>("", "status").unwrap(), "completed");
    assert_eq!(k.scalar_i64("SELECT COUNT(*) AS n FROM neev_attachments", vec![]).await, 0);
    // The next batch carries no prior-turn text and stages the page with its key.
    let c = k.shared_chat("C").await;
    k.post(&c.id, "later news").await;
    k.advance(120);
    k.capture().await;
    assert!(matches!(tick(&k).await.as_slice(), [Advance::Started { .. }]));
    k.settle(&job.id).await;
    let second = log.lock().unwrap()[1].clone();
    assert!(shares_no_32_bytes(secret, &second.prompt) && shares_no_32_bytes(secret, second.briefing.as_deref().unwrap()));
    assert!(!second.prompt.contains("accepted") && !second.briefing.as_deref().unwrap().contains("accepted"));
    let staged_page = staged_text(&second, "k-p01").unwrap();
    assert!(staged_page.contains(&format!("[^k-{}]", cited.cite_key)));
}

#[tokio::test]
async fn with_one_lane_free_a_batch_waits_for_a_slot_while_a_chat_run_can_start() {
    let pool = SlotPool::new();
    let held = pool.acquire_if(1).unwrap();
    let (k, _log) = setup(FakeRuntime::new(vec!["x".into()]).with_slots(pool.clone())).await;
    let a = k.shared_chat("A").await;
    k.post(&a.id, "one").await;
    k.capture().await;
    let job = k.start_job(JobMode::Incremental).await;
    assert_eq!(tick(&k).await, vec![Advance::WaitingForSlot]);
    assert_eq!(k.job(&job.id).await.wait_reason.as_deref(), Some("slot"));
    assert!(k.batches(&job.id).await.is_empty(), "the staged batch was removed");
    // A chat run (one lane needed) still starts on the last lane.
    let mut request = jobs::knowledge_request(&k.job(&job.id).await, &compile::stage_batch(&k.state, k.admin.org_id, &k.job(&job.id).await, k.current_snapshots(&a.id).await, k.now()).await.unwrap().unwrap(), "h".into());
    request.min_free_lanes = 1;
    let chat = k.state.agents.run(request).await;
    assert!(chat.is_ok());
    drop(chat);
    pool.release(held);
}

#[tokio::test]
async fn citations_stay_stable_across_batches_and_a_foreign_key_fails_the_batch() {
    let round = Arc::new(Mutex::new(0u32));
    let counter = round.clone();
    let runtime = FakeRuntime::new(vec![]).with_outputs(move |request: &RunRequest| -> FakeOutputs {
        let mut round = counter.lock().unwrap();
        *round += 1;
        let label = staged_labels(request)[0].clone();
        match *round {
            1 => vec![page("a", &format!("# A\n\nFirst.[^{label}]\n"))],
            2 => {
                let staged = staged_text(request, "k-p01").unwrap();
                // The page text sits inside its nonce fence.
                let fenced = staged.split("<<<DATA ").nth(1).unwrap();
                let (nonce, rest) = fenced.split_once('\n').unwrap();
                let body = rest.split(&format!("\nDATA {nonce}>>>")).next().unwrap().to_owned();
                assert!(staged.starts_with("slug: a\n\n<<<DATA "));
                vec![page("a", &format!("{body}Second.[^{label}]\n"))]
            }
            _ => vec![page("a", "# A\n\nForged.[^k-ffffffffffff]\n")],
        }
    });
    let (k, _) = setup(runtime).await;
    let one = k.shared_chat("One").await;
    k.post(&one.id, "first fact").await;
    k.capture().await;
    let job = k.start_job(JobMode::Incremental).await;
    tick(&k).await;
    k.settle(&job.id).await;
    let first_key = k.current_snapshots(&one.id).await[0].cite_key.clone();
    assert!(k.page_body("a").await.contains(&format!("[^k-{first_key}]")));
    let two = k.shared_chat("Two").await;
    k.post(&two.id, "second fact").await;
    k.advance(120);
    k.capture().await;
    tick(&k).await;
    k.settle(&job.id).await;
    let body = k.page_body("a").await;
    let second_key = k.current_snapshots(&two.id).await[0].cite_key.clone();
    assert!(body.contains(&format!("[^k-{first_key}]")) && body.contains(&format!("[^k-{second_key}]")), "{body}");
    let (_, head, _) = k.page("a").await.unwrap();
    assert_eq!(head, 2);
    let parent: Option<i32> = db::one(&k.state.database, "SELECT parent_revision FROM neev_knowledge_revisions WHERE revision=2", vec![]).await.unwrap().unwrap().try_get("", "parent_revision").unwrap();
    assert_eq!(parent, Some(1));
    // A key outside the staged deps fails the batch and stores nothing.
    let three = k.shared_chat("Three").await;
    k.post(&three.id, "third").await;
    k.advance(120);
    k.capture().await;
    tick(&k).await;
    k.settle(&job.id).await;
    let batches = k.batches(&job.id).await;
    assert_eq!((batches[2].1.as_str(), batches[2].2.as_deref()), ("failed", Some("invalid_output")));
    assert_eq!(k.job(&job.id).await.pause_reason.as_deref(), Some("invalid_output"));
    assert_eq!(k.page("a").await.unwrap().1, 2);
}

#[tokio::test]
async fn units_cited_only_by_ignored_files_end_uncovered_and_retry_resets_them() {
    let runtime = FakeRuntime::new(vec![]).with_outputs(|request: &RunRequest| -> FakeOutputs {
        vec![("notes.md".into(), format!("[^{}]", staged_labels(request)[0]).into_bytes())]
    });
    let (k, _) = setup(runtime).await;
    let a = k.shared_chat("A").await;
    k.post(&a.id, "fact").await;
    k.capture().await;
    let snapshot = k.current_snapshots(&a.id).await[0].id.clone();
    let job = k.start_job(JobMode::Incremental).await;
    tick(&k).await;
    k.settle(&job.id).await;
    assert_eq!(k.coverage(&snapshot).await, ("pending".into(), 1));
    tick(&k).await;
    k.settle(&job.id).await;
    assert_eq!(k.coverage(&snapshot).await, ("uncovered".into(), 2));
    assert_eq!(tick(&k).await, vec![Advance::Completed(JobState::CompletedWithGaps)]);
    assert_eq!(k.job(&job.id).await.state, JobState::CompletedWithGaps);
    assert_eq!(jobs::retry_uncovered(&k.state, &k.admin).await.unwrap(), 1);
    assert_eq!(k.coverage(&snapshot).await, ("pending".into(), 0));
}

#[tokio::test]
async fn accept_discards_on_a_moved_page_or_a_revoked_source_and_propagates_indexed_deps() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    k.connect(&k.admin, "claude").await;
    let s = k.shared_chat("S").await;
    k.post(&s.id, "s").await;
    k.capture().await;
    for index in 0..3 {
        k.seed_page(&format!("p{index}"), &format!("P{index}"), &format!("# P{index}\n\n{}", "b".repeat(60_000)), &[&s.id]).await;
    }
    let t = k.shared_chat("T").await;
    k.post(&t.id, "t").await;
    k.advance(120);
    k.capture().await;
    let job = k.start_job(JobMode::Incremental).await;
    let (kr, jr) = (&k, &job);
    let stage = move || async move {
        let units = compile::select_units(&kr.state.database, kr.admin.org_id, jr).await.unwrap();
        compile::stage_batch(&kr.state, kr.admin.org_id, jr, units, kr.now()).await.unwrap().unwrap()
    };
    let parsed = |label: &str| ParsedOutput {
        pages: output::parse_outputs(
            &[crate::neev::agents::generated::GeneratedFile { name: "wiki--new.md".into(), bytes: format!("# New\n\nx[^{label}]").into_bytes(), sha256: String::new() }],
            &[label.to_owned()].into_iter().collect(),
            &Default::default(),
        )
        .unwrap()
        .pages,
        ..Default::default()
    };
    // Indexed page deps propagate into every revision of the batch.
    let staged = stage().await;
    let indexed = staged.pages.iter().find(|p| p.role == PageRole::Indexed).unwrap().clone();
    let outcome = output::accept(&k.state, k.admin.org_id, &staged.batch_id, parsed(&staged.labels[0].0), k.now()).await.unwrap();
    assert!(matches!(outcome, AcceptOutcome::Accepted { pages: 1, .. }));
    let indexed_deps = k.deps(&indexed.slug, indexed.revision).await;
    assert!(k.deps("new", 1).await.is_superset(&indexed_deps));
    // A staged page whose head moved: discarded, nothing written.
    crate::neev::db::execute(&k.state.database, "UPDATE neev_knowledge_coverage SET state='pending'", vec![]).await.unwrap();
    let staged = stage().await;
    let moved = staged.pages.iter().find(|p| p.role == PageRole::Staged).unwrap().clone();
    crate::neev::db::execute(&k.state.database, "UPDATE neev_knowledge_pages SET head_revision=NULL WHERE id=$1", vec![moved.page_id.clone().into()]).await.unwrap();
    let files = k.blob_files().await;
    let outcome = output::accept(&k.state, k.admin.org_id, &staged.batch_id, parsed(&staged.labels[0].0), k.now()).await.unwrap();
    assert_eq!(outcome, AcceptOutcome::Discarded { reason: PauseReason::PageMoved });
    assert_eq!(k.blob_files().await, files);
    crate::neev::db::execute(&k.state.database, "UPDATE neev_knowledge_pages SET head_revision=1 WHERE id=$1", vec![moved.page_id.into()]).await.unwrap();
    // A unit source revoked between staging and accept: discarded, job paused.
    crate::neev::db::execute(&k.state.database, "UPDATE neev_knowledge_jobs SET state='running',pause_reason=NULL WHERE id=$1", vec![job.id.clone().into()]).await.unwrap();
    crate::neev::db::execute(&k.state.database, "UPDATE neev_knowledge_coverage SET state='pending'", vec![]).await.unwrap();
    let staged = stage().await;
    let revoked = staged.labels[0].1.source_id.clone();
    k.set_visibility(&revoked, Visibility::Private).await;
    let outcome = output::accept(&k.state, k.admin.org_id, &staged.batch_id, parsed(&staged.labels[0].0), k.now()).await.unwrap();
    assert_eq!(outcome, AcceptOutcome::Discarded { reason: PauseReason::SourceRevoked });
    assert_eq!(k.blob_files().await, files);
    assert_eq!(k.job(&job.id).await.pause_reason.as_deref(), Some("source_revoked"));
}

#[tokio::test]
async fn a_timed_out_batch_is_cancelled_and_pauses_the_job_with_nothing_stored() {
    let (k, _) = setup(FakeRuntime::new(vec![]).hanging().with_outputs(|r: &RunRequest| vec![page("t", &format!("# T\n\n[^{}]", staged_labels(r)[0]))])).await;
    let a = k.shared_chat("A").await;
    k.post(&a.id, "x").await;
    k.capture().await;
    let job = k.start_job(JobMode::Incremental).await;
    assert!(matches!(tick(&k).await.as_slice(), [Advance::Started { .. }]));
    k.advance(BATCH_TIMEOUT_SECS as i64 + 1);
    tick(&k).await;
    k.settle(&job.id).await;
    let batches = k.batches(&job.id).await;
    assert_eq!((batches[0].1.as_str(), batches[0].2.as_deref()), ("failed", Some("timeout")));
    let job = k.job(&job.id).await;
    assert_eq!((job.state, job.pause_reason.as_deref()), (JobState::Paused, Some("timeout")));
    assert!(k.page("t").await.is_none());
}

#[tokio::test]
async fn stopping_cancels_the_batch_stores_nothing_and_a_stopped_rebuild_archives_nothing() {
    let (k, _) = setup(FakeRuntime::new(vec![]).hanging().with_outputs(|r: &RunRequest| vec![page("fresh", &format!("# F\n\n[^{}]", staged_labels(r)[0]))])).await;
    let a = k.shared_chat("A").await;
    k.post(&a.id, "x").await;
    k.capture().await;
    k.seed_page("old", "Old", "# Old\n\nold", &[&a.id]).await;
    let job = k.start_job(JobMode::Rebuild).await;
    assert!(matches!(tick(&k).await.as_slice(), [Advance::Started { .. }]));
    let stopped = jobs::stop(&k.state, &k.admin, &job.id).await.unwrap();
    assert_eq!(stopped.state, JobState::Stopped);
    k.settle(&job.id).await;
    let batches = k.batches(&job.id).await;
    assert_eq!((batches[0].1.as_str(), batches[0].2.as_deref()), ("failed", Some("stopped")));
    assert!(k.page("fresh").await.is_none());
    assert_eq!(k.page("old").await.unwrap().2, "active");
    assert_eq!(k.job(&job.id).await.state, JobState::Stopped);
}

#[tokio::test]
async fn a_completed_rebuild_archives_the_pages_it_did_not_produce() {
    let (k, _) = setup(FakeRuntime::new(vec![]).with_outputs(|r: &RunRequest| vec![page("fresh", &format!("# F\n\n[^{}]", staged_labels(r)[0]))])).await;
    let a = k.shared_chat("A").await;
    k.post(&a.id, "x").await;
    k.capture().await;
    k.seed_page("old", "Old", "# Old\n\nold", &[&a.id]).await;
    let job = k.start_job(JobMode::Rebuild).await;
    tick(&k).await;
    k.settle(&job.id).await;
    assert_eq!(tick(&k).await, vec![Advance::Completed(JobState::Completed)]);
    assert_eq!(k.page("old").await.unwrap().2, "archived");
    assert_eq!(k.page("fresh").await.unwrap().2, "active");
}

#[tokio::test]
async fn startup_recovery_fails_running_batches_and_pauses_their_jobs() {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    k.connect(&k.admin, "claude").await;
    let a = k.shared_chat("A").await;
    k.post(&a.id, "x").await;
    k.capture().await;
    let job = k.start_job(JobMode::Incremental).await;
    let units = compile::select_units(&k.state.database, k.admin.org_id, &job).await.unwrap();
    let staged = compile::stage_batch(&k.state, k.admin.org_id, &job, units, k.now()).await.unwrap().unwrap();
    jobs::recover_startup(&k.state.database, k.now()).await.unwrap();
    let batches = k.batches(&job.id).await;
    assert_eq!((batches[0].0.as_str(), batches[0].1.as_str(), batches[0].2.as_deref()), (staged.batch_id.as_str(), "failed", Some("interrupted")));
    assert_eq!(k.job(&job.id).await.pause_reason.as_deref(), Some("interrupted"));
}

#[tokio::test]
async fn every_batch_rechecks_the_starters_admin_role_and_own_credential() {
    let (k, log) = setup(FakeRuntime::new(vec![])).await;
    let a = k.shared_chat("A").await;
    k.post(&a.id, "x").await;
    k.capture().await;
    let job = k.start_job(JobMode::Incremental).await;
    db::execute(&k.state.database, "UPDATE neev_memberships SET role='member' WHERE org_id=$1 AND user_id=$2", vec![k.admin.org_id.into(), k.admin.identity.user_id.into()]).await.unwrap();
    assert_eq!(tick(&k).await, vec![Advance::Paused(PauseReason::NotAdmin)]);
    db::execute(&k.state.database, "UPDATE neev_memberships SET role='admin' WHERE org_id=$1 AND user_id=$2", vec![k.admin.org_id.into(), k.admin.identity.user_id.into()]).await.unwrap();
    db::execute(&k.state.database, "DELETE FROM neev_agent_connections WHERE user_id=$1", vec![k.admin.identity.user_id.into()]).await.unwrap();
    db::execute(&k.state.database, "UPDATE neev_knowledge_jobs SET state='running',pause_reason=NULL WHERE id=$1", vec![job.id.clone().into()]).await.unwrap();
    assert_eq!(tick(&k).await, vec![Advance::Paused(PauseReason::CredentialMissing)]);
    assert!(log.lock().unwrap().is_empty(), "no run was ever attempted");
}
