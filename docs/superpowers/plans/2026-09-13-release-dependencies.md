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

### Task 2: Remove obsolete CLI graph and patch residual critical/high paths

**Files:** package.json, pnpm-workspace.yaml, pnpm-lock.yaml, src/app/globals.css; new vendored shadcn stylesheet and MIT notice; docs/operations/neev-dependency-security.md; narrowly required test-tool configuration/tests.

**Interfaces:** consume Task 1's patched graph, preserve rendered CSS and all existing product behavior. Keep @blocknote/shadcn and protected Next/BlockNote/Tiptap versions. Task 1 approved floors are lodash-es 4.18.1 and postcss 8.5.23, superseding obsolete original floors.

1. Read saved audit `/tmp/neev-dependency-audit-final-20260913.json` and reproduce affected resolutions. Verify official registry/advisory/migration sources before upgrades.
2. Vendor the exact installed standalone shadcn 3.8.4 tailwind.css (95 lines, 1669 bytes, SHA256 146941ac3ff65496fdf1cb306e697255328e4dadc7c105d66e36bb031b00f6d6), retain MIT copyright/license from official shadcn-ui/ui LICENSE.md, update globals.css import and remove standalone shadcn package. Do not remove @blocknote/shadcn or project-boot/shadcn. Prove exact stylesheet equivalence.
3. Upgrade Vitest and matching UI/coverage packages to compatible patched versions at least 3.2.6, Vite at least 6.4.3 with compatible React plugin, reading official migration guidance. No forced cross-major transitive override. Preserve test assertions; report real application regressions.
4. Patch remaining critical/high paths with compatible major-scoped overrides or direct dependency updates: brace-expansion, browserslist, flatted, form-data, glob, js-yaml, minimatch, picomatch, ws. Let obsolete shadcn-only paths disappear first. Verify actual patched floors from current advisories, not assumptions. No forced audit fix or unrelated whole-graph refresh.
5. Add focused coverage for the optional ansi-to-react linkification path using the Task 1 linkify-it override, recording a genuine failing baseline if behavior needs correction. Current app leaves optional path disabled; do not enable it.
6. Run fresh audit, full frontend tests, typecheck, static export, diff check serially using Node 22. Record exact residual advisory inventory and classify, without suppressions or claiming zero when not zero. Mac has about 11 GiB free; no cache deletions.
7. Update security operations evidence with official citations, exact versions/tests, outstanding moderate/low or other issues. Commit scoped changes only; no pushes/cloud/credentials/provider calls. Report full evidence and concerns in task-2-report.md for independent review.

### Task 3: Patch compatible residual browser and tooling advisories

**Files:** package.json, pnpm-workspace.yaml, pnpm-lock.yaml, docs/operations/neev-dependency-security.md; narrowly targeted existing sanitization/diagram tests only when needed. Preserve all existing assertions, native identity and disabled runtime.

**Interfaces:** consumes Task2 graph with zero critical/high and 38 residual entries. Produces compatible patched browser/rendering dependencies without a test-tool major migration. Do not redo Task2 or migrate Vitest4 in this task. Keep protected Next/BlockNote/Tiptap versions and accepted Vitest3.2.7/Vite6.4.3.

- [ ] Read `/tmp/neev-dependency-audit-task2-final-20260913.json`, inspect exact installed parent chains and official advisory/registry metadata. Known candidate floors: DOMPurify3.4.13, Mermaid11.16.1, next-intl/icu-minify4.9.2, uuid11.1.1, @babel/core7.29.1, @humanfs/node0.16.8, ajv6.14.0, @ai-sdk/provider-utils4.0.33, baseline-browser-mapping2.11.0. Verify compatible current majors before changing them; these are security floors, not permission for blind cross-major overrides.
- [ ] Apply compatible parent updates or major-scoped floors via apply_patch and regenerate lockfile with scripts disabled. Retain prior security floors. For an incompatible major, inspect the smallest parent update; if that requires a materially new product/API migration, report the exact chain and stop that package's change, not the entire task. Do not suppress audit findings or add arbitrary forced overrides.
- [ ] For sanitizer/diagram upgrades, run actual existing renderer and native rich-document tests. Where a directly consumed vulnerable path is exercised, add a focused regression preserving legitimate markup and rejecting executable markup, with a real failing baseline when applicable. Do not claim a test exercises an advisory when only checking its installed version.
- [ ] Run fresh audit, full frontend suite, typecheck, static export and diff check serially with Node22. No cache deletion, assertion weakening, cloud, real provider calls, credentials or push. Record exact remaining chains, reachability limits and whether they are development-only; a nonzero audit exit remains nonzero evidence.
- [ ] Update security operations evidence with exact versions/tests/advisory links and package-specific deferrals, then commit scoped files plus this controller plan amendment. Write task-3-report.md with full evidence, decisions, commit and concerns. This task does not accept a native image or production rollout.
