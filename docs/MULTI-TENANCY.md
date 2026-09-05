# WorkOS Multi-Tenancy — Design & Plan

Turn the codeg fork (single-operator, shared token, one SQLite DB) into a
multi-org platform: any org/user can join, GitHub login, shared DB scoped by
`org_id`. Start with Eazybe (1 org, ~10 users); scale to many orgs later.

## Decisions (locked)
- **Generic SaaS**: any org and user can join.
- **Auth**: Login with GitHub (OAuth). Every user ↔ a GitHub account.
- **Model**: separate `users` and `orgs` tables; **1 user → 1 org for now**.
- **Isolation**: shared DB, every tenant row scoped by `org_id`.
- **GitHub-native**: the OAuth token also authorizes git clone/push as that user/org.

---

## 1. Auth — GitHub OAuth (replaces the shared `CODEG_TOKEN`)
1. Register a GitHub OAuth App → `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET`,
   callback `/<host>/auth/github/callback`. Scopes: `read:user user:email read:org`
   (+ `repo` later if the server pushes on the user's behalf).
2. Flow: `/auth/github/login` → GitHub consent → callback → exchange code for token
   → fetch `GET /user`, `/user/emails`, `/user/orgs`.
3. **Upsert** `users` (by `github_user_id`); create a **session** (signed httpOnly
   cookie) instead of the shared token. Keep `CODEG_TOKEN` only as an optional
   break-glass/admin path, off by default in SaaS mode.
4. Store the user's GitHub access token **encrypted** (used for git operations).

## 2. Org mapping (1 user → 1 org for now)
On first login, resolve the user's org one of two ways (pick in Phase 1):
- **(a) Map to a GitHub org** — read `/user/orgs`, let the user pick one (or auto-pick
  if single); create/link a WorkOS `orgs` row keyed by `github_org_id`. Membership
  mirrors GitHub. Cleanest "natural structure."
- **(b) First-user-creates** — user creates a WorkOS org, becomes owner, invites
  others. Independent of GitHub orgs.
Recommendation: **(a)** — it's the GitHub-native structure you wanted and needs no
invite system for v1.

## 3. Data model (shared DB + `org_id`)
New tables (sea-orm migration, matching codeg's existing migration style):
```
orgs(   id, github_org_id, slug, name, created_at )
users(  id, github_user_id, github_login, email, name, avatar_url,
        org_id → orgs.id, role, gh_token_enc, created_at )
sessions( id, user_id → users.id, expires_at )     # login sessions
```
Add `org_id` (and usually `created_by_user_id`) to **every tenant-scoped table**
codeg already has: conversations, folders, projects, agent_settings, model_providers,
chat_channels, opened_tabs, quick_messages, etc. (the tables from codeg's migrations).

## 4. Authorization (the part you cannot get wrong)
- **Middleware** resolves session cookie → user → `org_id`, injects into request ctx.
- **Every** data query filters by `org_id`; every mutation stamps `org_id`. A single
  unscoped endpoint = cross-org data leak. Add a repository-layer helper so scoping
  is centralized, not sprinkled per-handler, and a test that every tenant table is
  always queried with an org filter.
- Roles minimal for v1: `owner` / `member` (1 user→1 org). RBAC expands later.

## 5. Agent execution isolation — the security gate ⚠
This is the dangerous part and it drives the phasing. WorkOS runs **file-editing,
shell-executing agents**. Shared-DB logical isolation protects *data rows*; it does
**not** isolate *running agent processes*.

- **Phase 1–3 (internal Eazybe, 1 trusted org):** logical isolation is fine — all
  users are one org, mutually trusted. Ship tenancy plumbing now.
- **Before onboarding ANY external org:** hard isolation is **mandatory** — per-org
  worktree paths, per-org sandbox (separate container/namespace or micro-VM), scoped
  git credentials, secret redaction, resource limits. Without it, org A's agent can
  read org B's code/secrets. **This is the gate: no external orgs until this is done.**

Stating it plainly so it's a conscious decision, not a surprise breach.

## 6. GitHub-native git access
The OAuth token (with `repo` scope) lets the server clone/push the org's repos and
the org-memory wiki as the user. Store it encrypted per user; use it for the
worktree/project operations codeg already does. Orgs bring their own GitHub repos.

## 7. Phased plan
- **Phase 0 — this doc.** ✅
- **Phase 1 — Identity:** GitHub OAuth + `orgs`/`users`/`sessions` tables + session
  middleware; replace the shared-token gate with login. Org mapping (3a). *Outcome:
  Eazybe users log in with GitHub; every request has a user+org.*
- **Phase 2 — Scoping:** add `org_id` to all tenant tables + migration + repository
  scoping + the "no unscoped query" test. *Outcome: data is org-isolated.*
- **Phase 3 — Org UX ("company workspace" UI):** login screen, org name/branding in
  the header, member list, account menu. *This is the UI cleanup you asked for —
  done properly as part of tenancy.*
- **Phase 4 — Isolation & hardening (GATE for external orgs):** per-org agent
  sandboxing, scoped creds, secret handling, rate/resource limits, audit log.
- **Phase 5 — Self-serve SaaS (later):** org onboarding, roles/invites, billing.

Ship **Phase 1–3 for Eazybe now**; **Phase 4 before the first outside org**.

## 8. Keep upstream sane
codeg is a fast-moving upstream. Put tenancy in clearly-scoped modules/migrations so
rebases stay manageable; avoid editing core files where a hook/middleware seam works.

## Open questions
- Org mapping 3(a) vs 3(b)? (Recommend 3a.)
- Does the server act on git as the user (needs `repo` scope) now, or later?
