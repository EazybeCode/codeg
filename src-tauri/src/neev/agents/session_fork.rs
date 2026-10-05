//! Fork a standalone Agent Chat into a NEW private Agent Chat, up through one
//! completed turn.
//!
//! The same contract as a resource-chat fork (`content::fork`): a copy, never a
//! link. Every COMPLETED turn up through the selection — prompt, response,
//! trace, the follow-ups it accepted — is written again into a session the
//! forker owns, renumbered from 1 in the same order, and every file those
//! turns carried (uploads and generated outputs) is copied byte-for-byte to a
//! fresh key owned by the new session. Runs are stateless per turn: the next
//! run in the fork is composed from these stored turns (`recent_turns`,
//! `transcript::build`), so the copied history IS its context, and no provider
//! session is resumed or shared.
//!
//! Failed and canceled turns are not part of any run's context and are not
//! copied. Sessions are always owner-private, so the fork is too.

use std::collections::BTreeMap;

use sea_orm::{ConnectionTrait, DatabaseTransaction};

use crate::neev::{
    auth::session,
    content::{
        access, attachment,
        fork::{self, CopiedFile, FileSnapshot, ForkError},
        resources,
        types::ContentError,
    },
    db,
    error::database_error,
    org,
    startup::ServerState,
    types::OrgContext,
};

/// What a session fork produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionFork {
    pub session_id: String,
    pub source_session_id: String,
    pub through_turn_id: String,
    pub turns: i64,
    pub attachments: i64,
    pub archived_attachments: i64,
    /// Completed turns only: how many failed or canceled turns up through the
    /// selection were left out (they never reach a run's context either).
    pub skipped_turns: i64,
    pub replayed: bool,
}

#[derive(Clone, Debug)]
struct TurnSnapshot {
    id: String,
    sequence: i64,
    prompt: String,
    response: String,
    activity: Option<String>,
    provider: String,
    model: String,
    created_at: i64,
    updated_at: i64,
    completed_at: Option<i64>,
    /// (position, key), in order.
    files: Vec<(i32, String)>,
    steers: Vec<SteerSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SteerSnapshot {
    request_id: String,
    text: String,
    accepted: bool,
    response_offset: Option<i64>,
    created_at: i64,
}

struct SourceSession {
    title: String,
    provider: String,
    model: String,
    effort: Option<String>,
    fast: bool,
    bound_chat_id: Option<String>,
    node_id: Option<i32>,
}

struct Snapshot {
    source: SourceSession,
    turns: Vec<TurnSnapshot>,
    files: BTreeMap<String, FileSnapshot>,
    archived: i64,
    skipped: i64,
}

const FORK_ACTION: &str = "agent.session.fork";
const SOURCE_ACTION: &str = "agent.session.fork.source";
/// The per-owner session allowance and per-session turn bound, as
/// `agents::sessions` enforces them.
const MAX_SESSIONS_PER_OWNER: i64 = 200;
const MAX_TURNS_PER_SESSION: usize = 500;

fn fork_hash(actor: &OrgContext, session_id: &str, through: &str) -> String {
    org::digest_parts(&[FORK_ACTION, &actor.org_id.to_string(), session_id, through])
}

/// The caller's own forkable Agent Chat: owned, a `chat` session (never a team
/// holder), and not an org-knowledge job's hidden session.
async fn source<C: ConnectionTrait>(
    db: &C,
    actor: &OrgContext,
    id: &str,
) -> Result<SourceSession, ForkError> {
    let row = db::one(
        db,
        "SELECT title,provider,model,effort,fast,bound_chat_id,node_id FROM neev_agent_sessions s WHERE org_id=$1 AND id=$2 AND owner_id=$3 AND kind='chat' AND NOT EXISTS (SELECT 1 FROM neev_knowledge_jobs j WHERE j.org_id=s.org_id AND j.session_id=s.id)",
        vec![
            actor.org_id.into(),
            id.into(),
            actor.identity.user_id.into(),
        ],
    )
    .await?
    .ok_or(ContentError::NotFound)?;
    Ok(SourceSession {
        title: row.try_get("", "title").map_err(database_error)?,
        provider: row.try_get("", "provider").map_err(database_error)?,
        model: row.try_get("", "model").map_err(database_error)?,
        effort: row.try_get("", "effort").map_err(database_error)?,
        fast: row.try_get("", "fast").map_err(database_error)?,
        bound_chat_id: row.try_get("", "bound_chat_id").map_err(database_error)?,
        node_id: row.try_get("", "node_id").map_err(database_error)?,
    })
}

async fn chat_in_trash(
    tx: &DatabaseTransaction,
    org_id: i32,
    chat_id: &str,
) -> Result<bool, ContentError> {
    Ok(db::one(
        tx,
        "SELECT 1 AS hit FROM neev_resources r JOIN neev_node_trash t ON t.org_id=r.org_id AND t.item_kind='node' AND t.item_id=CAST(r.node_id AS TEXT) WHERE r.org_id=$1 AND r.id=$2",
        vec![org_id.into(), chat_id.into()],
    )
    .await?
    .is_some())
}

async fn snapshot(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    session_id: &str,
    through: &str,
) -> Result<Snapshot, ForkError> {
    let source = source(tx, actor, session_id).await?;
    // A bound Agent Chat's context includes its chat; the fork keeps that
    // binding only while the forker may still read the chat, and is refused
    // otherwise rather than made with a context the original did not have.
    if let Some(chat_id) = source.bound_chat_id.as_deref() {
        let readable = access::authorized(tx, actor, chat_id, false).await.is_ok();
        if !readable || chat_in_trash(tx, actor.org_id, chat_id).await? {
            return Err(ForkError::BoundChatUnavailable);
        }
    }
    let selected = db::one(
        tx,
        "SELECT sequence,status FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2 AND id=$3",
        vec![actor.org_id.into(), session_id.into(), through.into()],
    )
    .await?
    .ok_or(ContentError::NotFound)?;
    let through_sequence: i64 = selected.try_get("", "sequence").map_err(database_error)?;
    let status: String = selected.try_get("", "status").map_err(database_error)?;
    if status != "completed" {
        return Err(ForkError::NotComplete);
    }
    let rows = tx
        .query_all(db::statement(
            tx,
            &format!(
                "SELECT id,sequence,status,prompt,response,activity,provider,model,created_at,updated_at,completed_at FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2 AND sequence<=$3 ORDER BY sequence LIMIT {}",
                MAX_TURNS_PER_SESSION + 1
            ),
            vec![actor.org_id.into(), session_id.into(), through_sequence.into()],
        ))
        .await
        .map_err(database_error)?;
    if rows.len() > MAX_TURNS_PER_SESSION {
        return Err(ForkError::TooLarge);
    }
    let mut turns = Vec::new();
    let mut index = BTreeMap::new();
    let mut skipped = 0;
    let mut text_bytes = 0i64;
    for row in &rows {
        let status: String = row.try_get("", "status").map_err(database_error)?;
        if status != "completed" {
            skipped += 1;
            continue;
        }
        let sequence: i64 = row.try_get("", "sequence").map_err(database_error)?;
        let prompt: String = row.try_get("", "prompt").map_err(database_error)?;
        let response: String = row.try_get("", "response").map_err(database_error)?;
        let activity: Option<String> = row.try_get("", "activity").map_err(database_error)?;
        text_bytes += (prompt.len() + response.len() + activity.as_ref().map_or(0, String::len))
            as i64;
        index.insert(sequence, turns.len());
        turns.push(TurnSnapshot {
            id: row.try_get("", "id").map_err(database_error)?,
            sequence,
            prompt,
            response,
            activity,
            provider: row.try_get("", "provider").map_err(database_error)?,
            model: row.try_get("", "model").map_err(database_error)?,
            created_at: row.try_get("", "created_at").map_err(database_error)?,
            updated_at: row.try_get("", "updated_at").map_err(database_error)?,
            completed_at: row.try_get("", "completed_at").map_err(database_error)?,
            files: Vec::new(),
            steers: Vec::new(),
        });
    }
    // The fork must be able to take its next turn: `sessions::begin` refuses a
    // turn once the session holds `MAX_TURNS_PER_SESSION`, so a fork that
    // already holds that many would be created only to refuse every continuation.
    if turns.len() >= MAX_TURNS_PER_SESSION {
        return Err(ForkError::TooLarge);
    }
    if text_bytes > fork::MAX_FORK_TEXT_BYTES {
        return Err(ForkError::TooLarge);
    }
    // The follow-ups each copied turn accepted, in order.
    let steers = tx
        .query_all(db::statement(
            tx,
            "SELECT turn_sequence,request_id,text,accepted,response_offset,created_at FROM neev_steer_inputs WHERE org_id=$1 AND session_id=$2 AND turn_sequence<=$3 ORDER BY turn_sequence,created_at,request_id",
            vec![actor.org_id.into(), session_id.into(), through_sequence.into()],
        ))
        .await
        .map_err(database_error)?;
    for row in &steers {
        let sequence: i64 = row.try_get("", "turn_sequence").map_err(database_error)?;
        if let Some(&at) = index.get(&sequence) {
            turns[at].steers.push(SteerSnapshot {
                request_id: row.try_get("", "request_id").map_err(database_error)?,
                text: row.try_get("", "text").map_err(database_error)?,
                accepted: row.try_get("", "accepted").map_err(database_error)?,
                response_offset: row.try_get("", "response_offset").map_err(database_error)?,
                created_at: row.try_get("", "created_at").map_err(database_error)?,
            });
        }
    }
    // The files those turns carried. Each must belong to THIS session.
    let refs = tx
        .query_all(db::statement(
            tx,
            "SELECT l.turn_sequence,l.position,a.key,a.session_id,a.archived,a.file_name,a.content_type,a.byte_length,a.sha256,a.created_by,a.created_at FROM neev_attachment_refs l JOIN neev_attachments a ON a.org_id=l.org_id AND a.key=l.attachment_key WHERE l.org_id=$1 AND l.turn_session_id=$2 AND l.turn_sequence<=$3 ORDER BY l.turn_sequence,l.position",
            vec![actor.org_id.into(), session_id.into(), through_sequence.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut files = BTreeMap::new();
    let mut archived = std::collections::BTreeSet::new();
    for row in &refs {
        let sequence: i64 = row.try_get("", "turn_sequence").map_err(database_error)?;
        let Some(&at) = index.get(&sequence) else {
            continue; // a skipped (failed) turn's files go with it
        };
        let owner: Option<String> = row.try_get("", "session_id").map_err(database_error)?;
        if owner.as_deref() != Some(session_id) {
            return Err(ContentError::Storage.into());
        }
        let file = fork::file_row(row)?;
        if row.try_get::<bool>("", "archived").map_err(database_error)? {
            archived.insert(file.key);
            continue;
        }
        let position: i32 = row.try_get("", "position").map_err(database_error)?;
        turns[at].files.push((position, file.key.clone()));
        files.insert(file.key.clone(), file);
    }
    fork::check_file_bounds(&files)?;
    Ok(Snapshot {
        source,
        turns,
        files,
        archived: archived.len() as i64,
        skipped,
    })
}

async fn replay(
    tx: &DatabaseTransaction,
    actor: &OrgContext,
    fork_id: &str,
    session_id: &str,
    through: &str,
) -> Result<SessionFork, ForkError> {
    source(tx, actor, fork_id).await?;
    let counts = db::one(
        tx,
        "SELECT (SELECT COUNT(*) FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2) AS t,(SELECT COUNT(*) FROM neev_attachments WHERE org_id=$1 AND session_id=$2) AS a",
        vec![actor.org_id.into(), fork_id.into()],
    )
    .await?
    .ok_or(ContentError::Storage)?;
    Ok(SessionFork {
        session_id: fork_id.to_owned(),
        source_session_id: session_id.to_owned(),
        through_turn_id: through.to_owned(),
        turns: counts.try_get("", "t").map_err(database_error)?,
        attachments: counts.try_get("", "a").map_err(database_error)?,
        archived_attachments: 0,
        skipped_turns: 0,
        replayed: true,
    })
}

/// Fork the caller's Agent Chat `session_id` up through the completed turn
/// `through` into a new private Agent Chat. Idempotent on `request_id`.
pub(crate) async fn fork_session(
    state: &ServerState,
    actor: &OrgContext,
    session_id: &str,
    through: &str,
    request_id: &str,
) -> Result<SessionFork, ForkError> {
    canonical(session_id)?;
    canonical(through)?;
    let request = org::normalize_request_id(request_id).map_err(|_| ContentError::Invalid)?;
    let hash = fork_hash(actor, session_id, through);

    let tx = resources::begin(state, actor).await?;
    if let Some(id) = resources::receipt(&tx, actor, FORK_ACTION, &request, &hash).await? {
        let done = replay(&tx, actor, &id, session_id, through).await?;
        session::recheck(&tx, actor, state.clock.now_unix()).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(done);
    }
    let snap = snapshot(&tx, actor, session_id, through).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;

    let copied = fork::copy_files(state, actor.org_id, &snap.files).await?;
    match commit(state, actor, session_id, through, &request, &hash, &snap, &copied).await {
        Ok(done) => {
            if done.replayed {
                fork::discard_files(state, actor.org_id, &copied).await;
            }
            Ok(done)
        }
        Err(error) => {
            fork::discard_files(state, actor.org_id, &copied).await;
            Err(error)
        }
    }
}

fn canonical(id: &str) -> Result<(), ContentError> {
    if uuid::Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id) {
        Ok(())
    } else {
        Err(ContentError::NotFound)
    }
}

#[allow(clippy::too_many_arguments)]
async fn commit(
    state: &ServerState,
    actor: &OrgContext,
    session_id: &str,
    through: &str,
    request: &str,
    hash: &str,
    snap: &Snapshot,
    copied: &[CopiedFile],
) -> Result<SessionFork, ForkError> {
    let tx = resources::begin(state, actor).await?;
    if let Some(id) = resources::receipt(&tx, actor, FORK_ACTION, request, hash).await? {
        let done = replay(&tx, actor, &id, session_id, through).await?;
        session::recheck(&tx, actor, state.clock.now_unix()).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(done);
    }
    let again = snapshot(&tx, actor, session_id, through).await?;
    let same = again.turns.len() == snap.turns.len()
        && again.turns.iter().zip(&snap.turns).all(|(a, b)| {
            a.id == b.id
                && a.sequence == b.sequence
                && a.files == b.files
                && a.steers == b.steers
                && a.response == b.response
        })
        && again.files.keys().eq(snap.files.keys());
    if !same {
        return Err(ContentError::Conflict.into());
    }
    fork::verify_files(&tx, actor.org_id, copied).await?;
    let owned: i64 = db::one(
        &tx,
        "SELECT COUNT(*) AS n FROM neev_agent_sessions s WHERE org_id=$1 AND owner_id=$2 AND NOT EXISTS (SELECT 1 FROM neev_knowledge_jobs j WHERE j.org_id=s.org_id AND j.session_id=s.id)",
        vec![actor.org_id.into(), actor.identity.user_id.into()],
    )
    .await?
    .ok_or(ContentError::Storage)?
    .try_get("", "n")
    .map_err(database_error)?;
    if owned >= MAX_SESSIONS_PER_OWNER {
        return Err(ContentError::Quota.into());
    }
    fork::charge_files(&tx, actor.org_id, copied).await?;
    // A placement in a folder now in the trash is dropped (unfiled): a session
    // is owner-private wherever it sits, so placement never widens it.
    let node_id = match snap.source.node_id {
        Some(node) if !crate::neev::catalog::trash::node_trashed(&tx, actor.org_id, node).await? => {
            Some(node)
        }
        _ => None,
    };
    let now = state.clock.now_unix();
    if now < 0 {
        return Err(ContentError::Invalid.into());
    }
    let fork_id = uuid::Uuid::new_v4().to_string();
    db::execute(
        &tx,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,effort,fast,visibility,kind,bound_chat_id,bound_kind,node_id,archived,revision,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'private','chat',$9,$10,$11,false,1,$12,$12)",
        vec![
            actor.org_id.into(),
            fork_id.clone().into(),
            actor.identity.user_id.into(),
            fork::fork_title(&snap.source.title).into(),
            snap.source.provider.clone().into(),
            snap.source.model.clone().into(),
            snap.source.effort.clone().into(),
            snap.source.fast.into(),
            snap.source.bound_chat_id.clone().into(),
            snap.source.bound_chat_id.as_ref().map(|_| "chat").into(),
            node_id.into(),
            now.into(),
        ],
    )
    .await?;
    fork::insert_files(&tx, actor.org_id, attachment::Owner::Session(&fork_id), copied).await?;
    let mapped: BTreeMap<&str, &str> = copied
        .iter()
        .map(|file| (file.old_key.as_str(), file.new_key.as_str()))
        .collect();
    for (offset, turn) in snap.turns.iter().enumerate() {
        let sequence = offset as i64 + 1;
        db::execute(
            &tx,
            "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,request_id,prompt,response,activity,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,$3,$4,$5,NULL,$6,$7,$8,'completed',NULL,$9,$10,$11,$12,$13)",
            vec![
                actor.org_id.into(),
                fork_id.clone().into(),
                sequence.into(),
                uuid::Uuid::new_v4().to_string().into(),
                uuid::Uuid::new_v4().to_string().into(),
                turn.prompt.clone().into(),
                fork::rewrite_links(&turn.response, copied).into(),
                turn.activity
                    .as_deref()
                    .map(|activity| fork::rewrite_links(activity, copied))
                    .into(),
                turn.provider.clone().into(),
                turn.model.clone().into(),
                turn.created_at.into(),
                turn.updated_at.into(),
                turn.completed_at.into(),
            ],
        )
        .await?;
        for steer in &turn.steers {
            db::execute(
                &tx,
                "INSERT INTO neev_steer_inputs(org_id,session_id,turn_sequence,request_id,text,accepted,response_offset,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
                vec![
                    actor.org_id.into(),
                    fork_id.clone().into(),
                    sequence.into(),
                    steer.request_id.clone().into(),
                    steer.text.clone().into(),
                    steer.accepted.into(),
                    steer.response_offset.into(),
                    steer.created_at.into(),
                ],
            )
            .await?;
        }
        for (position, key) in &turn.files {
            let new_key = mapped.get(key.as_str()).ok_or(ContentError::Storage)?;
            db::execute(
                &tx,
                "INSERT INTO neev_attachment_refs(org_id,attachment_key,turn_session_id,turn_sequence,position) VALUES($1,$2,$3,$4,$5)",
                vec![
                    actor.org_id.into(),
                    (*new_key).to_owned().into(),
                    fork_id.clone().into(),
                    sequence.into(),
                    (*position).into(),
                ],
            )
            .await?;
        }
    }
    resources::record_receipt(&tx, actor, FORK_ACTION, request, hash, &fork_id).await?;
    let provenance = org::digest_parts(&[SOURCE_ACTION, session_id, through]);
    resources::record_receipt(&tx, actor, SOURCE_ACTION, &fork_id, &provenance, session_id)
        .await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(SessionFork {
        session_id: fork_id,
        source_session_id: session_id.to_owned(),
        through_turn_id: through.to_owned(),
        turns: snap.turns.len() as i64,
        attachments: copied.len() as i64,
        archived_attachments: snap.archived,
        skipped_turns: snap.skipped,
        replayed: false,
    })
}

/// Which Agent Chat `fork_id` was forked from, while the caller still owns it.
pub(crate) async fn session_fork_source(
    state: &ServerState,
    actor: &OrgContext,
    fork_id: &str,
) -> Result<Option<String>, ContentError> {
    canonical(fork_id)?;
    let tx = resources::begin(state, actor).await?;
    source(&tx, actor, fork_id).await.map_err(|error| match error {
        ForkError::Content(error) => error,
        _ => ContentError::NotFound,
    })?;
    let found = db::one(
        &tx,
        "SELECT resource_id FROM neev_requests WHERE user_id=$1 AND action=$2 AND request_id=$3",
        vec![
            actor.identity.user_id.into(),
            SOURCE_ACTION.into(),
            fork_id.into(),
        ],
    )
    .await?
    .map(|row| row.try_get::<String>("", "resource_id").map_err(database_error))
    .transpose()?;
    let owned = match &found {
        Some(id) => source(&tx, actor, id).await.is_ok(),
        None => false,
    };
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(found.filter(|_| owned))
}
