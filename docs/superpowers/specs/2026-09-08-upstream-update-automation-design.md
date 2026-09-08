# Reviewed codeg update automation

Status: product direction approved 2026-09-08; this written implementation design awaits review. No schedule, automatic merge, or automatic deployment is enabled by this document.

## Objective

Keep EazybeCode/Workos current with useful improvements from xintaofei/codeg without overwriting WorkOS behavior, exposing production secrets to upstream code, or silently shipping sensitive changes. Monitoring and preparation happen without the user's laptop. Compatible low-risk improvements can eventually advance automatically; sensitive or uncertain changes require human review.

## Existing constraints

- Source checkout is an existing linked worktree on workos/team-workspace. Do not rename it or change its existing remotes as a side effect.
- EazybeCode/Workos is a separate GitHub repository with inherited Git history. Its actual default branch must be resolved at setup; do not assume main or change the default branch.
- Coolify currently deploys a pinned commit. Updating a branch does not itself update the running app.
- Current CI has a main-only push trigger and pull-request triggers. Bot-created PRs must explicitly receive checks; do not assume GITHUB_TOKEN-created events run other workflows.
- Existing browser acceptance is not green. Automatic production deployment stays disabled until the internal-release baseline is green.
- This automation never runs in the production app container and never receives customer agent credentials, user GitHub tokens, production volumes, the Docker management socket, or the Coolify token in its preparation/test jobs.

## Execution architecture

Use a scheduled GitHub Actions workflow on the repository's existing default branch, with manual workflow_dispatch support. Check every six hours, at minute 17 to avoid the hour boundary. Scheduling is best effort. A single concurrency group prevents overlapping update writers; a manual rerun resumes or updates the existing proposal rather than opening duplicates.

Separate four trust boundaries:

1. **Discovery:** read-only access; fetch upstream branch metadata and compare against the recorded observed and incorporated SHAs. Treat commit messages, code, documentation and PR descriptions as untrusted input, never shell commands or agent instructions.
2. **Preparation and verification:** disposable runner/checkouts; incorporate candidate commits without any production credentials. Tests run with no repository write token. Never execute modified workflow definitions with privileged pull_request_target context. Trusted orchestration invokes the candidate tests explicitly.
3. **Proposal publication:** separate least-privileged repository-scoped identity creates/updates a bot-owned branch and PR. Validate the proposed commit ancestry, file set and immutable tested SHA before publishing. No force push to user branches, default-branch direct push, upstream push, or unrelated branch deletion.
4. **Release:** separate protected job, disabled initially, consumes only an approved and tested immutable SHA/image digest. Production deployment credentials exist only in this job, never alongside candidate code execution.

Use GitHub-hosted ephemeral runners initially rather than adding an always-on VM or executing third-party builds on the Hetzner production host. Repository Actions permissions, available minutes, protected-environment support and the publishing identity must be verified before activation. Do not add a broad personal token as a shortcut.

## Discovery, incorporation and bookkeeping

- Track the latest observed upstream SHA separately from incorporated commits. Never advance the incorporated checkpoint merely because a commit was seen or a PR failed.
- Each proposal identifies the downstream base SHA, upstream commit range, actual incorporated commits, omitted/deferred commits and reasons.
- First inventory the backlog without silently incorporating all existing commits. Select a coherent, reviewable batch; dependency-sensitive commits stay together.
- Prefer a normal merge for a reviewed coherent range. Selective cherry-picks must retain original upstream attribution and dependency notes; do not use history rewriting to disguise conflicts.
- Keep at most one open active integration proposal initially. New discovery updates the backlog record; it does not cancel an already running review/test job.
- Conflicts, diverged/rewritten upstream history, changed default branch, removed refs, unknown files and failed checks produce a blocked proposal with a concrete reason. Production remains untouched.
- A checkpoint is recorded only after the exact proposal SHA is merged. Repeated schedules and retried publication must not double-apply commits or create duplicate PRs.

## Risk and automation policy

Classification is conservative and examines the complete resulting downstream diff, not just upstream labels or file extensions. Unknown or mixed-risk changes take the highest risk. A clean Git merge and passing unit tests do not prove safety or product relevance.

### Always require human approval

- Authentication, authorization, membership, roles, credentials or secret handling.
- API/event/file/terminal access, agent execution, runner isolation or resource limits.
- Database/schema/migrations, persistence, backup/restore, Git/worktree lifecycle or publishing behavior.
- Dependencies, lockfiles, installation/build scripts, binaries, CI/workflows, deployment, networking, telemetry or external destinations.
- Agent instructions (including AGENTS.md, CLAUDE.md and SKILL.md), automated reviewer policy, the risk classifier and this automation itself.
- Changes to confirmation/permission UI, ownership/visibility, billing, model-routing/cost behavior, or the approved project/folder/chat model.
- Any merge conflict, weakened/removed test, generated artifact without provenance, failed baseline, or uncertain applicability.

### Initial automatic-merge eligibility

Start with a deliberately narrow allowlist: human-facing documentation corrections and non-security localization copy that preserves key structure and interpolation placeholders, does not change URLs/commands/embedded HTML, and does not alter instructions, consent, permissions or cost messaging. Added/removed keys or code blocks require review. The immutable diff must pass deterministic checks and all required regression checks.

Other useful frontend/backend improvements may be prepared and tested automatically but initially require review to merge. Broaden the allowlist only through a reviewed policy change backed by regression coverage. This prevents an opaque AI "looks safe" assessment from granting itself release authority.

AI-assisted adaptation is optional and separate from monitoring. It needs an explicitly authorized provider, bounded runtime/token spend, an isolated checkout and no deployment secrets. Without it, clean incorporation can proceed but semantic conflicts become human-review work; the bot must not claim it implemented adaptations it could not perform.

### Production policy

Initially keep all production deployment manual while baseline bugs and internal-team security are unfinished. After release readiness is verified, eligible low-risk merged changes may deploy through the separate release job with the tested image digest. Sensitive changes always require approval of that exact SHA. Moving the branch after approval invalidates it. Deployment failure must retain/restore the previous healthy image, report the failure and disable repeated automatic retries. Never auto-rollback database migrations; those are never in the automatic lane.

## Product-fit checks

Preserve organization → project → arbitrary nested containers → repository-backed conversations. Preserve per-conversation branch/worktree identity, chat history and unpublished changes. Do not restore legacy global New chat/desktop administration into the SaaS experience, add a duplicate Documents hierarchy, or replace WorkOS branding with an upstream binary updater. Keep upstream license/attribution intact.

## Required checks and evidence

- Lint, TypeScript, frontend unit tests and static export.
- Server-mode Rust tests and build; targeted authorization, repository and worktree regression tests.
- Real browser acceptance for settings return navigation, nested creation, isolated chats, Files, reconnect and restart. Fixtures are allowed for model output but must be labeled; they do not replace real-provider release smoke tests.
- Changes to test tooling or security-critical assertions cannot approve themselves.
- PR records commands, counts, artifacts, failures, known unrelated baseline failures and the tested SHA. Skipped/unavailable checks block automatic merging rather than being reported as passing.

## Acceptance tests for the automation

1. No upstream changes: no new branch/PR/checkpoint movement.
2. Repeated discovery of the same commit: one proposal, no duplicate application.
3. Eligible copy correction with unchanged schema/placeholders and passing checks: eligible merge recorded at the tested SHA.
4. Any sensitive path or mixed-risk batch: human-review requirement cannot be overridden by upstream labels or prompt text.
5. Conflict, failed test, unavailable review or missing credentials: report blocked, preserve downstream branch and production.
6. Changed base/candidate SHA after checks: reject stale approval and rerun checks.
7. Malicious commit title/path/PR text cannot invoke shell commands or alter the automation policy.
8. Candidate build cannot access repository write/deployment credentials or customer data.
9. Publishing retries do not force-push user changes or duplicate PRs.
10. Release failure leaves the previous deployment available; sensitive migrations never enter automated rollback.

## Delivery sequence

1. Implement discovery, deterministic risk classification, idempotent backlog/proposal preparation and local fixture tests.
2. Add trusted verification and scoped publication; dry-run against the actual repository without merging.
3. Activate the six-hour monitor and reviewed PR flow after validating repository permissions and initial checkpoint.
4. Enable only the narrow automatic-merge lane after a successful supervised cycle.
5. Enable low-risk automatic deployment only after the internal-release baseline and rollback smoke test are green.

The agent-onboarding, multiplayer authorization and runner/job subsystems have separate designs and acceptance gates. This workflow does not implement them.
