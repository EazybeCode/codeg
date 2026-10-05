//! Server-owned agent **sessions** and their **turn/run history** (spec §5.1).
//!
//! This is the durable half of Agent Chats: it replaces the one-shot,
//! non-persisted `RunRequest`/`agentText` path with a session entity that owns
//! an ordered history of turns, so a reload restores the conversation and a
//! dropped stream can reattach to a run instead of re-executing a prompt.
//!
//! Three invariants are load-bearing:
//! - **Owner-only, always private.** Every read and write is scoped by
//!   `(org_id, owner_id)` from the server-authoritative actor — never from the
//!   request — and `visibility` is CHECK-pinned to `'private'` in the schema, so
//!   a session's content cannot reach team-visible storage from here at all.
//! - **The prompt is durable before the run starts.** A turn row is written
//!   (status `running`) *before* the runtime is asked to execute, so a refusal
//!   (runtime disabled, both lanes busy, provider error) is recorded as a real
//!   failed turn the user can see and retry, rather than vanishing.
//! - **No turn stays `running` across a restart.** [`reconcile_interrupted`]
//!   runs at startup and settles every live turn as `failed/interrupted`,
//!   because the process that owned its stream is gone and it can never
//!   complete.

use sea_orm::{ConnectionTrait, DatabaseConnection, QueryResult};

use crate::neev::{
    auth::session,
    content::{resources, types::ContentError},
    db,
    error::database_error,
    org,
    startup::ServerState,
    types::OrgContext,
};

/// One user may keep this many sessions in an org (archived included). A bound
/// list keeps the sidebar and the per-user storage footprint finite.
const MAX_SESSIONS_PER_OWNER: i64 = 200;
/// Turns per session. Past this the user starts a new session; the cap keeps a
/// single session's replay bounded.
const MAX_TURNS_PER_SESSION: i64 = 500;
/// Mirrors the schema's `length(response)<=262144`.
pub(crate) const MAX_RESPONSE_CHARS: usize = 262_144;
/// Mirrors the schema's `length(prompt) BETWEEN 1 AND 32768`.
const MAX_PROMPT_CHARS: usize = 32_768;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnStatus {
    Running,
    Completed,
    Failed,
    Canceled,
}

impl TurnStatus {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    fn parse(value: &str) -> Result<Self, ContentError> {
        match value {
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            _ => Err(ContentError::Storage),
        }
    }
}

/// What a session is for.
///
/// Both kinds are private to their owner; the difference is *what the run is
/// allowed to do afterwards*. A `Chat` session is a saved Agent Chat and its
/// replies stay with the owner. A `Team` session exists only to hold the
/// durable turn for an explicit team invocation, and its reply is published as
/// an agent-authored message into the bound chat (§5.3). Keeping them apart is
/// what stops a team invocation from ever replaying a private session's
/// history as context — a `Team` session's runs are sent the explicit prompt
/// and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionKind {
    Chat,
    Team,
}

impl SessionKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Team => "team",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "chat" => Some(Self::Chat),
            "team" => Some(Self::Team),
            _ => None,
        }
    }
}

/// A team session's id is **derived** from `(org, owner, chat, provider)`, so
/// "get or create" is a plain insert that loses harmlessly against a concurrent
/// one — the primary key does the deduplication, and no extra index or lock is
/// needed.
///
/// The provider is part of the key because a turn may never contradict its
/// session's provider (the schema validator proves this by join). Switching
/// provider therefore starts a *separate* team session for the same chat rather
/// than rewriting history that already ran elsewhere.
///
/// The digest is laid out in the canonical 8-4-4-4-12 shape so the value passes
/// the same id validation as every other resource id.
pub(crate) fn team_session_id(org_id: i32, owner_id: i32, chat_id: &str, provider: &str) -> String {
    let digest = org::digest_parts(&[
        "agent.session.team",
        &org_id.to_string(),
        &owner_id.to_string(),
        chat_id,
        provider,
    ]);
    let hex = &digest[..32];
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Get or create the invoker's team session for one chat and provider.
///
/// The id is deterministic, so a concurrent duplicate loses on the primary key
/// and both callers end up with the same session — no lock, no extra index.
pub(crate) async fn ensure_team_session(
    state: &ServerState,
    actor: &OrgContext,
    chat_id: &str,
    provider_name: &str,
    model_name: &str,
    title_text: &str,
    effort: Option<&str>,
    fast: bool,
) -> Result<AgentSession, ContentError> {
    let provider_name = provider(provider_name)?.to_owned();
    let model_name = model(model_name)?.to_owned();
    let title_text = title(title_text)?;
    let id = team_session_id(actor.org_id, actor.identity.user_id, chat_id, &provider_name);
    let provider_name_owned = provider_name.clone();
    let model_name_owned = model_name.clone();
    // Settled BEFORE the transaction opens, like every other caller of
    // `validated_for` in this file.
    //
    // It cannot run inside one. `resources::begin` takes `neev_schema_version`
    // id=1 `FOR UPDATE` — one global row that serializes every content
    // transaction — and validation reaches model discovery, which opens its own
    // transaction (`store::authorized_transaction`) on a DIFFERENT pooled
    // connection and takes that same row. The inner transaction then waits for a
    // lock the outer one is holding while the outer waits for the inner: a
    // deadlock no database can break, because to Postgres the outer session is
    // simply idle in transaction. Once the provider is connected — and only
    // then, because before that discovery returns early without touching the
    // database — every team invocation hit it.
    let (level, fast) = super::capabilities::validated_for(
        state,
        actor,
        &provider_name_owned,
        &model_name_owned,
        effort,
        fast,
    )
    .await
    .map_err(|_| ContentError::Invalid)?;
    let tx = resources::begin(state, actor).await?;
    // Write authority over the chat is the invoker's right to have an agent
    // speak there; it is re-proved again at publish time.
    let chat = crate::neev::content::access::authorized(&tx, actor, chat_id, true).await?;
    if chat.kind != crate::neev::content::types::ResourceKind::Chat {
        return Err(ContentError::NotFound);
    }
    if chat_in_trash(&tx, actor.org_id, chat_id).await? {
        return Err(ContentError::Archived);
    }
    let now = now(state)?;
    db::execute(
        &tx,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,visibility,kind,bound_chat_id,bound_kind,archived,revision,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,'private','team',$7,'chat',false,1,$8,$8) ON CONFLICT DO NOTHING",
        vec![
            actor.org_id.into(),
            id.clone().into(),
            actor.identity.user_id.into(),
            title_text.into(),
            provider_name.into(),
            model_name.into(),
            chat_id.into(),
            now.into(),
        ],
    )
    .await?;
    // The chosen controls are applied to the holder INSIDE this transaction,
    // whether it was just created or already existed, so the first ask in a
    // bound chat runs at the settings the reader picked. Doing it after the
    // launch would charge them for a turn at the wrong settings, and doing it
    // in a second request would leave a window where a holder exists with the
    // wrong ones. What is applied was validated above.
    // Only when something actually differs, and then WITH a revision bump.
    //
    // Writing unconditionally and without bumping made this a hole in the
    // optimistic-concurrency protocol: a `PATCH /settings` holding a revision
    // this write had already invalidated would still be accepted, and the two
    // callers' choices would interleave into a combination neither made. An
    // invoke that changes nothing leaves the revision alone, so repeating an ask
    // does not invalidate anybody's open editor either.
    let current = owned(&tx, actor, &id).await?;
    let desired_effort = level;
    let found = if current.model != model_name_owned
        || current.effort != desired_effort
        || current.fast != fast
    {
        let revision = current
            .revision
            .checked_add(1)
            .ok_or(ContentError::Conflict)?;
        let changed = db::execute(
            &tx,
            "UPDATE neev_agent_sessions SET model=$1,effort=$2,fast=$3,revision=$4,updated_at=$5 WHERE org_id=$6 AND id=$7 AND owner_id=$8 AND revision=$9",
            vec![
                model_name_owned.clone().into(),
                desired_effort.into(),
                fast.into(),
                revision.into(),
                now.into(),
                actor.org_id.into(),
                id.clone().into(),
                actor.identity.user_id.into(),
                current.revision.into(),
            ],
        )
        .await?;
        if changed.rows_affected() != 1 {
            // Somebody changed this holder between the read and the write.
            return Err(ContentError::Conflict);
        }
        owned(&tx, actor, &id).await?
    } else {
        current
    };
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(found)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentSession {
    pub(crate) id: String,
    pub(crate) org_id: i32,
    pub(crate) owner_id: i32,
    pub(crate) title: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    /// The reasoning effort this session runs at, or `None` for the provider's
    /// own default.
    ///
    /// Stored so the choice survives a reload and is re-applied on every later
    /// turn: a control that has to be re-picked each time is not a setting, and
    /// a resumed conversation that quietly drops back to the default would be
    /// reporting a configuration it is not running.
    pub(crate) effort: Option<String>,
    /// Whether this session runs in the provider's fast mode. Never inherited:
    /// false unless the reader turned it on for this session.
    pub(crate) fast: bool,
    pub(crate) kind: SessionKind,
    pub(crate) bound_chat_id: Option<String>,
    /// Where this session sits in the workspace tree, or `None` for unfiled.
    ///
    /// Server-held so the placement follows the user to another device. Only a
    /// `chat` session can carry one; a `team` holder is internal and is never
    /// listed, so it is never in the tree.
    pub(crate) node_id: Option<i32>,
    pub(crate) archived: bool,
    pub(crate) revision: i64,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentTurn {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) sequence: i64,
    pub(crate) run_id: String,
    pub(crate) prompt: String,
    pub(crate) response: String,
    /// The turn's recorded tool trace, or `None` when it used no tools.
    pub(crate) activity: Vec<super::activity::ActivityItem>,
    pub(crate) status: TurnStatus,
    pub(crate) error_code: Option<String>,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) completed_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionInput {
    pub(crate) title: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) bound_chat_id: Option<String>,
    /// Optional placement in the workspace tree. Omitted means unfiled, which is
    /// what every session created before this existed stays.
    pub(crate) node_id: Option<i32>,
    /// Reasoning effort, or `None` for the provider's own default. Validated
    /// against what this provider's CLI accepts before it is stored, so a
    /// session cannot hold a level its every run would silently ignore.
    pub(crate) effort: Option<String>,
    /// Fast mode. Validated against the chosen MODEL, not just the provider.
    pub(crate) fast: bool,
    pub(crate) request_id: String,
}

const SESSION_COLUMNS: &str = "org_id,id,owner_id,title,provider,model,effort,fast,kind,bound_chat_id,node_id,archived,revision,created_at,updated_at";
const TURN_COLUMNS: &str = "org_id,session_id,sequence,id,run_id,prompt,response,activity,status,error_code,provider,model,created_at,updated_at,completed_at";

fn session_row(row: &QueryResult) -> Result<AgentSession, ContentError> {
    Ok(AgentSession {
        org_id: row.try_get("", "org_id").map_err(database_error)?,
        id: row.try_get("", "id").map_err(database_error)?,
        owner_id: row.try_get("", "owner_id").map_err(database_error)?,
        title: row.try_get("", "title").map_err(database_error)?,
        provider: row.try_get("", "provider").map_err(database_error)?,
        model: row.try_get("", "model").map_err(database_error)?,
        effort: row.try_get("", "effort").map_err(database_error)?,
        fast: row.try_get("", "fast").map_err(database_error)?,
        kind: SessionKind::parse(
            &row.try_get::<String>("", "kind").map_err(database_error)?,
        )
        .ok_or(ContentError::Storage)?,
        bound_chat_id: row.try_get("", "bound_chat_id").map_err(database_error)?,
        node_id: row.try_get("", "node_id").map_err(database_error)?,
        archived: row.try_get("", "archived").map_err(database_error)?,
        revision: row.try_get("", "revision").map_err(database_error)?,
        created_at: row.try_get("", "created_at").map_err(database_error)?,
        updated_at: row.try_get("", "updated_at").map_err(database_error)?,
    })
}

fn turn_row(row: &QueryResult) -> Result<AgentTurn, ContentError> {
    let status: String = row.try_get("", "status").map_err(database_error)?;
    Ok(AgentTurn {
        session_id: row.try_get("", "session_id").map_err(database_error)?,
        sequence: row.try_get("", "sequence").map_err(database_error)?,
        id: row.try_get("", "id").map_err(database_error)?,
        run_id: row.try_get("", "run_id").map_err(database_error)?,
        prompt: row.try_get("", "prompt").map_err(database_error)?,
        response: row.try_get("", "response").map_err(database_error)?,
        activity: super::activity::parse_stored(
            row.try_get::<Option<String>>("", "activity")
                .map_err(database_error)?
                .as_deref(),
        ),
        status: TurnStatus::parse(&status)?,
        error_code: row.try_get("", "error_code").map_err(database_error)?,
        provider: row.try_get("", "provider").map_err(database_error)?,
        model: row.try_get("", "model").map_err(database_error)?,
        created_at: row.try_get("", "created_at").map_err(database_error)?,
        updated_at: row.try_get("", "updated_at").map_err(database_error)?,
        completed_at: row.try_get("", "completed_at").map_err(database_error)?,
    })
}

/// A session title: same shape rule as a resource title, so the two surfaces
/// cannot disagree about what a user may type.
fn title(value: &str) -> Result<String, ContentError> {
    if value.chars().any(char::is_control) {
        return Err(ContentError::Invalid);
    }
    let value = value.trim();
    if !(1..=100).contains(&value.chars().count()) {
        return Err(ContentError::Invalid);
    }
    Ok(value.to_owned())
}

fn provider(value: &str) -> Result<&str, ContentError> {
    // Claude/Codex always; an ACP provider only while it is gated on.
    match super::types::Provider::parse_offered(value) {
        Some(_) => Ok(value),
        None => Err(ContentError::Invalid),
    }
}

/// Accepted model names. `default` means "the provider's own default"; anything
/// else must satisfy the SAME rule the runtime applies before building an argv,
/// so creation cannot produce a session whose every run would be refused.
fn model(value: &str) -> Result<&str, ContentError> {
    if value == "default" || crate::neev::agents::execution::safe_model(value) {
        Ok(value)
    } else {
        Err(ContentError::Invalid)
    }
}

fn prompt(value: &str) -> Result<&str, ContentError> {
    if !(1..=MAX_PROMPT_CHARS).contains(&value.chars().count()) {
        return Err(ContentError::Invalid);
    }
    Ok(value)
}

fn now(state: &ServerState) -> Result<i64, ContentError> {
    let now = state.clock.now_unix();
    if now < 0 {
        Err(ContentError::Invalid)
    } else {
        Ok(now)
    }
}

/// Load a session the actor owns, or `NotFound`. Ownership is part of the WHERE
/// clause, so another member of the same org gets the same answer as a stranger:
/// the session does not exist for them.
async fn owned<C: ConnectionTrait>(
    db: &C,
    actor: &OrgContext,
    id: &str,
) -> Result<AgentSession, ContentError> {
    db::one(
        db,
        &format!("SELECT {SESSION_COLUMNS} FROM neev_agent_sessions WHERE org_id=$1 AND id=$2 AND owner_id=$3"),
        vec![
            actor.org_id.into(),
            id.into(),
            actor.identity.user_id.into(),
        ],
    )
    .await?
    .map(|row| session_row(&row))
    .transpose()?
    .ok_or(ContentError::NotFound)
}

/// Load a session the actor may VIEW: their own, or a standalone Agent Chat a
/// member shared with them (`content::grants`). Only reads use this; every
/// write, run and setting stays on `owned`, so a viewer gets `NotFound` there.
async fn readable<C: ConnectionTrait>(
    db: &C,
    actor: &OrgContext,
    id: &str,
) -> Result<AgentSession, ContentError> {
    db::one(
        db,
        &format!(
            "SELECT {SESSION_COLUMNS} FROM neev_agent_sessions s WHERE org_id=$1 AND id=$2 AND {}",
            crate::neev::content::grants::session_readable_sql("s", "$3")
        ),
        vec![
            actor.org_id.into(),
            id.into(),
            actor.identity.user_id.into(),
        ],
    )
    .await?
    .map(|row| session_row(&row))
    .transpose()?
    .ok_or(ContentError::NotFound)
}

/// Who shared an Agent Chat, as the viewer sees them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionOwner {
    pub user_id: i32,
    pub display_name: String,
    pub avatar_url: Option<String>,
}

/// Agent Chats other members shared with the actor, newest first, with their
/// owners.
pub(crate) async fn shared_with(
    state: &ServerState,
    actor: &OrgContext,
) -> Result<Vec<(AgentSession, SessionOwner)>, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let mut items = Vec::new();
    for id in crate::neev::content::grants::shared_session_ids(&tx, actor).await? {
        let found = readable(&tx, actor, &id).await?;
        let row = db::one(
            &tx,
            "SELECT display_name,avatar_url FROM neev_accounts WHERE id=$1",
            vec![found.owner_id.into()],
        )
        .await?
        .ok_or(ContentError::Storage)?;
        let owner = SessionOwner {
            user_id: found.owner_id,
            display_name: row.try_get("", "display_name").map_err(database_error)?,
            avatar_url: row.try_get("", "avatar_url").map_err(database_error)?,
        };
        items.push((found, owner));
    }
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(items)
}

pub(crate) async fn create(
    state: &ServerState,
    actor: &OrgContext,
    input: SessionInput,
) -> Result<AgentSession, ContentError> {
    let title = title(&input.title)?;
    let provider = provider(&input.provider)?.to_owned();
    let model = model(&input.model)?.to_owned();
    // Refused at creation, not at run time: a session stored with a level its
    // provider ignores would look configured and behave as though it were not.
    // Against the CALLER's own catalogue, so a model their account advertises is
    // accepted and one it does not is refused. Never coerced by switching the
    // model: Fast on a model that cannot run it is two requests, and honouring
    // one by overriding the other spends money nobody agreed to.
    let (effort, fast) = super::capabilities::validated_for(
        state,
        actor,
        &provider,
        &model,
        input.effort.as_deref(),
        input.fast,
    )
    .await
    .map_err(|_| ContentError::Invalid)?;
    let request = org::normalize_request_id(&input.request_id).map_err(|_| ContentError::Invalid)?;
    let hash = org::digest_parts(&[
        "agent.session.create",
        &actor.org_id.to_string(),
        &title,
        &provider,
        &model,
        input.bound_chat_id.as_deref().unwrap_or(""),
        // In the digest so a retry that asks for a DIFFERENT placement is a
        // different request rather than silently resolving to the first one.
        &input.node_id.map(|id| id.to_string()).unwrap_or_default(),
    ]);
    let tx = resources::begin(state, actor).await?;
    // A committed retry returns the same session rather than creating a second.
    if let Some(id) = resources::receipt(&tx, actor, "agent.session.create", &request, &hash).await?
    {
        let existing = owned(&tx, actor, &id).await?;
        session::recheck(&tx, actor, state.clock.now_unix()).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(existing);
    }
    // A bound chat must be one the actor may actually read right now; binding
    // never grants access and never survives losing it.
    if let Some(chat_id) = input.bound_chat_id.as_deref() {
        let chat = crate::neev::content::access::authorized(&tx, actor, chat_id, false).await?;
        if chat.kind != crate::neev::content::types::ResourceKind::Chat {
            return Err(ContentError::NotFound);
        }
        if chat_in_trash(&tx, actor.org_id, chat_id).await? {
            return Err(ContentError::Archived);
        }
    }
    if let Some(node_id) = input.node_id {
        require_node(&tx, actor, node_id).await?;
    }
    let count: i64 = db::one(
        &tx,
        // Org-knowledge compile sessions are hidden and system-owned per job;
        // they never consume the owner's own session allowance (M2).
        "SELECT COUNT(*) AS n FROM neev_agent_sessions s WHERE org_id=$1 AND owner_id=$2 AND NOT EXISTS (SELECT 1 FROM neev_knowledge_jobs j WHERE j.org_id=s.org_id AND j.session_id=s.id)",
        vec![actor.org_id.into(), actor.identity.user_id.into()],
    )
    .await?
    .ok_or(ContentError::Storage)?
    .try_get("", "n")
    .map_err(database_error)?;
    if count >= MAX_SESSIONS_PER_OWNER {
        return Err(ContentError::Quota);
    }
    let now = now(state)?;
    let id = uuid::Uuid::new_v4().to_string();
    db::execute(
        &tx,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,effort,fast,visibility,kind,bound_chat_id,bound_kind,node_id,archived,revision,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'private','chat',$9,$10,$11,false,1,$12,$12)",
        vec![
            actor.org_id.into(),
            id.clone().into(),
            actor.identity.user_id.into(),
            title.clone().into(),
            provider.clone().into(),
            model.clone().into(),
            effort.clone().into(),
            fast.into(),
            input.bound_chat_id.clone().into(),
            input.bound_chat_id.as_ref().map(|_| "chat").into(),
            input.node_id.into(),
            now.into(),
        ],
    )
    .await?;
    resources::record_receipt(&tx, actor, "agent.session.create", &request, &hash, &id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(AgentSession {
        id,
        org_id: actor.org_id,
        owner_id: actor.identity.user_id,
        title,
        provider,
        model,
        effort,
        fast,
        kind: SessionKind::Chat,
        bound_chat_id: input.bound_chat_id,
        node_id: input.node_id,
        archived: false,
        revision: 1,
        created_at: now,
        updated_at: now,
    })
}

/// The owner's sessions, most recently updated first. Archived sessions are
/// excluded unless asked for, so the sidebar shows live work by default.
pub(crate) async fn list(
    state: &ServerState,
    actor: &OrgContext,
    include_archived: bool,
) -> Result<Vec<AgentSession>, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let filter = if include_archived {
        ""
    } else {
        " AND archived=false"
    };
    // Only saved Agent Chats are listed. A `team` session is an internal holder
    // for one chat's invocations and would be noise in the sidebar.
    let sql = format!(
        // An org-knowledge job's private compile session is not a chat the
        // admin started, so it is never listed (Amendment 1, E5).
        "SELECT {SESSION_COLUMNS} FROM neev_agent_sessions s WHERE org_id=$1 AND owner_id=$2 AND kind='chat'{filter} AND NOT EXISTS (SELECT 1 FROM neev_knowledge_jobs j WHERE j.org_id=s.org_id AND j.session_id=s.id) ORDER BY updated_at DESC,id LIMIT {MAX_SESSIONS_PER_OWNER}"
    );
    let rows = tx
        .query_all(db::statement(
            &tx,
            &sql,
            vec![actor.org_id.into(), actor.identity.user_id.into()],
        ))
        .await
        .map_err(database_error)?;
    let items = rows
        .iter()
        .map(session_row)
        .collect::<Result<Vec<_>, _>>()?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(items)
}

pub(crate) async fn get(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
) -> Result<AgentSession, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let found = owned(&tx, actor, id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(found)
}

/// `get` for a reader: the owner, or a member it is shared with (View). Only
/// the read endpoints use this; `get` stays the owner gate everywhere else.
pub(crate) async fn view(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
) -> Result<AgentSession, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let found = readable(&tx, actor, id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(found)
}

/// The session's turns in order. `after` is a sequence cursor, so a reload can
/// fetch the whole history and a live view can fetch only what it is missing.
pub(crate) async fn turns(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    after: Option<i64>,
) -> Result<crate::neev::content::types::Page<AgentTurn>, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let viewer = readable(&tx, actor, id).await?.owner_id != actor.identity.user_id;
    if after.is_some_and(|cursor| cursor < 0) {
        return Err(ContentError::Invalid);
    }
    let sql = format!(
        "SELECT {TURN_COLUMNS} FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2 AND sequence>$3 ORDER BY sequence LIMIT 101"
    );
    let rows = tx
        .query_all(db::statement(
            &tx,
            &sql,
            vec![actor.org_id.into(), id.into(), after.unwrap_or(0).into()],
        ))
        .await
        .map_err(database_error)?;
    let has_more = rows.len() > 100;
    let mut items = rows
        .iter()
        .take(100)
        .map(turn_row)
        .collect::<Result<Vec<_>, _>>()?;
    // A viewer sees what was said, never the owner's trace: tool activity can
    // hold their personal connectors' output and private context.
    if viewer {
        for turn in &mut items {
            turn.activity.clear();
        }
    }
    let next_after = if has_more {
        items.last().map(|turn| turn.sequence.to_string())
    } else {
        None
    };
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(crate::neev::content::types::Page { items, next_after })
}

/// Rename, (un)archive or re-file, guarded by the caller's expected revision so
/// two tabs cannot silently overwrite each other.
///
/// `node` names a tree node that must exist in the actor's org, checked inside
/// this transaction so a node cannot be deleted between the check and the write.
async fn mutate(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    expected_revision: i64,
    assignment: &str,
    value: sea_orm::Value,
    node: Option<i32>,
) -> Result<AgentSession, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let current = owned(&tx, actor, id).await?;
    if current.revision != expected_revision {
        return Err(ContentError::Conflict);
    }
    // Archived by a folder trash: frozen until that folder is restored, like a
    // batch resource, so it is neither unarchived nor re-filed from the trash.
    if matches!(assignment, "archived" | "node_id")
        && crate::neev::catalog::trash::session_trashed(&tx, actor.org_id, id).await?
    {
        return Err(ContentError::Archived);
    }
    // Archived on its own before its folder was trashed, so not in the batch:
    // it still may not come back while filed under a hidden node.
    if assignment == "archived" {
        if let Some(node_id) = current.node_id {
            if crate::neev::catalog::trash::node_trashed(&tx, actor.org_id, node_id).await? {
                return Err(ContentError::Archived);
            }
        }
    }
    if let Some(node_id) = node {
        require_node(&tx, actor, node_id).await?;
    }
    let now = now(state)?;
    let revision = current.revision.checked_add(1).ok_or(ContentError::Conflict)?;
    let changed = db::execute(
        &tx,
        &format!("UPDATE neev_agent_sessions SET {assignment}=$1,revision=$2,updated_at=$3 WHERE org_id=$4 AND id=$5 AND owner_id=$6 AND revision=$7"),
        vec![
            value,
            revision.into(),
            now.into(),
            actor.org_id.into(),
            id.into(),
            actor.identity.user_id.into(),
            expected_revision.into(),
        ],
    )
    .await?;
    if changed.rows_affected() != 1 {
        return Err(ContentError::Conflict);
    }
    let updated = owned(&tx, actor, id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(updated)
}

/// Change the session's reasoning effort.
///
/// Validated against what THIS session's provider accepts, because the CLI will
/// not do it: `claude` warns about an unknown level and runs at its default, so
/// an unchecked value would present as a working setting while changing
/// nothing. Refused rather than coerced to the nearest level — silently running
/// at an effort the reader did not pick is the same failure in a different coat.
/// Replace the session's model, effort and speed in ONE revision.
///
/// The whole triple is sent and validated together, so a caller can never leave
/// the session in a pairing no run could honour — Fast still on after moving to
/// a model that cannot do it — and two people changing settings at once cannot
/// interleave three separate bumps into a combination neither chose.
///
/// The provider is deliberately NOT changeable. A turn may never contradict its
/// session's provider (the schema validator proves it by join), and a team
/// holder's identity is derived from its provider, so switching it would
/// rewrite history that already ran. Changing provider means a new chat.
pub(crate) async fn set_settings(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    model_name: &str,
    effort: Option<&str>,
    fast: bool,
    expected_revision: i64,
) -> Result<AgentSession, ContentError> {
    let current = get(state, actor, id).await?;
    let model_name = model(model_name)?.to_owned();
    let (level, fast) = super::capabilities::validated_for(
        state,
        actor,
        &current.provider,
        &model_name,
        effort,
        fast,
    )
    .await
    .map_err(|_| ContentError::Invalid)?;
    let tx = resources::begin(state, actor).await?;
    let held = owned(&tx, actor, id).await?;
    if held.revision != expected_revision {
        return Err(ContentError::Conflict);
    }
    if held.archived {
        return Err(ContentError::Archived);
    }
    let now = now(state)?;
    let revision = held
        .revision
        .checked_add(1)
        .ok_or(ContentError::Conflict)?;
    let changed = db::execute(
        &tx,
        "UPDATE neev_agent_sessions SET model=$1,effort=$2,fast=$3,revision=$4,updated_at=$5 WHERE org_id=$6 AND id=$7 AND owner_id=$8 AND revision=$9",
        vec![
            model_name.clone().into(),
            level.into(),
            fast.into(),
            revision.into(),
            now.into(),
            actor.org_id.into(),
            id.into(),
            actor.identity.user_id.into(),
            expected_revision.into(),
        ],
    )
    .await?;
    if changed.rows_affected() != 1 {
        return Err(ContentError::Conflict);
    }
    let updated = owned(&tx, actor, id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(updated)
}

/// Change the session's fast mode.
///
/// Validated against the session's CURRENT model, so turning Fast on beside a
/// model that cannot run it is refused here rather than discovered as a failed
/// run — or worse, quietly served by a different model.
pub(crate) async fn set_fast(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    fast: bool,
    expected_revision: i64,
) -> Result<AgentSession, ContentError> {
    let current = get(state, actor, id).await?;
    let (_, validated) = super::capabilities::validated_for(
        state,
        actor,
        &current.provider,
        &current.model,
        current.effort.as_deref(),
        fast,
    )
    .await
    .map_err(|_| ContentError::Invalid)?;
    mutate(
        state,
        actor,
        id,
        expected_revision,
        "fast",
        validated.into(),
        None,
    )
    .await
}

pub(crate) async fn set_effort(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    effort: Option<&str>,
    expected_revision: i64,
) -> Result<AgentSession, ContentError> {
    let current = get(state, actor, id).await?;
    // Against the session's CURRENT model and the caller's own catalogue, not
    // just the provider: otherwise this route stores a level the chosen model
    // rejects and the run fails later, or refuses one the account advertises.
    let (validated, _) = super::capabilities::validated_for(
        state,
        actor,
        &current.provider,
        &current.model,
        effort,
        current.fast,
    )
    .await
    .map_err(|_| ContentError::Invalid)?;
    mutate(
        state,
        actor,
        id,
        expected_revision,
        "effort",
        validated.into(),
        // Not a placement change: the session stays where it is filed.
        None,
    )
    .await
}

pub(crate) async fn rename(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    new_title: &str,
    expected_revision: i64,
) -> Result<AgentSession, ContentError> {
    let new_title = title(new_title)?;
    mutate(
        state,
        actor,
        id,
        expected_revision,
        "title",
        new_title.into(),
        None,
    )
    .await
}

/// Whether `node_id` is a node in the ACTOR'S OWN org.
///
/// The org predicate is the whole point: node ids are globally sequential, so
/// without it a valid id from another tenant would file a session against that
/// tenant's project. Node visibility is org membership — `neev_nodes` has no
/// owner column — so membership, which `resources::begin` already established, is
/// the correct and only ACL here. Every node kind is allowed; where a session may
/// sit in the tree is the workspace's decision, not this layer's.
async fn require_node(
    tx: &sea_orm::DatabaseTransaction,
    actor: &OrgContext,
    node_id: i32,
) -> Result<(), ContentError> {
    if node_id <= 0 {
        return Err(ContentError::Invalid);
    }
    db::one(
        tx,
        "SELECT 1 AS present FROM neev_nodes WHERE org_id=$1 AND id=$2",
        vec![actor.org_id.into(), node_id.into()],
    )
    .await?
    .ok_or(ContentError::NotFound)?;
    // A project or folder in the trash (schema 23) is hidden: nothing is filed
    // into it, exactly as no resource may be created in or moved into it.
    if crate::neev::catalog::trash::node_trashed(tx, actor.org_id, node_id).await? {
        return Err(ContentError::NotFound);
    }
    // Restricted folders (schema 24): filing needs a place the actor may put
    // things — a hidden folder is missing, a View-only one is refused.
    crate::neev::content::resources::placeable(tx, actor, node_id).await?;
    Ok(())
}

/// Whether `chat_id` sits on a project or folder that is in the trash.
async fn chat_in_trash(
    tx: &sea_orm::DatabaseTransaction,
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

/// New agent work is refused for a session filed under a trashed project or
/// folder, or bound to a chat that lies in one. Enforced here, not in the UI,
/// so a direct API call cannot start a run inside the trash. Reading the
/// session and its history is unaffected; restore makes it runnable again.
async fn require_live_placement(
    tx: &sea_orm::DatabaseTransaction,
    actor: &OrgContext,
    found: &AgentSession,
) -> Result<(), ContentError> {
    if let Some(node_id) = found.node_id {
        if crate::neev::catalog::trash::node_trashed(tx, actor.org_id, node_id).await? {
            return Err(ContentError::Archived);
        }
    }
    if let Some(chat_id) = found.bound_chat_id.as_deref() {
        if chat_in_trash(tx, actor.org_id, chat_id).await? {
            return Err(ContentError::Archived);
        }
    }
    Ok(())
}

/// File a saved session under a node, or unfile it with `None`.
///
/// Placement is stored rather than kept per browser, so the session appears in
/// the same project on the user's other devices. The session's own ACL is
/// untouched: it stays private to its owner, and filing it under a node that
/// other members can see does not make the session visible to them.
pub(crate) async fn move_to_node(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    node_id: Option<i32>,
    expected_revision: i64,
) -> Result<AgentSession, ContentError> {
    mutate(
        state,
        actor,
        id,
        expected_revision,
        "node_id",
        node_id.into(),
        node_id,
    )
    .await
}

/// Unfile every session pointing at `node_id`, for a transaction that is about to
/// delete or archive that node.
///
/// Node deletion does not exist yet; when it does it MUST call this in its own
/// transaction, before removing the node. Unfiling rather than refusing is what
/// keeps someone else's private session from either blocking the delete or
/// revealing, by blocking it, that a session was filed there at all. Returns
/// nothing for the same reason: a count would leak how many private sessions
/// other members had under that node.
pub(crate) async fn clear_node_pointers(
    tx: &sea_orm::DatabaseTransaction,
    org_id: i32,
    node_id: i32,
) -> Result<(), ContentError> {
    db::execute(
        tx,
        "UPDATE neev_agent_sessions SET node_id=NULL WHERE org_id=$1 AND node_id=$2",
        vec![org_id.into(), node_id.into()],
    )
    .await?;
    Ok(())
}

pub(crate) async fn set_archived(
    state: &ServerState,
    actor: &OrgContext,
    id: &str,
    archived: bool,
    expected_revision: i64,
) -> Result<AgentSession, ContentError> {
    mutate(
        state,
        actor,
        id,
        expected_revision,
        "archived",
        archived.into(),
        None,
    )
    .await
}

/// Write the prompt as a live turn and hand back the durable run id. The caller
/// starts the runtime *after* this commits, so a refusal still has a row to
/// settle (see [`finish`]).
/// `request_id`, when given, makes the whole turn idempotent: a replay — whether
/// it races the first attempt or arrives long after it finished — resolves to
/// the SAME turn and run id, so the provider is never asked to execute the same
/// ask twice and no second reply can be published. `None` is for saved-session
/// turns, where the client already owns the turn it just created.
pub(crate) async fn begin_turn(
    state: &ServerState,
    actor: &OrgContext,
    session_id: &str,
    text: &str,
    request_id: Option<&str>,
) -> Result<(AgentTurn, bool), ContentError> {
    begin_turn_with_attachments(state, actor, session_id, text, request_id, &[]).await
}

/// Prove every key was uploaded to THIS session before it is bound to the turn.
///
/// An Agent Chat is private to its owner, so an attachment that came from
/// somewhere else would be a file crossing into a conversation it was never
/// uploaded to. The check shares the turn's transaction, so a refusal takes the
/// turn with it rather than leaving a run pointing at files it may not have.
async fn owned_attachments(
    tx: &sea_orm::DatabaseTransaction,
    actor: &OrgContext,
    session_id: &str,
    keys: &[String],
) -> Result<(), ContentError> {
    if keys.len() > crate::neev::content::attachment::MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(ContentError::Invalid);
    }
    let mut seen = std::collections::BTreeSet::new();
    for key in keys {
        if !seen.insert(key.as_str()) {
            return Err(ContentError::Invalid);
        }
        let row = db::one(
            tx,
            "SELECT session_id FROM neev_attachments WHERE org_id=$1 AND key=$2",
            vec![actor.org_id.into(), key.clone().into()],
        )
        .await?
        .ok_or(ContentError::Invalid)?;
        let owner: Option<String> = row.try_get("", "session_id").map_err(database_error)?;
        if owner.as_deref() != Some(session_id) {
            return Err(ContentError::Invalid);
        }
        // No aggregate byte ceiling for the turn: the count cap bounds how many
        // files a run stages, and the tenant storage quota already bounded each
        // one at upload. A turn that stored its files must be executable.
    }
    Ok(())
}

/// The attachment keys already recorded against a turn, in stored order.
async fn recorded_attachments(
    tx: &sea_orm::DatabaseTransaction,
    actor: &OrgContext,
    session_id: &str,
    sequence: i64,
) -> Result<Vec<String>, ContentError> {
    let rows = tx
        .query_all(db::statement(
            tx,
            "SELECT attachment_key FROM neev_attachment_refs WHERE org_id=$1 AND turn_session_id=$2 AND turn_sequence=$3 ORDER BY position",
            vec![actor.org_id.into(), session_id.to_owned().into(), sequence.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut keys = Vec::with_capacity(rows.len());
    for row in &rows {
        keys.push(row.try_get("", "attachment_key").map_err(database_error)?);
    }
    Ok(keys)
}

/// Begin a turn that carries uploaded files.
///
/// The references are written in the SAME transaction as the turn row, so the
/// caller can read them back and know the run it is about to start is described
/// by durable storage rather than by the request that asked for it.
/// The selection an Auto turn runs on, already validated against the
/// caller's catalogue, and the session revision it was decided against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RoutedSettings {
    pub(crate) model: String,
    pub(crate) effort: Option<String>,
    pub(crate) fast: bool,
    pub(crate) expected_revision: i64,
}

pub(crate) async fn begin_turn_with_attachments(
    state: &ServerState,
    actor: &OrgContext,
    session_id: &str,
    text: &str,
    request_id: Option<&str>,
    attachment_keys: &[String],
) -> Result<(AgentTurn, bool), ContentError> {
    begin(state, actor, session_id, text, request_id, attachment_keys, None, None).await
}

/// Begin a team ask on one of the invoker's holders for `chat_id`, idempotent
/// by `request_id` across ALL of their holders for that chat (see `begin`):
/// whichever provider an earlier attempt ran on, the same request is that turn.
pub(crate) async fn begin_team_turn(
    state: &ServerState,
    actor: &OrgContext,
    chat_id: &str,
    session_id: &str,
    text: &str,
    request_id: &str,
) -> Result<(AgentTurn, bool), ContentError> {
    begin(state, actor, session_id, text, Some(request_id), &[], None, Some(chat_id)).await
}

/// The invoker's holder ids for one chat: one per provider, deterministic.
fn team_holder_ids(actor: &OrgContext, chat_id: &str) -> Vec<String> {
    super::types::Provider::ALL
        .iter()
        .map(|provider| team_session_id(actor.org_id, actor.identity.user_id, chat_id, provider.name()))
        .collect()
}

/// A turn for `request_id` on one of the invoker's OTHER holders for the chat.
async fn sibling_turn_by_request<C: ConnectionTrait>(
    db: &C,
    actor: &OrgContext,
    chat_id: &str,
    session_id: &str,
    request_id: &str,
) -> Result<Option<AgentTurn>, ContentError> {
    for id in team_holder_ids(actor, chat_id) {
        if id == session_id {
            continue;
        }
        if let Some(turn) = turn_by_request(db, actor.org_id, &id, request_id).await? {
            return Ok(Some(turn));
        }
    }
    Ok(None)
}

/// The turn a team ask with this `request_id` already created, on any of the
/// invoker's holders for the chat — looked up BEFORE routing, so a retry
/// replays its first attempt instead of being decided (and run) again.
pub(crate) async fn team_turn_by_request(
    state: &ServerState,
    actor: &OrgContext,
    chat_id: &str,
    request_id: &str,
) -> Result<Option<AgentTurn>, ContentError> {
    let request_id = org::normalize_request_id(request_id).map_err(|_| ContentError::Invalid)?;
    let tx = resources::begin(state, actor).await?;
    let found = sibling_turn_by_request(&tx, actor, chat_id, "", &request_id).await?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(found)
}

/// The invoker's most recently used holder for this chat, if any: what an Auto
/// follow-up continues (its provider is fixed, like an Agent Chat's). "Used"
/// means it has the newest TURN — a holder whose settings were merely written
/// by an attempt that then replayed elsewhere does not count.
pub(crate) async fn latest_team_session(
    state: &ServerState,
    actor: &OrgContext,
    chat_id: &str,
) -> Result<Option<AgentSession>, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let row = db::one(
        &tx,
        &format!(
            "SELECT {SESSION_COLUMNS} FROM neev_agent_sessions s WHERE org_id=$1 AND owner_id=$2 AND kind='team' AND bound_chat_id=$3 AND archived=false ORDER BY COALESCE((SELECT MAX(t.created_at) FROM neev_agent_turns t WHERE t.org_id=s.org_id AND t.session_id=s.id),0) DESC,updated_at DESC,id LIMIT 1"
        ),
        vec![actor.org_id.into(), actor.identity.user_id.into(), chat_id.into()],
    )
    .await?;
    let found = row.map(|row| session_row(&row)).transpose()?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    // Only a holder whose id is the deterministic one for its provider counts.
    Ok(found.filter(|session| {
        session.id == team_session_id(actor.org_id, actor.identity.user_id, chat_id, &session.provider)
    }))
}

/// Begin an Auto turn: the routed selection becomes the session's settings in
/// the SAME transaction as the turn row, so the session always says what its
/// latest turn ran on, a retry re-runs that choice, and the next brief
/// follow-up keeps it. Refused with `Conflict` when the session changed since
/// routing read it — a concurrent manual change wins rather than being
/// overwritten by a decision made against the old state.
pub(crate) async fn begin_routed_turn(
    state: &ServerState,
    actor: &OrgContext,
    session_id: &str,
    text: &str,
    attachment_keys: &[String],
    routed: &RoutedSettings,
) -> Result<(AgentTurn, bool), ContentError> {
    model(&routed.model)?;
    begin(state, actor, session_id, text, None, attachment_keys, Some(routed), None).await
}

#[allow(clippy::too_many_arguments)]
async fn begin(
    state: &ServerState,
    actor: &OrgContext,
    session_id: &str,
    text: &str,
    request_id: Option<&str>,
    attachment_keys: &[String],
    routed: Option<&RoutedSettings>,
    team_chat: Option<&str>,
) -> Result<(AgentTurn, bool), ContentError> {
    let text = prompt(text)?;
    let request_id = request_id
        .map(|value| org::normalize_request_id(value).map_err(|_| ContentError::Invalid))
        .transpose()?;
    let tx = resources::begin(state, actor).await?;
    let found = owned(&tx, actor, session_id).await?;
    // A replay is answered BEFORE the archived/live-turn guards: retrying a
    // request that already succeeded must return its result, not a late
    // objection about the session's current state.
    if let Some(request) = request_id.as_deref() {
        if let Some(existing) = turn_by_request(&tx, actor.org_id, session_id, request).await? {
            // A replay must mean the same ask, files included: a retry that
            // swapped them is a conflict, not a replay. (The prompt itself is
            // already pinned upstream — the only caller that passes a request
            // id hashes it into the message receipt first.)
            if recorded_attachments(&tx, actor, session_id, existing.sequence).await?
                != attachment_keys
            {
                return Err(ContentError::Conflict);
            }
            session::recheck(&tx, actor, state.clock.now_unix()).await?;
            tx.commit().await.map_err(database_error)?;
            // `false` = this caller did NOT create the turn, so it must not
            // execute anything. Deciding that here, inside the transaction that
            // the unique `(org_id, session_id, request_id)` index arbitrates, is
            // what makes two simultaneous retries safe: exactly one caller can
            // ever be told it created the turn.
            return Ok((existing, false));
        }
        // A team ask may run on whichever provider Auto chose, so the same
        // request can arrive at a DIFFERENT holder of this chat (a retry that
        // routed elsewhere, or two first attempts racing). Answered here, in the
        // transaction the global admission guard serializes, so exactly one
        // attempt ever creates the turn and every other replays it.
        if let Some(chat_id) = team_chat {
            if let Some(existing) =
                sibling_turn_by_request(&tx, actor, chat_id, session_id, request).await?
            {
                session::recheck(&tx, actor, state.clock.now_unix()).await?;
                tx.commit().await.map_err(database_error)?;
                return Ok((existing, false));
            }
        }
    }
    // After the replay answer, so a retry of a turn that already exists still
    // returns it (without executing) once its folder is trashed.
    require_live_placement(&tx, actor, &found).await?;
    if found.archived {
        return Err(ContentError::Archived);
    }
    // One live turn per session: a second concurrent run in the same session
    // would interleave two replies into one ordered history.
    if let Some(live) = live_turn(&tx, actor.org_id, session_id).await? {
        let _ = live;
        return Err(ContentError::Conflict);
    }
    if let Some(routed) = routed {
        if found.revision != routed.expected_revision {
            return Err(ContentError::Conflict);
        }
    }
    let run_model = routed.map_or_else(|| found.model.clone(), |routed| routed.model.clone());
    let sequence: i64 = db::one(
        &tx,
        "SELECT COALESCE(MAX(sequence),0) AS n FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2",
        vec![actor.org_id.into(), session_id.into()],
    )
    .await?
    .ok_or(ContentError::Storage)?
    .try_get::<i64>("", "n")
    .map_err(database_error)?;
    if sequence >= MAX_TURNS_PER_SESSION {
        return Err(ContentError::Quota);
    }
    let sequence = sequence.checked_add(1).ok_or(ContentError::Conflict)?;
    let now = now(state)?;
    let id = uuid::Uuid::new_v4().to_string();
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    db::execute(
        &tx,
        "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,request_id,prompt,response,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,$3,$4,$5,$6,$7,'','running',NULL,$8,$9,$10,$10,NULL)",
        vec![
            actor.org_id.into(),
            session_id.into(),
            sequence.into(),
            id.clone().into(),
            run_id.clone().into(),
            request_id.clone().into(),
            text.into(),
            found.provider.clone().into(),
            run_model.clone().into(),
            now.into(),
        ],
    )
    .await?;
    // Atomic with the turn: the row and the files it carries commit together,
    // or neither does.
    owned_attachments(&tx, actor, session_id, attachment_keys).await?;
    crate::neev::content::attachment::link_to_turn(
        &tx,
        actor,
        session_id,
        sequence,
        attachment_keys,
    )
    .await?;
    // The session's own timestamp/revision move so the sidebar reorders and a
    // concurrent rename is detected.
    match routed {
        None => {
            db::execute(
                &tx,
                "UPDATE neev_agent_sessions SET revision=revision+1,updated_at=$1 WHERE org_id=$2 AND id=$3 AND owner_id=$4",
                vec![
                    now.into(),
                    actor.org_id.into(),
                    session_id.into(),
                    actor.identity.user_id.into(),
                ],
            )
            .await?;
        }
        Some(routed) => {
            let changed = db::execute(
                &tx,
                "UPDATE neev_agent_sessions SET model=$1,effort=$2,fast=$3,revision=revision+1,updated_at=$4 WHERE org_id=$5 AND id=$6 AND owner_id=$7 AND revision=$8",
                vec![
                    routed.model.clone().into(),
                    routed.effort.clone().into(),
                    routed.fast.into(),
                    now.into(),
                    actor.org_id.into(),
                    session_id.into(),
                    actor.identity.user_id.into(),
                    routed.expected_revision.into(),
                ],
            )
            .await?;
            if changed.rows_affected() != 1 {
                return Err(ContentError::Conflict);
            }
        }
    }
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok((AgentTurn {
        id,
        session_id: session_id.to_owned(),
        sequence,
        run_id,
        prompt: text.to_owned(),
        response: String::new(),
        // A turn records its trace as it runs; none exists at the moment the
        // prompt becomes durable.
        activity: Vec::new(),
        status: TurnStatus::Running,
        error_code: None,
        provider: found.provider,
        model: run_model,
        created_at: now,
        updated_at: now,
        completed_at: None,
    }, true))
}

/// How many earlier turns may be replayed to the provider as context.
const CONTEXT_TURNS: usize = 12;
/// Total characters of replayed context. The newest turns are kept; older ones
/// are dropped rather than truncated mid-turn, so the provider never sees half
/// an exchange.
const CONTEXT_CHARS: usize = 24_000;

/// Compose what the provider actually receives for this turn: the session's
/// recent completed exchanges followed by the new prompt. This is what makes a
/// session multi-turn rather than a series of unrelated one-shots.
///
/// Only *completed* turns are replayed — a failed or canceled turn produced no
/// trustworthy answer, and replaying its partial text would invent context. The
/// composed transcript is never stored: the turn row keeps the user's own
/// prompt, so the history a user reads is the history they wrote.
pub(crate) fn conversation_prompt(history: &[AgentTurn], next: &str) -> String {
    let mut exchanges: Vec<String> = Vec::new();
    let mut budget = CONTEXT_CHARS;
    for turn in history
        .iter()
        .rev()
        .filter(|turn| turn.status == TurnStatus::Completed && !turn.response.is_empty())
        .take(CONTEXT_TURNS)
    {
        let block = format!("User: {}\n\nAssistant: {}", turn.prompt, turn.response);
        let cost = block.chars().count();
        if cost > budget {
            break;
        }
        budget -= cost;
        exchanges.push(block);
    }
    if exchanges.is_empty() {
        return next.to_owned();
    }
    exchanges.reverse();
    format!("{}\n\nUser: {}", exchanges.join("\n\n"), next)
}

/// The recent turns used to build [`conversation_prompt`], newest last.
pub(crate) async fn recent_turns(
    state: &ServerState,
    actor: &OrgContext,
    session_id: &str,
) -> Result<Vec<AgentTurn>, ContentError> {
    let tx = resources::begin(state, actor).await?;
    owned(&tx, actor, session_id).await?;
    let sql = format!(
        "SELECT {TURN_COLUMNS} FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2 AND status='completed' ORDER BY sequence DESC LIMIT {CONTEXT_TURNS}"
    );
    let rows = tx
        .query_all(db::statement(
            &tx,
            &sql,
            vec![actor.org_id.into(), session_id.into()],
        ))
        .await
        .map_err(database_error)?;
    let mut items = rows
        .iter()
        .map(turn_row)
        .collect::<Result<Vec<_>, _>>()?;
    items.reverse();
    // Context-only copies: keep accepted follow-ups in subsequent prompts.
    // Stored turn responses remain untouched for rendering and export.
    for turn in &mut items {
        let inputs = db::list_steer_inputs(&tx, actor.org_id, session_id, turn.sequence).await?;
        if !inputs.is_empty() {
            let chars: Vec<char> = turn.response.chars().collect();
            let mut response = String::new();
            let mut offset = 0;
            for input in inputs {
                let cut = (input.response_offset.unwrap_or(0).max(0) as usize).min(chars.len()).max(offset);
                response.extend(chars[offset..cut].iter());
                response.push_str("\n\nUser: ");
                response.push_str(&input.text);
                response.push_str("\n\nAssistant: ");
                offset = cut;
            }
            response.extend(chars[offset..].iter());
            turn.response = response;
        }
    }
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(items)
}

/// The turn a `(session, request_id)` already produced, if any. The unique
/// index on `(org_id, session_id, request_id)` means this is the only turn a
/// replay can resolve to.
async fn turn_by_request<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    session_id: &str,
    request_id: &str,
) -> Result<Option<AgentTurn>, ContentError> {
    db::one(
        db,
        &format!("SELECT {TURN_COLUMNS} FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2 AND request_id=$3"),
        vec![org_id.into(), session_id.into(), request_id.into()],
    )
    .await?
    .map(|row| turn_row(&row))
    .transpose()
}

async fn live_turn<C: ConnectionTrait>(
    db: &C,
    org_id: i32,
    session_id: &str,
) -> Result<Option<String>, ContentError> {
    Ok(db::one(
        db,
        "SELECT run_id FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2 AND status='running' LIMIT 1",
        vec![org_id.into(), session_id.into()],
    )
    .await?
    .map(|row| row.try_get::<String>("", "run_id"))
    .transpose()
    .map_err(database_error)?)
}

/// The turn a run id names, for reattach. Ownership is proved through the
/// session join, so a run id leaked to another user resolves to `NotFound`.
pub(crate) async fn turn_by_run(
    state: &ServerState,
    actor: &OrgContext,
    run_id: &str,
) -> Result<AgentTurn, ContentError> {
    let tx = resources::begin(state, actor).await?;
    let row = db::one(
        &tx,
        &format!("SELECT {} FROM neev_agent_turns t WHERE t.org_id=$1 AND t.run_id=$2 AND EXISTS(SELECT 1 FROM neev_agent_sessions s WHERE s.org_id=t.org_id AND s.id=t.session_id AND s.owner_id=$3)", TURN_COLUMNS.split(',').map(|c| format!("t.{c}")).collect::<Vec<_>>().join(",")),
        vec![
            actor.org_id.into(),
            run_id.into(),
            actor.identity.user_id.into(),
        ],
    )
    .await?
    .ok_or(ContentError::NotFound)?;
    let turn = turn_row(&row)?;
    session::recheck(&tx, actor, state.clock.now_unix()).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(turn)
}

/// The turn a run id names, without an actor. For the run driver and for the
/// stream's terminal read — both of which happen *after* the request that
/// proved authorization, and neither of which may re-run a session check
/// (the run is server-owned and must settle even if the browser left).
pub(crate) async fn turn_by_run_id(
    database: &DatabaseConnection,
    run_id: &str,
) -> Result<Option<AgentTurn>, ContentError> {
    db::one(
        database,
        &format!("SELECT {TURN_COLUMNS} FROM neev_agent_turns WHERE run_id=$1"),
        vec![run_id.into()],
    )
    .await?
    .map(|row| turn_row(&row))
    .transpose()
}

/// Persist the accumulated output of a live run. Called by the run driver, not
/// by a request, so it takes the raw connection and carries no session recheck:
/// the run is server-owned and must finish recording even if the browser left.
pub(crate) async fn persist_response(
    database: &DatabaseConnection,
    run_id: &str,
    response: &str,
    now: i64,
) -> Result<(), ContentError> {
    let response = clamp(response);
    db::execute(
        database,
        "UPDATE neev_agent_turns SET response=$1,updated_at=$2 WHERE run_id=$3 AND status='running'",
        vec![response.into(), now.into(), run_id.into()],
    )
    .await?;
    Ok(())
}

/// Settle a run. Idempotent: only a `running` row is moved, so a late cancel
/// after a completion cannot rewrite history.
pub(crate) async fn finish(
    database: &DatabaseConnection,
    run_id: &str,
    response: &str,
    activity: Option<&str>,
    status: TurnStatus,
    error_code: Option<&str>,
    now: i64,
) -> Result<(), ContentError> {
    if status == TurnStatus::Running {
        return Err(ContentError::Invalid);
    }
    // The schema pairs a terminal status with an error code exactly when it is
    // a failure; keep the two in step here rather than trusting the caller.
    let error_code = match status {
        TurnStatus::Completed => None,
        TurnStatus::Canceled => Some(error_code.unwrap_or("canceled")),
        _ => Some(error_code.unwrap_or("unavailable")),
    };
    db::execute(
        database,
        "UPDATE neev_agent_turns SET response=$1,activity=$2,status=$3,error_code=$4,updated_at=$5,completed_at=$5 WHERE run_id=$6 AND status='running'",
        vec![
            clamp(response).into(),
            // Oversized activity stores as NULL rather than failing the write:
            // losing the decoration is recoverable, losing the reply is not.
            activity
                .filter(|text| text.len() <= super::activity::MAX_ACTIVITY_CHARS)
                .map(str::to_owned)
                .into(),
            status.name().into(),
            error_code.into(),
            now.into(),
            run_id.into(),
        ],
    )
    .await?;
    Ok(())
}

/// Truncate to the stored cap on a character boundary. The driver stops a run
/// that reaches the cap, so this is a last-resort guard rather than the normal
/// path — but it must never let an oversized value reach the CHECK.
fn clamp(value: &str) -> String {
    if value.chars().count() <= MAX_RESPONSE_CHARS {
        return value.to_owned();
    }
    value.chars().take(MAX_RESPONSE_CHARS).collect()
}

/// Settle every turn left `running` by a previous process. Run once at startup,
/// before the HTTP surface accepts traffic: the streams those turns belonged to
/// died with their process, so leaving them live would show a permanently
/// spinning reply that can never resolve.
pub(crate) async fn reconcile_interrupted(
    database: &DatabaseConnection,
    now: i64,
) -> Result<u64, ContentError> {
    use crate::neev::agents::runs::supervision::settle_interrupted_review;
    let mut settled = 0u64;
    // A supervised turn whose EXECUTION completed and was made durable before
    // its review began (`runs::persist_progress`). Only the review was cut
    // off, so the turn is the completed answer it already was, with the review
    // step saying it was interrupted. The LIKE is a cheap prefilter; the
    // decision is made on the parsed trace.
    let rows = database
        .query_all(db::statement(
            database,
            "SELECT run_id, activity FROM neev_agent_turns WHERE status='running' AND activity LIKE '%review_pending%'",
            vec![],
        ))
        .await
        .map_err(database_error)?;
    for row in rows {
        let Ok(run_id) = row.try_get::<String>("", "run_id") else {
            continue;
        };
        let activity: Option<String> = row.try_get("", "activity").ok().flatten();
        if let Some(trace) = settle_interrupted_review(activity.as_deref()) {
            let changed = db::execute(
                database,
                "UPDATE neev_agent_turns SET status='completed',error_code=NULL,activity=$1,updated_at=$2,completed_at=$2 WHERE run_id=$3 AND status='running'",
                vec![trace.into(), now.into(), run_id.into()],
            )
            .await?;
            settled += changed.rows_affected();
        }
    }
    // Everything else was interrupted mid-plan or mid-execution, as before.
    let changed = db::execute(
        database,
        "UPDATE neev_agent_turns SET status='failed',error_code='interrupted',updated_at=$1,completed_at=$1 WHERE status='running'",
        vec![now.into()],
    )
    .await?;
    Ok(settled + changed.rows_affected())
}

/// Map an execution refusal to the durable error code stored on the turn.
pub(crate) fn execution_error_code(
    error: &crate::neev::agents::execution::ExecutionError,
) -> &'static str {
    use crate::neev::agents::execution::ExecutionError;
    match error {
        ExecutionError::Disabled => "disabled",
        ExecutionError::Unauthorized => "unauthorized",
        ExecutionError::Isolation => "isolation",
        ExecutionError::Provider => "provider",
        ExecutionError::Unavailable | ExecutionError::LanesBusy => "unavailable",
        ExecutionError::Unsupported => "unsupported",
    }
}

/// Map a streamed failure reason to a stored code. Unknown reasons become
/// `provider` rather than being invented into the domain.
pub(crate) fn failure_code(reason: &str) -> (TurnStatus, &'static str) {
    match reason {
        "canceled" => (TurnStatus::Canceled, "canceled"),
        "io" => (TurnStatus::Failed, "io"),
        _ => (TurnStatus::Failed, "provider"),
    }
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod sessions_tests;
#[cfg(test)]
#[path = "sessions_trash_tests.rs"]
mod sessions_trash_tests;

/// The HTTP layer's cursor parsing: sequences travel as strings so a 64-bit
/// value survives JSON, exactly like the message/revision cursors.
pub(crate) fn parse_cursor(value: &str) -> Result<i64, ContentError> {
    value.parse::<i64>().map_err(|_| ContentError::Invalid)
}
