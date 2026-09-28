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
