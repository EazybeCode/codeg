# Shipdoc: teams/client-layer-oct12

- author: sagardewan
- date: 2026-10-26
- commits: 3

**What changed**

- DOCX files (`.docx`) are now accepted as chat attachments. The server extracts plain text from `word/document.xml` and stages a `.docx.txt` companion alongside the original so all agent providers can read the content.
- The Teams app package now declares narrow RSC (resource-specific consent) permissions (`TeamMember.Read.Group`, `ChannelSettings.Read.Group`) so the bot can query team membership and channel settings without broad tenant-wide scopes.
- A comprehensive handover document (`docs/NEEV-HANDOVER.md`) and a reliability/OpenUI design spec were added; README, AGENTS.md, and CLAUDE.md point maintainers there first.
- Docker build contexts were corrected to include operations scripts, MCP manifests, and the browser extension's `signin.js`.

**How it works**

- **DOCX extraction:** `attachment.rs` adds `quick-xml` parsing of the WordprocessingML body. Only `<w:t>` text runs are collected; macros, embedded objects, and relationships are ignored. A 4 MB text / 8 MB XML cap prevents memory exhaustion. When a DOCX is staged for a run, `execution.rs` writes both the original and the extracted `.txt`; the injected prompt tells the agent to prefer the companion.
- **Schema migration:** `allow_docx_content_type()` widens the existing `CHECK` constraint on `neev_attachments.content_type`. It is idempotent and runs before schema validation on any version ≥15, so both fresh installs and upgrades include DOCX without bumping the schema marker.
- **RSC permissions:** `build_app_package.py` emits `authorization.permissions.resourceSpecific` in the manifest. These bind Graph consent to the specific Team on install rather than requiring tenant-admin approval.

**Gotchas / risks**

- The DOCX extractor trusts ZIP decompression bounds but does not validate the full OOXML schema; a malformed document could produce incomplete text. Agents must still verify generated artifacts against the original when layout or embedded media matters.
- RSC permissions only take effect after reinstalling or updating the Teams app in each Team; existing installs keep the old (none) scope until refreshed.
- The SQLite migration uses `defer_foreign_keys` and table recreation; it preserves rows but is not crash-safe mid-transaction—run a backup before upgrading busy production databases.

**Follow-ups**

- Live DOCX acceptance test: upload a real Word doc, confirm agent reads extracted text, verify history retains both files.
- Teams pilot: reinstall the app to activate RSC, confirm `TeamMember.Read.Group` queries work, verify channel bindings.
- Implement the reliability/OpenUI spec (`docs/superpowers/specs/2026-10-16-neev-reliability-openui-design.md`)—this commit adds the design only.
