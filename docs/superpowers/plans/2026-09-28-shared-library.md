# Shared Workspace Library Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Team members reuse organization skills and MCP integrations inside agent chats.
**Architecture:** Extend native organization storage and authorization, stage trusted skills/config for each isolated run, expose one Settings library view.
**Tech Stack:** Rust/Axum/SeaORM, PostgreSQL + SQLite, React/Next.js.
**Spec:** docs/superpowers/specs/2026-09-28-shared-library.md

## Global constraints
Preserve existing uncommitted globals.css/native-workspace.tsx edits. Never git push. No user credential sharing. No network-policy bypass. Use current linked worktree. Claude backend worker and root UI work have separate file ownership.

## Task 1: Storage, imports, API (Claude)
Files: new src-tauri/src/neev/library.rs or library/; db/library_migration.rs; db/mod.rs; http/library.rs; http/mod.rs; neev/mod.rs.
Interface: GET /api/neev/library → {items:[{id,name,description,kind:"skill"|"mcp",enabled,revision,files:string[],setupNote:string|null}]}; POST /api/neev/library/import {fileName,data:base64} → same items envelope (imported only); PATCH /api/neev/library/{id} {expectedRevision,enabled}; DELETE same path with {expectedRevision}. Revision string, UUID id. PATCH returns item; DELETE returns {ok:true}. Existing native error envelope with safe code.
- [x] Tests first for same-org sharing, cross-org denial, member mutation denial, stale revisions, ZIP traversal/expansion limits, MCP credential rejection, atomic imports.
- [x] Add schema migration 18, idempotence and validation, CRUD/import implementation and CSRF routes.
- [x] Run focused tests and review.

## Task 2: Runtime (Claude)
Files: agents/execution.rs, http/agent_sessions.rs, http/agent.rs plus focused tests.
- [x] Test skills/config reach Claude and Codex, empty library preserves behavior, disabled/cross-org entries excluded.
- [x] Load enabled items with current organization authorization, stage safe relative skill paths, actual provider MCP config. Preserve sandbox boundary and contextual attachment distinction.
- [x] Verify real provider option syntax from installed CLIs/local implementation, document remote MCP allowlist requirement honestly.

## Task 3: UI (root)
Files: src/lib/neev-library.ts, src/components/neev/native-library.tsx, native-settings.tsx, corresponding tests.
- [x] Write import/list/admin/member interaction tests.
- [x] Implement API client and organization-scoped Settings library UI matching interface above.
- [x] Run focused tests, ESLint and production build.

## Task 4: Review/release
- [x] Review full diff, ACL/runtime failure handling and migration tests.
- [x] Verify server disk space and reusable backend build cache, build candidate.
- [x] Back up database, deploy schema-aware candidate, live-check shared library and existing workspace.
- [x] Record validation, limitations, release evidence. Never claim live before verification.


## Release evidence — 2026-09-28
- Live image: `neev-native-agent:shared-library-20260928`; schema 18.
- Workspace HTML SHA-256: `8a1fc17a0d84a83733d0224f5c9fd983060cf5a06815f4b647474376f9cf71cd`.
- Server SHA-256: `f75c8b384b72e58230f63b4f8afd3b7d92298e0b9ebb13a64ceca778615d9685`.
- Verified database backup before migration; no active sign-ins or running turns at deployment.
- Tests: 68 library/backend/HTTP tests; PostgreSQL schema17 preservation migration test; 34 existing execution tests; 37 existing agent-session tests; 27 frontend tests; 79 launcher tests; 31 streaming-adapter tests. Targeted suites only; unrelated legacy DB tests have stale schema16 expectations and were not claimed passing.
- ESLint, Next production build, optimized Rust server/content-verifier build passed.
- Actual installed Codex accepted generated MCP command/empty-args overrides. Both provider argv and config write failures are covered by launch tests. No paid live model run or third-party MCP connectivity claim.
- Live browser verified organization Settings, Shared library Skills and Integrations tabs, successful library loading, admin import control, and rendered appearance.
- Claude performed final read-only review. Its private-IP import concern is contained by the unchanged production default-deny egress policy and public-IP validator (`validated_ipv4`); import validation alone is not the network boundary. Credentials in arbitrary skill text/command args cannot be perfectly detected; administrators remain responsible for trusted imports.
- Remote MCP destinations still require operator allowlisting; stdio tools need installed runtime dependencies. Embedded credentials are not supported. Personal AI accounts are not shared.
- Existing uncommitted `globals.css` and `native-workspace.tsx` edits preserved and excluded from feature commit. No git push performed; organization `ship` gate remains applicable.
