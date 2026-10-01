//! Compile jobs (plan Tasks 7/8 as amended: A1.3 job loop, lanes, timeout,
//! stop, retry uncovered; E3/E4/E5, D1, G3/G4).
//!
//! Every batch runs on the job starter's OWN connected credential, re-checked
//! with their admin role at every batch start. The next batch is started only
//! by the job loop (`run_tick`), never from inside the run driver.
//!
//! The loop runs with no browser session, so it cannot use
//! `sessions::begin_turn` / `intents::credential` (both re-check a cookie
//! session). It uses session-free equivalents over the same rows instead:
//! `starter_context`, `starter_credential` and `insert_turn`.

use super::compile::{self, StagedBatch};
use super::output;
use super::store;
use super::types::*;
use crate::neev::{
    agents::{
        execution::{ExecutionError, RunRequest},
        runs::{self, RunPurpose},
        sessions::{self, SessionInput},
        types::Provider,
    },
    auth::session,
    content::resources,
    db,
    startup::ServerState,
    types::{IdentityContext, OrgContext, Role},
};
use sea_orm::{ConnectionTrait, DatabaseConnection, QueryResult, TransactionTrait};
use std::sync::Arc;

pub(crate) const SESSION_TITLE: &str = "Knowledge compile";
const MAX_TURNS_PER_SESSION: i64 = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Job {
    pub id: String,
    pub org_id: i32,
    pub mode: JobMode,
    pub state: JobState,
    pub provider: String,
    pub model: String,
    pub pause_reason: Option<String>,
    pub wait_reason: Option<String>,
    pub session_id: Option<String>,
    pub started_by: i32,
    pub created_at: i64,
    pub updated_at: i64,
}

const JOB_COLUMNS: &str = "org_id,id,mode,state,started_by,provider,model,pause_reason,session_id,wait_reason,created_at,updated_at";

fn job_row(row: &QueryResult) -> Result<Job, KnowledgeError> {
    Ok(Job {
        id: row.try_get("", "id").map_err(db_error)?,
        org_id: row.try_get("", "org_id").map_err(db_error)?,
        mode: JobMode::parse(&row.try_get::<String>("", "mode").map_err(db_error)?).ok_or_else(KnowledgeError::storage)?,
        state: JobState::parse(&row.try_get::<String>("", "state").map_err(db_error)?).ok_or_else(KnowledgeError::storage)?,
        provider: row.try_get("", "provider").map_err(db_error)?,
        model: row.try_get("", "model").map_err(db_error)?,
        pause_reason: row.try_get("", "pause_reason").map_err(db_error)?,
        wait_reason: row.try_get("", "wait_reason").map_err(db_error)?,
        session_id: row.try_get("", "session_id").map_err(db_error)?,
        started_by: row.try_get("", "started_by").map_err(db_error)?,
        created_at: row.try_get("", "created_at").map_err(db_error)?,
        updated_at: row.try_get("", "updated_at").map_err(db_error)?,
    })
}

pub(crate) async fn load<C: ConnectionTrait>(db: &C, org_id: i32, job_id: &str) -> Result<Option<Job>, KnowledgeError> {
    db::one(
        db,
        &format!("SELECT {JOB_COLUMNS} FROM neev_knowledge_jobs WHERE org_id=$1 AND id=$2"),
        vec![org_id.into(), job_id.to_owned().into()],
    )
    .await?
    .map(|row| job_row(&row))
    .transpose()
}

/// The active (running/paused) job, else the most recent one.
pub(crate) async fn current<C: ConnectionTrait>(db: &C, org_id: i32) -> Result<Option<Job>, KnowledgeError> {
    db::one(
        db,
        &format!("SELECT {JOB_COLUMNS} FROM neev_knowledge_jobs WHERE org_id=$1 ORDER BY CASE WHEN state IN ('running','paused') THEN 0 ELSE 1 END,created_at DESC,id DESC LIMIT 1"),
        vec![org_id.into()],
    )
    .await?
    .map(|row| job_row(&row))
    .transpose()
}

/// Compile runs on Claude or Codex only (spec §5; frontend contract D2). The
/// ACP beta providers are refused with `invalid`, never silently accepted.
fn provider(value: &str) -> Result<Provider, KnowledgeError> {
    match Provider::parse(value) {
        Some(provider) if matches!(value, "claude" | "codex") => Ok(provider),
        _ => Err(KnowledgeError::invalid()),
    }
}

/// The starter's own connected credential handle, read without a browser
/// session (same row and rule as `intents::credential`).
pub(crate) async fn starter_credential<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    user_id: i32,
    provider_name: &str,
) -> Result<Option<String>, KnowledgeError> {
    let row = db::one(
        db,
        "SELECT state,storage_handle FROM neev_agent_connections WHERE org_id=$1 AND user_id=$2 AND provider=$3",
        vec![org_id.into(), user_id.into(), provider_name.to_owned().into()],
    )
    .await?;
    Ok(match row {
        Some(row) => {
            let state: String = row.try_get("", "state").map_err(db_error)?;
            let handle: Option<String> = row.try_get("", "storage_handle").map_err(db_error)?;
            (state == "connected").then_some(handle).flatten()
        }
        None => None,
    })
}

/// The starter as an org context, re-checked from the database: account not
/// disabled, org not disabled, membership active with role admin. `None`
/// means they may no longer run this job. The context carries no browser
/// session and is used only to own the run (never for a session recheck).
pub(crate) async fn starter_context<C: ConnectionTrait>(db: &C, org_id: i32, user_id: i32) -> Result<Option<OrgContext>, KnowledgeError> {
    let row = db::one(
        db,
        "SELECT a.auth_revision,o.revision AS org_revision,m.id AS member_id,m.revision AS member_revision FROM neev_accounts a JOIN neev_memberships m ON m.user_id=a.id JOIN neev_organizations o ON o.id=m.org_id WHERE a.id=$1 AND m.org_id=$2 AND a.disabled=false AND o.disabled=false AND m.active=true AND m.role='admin'",
        vec![user_id.into(), org_id.into()],
    )
    .await?;
    let Some(row) = row else { return Ok(None) };
    Ok(Some(OrgContext {
        identity: IdentityContext {
            user_id,
            session_id: String::new(),
            auth_revision: row.try_get("", "auth_revision").map_err(db_error)?,
            authenticated_at: 0,
        },
        org_id,
        member_id: row.try_get("", "member_id").map_err(db_error)?,
        member_revision: row.try_get("", "member_revision").map_err(db_error)?,
        org_revision: row.try_get("", "org_revision").map_err(db_error)?,
        role: Role::Admin,
    }))
}

/// Start a job. Admins only, with a connected credential of their own. One
/// active job per org (the partial unique index decides). The job gets its own
/// new private session, hidden from session lists. The loop starts batches.
pub(crate) async fn start(
    state: &ServerState,
    actor: &OrgContext,
    mode: JobMode,
    provider_name: &str,
    model: &str,
) -> Result<Job, KnowledgeError> {
    if actor.role != Role::Admin {
        return Err(KnowledgeError::forbidden());
    }
    store::store(state)?;
    provider(provider_name)?;
    if model.trim().is_empty() || model.chars().count() > 80 {
        return Err(KnowledgeError::invalid());
    }
    let now = state.clock.now_unix();
    let tx = resources::begin(state, actor).await?;
    if starter_credential(&tx, actor.org_id, actor.identity.user_id, provider_name).await?.is_none() {
        return Err(KnowledgeError::CredentialMissing);
    }
    if db::one(
        &tx,
        "SELECT id FROM neev_knowledge_jobs WHERE org_id=$1 AND state IN ('running','paused')",
        vec![actor.org_id.into()],
    )
    .await?
    .is_some()
    {
        return Err(KnowledgeError::JobActive);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let inserted = db::execute(
        &tx,
        "INSERT INTO neev_knowledge_jobs(org_id,id,mode,state,started_by,provider,model,pause_reason,session_id,wait_reason,created_at,updated_at) VALUES($1,$2,$3,'running',$4,$5,$6,NULL,NULL,NULL,$7,$7)",
        vec![
            actor.org_id.into(),
            id.clone().into(),
            mode.as_str().into(),
            actor.identity.user_id.into(),
            provider_name.to_owned().into(),
            model.to_owned().into(),
            now.into(),
        ],
    )
    .await;
    if let Err(error) = inserted {
        return Err(match error {
            crate::neev::error::AuthError::Conflict => KnowledgeError::JobActive,
            other => KnowledgeError::Auth(other),
        });
    }
    session::recheck(&tx, actor, now).await?;
    tx.commit().await.map_err(db_error)?;
    let created = sessions::create(
        state,
        actor,
        SessionInput {
            title: SESSION_TITLE.into(),
            provider: provider_name.to_owned(),
            model: model.to_owned(),
            bound_chat_id: None,
            node_id: None,
            effort: None,
            fast: false,
            request_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .await;
    let session_row = match created {
        Ok(session) => session,
        Err(error) => {
            // The job never got a session: remove it, nothing ran.
            let _ = db::execute(
                &state.database,
                "DELETE FROM neev_knowledge_jobs WHERE org_id=$1 AND id=$2",
                vec![actor.org_id.into(), id.clone().into()],
            )
            .await;
            return Err(KnowledgeError::Content(error));
        }
    };
    let tx = db::begin(&state.database).await?;
    db::execute(
        &tx,
        "UPDATE neev_knowledge_jobs SET session_id=$1 WHERE org_id=$2 AND id=$3",
        vec![session_row.id.clone().into(), actor.org_id.into(), id.clone().into()],
    )
    .await?;
    super::log::append(&tx, actor.org_id, Some(actor.identity.user_id), "job_started", None, Some(&id), &format!("mode={}", mode.as_str()), now).await?;
    tx.commit().await.map_err(db_error)?;
    load(&state.database, actor.org_id, &id).await?.ok_or_else(KnowledgeError::storage)
}

async fn control_begin(state: &ServerState, actor: &OrgContext, job_id: &str) -> Result<(sea_orm::DatabaseTransaction, Job), KnowledgeError> {
    if actor.role != Role::Admin {
        return Err(KnowledgeError::forbidden());
    }
    let tx = resources::begin(state, actor).await?;
    // Job row first, then its batch rows: the same lock order as accept and
    // settle_failed, so control and settlement serialize (B1).
    lock_job(&tx, actor.org_id, job_id).await?;
    let job = load(&tx, actor.org_id, job_id).await?.ok_or_else(KnowledgeError::not_found)?;
    Ok((tx, job))
}

/// Lock a job row on PostgreSQL (SQLite transactions already hold the write
/// lock from `db::begin`). Lock order everywhere: job, then batch.
pub(crate) async fn lock_job<C: ConnectionTrait>(db: &C, org_id: i32, job_id: &str) -> Result<(), KnowledgeError> {
    if db.get_database_backend() == sea_orm::DbBackend::Postgres {
        db::one(
            db,
            "SELECT id FROM neev_knowledge_jobs WHERE org_id=$1 AND id=$2 FOR UPDATE",
            vec![org_id.into(), job_id.to_owned().into()],
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn lock_batch<C: ConnectionTrait>(db: &C, org_id: i32, batch_id: &str) -> Result<(), KnowledgeError> {
    if db.get_database_backend() == sea_orm::DbBackend::Postgres {
        db::one(
            db,
            "SELECT id FROM neev_knowledge_batches WHERE org_id=$1 AND id=$2 FOR UPDATE",
            vec![org_id.into(), batch_id.to_owned().into()],
        )
        .await?;
    }
    Ok(())
}

/// Archive a finished job's hidden session so it never accumulates against the
/// owner's session cap or stays writable (M2).
async fn archive_session<C: ConnectionTrait>(db: &C, org_id: i32, job_id: &str, now: i64) -> Result<(), KnowledgeError> {
    db::execute(
        db,
        "UPDATE neev_agent_sessions SET archived=true,revision=revision+1,updated_at=$1 WHERE org_id=$2 AND archived=false AND id=(SELECT session_id FROM neev_knowledge_jobs WHERE org_id=$2 AND id=$3)",
        vec![now.into(), org_id.into(), job_id.to_owned().into()],
    )
    .await?;
    Ok(())
}

/// Pause a running job. A running batch finishes and its accept still records
/// results; the loop then sees `paused` and starts nothing.
pub(crate) async fn pause(state: &ServerState, actor: &OrgContext, job_id: &str) -> Result<Job, KnowledgeError> {
    let now = state.clock.now_unix();
    let (tx, job) = control_begin(state, actor, job_id).await?;
    if job.state != JobState::Running {
        return Err(KnowledgeError::conflict());
    }
    output::pause_running_job(&tx, actor.org_id, job_id, PauseReason::Manual, now).await?;
    super::log::append(&tx, actor.org_id, Some(actor.identity.user_id), "job_paused", None, Some(job_id), "reason=manual", now).await?;
    session::recheck(&tx, actor, now).await?;
    tx.commit().await.map_err(db_error)?;
    load(&state.database, actor.org_id, job_id).await?.ok_or_else(KnowledgeError::storage)
}

/// Resume only sets `running`; the loop does the rest (and re-checks the
/// starter's role and credential before any batch).
pub(crate) async fn resume(state: &ServerState, actor: &OrgContext, job_id: &str) -> Result<Job, KnowledgeError> {
    let now = state.clock.now_unix();
    let (tx, job) = control_begin(state, actor, job_id).await?;
    if job.state != JobState::Paused {
        return Err(KnowledgeError::conflict());
    }
    db::execute(
        &tx,
        "UPDATE neev_knowledge_jobs SET state='running',pause_reason=NULL,wait_reason=NULL,updated_at=$1 WHERE org_id=$2 AND id=$3 AND state='paused'",
        vec![now.into(), actor.org_id.into(), job_id.to_owned().into()],
    )
    .await?;
    super::log::append(&tx, actor.org_id, Some(actor.identity.user_id), "job_resumed", None, Some(job_id), "", now).await?;
    session::recheck(&tx, actor, now).await?;
    tx.commit().await.map_err(db_error)?;
    load(&state.database, actor.org_id, job_id).await?.ok_or_else(KnowledgeError::storage)
}

/// Cancel the live run of a batch, if any.
async fn cancel_batch_run(state: &ServerState, org_id: i32, batch_id: &str) -> Result<(), KnowledgeError> {
    let row = db::one(
        &state.database,
        "SELECT t.run_id FROM neev_knowledge_batches b JOIN neev_agent_turns t ON t.org_id=b.org_id AND t.id=b.turn_id WHERE b.org_id=$1 AND b.id=$2",
        vec![org_id.into(), batch_id.to_owned().into()],
    )
    .await?;
    if let Some(row) = row {
        let run_id: String = row.try_get("", "run_id").map_err(db_error)?;
        if let Some(live) = runs::find(&state.agent_runs, &run_id) {
            live.cancel();
        }
    }
    Ok(())
}

/// Stop a job: a running batch is cancelled and settles failed (`stopped`)
/// with nothing stored; a stopped rebuild archives nothing.
pub(crate) async fn stop(state: &ServerState, actor: &OrgContext, job_id: &str) -> Result<Job, KnowledgeError> {
    let now = state.clock.now_unix();
    let (tx, job) = control_begin(state, actor, job_id).await?;
    if !matches!(job.state, JobState::Running | JobState::Paused) {
        return Err(KnowledgeError::conflict());
    }
    db::execute(
        &tx,
        "UPDATE neev_knowledge_jobs SET state='stopped',pause_reason=NULL,wait_reason=NULL,updated_at=$1 WHERE org_id=$2 AND id=$3",
        vec![now.into(), actor.org_id.into(), job_id.to_owned().into()],
    )
    .await?;
    let running = db::one(
        &tx,
        "SELECT id FROM neev_knowledge_batches WHERE org_id=$1 AND job_id=$2 AND state='running'",
        vec![actor.org_id.into(), job_id.to_owned().into()],
    )
    .await?
    .map(|row| row.try_get::<String>("", "id"))
    .transpose()
    .map_err(db_error)?;
    if let Some(batch) = &running {
        lock_batch(&tx, actor.org_id, batch).await?;
        db::execute(
            &tx,
            "UPDATE neev_knowledge_batches SET state='failed',reason='stopped',settled_at=$1 WHERE org_id=$2 AND id=$3 AND state='running'",
            vec![now.into(), actor.org_id.into(), batch.clone().into()],
        )
        .await?;
    }
    archive_session(&tx, actor.org_id, job_id, now).await?;
    super::log::append(&tx, actor.org_id, Some(actor.identity.user_id), "job_stopped", None, Some(job_id), "", now).await?;
    session::recheck(&tx, actor, now).await?;
    tx.commit().await.map_err(db_error)?;
    if let Some(batch) = running {
        cancel_batch_run(state, actor.org_id, &batch).await?;
    }
    load(&state.database, actor.org_id, job_id).await?.ok_or_else(KnowledgeError::storage)
}

/// "Retry uncovered": uncovered snapshots become pending again.
pub(crate) async fn retry_uncovered(state: &ServerState, actor: &OrgContext) -> Result<u64, KnowledgeError> {
    if actor.role != Role::Admin {
        return Err(KnowledgeError::forbidden());
    }
    let now = state.clock.now_unix();
    let tx = resources::begin(state, actor).await?;
    let changed = db::execute(
        &tx,
        "UPDATE neev_knowledge_coverage SET state='pending',attempts=0,updated_at=$1 WHERE org_id=$2 AND state='uncovered'",
        vec![now.into(), actor.org_id.into()],
    )
    .await?
    .rows_affected();
    super::log::append(&tx, actor.org_id, Some(actor.identity.user_id), "uncovered_reset", None, None, &format!("units={changed}"), now).await?;
    session::recheck(&tx, actor, now).await?;
    tx.commit().await.map_err(db_error)?;
    Ok(changed)
}

/// At startup: running batches belonged to a dead process. They become
/// failed (interrupted) and their jobs paused (interrupted).
pub(crate) async fn recover_startup(database: &DatabaseConnection, now: i64) -> Result<(), KnowledgeError> {
    let tx = database.begin().await.map_err(db_error)?;
    let rows = tx
        .query_all(db::statement(
            &tx,
            "SELECT org_id,id,job_id FROM neev_knowledge_batches WHERE state='running'",
            vec![],
        ))
        .await
        .map_err(db_error)?;
    for row in &rows {
        let org_id: i32 = row.try_get("", "org_id").map_err(db_error)?;
        let id: String = row.try_get("", "id").map_err(db_error)?;
        let job_id: String = row.try_get("", "job_id").map_err(db_error)?;
        db::execute(
            &tx,
            "UPDATE neev_knowledge_batches SET state='failed',reason='interrupted',settled_at=$1 WHERE org_id=$2 AND id=$3",
            vec![now.into(), org_id.into(), id.into()],
        )
        .await?;
        output::pause_running_job(&tx, org_id, &job_id, PauseReason::Interrupted, now).await?;
        super::log::append(&tx, org_id, None, "batch_failed", None, Some(&job_id), "reason=interrupted", now).await?;
    }
    tx.commit().await.map_err(db_error)?;
    Ok(())
}

/// Insert a running turn into the job's own session without a browser
/// session (same columns and guards as `sessions::begin_turn`).
async fn insert_turn(state: &ServerState, org_id: i32, user_id: i32, session_id: &str, prompt: &str) -> Result<(String, String), KnowledgeError> {
    let now = state.clock.now_unix();
    let tx = db::begin(&state.database).await?;
    let found = db::one(
        &tx,
        "SELECT provider,model,archived FROM neev_agent_sessions WHERE org_id=$1 AND id=$2 AND owner_id=$3",
        vec![org_id.into(), session_id.to_owned().into(), user_id.into()],
    )
    .await?
    .ok_or_else(KnowledgeError::not_found)?;
    if found.try_get::<bool>("", "archived").map_err(db_error)? {
        return Err(KnowledgeError::conflict());
    }
    if db::one(
        &tx,
        "SELECT id FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2 AND status='running'",
        vec![org_id.into(), session_id.to_owned().into()],
    )
    .await?
    .is_some()
    {
        return Err(KnowledgeError::conflict());
    }
    let sequence: i64 = db::one(
        &tx,
        "SELECT COALESCE(MAX(sequence),0) AS n FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2",
        vec![org_id.into(), session_id.to_owned().into()],
    )
    .await?
    .ok_or_else(KnowledgeError::storage)?
    .try_get("", "n")
    .map_err(db_error)?;
    if sequence >= MAX_TURNS_PER_SESSION {
        return Err(KnowledgeError::conflict());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    db::execute(
        &tx,
        "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,request_id,prompt,response,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,$3,$4,$5,NULL,$6,'','running',NULL,$7,$8,$9,$9,NULL)",
        vec![
            org_id.into(),
            session_id.to_owned().into(),
            (sequence + 1).into(),
            id.clone().into(),
            run_id.clone().into(),
            prompt.to_owned().into(),
            found.try_get::<String>("", "provider").map_err(db_error)?.into(),
            found.try_get::<String>("", "model").map_err(db_error)?.into(),
            now.into(),
        ],
    )
    .await?;
    db::execute(
        &tx,
        "UPDATE neev_agent_sessions SET revision=revision+1,updated_at=$1 WHERE org_id=$2 AND id=$3",
        vec![now.into(), org_id.into(), session_id.to_owned().into()],
    )
    .await?;
    tx.commit().await.map_err(db_error)?;
    Ok((id, run_id))
}

/// The `RunRequest` of a knowledge batch, built directly: fixed prompt and
/// briefing, knowledge files only, no history, no library, two free lanes.
pub(crate) fn knowledge_request(job: &Job, staged: &StagedBatch, credential_handle: String) -> RunRequest {
    RunRequest {
        user_id: job.started_by,
        org_id: job.org_id,
        chat_id: job.session_id.clone().unwrap_or_default(),
        provider: job.provider.clone(),
        model: job.model.clone(),
        prompt: staged.prompt.clone(),
        credential_handle: Some(credential_handle),
        briefing: Some(staged.briefing.clone()),
        effort: None,
        fast: false,
        attachments: Vec::new(),
        context_files: staged.context_files.clone(),
        library_items: Vec::new(),
        min_free_lanes: MIN_FREE_LANES_FOR_KNOWLEDGE,
    }
}

async fn finish_job(state: &ServerState, job: &Job, now: i64) -> Result<JobState, KnowledgeError> {
    let tx = db::begin(&state.database).await?;
    lock_job(&tx, job.org_id, &job.id).await?;
    let eligible = super::access::eligible_predicate("r");
    let gaps = db::one(
        &tx,
        &format!("SELECT 1 AS gap FROM neev_knowledge_snapshots s JOIN neev_knowledge_coverage c ON c.org_id=s.org_id AND c.snapshot_id=s.id JOIN neev_resources r ON r.org_id=s.org_id AND r.id=s.source_id WHERE s.org_id=$1 AND s.state='current' AND c.state='uncovered' AND {eligible} LIMIT 1"),
        vec![job.org_id.into()],
    )
    .await?
    .is_some();
    let final_state = if gaps { JobState::CompletedWithGaps } else { JobState::Completed };
    let changed = db::execute(
        &tx,
        "UPDATE neev_knowledge_jobs SET state=$1,pause_reason=NULL,wait_reason=NULL,updated_at=$2 WHERE org_id=$3 AND id=$4 AND state='running'",
        vec![final_state.as_str().into(), now.into(), job.org_id.into(), job.id.clone().into()],
    )
    .await?
    .rows_affected();
    if changed == 1 && job.mode == JobMode::Rebuild {
        // Only a completed rebuild archives the pages it did not produce.
        db::execute(
            &tx,
            "UPDATE neev_knowledge_pages SET status='archived',updated_at=$1 WHERE org_id=$2 AND status='active' AND NOT EXISTS (SELECT 1 FROM neev_knowledge_revisions v JOIN neev_knowledge_batches b ON b.org_id=v.org_id AND b.id=v.batch_id WHERE v.org_id=neev_knowledge_pages.org_id AND v.page_id=neev_knowledge_pages.id AND v.revision=neev_knowledge_pages.head_revision AND b.job_id=$3)",
            vec![now.into(), job.org_id.into(), job.id.clone().into()],
        )
        .await?;
    }
    if changed == 1 {
        archive_session(&tx, job.org_id, &job.id, now).await?;
        super::log::append(&tx, job.org_id, None, "job_completed", None, Some(&job.id), &format!("state={}", final_state.as_str()), now).await?;
    }
    tx.commit().await.map_err(db_error)?;
    Ok(final_state)
}

async fn set_wait(state: &ServerState, job: &Job, wait: Option<&str>, now: i64) -> Result<(), KnowledgeError> {
    db::execute(
        &state.database,
        "UPDATE neev_knowledge_jobs SET wait_reason=$1,updated_at=$2 WHERE org_id=$3 AND id=$4 AND state='running'",
        vec![wait.map(str::to_owned).into(), now.into(), job.org_id.into(), job.id.clone().into()],
    )
    .await?;
    Ok(())
}

async fn pause_job(state: &ServerState, job: &Job, reason: PauseReason, now: i64) -> Result<(), KnowledgeError> {
    let tx = db::begin(&state.database).await?;
    output::pause_running_job(&tx, job.org_id, &job.id, reason, now).await?;
    super::log::append(&tx, job.org_id, None, "job_paused", None, Some(&job.id), &format!("reason={}", reason.as_str()), now).await?;
    tx.commit().await.map_err(db_error)?;
    Ok(())
}

/// What the loop did for one job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Advance {
    Started { batch_id: String },
    WaitingForSlot,
    Completed(JobState),
    Paused(PauseReason),
    Busy,
}

/// Start the next batch of a running job with no running batch.
pub(crate) async fn advance(state: &Arc<ServerState>, job: &Job, now: i64) -> Result<Advance, KnowledgeError> {
    store::store(state)?;
    if db::one(
        &state.database,
        "SELECT id FROM neev_knowledge_batches WHERE org_id=$1 AND job_id=$2 AND state='running'",
        vec![job.org_id.into(), job.id.clone().into()],
    )
    .await?
    .is_some()
    {
        return Ok(Advance::Busy);
    }
    // 1-2. The starter must still be an admin with their own credential.
    let Some(starter) = starter_context(&state.database, job.org_id, job.started_by).await? else {
        pause_job(state, job, PauseReason::NotAdmin, now).await?;
        return Ok(Advance::Paused(PauseReason::NotAdmin));
    };
    let Some(handle) = starter_credential(&state.database, job.org_id, job.started_by, &job.provider).await? else {
        pause_job(state, job, PauseReason::CredentialMissing, now).await?;
        return Ok(Advance::Paused(PauseReason::CredentialMissing));
    };
    let Some(session_id) = job.session_id.clone() else {
        pause_job(state, job, PauseReason::RunFailed, now).await?;
        return Ok(Advance::Paused(PauseReason::RunFailed));
    };
    // 3. Units, or completion.
    let units = compile::select_units(&state.database, job.org_id, job).await?;
    if units.is_empty() {
        return Ok(Advance::Completed(finish_job(state, job, now).await?));
    }
    // 4. Stage.
    let Some(staged) = compile::stage_batch(state, job.org_id, job, units, now).await? else {
        return Ok(Advance::Busy);
    };
    // 5-6. Run, only if two lanes are free (atomic inside the runtime).
    let request = knowledge_request(job, &staged, handle);
    let mut handle = match state.agents.run(request).await {
        Ok(handle) => handle,
        // Only a lane-capacity refusal is a wait; any other runtime fault
        // (staging I/O, workdir, a broken launcher) pauses the job honestly
        // instead of re-staging every tick under "waiting for a slot".
        Err(ExecutionError::LanesBusy) => {
            compile::delete_staged(&state.database, job.org_id, &staged.batch_id).await?;
            set_wait(state, job, Some("slot"), now).await?;
            return Ok(Advance::WaitingForSlot);
        }
        Err(_) => {
            output::settle_failed(state, job.org_id, &staged.batch_id, PauseReason::RunFailed, now).await?;
            return Ok(Advance::Paused(PauseReason::RunFailed));
        }
    };
    // 7. The turn, only after the run started.
    let (turn_id, run_id) = match insert_turn(state, job.org_id, job.started_by, &session_id, &staged.prompt).await {
        Ok(ids) => ids,
        Err(_) => {
            if let Some(cancel) = handle.take_canceler() {
                let _ = cancel.send(());
            }
            compile::delete_staged(&state.database, job.org_id, &staged.batch_id).await?;
            pause_job(state, job, PauseReason::RunFailed, now).await?;
            return Ok(Advance::Paused(PauseReason::RunFailed));
        }
    };
    db::execute(
        &state.database,
        "UPDATE neev_knowledge_batches SET turn_id=$1,started_at=$2 WHERE org_id=$3 AND id=$4",
        vec![turn_id.clone().into(), now.into(), job.org_id.into(), staged.batch_id.clone().into()],
    )
    .await?;
    set_wait(state, job, None, now).await?;
    runs::start_with_purpose(
        state.clone(),
        run_id,
        starter,
        session_id,
        turn_id,
        handle,
        None,
        RunPurpose::Knowledge {
            org_id: job.org_id,
            batch_id: staged.batch_id.clone(),
            job_id: job.id.clone(),
        },
    );
    Ok(Advance::Started { batch_id: staged.batch_id })
}

/// Cancel and settle every batch past `BATCH_TIMEOUT_SECS`.
async fn enforce_timeouts(state: &ServerState, now: i64) -> Result<(), KnowledgeError> {
    let rows = state
        .database
        .query_all(db::statement(
            &state.database,
            "SELECT org_id,id FROM neev_knowledge_batches WHERE state='running' AND started_at IS NOT NULL AND started_at<$1",
            vec![(now - BATCH_TIMEOUT_SECS as i64).into()],
        ))
        .await
        .map_err(db_error)?;
    for row in &rows {
        let org_id: i32 = row.try_get("", "org_id").map_err(db_error)?;
        let id: String = row.try_get("", "id").map_err(db_error)?;
        output::settle_failed(state, org_id, &id, PauseReason::Timeout, now).await?;
        cancel_batch_run(state, org_id, &id).await?;
    }
    Ok(())
}

/// One job-loop tick: enforce timeouts, then advance each running job that has
/// no running batch.
pub(crate) async fn run_tick(state: &Arc<ServerState>, now: i64) -> Result<Vec<Advance>, KnowledgeError> {
    store::store(state)?;
    enforce_timeouts(state, now).await?;
    let rows = state
        .database
        .query_all(db::statement(
            &state.database,
            &format!("SELECT {JOB_COLUMNS} FROM neev_knowledge_jobs j WHERE state='running' AND session_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM neev_knowledge_batches b WHERE b.org_id=j.org_id AND b.job_id=j.id AND b.state='running') ORDER BY org_id"),
            vec![],
        ))
        .await
        .map_err(db_error)?;
    let mut done = Vec::new();
    for row in &rows {
        let job = job_row(row)?;
        match advance(state, &job, now).await {
            Ok(step) => done.push(step),
            Err(KnowledgeError::Unavailable) => return Err(KnowledgeError::Unavailable),
            Err(_) => {
                let _ = pause_job(state, &job, PauseReason::RunFailed, now).await;
                done.push(Advance::Paused(PauseReason::RunFailed));
            }
        }
    }
    Ok(done)
}
