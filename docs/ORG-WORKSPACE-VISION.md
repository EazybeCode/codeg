# Eazybe WorkOS — Organization Workspace Vision

*Captured 2026-09-06 from Sagar's direction. This is the north star for what WorkOS
becomes: an org's coding workspace + knowledge base + chat, built on git branches.
Honest about what's already built vs. greenfield, and sequenced so each phase ships
before the next.*

## The vision (Sagar's words, distilled)
An **Organization Workspace** where:
- Every **project** is divided into small parts, and everyone works on it **together
  in different branches** (independent work, no stepping on each other).
- A **general** branch collects work that's finalized on personal branches (via review).
- A **main** branch is promoted from general when things are finalized for release.
- This makes **what-needs-doing and best-practices clear** by construction.
- It combines **chat + workspace** — "a combination of Notion and Slack."

## The core insight: git branches ARE the "small parts"
This vision fits our substrate perfectly. In WorkOS (codeg), each session already runs
in its own **git worktree on a branch**. So "everyone works on different parts in
different branches" isn't new architecture — it's the model we already have. The work
is to (a) make the branch *workflow* explicit and enforced, and (b) add the
collaboration (chat + docs) around it.

## The branching model (a best-practice workflow, made the default)
```
  personal/feature branches   →   general (integration)   →   main (release)
  (one per person/task,            (finalized work,             (promoted from
   isolated worktree)               merged via PR + review)      general when shipped)
```
- **Personal branches**: each person/task = an isolated worktree. Independent, safe.
- **general**: PR-only, review-required. Our **ship gate** (interview → shipdoc) +
  the **shipdoc CI check** already enforce "you don't merge without documenting why."
  That's the "best practices are clear" mechanism — it's built.
- **main**: protected; promoted from general on release. Branch protection + required
  checks (GitHub) enforce it.
This is essentially GitHub Flow + a staging branch — well-understood, low-surprise.

## Chat + workspace = "Notion + Slack", mapped to what exists
- **Notion (knowledge)** → the **org-memory wiki** we already built (`eazybe-wiki`:
  decisions, incidents, architecture, per-project docs) + a viewer. Git-native, reviewed.
- **Slack (discussion)** → codeg **already has a chat-channel concept**
  (`chat_channel*` tables/migrations). Per-project channels + session threads people
  comment on + @mentions is the layer to surface and wire.
- **Workspace (doing)** → the codeg coding workspace (worktrees, agents) — working.
The combination: each **project** has three faces — *work* (branches/worktrees),
*talk* (chat channels/threads), *know* (wiki/memory) — all scoped to the **org**.

## What's already built toward this
- **Org container**: multi-tenancy — `orgs`/`users`/`sessions`, GitHub login, per-org
  scoping (decision 0006). ✓
- **Projects**: GitHub repo picker + server-side clone into the org's space. ✓ (new)
- **Small parts**: git worktrees per session (codeg). ✓
- **Finalize-with-review**: the ship gate + shipdoc CI check (decision 0004). ✓
- **Knowledge**: the org-memory wiki + pipeline (decisions 0001, 0005). ✓
- **Chat substrate**: codeg's chat channels — present, not yet surfaced as project chat.

## Honest sequencing (ship each before the next)
- **Phase A — Coding workspace (now):** GitHub project picker → clone → worktree →
  agent. Deploy to Coolify. Get Eazybe actually using it. *This is the foundation; don't
  build B/C until people use A.*
- **Phase B — Branching workflow:** make personal→general→main the default; branch
  protection + PR + ship-gate wired so promotion is a guided action, not tribal
  knowledge. Mostly convention + config + light UI on top of A.
- **Phase C — Collaboration (the Notion+Slack layer):** per-project chat channels +
  session threads/comments + the wiki surfaced in-app. This is where it *feels* like
  Notion+Slack. Biggest build; do it once A+B are real.
- **Phase D — External orgs (gated):** per-org agent **execution isolation** +
  real per-user API auth (drop the token bridge) BEFORE any org outside Eazybe. Non-
  negotiable security gate (decisions 0003/0006).

## The honest risk
This is **three product categories** (coding workspace + docs + chat). The failure mode
is building all of it at once and shipping none. The discipline: **Phase A working and
used first.** The git substrate means B and C are additive, not rewrites — so there's no
rush to build them before A earns its place. Notion and Slack are excellent and cheap;
only build our versions where being *git-native and in the same workspace as the code*
is the real advantage (which, for an eng org, it genuinely is).
