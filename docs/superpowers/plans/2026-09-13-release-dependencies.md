# Release dependency remediation implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Execute task-by-task with tests and review.

**Goal:** Remove the reviewed vulnerable frontend dependency paths without changing product behavior or enabling runtime.

**Architecture:** Keep the accepted static Next/BlockNote application. Remove the unused standalone component-generator CLI after verifying no runtime consumer; retain the actual BlockNote shadcn package. Update only the affected resolution graph to verified compatible patched versions.

**Tech Stack:** Node 22.23.2, pnpm 11.9.0, Vitest, Next static export.

**Spec:** `docs/operations/neev-release-plan.md` and the dependency security gate in the owner's approved remaining-release request. This is a security-maintenance slice, not a new feature or release acceptance.

## Global Constraints

- Preserve native identity/privacy and `runtimeEnabled:false`; do not mount legacy runtime.
- Preserve unrelated edits and all operator data, GitHub resources, credentials and archives.
- No cloud changes, push, provider calls or real credential access from workers.
- Keep Next 16.3.4, BlockNote 0.54.2 and Tiptap 3.31.3 unchanged in this slice.
- Do not suppress advisories or alter test assertions to pass dependency updates.
- Runtime has no Node, but browser and build dependencies still require review.
- Run package commands using Node 22 at `/opt/homebrew/opt/node@22/bin`.
- Only controller may publish through Workos clone; normal hooks, never force-push.

### Task 1: Remediate reviewed dependency paths

**Files:**
- Modify: `package.json`, `pnpm-workspace.yaml`, `pnpm-lock.yaml` as needed.
- Create: `docs/operations/neev-dependency-security.md`.
- Existing covering tests: terminal rendering, streamdown/mermaid, native rich-document components; full frontend test suite and static export.

**Interfaces:**
- Consumes the accepted frontend's existing imports and pnpm override policy.
- Produces the same runtime imports and public UI behavior with patched resolved dependencies. No new application API.

- [ ] **Step 1: Reproduce the dependency failure and inspect actual consumers.**

```sh
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm audit --json
git grep -n -E 'from ["\x27]shadcn|require\(["\x27]shadcn|shadcn/' -- src scripts package.json
git grep -n -E 'ansi-to-react|streamdown/mermaid|postcss' -- src package.json pnpm-workspace.yaml
```

Save audit output privately, never emit credentials. Record advisory IDs, installed versions, patched versions and actual import paths; distinguish no-match grep status from command failures.

- [ ] **Step 2: Apply minimal resolution changes after the failing baseline.**

Use `apply_patch` for package/workspace edits. Remove standalone `shadcn` only if its app/script consumers are absent, not `@blocknote/shadcn`. Pin compatible patched `linkify-it` 5.0.2, `lodash-es` 4.18.0 and `postcss` 8.5.18 via pnpm's existing workspace overrides where needed. The advisory for lodash-es specifies 4.18.0 even if an audit lower bound differs. Confirm package registry metadata before installing; do not update the whole graph or run a forced audit fix. Generate lockfile through pnpm; package-manager lockfile generation is allowed. No new hand-written behavior is required unless the update exposes a real regression; report such a defect rather than altering application code outside this scope.

```sh
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm view linkify-it@5.0.2 version
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm view lodash-es@4.18.0 version
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm view postcss@8.5.18 version
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm install --ignore-scripts
```

- [ ] **Step 3: Verify resolved graph and unchanged product behavior.**

```sh
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm audit --json
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm test
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm exec tsc --noEmit
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm build
git diff --check
```

Record exact counts and failures. Do not claim a clean audit if advisories remain; classify remaining affected paths and severity without treating absence of a direct import as a full reachability proof. Run one build/test command at a time to avoid exhausting the Mac. No permission to delete caches or existing build outputs as cleanup.

- [ ] **Step 4: Document and commit.**

Write `docs/operations/neev-dependency-security.md` with exact before/after audit counts, package changes, affected consumers, full test/export evidence and remaining issues. Cite official advisory pages. Distinguish local package acceptance from rebuilding or deploying the native image.

```sh
git add package.json pnpm-workspace.yaml pnpm-lock.yaml docs/operations/neev-dependency-security.md
git commit -m 'fix(neev): remediate reviewed frontend dependency advisories'
```

Controller task review and final scoped review cover changed graph and evidence before publication. This plan does not supersede the separate personal-agent runtime and production deployment gates.
