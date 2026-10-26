# Neev / Neevz handover for Claude and Codex

Last checked: **2026-10-26**. Recheck GitHub and the running image before a
release. Application paths below refer to `teams/client-layer-oct12`; many
do not exist on `main`.

## Branch and release status

| Item | Verified state |
| --- | --- |
| Application repository | `https://github.com/EazybeCode/codeg.git` |
| Application / release branch | `teams/client-layer-oct12` |
| Default branch | `main`: upstream baseline plus this handover |
| Live app | `https://app.neev.so` |
| Live source commit | `160a45bca3a09e7b644d776f40651904ec14dc93` |
| Live image tag | `neev-native-agent:teams-160a45bc` |
| Database schema | The deployed Teams release uses schema 25; confirm on the host before a swap. |
| Pending release | DOCX support (`62741f62`), Teams package permissions (`c19a5935`), and Docker build-context corrections are being consolidated onto the release branch. Verify the remote tip; do not assume they are live. |

The original clone also has a `workos` remote for `EazybeCode/neev`. That is
not this publication's target. The surrounding Conductor workspace belongs
to `sagardewan/new-idea`, a separate planning repository.

On the owner's Mac, the persistent application checkout is
`~/conductor/workspaces/new-idea-v1/rio-de-janeiro/.context/teams-oct12-src`.
The sibling `hosted-oct8-src` has 21 dirty files: applying those changes
three-way onto the prepared DOCX commit produced no additional delta. Keep
that checkout until publication is verified; do not overwrite the newer Teams
schema files with its older versions. The former `/tmp/neev-deploy-20261022`
worktree is gone, but its DOCX commit survives in local Git.

## Product direction

Build a fast shared workspace with Notion-style documents/navigation,
Conductor-style agent chat/streaming, and concise interfaces for sales and
marketing teams. Projects contain folders, documents, and agent chats.
Generated files belong to their chat and need preview, download, and deletion.
Individual/team sharing must honor private exceptions.

The document engine is **La Suite Docs**; do not restart a Docmost migration
without a new decision. Shared knowledge, MCP integrations, and skills belong
to Neev's organization layer above the selected agent. Preserve agent history,
attachments, user attribution, and Neev workspace context.

Teams integration is intended to be native: Team to project, channel/thread to
shared task context, and generated documents/files attached to the workflow.
An embedded web tab alone does not meet that goal.

## Source map

| Area | Application-branch path |
| --- | --- |
| Workspace mode gate | `src/app/workspace/layout.tsx` |
| Hosted workspace/chat/docs/settings | `src/components/neev/` |
| Frontend helpers and contracts | `src/lib/neev-*.ts` |
| Native server entry | `src-tauri/src/bin/neev_server.rs` |
| Native API, identity, organizations, permissions | `src-tauri/src/neev/{http,auth,org,catalog}/` |
| Agent routing, execution, history | `src-tauri/src/neev/agents/` |
| Content, attachments, migrations | `src-tauri/src/neev/{content,db}/` |
| Organization knowledge | `src-tauri/src/neev/knowledge/` |
| Teams bot/Graph/bindings/worker | `src-tauri/src/neev/msteams/`, `src-tauri/src/neev/http/teams.rs` |
| Sandbox, login, MCP broker | `scripts/operations/neev_*.py` |
| La Suite bridge | `deploy/lasuite/` |
| Browser extension and companion | `tools/neev-browser/` |
| Native images | `Dockerfile.native`, `Dockerfile.native.agent`, their `.dockerignore` files |
| Runbooks | `docs/operations/` |

The frontend is Next.js static export, React, and TypeScript. The hosted
backend is Rust/Axum with PostgreSQL. This repository also contains upstream
desktop, SQLite, and generic `codeg-server` paths. Read the mode gate first:
editing an operator screen can leave the hosted native UI unchanged.

## Development and verification

Use Node 22, Corepack and the pnpm version from `package.json`. Release builds
use the Rust toolchain pinned in the native Dockerfile. From the application
branch, focused checks for the pending release are:

```bash
corepack pnpm install --frozen-lockfile
corepack pnpm exec vitest run src/lib/neev-attachments.test.ts src/lib/neev-library.test.ts src/components/neev/native-library.test.tsx
python3 -m venv .venv
.venv/bin/python -m pip install -r scripts/teams/requirements.txt
.venv/bin/python -m unittest discover -s scripts/tests -p test_teams_app_package.py
.venv/bin/python -m unittest discover -s scripts/tests -p test_neev_mcp_broker.py
cd src-tauri
cargo test --locked --lib --no-default-features docx_
cargo check --locked --no-default-features --bin neev-server
```

Run broader suites and `corepack pnpm build` as required by the actual change;
these focused checks are not full release acceptance. Report skips/failures.
Keep dependencies, virtualenvs, test output, secrets, and caches out of commits.
For local configuration read `src-tauri/src/neev/config.rs` and native runbooks;
use a development database and dedicated storage. Missing protected settings
intentionally block startup; `neev-server --help` is not a health check.

## Publishing and deployment

1. Check branch, remote, local diff, other working copies, and running SHA.
   Preserve the Teams implementation and schema 25 when incorporating changes.
2. Run relevant checks and a secret scan. Preserve tenant isolation, shared and
   private permissions, provider credentials, and sandbox boundaries.
3. Follow the organization's `ship` workflow. On the owner's Mac the command
   is `~/eazybe-wiki/scripts/ship`. Code changes need the developer's terminal
   interview; do not invent answers or bypass the hook. Documentation-only
   changes follow the workflow's no-interview path.
4. Verify the remote commit after publication. Build native base/agent images
   from an exact, secret-scanned Git archive. The frontend build needs the
   extension's `signin.js`; the agent context needs operations scripts, MCP
   manifests, and the response-style file.
5. Before deployment check schema, active runs/sign-ins, a verified database
   backup, and a saved compose configuration. Preserve mounts, Teams, routing,
   knowledge, provider configuration, and the originals-storage broker.
6. After deployment verify immutable image ID, backend/frontend hashes, schema,
   `/healthz`, `/workspace`, authenticated flows, and a real attachment run.
   Record release state and rollback evidence.

Observed host paths for an authorized operator:

- Compose: `/opt/neev-native-https-20260913/compose.yml`, service `neev`.
- Container: `neev-native-https-20260913-neev-1`.
- Release record: `/var/lib/neev-upstream-release/state.json`.
- Release configuration: `/etc/neev-upstream-release/release.json`.
- Protected configuration: `/etc/neev-native/`. Inspect names/presence, never
  dump secret values into chat or Git.
- Durable data: `/var/lib/neev-native/documents` and
  `/var/lib/neev-native/knowledge`. These are not cache-cleanup targets.

The previous Teams deploy script performs a schema 24-to-25 migration. Do not
rerun it unchanged against schema 25. A DOCX candidate image was built but was
not deployed as of this handover. Review any prepared swap script, including
release-state updates and rollback, before executing it. A branch name or a
successful push does not imply automatic deployment.

## Remaining work

- **Publish/deploy:** verify the consolidated release on GitHub, then use a
  guarded live swap that preserves Teams.
- **DOCX acceptance:** upload a real DOCX in the browser, ask an agent to use
  its extracted text, and verify later turns retain access.
- **Reliability/OpenUI:** the approved design is
  `docs/superpowers/specs/2026-10-16-neev-reliability-openui-design.md`.
  It is a design, not proof of implementation. Audit each requirement before
  claiming structured response UIs or corrected run outcomes are complete.
- **Teams pilot:** verify app installation/consent, account/Team/channel
  bindings, replies, and file delivery. The runbook lists unimplemented action
  approvals, live steering, and progress updates. Group chats, private/shared
  channels, and external audiences are outside the current pilot.
- **Operations:** recheck shared MCP connections, provider sign-in, storage
  read-through/eviction, knowledge capture, backups, and the browser companion.
  Dated runbooks are not evidence of current service health or tenant consent.

Useful application-branch runbooks under `docs/operations/`:
`microsoft-teams.md`, `neev-native-image.md`, `neev-native-agents-acceptance.md`,
`neev-storage-box.md`, `neev-content-backup.md`, `neev-knowledge-storage.md`,
`neev-auto-routing.md`, `neev-browser.md`, and `upstream-monitor.md`.

## Prompt for the next agent

> Read README.md, docs/NEEV-HANDOVER.md, AGENTS.md, and CLAUDE.md. Work on
> teams/client-layer-oct12 in EazybeCode/codeg. Compare local changes, GitHub,
> and the live release record first. Preserve Teams and schema 25. Report what
> is pushed, what is live, and what remains. Continue with relevant tests and
> honest live verification. Follow ship, keep credentials private, and never
> delete durable data or claim that a design document is implemented.
