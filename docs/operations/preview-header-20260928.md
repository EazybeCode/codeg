# Static preview header restoration — 2026-09-28

V106 stores its hero/navigation and footer in template content. Scripts mount
these into shadow roots. The previous sanitizer removed all templates along
with scripts, dropping entire visible sections.

Preserve existing declarative shadow templates and recognize the common
currentScript.previousElementSibling + attachShadow bootstrap pattern. Serialize
those templates with shadowrootmode=open; browsers can render them without
executing uploaded JavaScript. Sanitize every template fragment recursively.
Unrelated inert templates remain omitted. CSP and iframe sandbox are unchanged.

Reference: https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/template

Both regressions failed before the change. All 23 preview/file-action tests pass,
including nested-template sanitization and the 22 MB full-document fixture.
Scripts that modify the page after mounting still do not execute: this restores
static template content, not full interaction fidelity.

Production build/typecheck and ESLint passed. Deployed frontend-only image
`neev-native-agent:preview-header-20260928`; live workspace SHA256 verified as
`12559b1df745fc482ffa7579f7e7a60b7a98b7ec33600e05165cbbf143641886`.
Backend and schema17 unchanged; release guarded by zero active runs/sign-ins and
a verified database backup.
Actual V106 input inspection confirms restored v43-hero and v43-footer templates.
Live screenshot verified the navigation bar and hero headline now render.
The preview is static: external logo/flag images are still blocked, and later
JavaScript-driven visual modifications are not applied. Review tab left open.
