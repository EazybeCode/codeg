# Team workspace skills and integrations

Approved request: team members share a workspace and reuse uploaded skills and MCP configurations.

Use the existing organization workspace and memberships. Add a clean Settings → Shared library section. Organization admins manage the library; all current members can read and use it. Existing shared projects, chats, documents, and invites remain the collaboration foundation. Do not create a second workspace hierarchy.

Support bulk skill ZIP imports (SKILL.md and relative supporting files), individual SKILL.md uploads, and MCP JSON configuration imports. An import is atomic, reports validation errors, rejects unsafe archive paths/symlinks and unsupported config. Each library item has a stable UUID, name, description, kind, enabled flag and revision. Explicit admin enablement applies items to future organization agent turns; no modification of running turns. Imported instructions are intentionally trusted by the organization, unlike ordinary chat attachments.

Both Claude and Codex receive enabled skills in their isolated run directory and enabled MCP configuration through their actual CLI configuration. Maintain org isolation, per-user provider logins, existing sandbox/network policy, stale-membership checks, CSRF and optimistic revision protection. Never silently label an unreachable integration as connected. MCP secrets must not be exposed in member listing. Start with secret-free shared configurations, reject credential-bearing imports with an actionable message; personal/provider credentials must not be copied. Remote MCP destinations require existing operator network allowlisting and must be identified clearly as requiring setup if not reachable. Do not widen network access for arbitrary URLs.

UI: concise list with Skills/Integrations tabs, add/import, enabled switch, delete confirmation, visible workspace-wide scope. Display file names and import failures. No secret values in errors. Empty states explain team sharing. Agent conversations inherit enabled org items without redundant composer controls.

No live release until focused ACL/import/runtime tests, UI checks, production build, and schema-aware deployment verification pass.
