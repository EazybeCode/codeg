# Neev Auto model routing (TypeSafe Jev): operator guide

Auto lets a person's Agent Chat pick which of **their own connected agents**
runs a turn, and with which model and reasoning effort. They can still choose
the agent and model by hand.

## Roles

- **TypeSafe Jev** makes the decision. It is a typed decision API
  (`POST https://api.typesafe.ai/v1/systemone`), not a chat model.
- **The person's own subscription-backed CLIs** (Claude Code, Codex, …) do all
  the work.
- **Neev never proxies or resells model access.** There is no OpenRouter
  gateway, and no subscription credential is ever sent to TypeSafe.

## Current status

- **No TypeSafe key is configured in production.** Until one is, Auto stays
  off, or shows as "key missing" if the provider variable is set. Every turn
  runs on the manual selection.
- **Supervision is off by default.** When an operator turns it on, complex
  Auto turns are planned and/or reviewed, without tools, by Claude on the
  person's own Claude connection. See "Supervision of complex Auto turns"
  below. A review is a model's reading of the reply text. It is never a
  verification, and the UI says so.

## Configuration

Set these on the Neev server only. The browser never sees the key.

| Variable | Values | Default |
|---|---|---|
| `NEEV_ROUTER_PROVIDER` | absent, or `jev` | absent = Auto off and hidden |
| `NEEV_TYPESAFE_API_KEY` | the TypeSafe API key | absent = `missing_credential` |
| `NEEV_ROUTER_JEV_MODEL` | `jev-1.13.0` or `jev-latest` | `jev-1.13.0` (pinned) |
| `NEEV_ROUTER_TIMEOUT_MS` | 500–10000 | 3000 |
| `NEEV_ROUTER_MIN_CONFIDENCE` | 0–1 | 0.3 |

### Supervision (optional; off unless set)

| Variable | Values | Default |
|---|---|---|
| `NEEV_SUPERVISION` | `off`, `review`, `plan_review` | `off` |
| `NEEV_SUPERVISOR_MODEL` | a Claude model id from the Claude catalogue | `opus` |
| `NEEV_SUPERVISOR_EFFORT` | a Claude effort level, or empty for the CLI default | `high` |
| `NEEV_SUPERVISION_MIN_COMPLEXITY` | 0–3 (Trivial, Simple, Substantial, Hard) | `2` |
| `NEEV_SUPERVISION_PLAN_TIMEOUT_MS` | 5000–180000 | 60000 |
| `NEEV_SUPERVISION_REVIEW_TIMEOUT_MS` | 5000–180000 | 90000 |

**Invalid values.** Any other value for these variables stops startup with a
provider configuration error. A typo never silently turns routing on or off.
An empty or absent key with `NEEV_ROUTER_PROVIDER=jev` does not stop startup.
It is reported as `missing_credential`:

- `GET /api/neev/agent/routing` answers `{"state":"missing_credential"}`;
- the Auto chip shows as unavailable, with the reason;
- every turn uses the manual selection.

Neev never reports Jev as ready without a key.

**Why the model is pinned.** The default is the pinned `jev-1.13.0`, as
TypeSafe's model docs advise once thresholds are tuned. `jev-latest` moves with
TypeSafe's releases.

**Egress.** The Neev server process, not the agent sandbox, must be able to
reach `api.typesafe.ai:443`. Redirects are never followed, so the bearer key is
only ever sent to that host.

### Enabling (when a key exists)

1. Store the key in the deployment's secret store, the same way as
   `NEEV_RESEND_API_KEY`, and set `NEEV_TYPESAFE_API_KEY` from it. Do not put
   it in a committed file.
2. Set `NEEV_ROUTER_PROVIDER=jev`.
3. Restart Neev.
4. Check `GET /api/neev/agent/routing` while signed in. Expected:
   `{"state":"ready","router":"jev","maxTaskChars":6000}`.
5. Send one Auto turn in a test chat and confirm the route line under the
   composer, for example "Auto: Opus · high".

   If it says "Auto used your selection … (request refused)", TypeSafe rejected
   the request (401, 403 or 422): check the key and the model. "router
   unavailable" means 429, 529 or 5xx, which is transient.

**Disabling.** Unset `NEEV_ROUTER_PROVIDER` and restart. No data changes: there
is no migration, and the schema stays at 22.

## What is sent to TypeSafe

Each decision is one request with:

- the new message, at most 6,000 characters (cut off after that);
- for a follow-up, the previous request and reply, at most 1,000 characters
  each;
- the number of attached files and earlier turns;
- the list of options, as agent and model names from the person's own
  catalogue.

**Never sent:**

- identity, organisation, credentials or subscription tokens;
- file contents;
- the full history.

The Auto chip's tooltip tells people that their message is sent to TypeSafe.
TypeSafe documents that it does not train on requests. See their Legal page for
the DPA and zero data retention.

**Cost.** Jev charges per input token: $0.042 per million (`jev-1.13.0`, docs
fetched 2026-10-03). A routing request is a few hundred to a few thousand
tokens.

**No cost-saving claim.** Auto makes no cost-saving guarantee for subscription
plans. It chooses a suitable model; the subscription's own limits still apply.

## How a decision is made and checked

### 1. The allowlist

The candidates are every catalogued model of every connected provider (for a
new chat), or of the chat's own provider (for later turns: a chat's provider
never changes). The list is limited to:

- 16 models per provider and 32 in total;
- each model's own allowed effort levels.

### 2. The questions

Jev is asked two typed questions:

- a **Choice** over those options;
- a **Score** of task complexity, on four levels.

### 3. Validation

The answer must name an option that was offered and have finite numbers in
range. The Choice confidence must be at least `NEEV_ROUTER_MIN_CONFIDENCE`.

### 4. Effort

Effort comes from the complexity score, restricted to the levels that model
accepts:

- it is never `max`, which stays a manual choice;
- it is rounded up when Jev is unsure of the complexity (confidence below 0.5);
- Fast mode is never turned on by Auto.

### 5. Re-check

The chosen (provider, model, effort) is re-validated exactly like a manual
choice before anything runs.

### 6. Fallback

Anything wrong falls back to the manual selection, and the UI names the reason:

- off;
- key missing;
- timeout;
- router unavailable or refused;
- unusable answer;
- not confident enough;
- nothing to choose from.

### Brief follow-ups

A follow-up of 48 characters or fewer with no files ("yes", "continue", "fix
it") keeps the selection the chat is already running on, without asking Jev.
This is judged by length, not by a phrase list, so it works in every language.

### Ordering and safety

- The decision is made **before** the turn exists, so it cannot change models
  during a run or a tool loop, or race a steer or a stop.
- The existing one-live-turn rule still applies.
- A routed turn stores its selection as the chat's settings in the same
  transaction as the turn, guarded by the chat's revision. A concurrent manual
  change wins: the send is refused, never overwritten.
- Retry re-runs the stored (routed) selection. Retrying a supervised turn
  re-resolves supervision on the person's own Claude connection; if that is
  no longer possible, the retry runs unsupervised and shows no supervision
  step.
- **A chat's provider never changes.** Auto picks across connected providers
  only when it starts a new chat. Within a chat, it picks only that provider's
  models and effort levels.

### Decision rate limit

Each person may request at most 20 Auto decisions per minute, and the whole
deployment at most 1,200 per minute. Both are in-memory sliding windows, so a
client loop cannot spend the operator's TypeSafe quota for everyone. A
decision over the limit is not sent to TypeSafe; it falls back to the manual
selection with the reason "throttled".

These are not limited:
- brief follow-ups, which never call the router;
- the first turn of a new chat, which only presents its decision;
- manual sends;
- execution itself.
- A new chat is routed **once**, before it is created. Its first turn is not
  routed again.
- Cancelling or leaving the draft while Auto is deciding creates nothing.

## Supervision of complex Auto turns

### When it runs

Only for an Auto turn that Jev **routed**, where the routed complexity level
is at least `NEEV_SUPERVISION_MIN_COMPLEXITY`. Simple work never calls a
supervisor, and neither do brief follow-ups, fallbacks or manual sends.

### Who supervises

**Claude only.** Claude is the only provider with an enforced tool-free mode:
- the CLI runs with no tools, no MCP, no hooks or skills, and no session
  persistence;
- the adapter refuses to continue unless the CLI's own `system/init` reports
  `tools: []`.

**Codex executors can be supervised.** The supervisor still uses Claude.

### Whose credential

- The supervisor runs on **the invoker's own connected Claude credential**,
  never anyone else's.
- The executor's credential is never used for a different provider.
- The supervisor model is checked against the invoker's own Claude catalogue.
- It is not a stronger model than configured: if the configured model is not
  available, the turn is unsupervised and says so.

### What the person sees

**When supervision is unavailable**, the route line says why ("connect Claude
to enable it", "the review model is not available"), and the turn runs
unsupervised.

**Phases are shown on the turn**:
- planning, then the plan, then the agent's run, then reviewing, then the
  review (`pass`, `concerns`, `fail` or unavailable);
- each step can be expanded to its plain text;
- every review states its scope and "Nothing was run, tested or verified."

**While a supervisor works:**
- steering is disabled;
- during review, the button reads **Stop review**;
- Stop during planning cancels the turn before the agent starts;
- Stop during review keeps the completed answer.

### Crash recovery

A crash during review settles the turn as completed. Its answer and outputs
stay in history, and the review step reads `unavailable: interrupted`. A
crash during planning or execution is an interruption, as before.

### Cost

Supervision uses up to two extra strong-model calls from the person's own
Claude plan per supervised turn. These calls are bounded:
- in time, by the timeouts above;
- in size, at ≤16 KiB out and ≤48k characters in.

## Usage and reset awareness

Auto avoids sending work into a limit the provider itself has reported:

- **Codex.** The in-sandbox adapter forwards the CLI's documented
  `account/rateLimits` windows: used percent, window length and reset time.
  These come from one read per turn plus any updates during the turn. Credits,
  plan type and names are dropped before they leave the sandbox.
- **Claude** has no documented usage feed. Its headroom stays "unknown", and
  it gets cooldowns only from its own structured `rate_limit` and
  `billing_error` failures.
- **Failure text proves nothing.** Only a provider's structured failure class
  counts. Auth, network, overload and timeout failures never cause a cooldown.
- **Scope.** Usage is kept in memory per (organisation, person, provider).
  Disconnecting or reconnecting forgets it, and so does a restart. Data older
  than 15 minutes, or past its reset time, reads as unknown.
- **How Auto uses it.**
  - A spent or cooling-down provider is skipped for new chats, and the route
    line says so: "codex skipped until 14:00".
  - An existing chat on a spent provider keeps its selection, with the reason
    "usage limit reached". Its provider is never switched.
  - Jev sees only one word per option: `plenty`, `limited`, `low` or
    `unknown`.
  - Manual sends are never filtered.
- **No dollar figures and no per-model budgets** are invented.

## Changing the decision provider

`agents::routing::DecisionProvider` is the whole seam. A future provider, such
as OpenAI's Decisions API (limited preview; **not integrated**), would be:

- a new implementation of that trait;
- a new `NEEV_ROUTER_PROVIDER` value in `config::routing`.

The allowlist, validation, fallback and UI stay as they are.
