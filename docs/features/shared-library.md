# Shared workspace library

The library belongs to the current Neev organization. Existing members use the same shared projects and can share chats and documents with that organization. Invite teammates from **Settings → Members** or use the organization join code.

## Add reusable skills

An administrator opens **Settings → Shared library → Import** and selects one or more files:

- `SKILL.md` for a single skill.
- A ZIP containing a skill folder with `SKILL.md` and supporting files.
- A ZIP containing several skill folders, each with its own `SKILL.md`.

Example:

```
team-skills.zip
  brand-writing/
    SKILL.md
    references/tone.md
  weekly-review/
    SKILL.md
```

Imports start disabled. Review the imported names and files, then enable the items you want the team to use. Enabled skills apply to subsequent agent turns; a running turn keeps the configuration it started with. Deleting a library item does not delete conversation history.

## Add MCP integrations

Import an MCP JSON configuration using the `mcpServers` map:

```json
{
  "mcpServers": {
    "team-tools": {
      "type": "http",
      "url": "https://tools.example.com/mcp"
    }
  }
}
```

This is a configuration import, not a sign-in or a connectivity test. Remote destinations require the server's network allowlist, and local commands require their dependencies to be installed in the agent runtime. Follow the item's setup note before enabling it.

This first version accepts shared configurations without embedded credentials. Do not put personal tokens, passwords, authorization headers, or credential-bearing URLs in an import. Per-user Claude and Codex connections remain under **Personal AI connections**. Importing an MCP configuration does not share those accounts.

## Permissions

- Current members can see the organization library and use enabled items.
- Organization administrators can import, enable, disable, and delete items.
- Organization changes and membership removal revoke access through the existing workspace session checks.
- Uploaded chat attachments remain ordinary context; importing a skill into this library intentionally makes it a trusted instruction source when enabled.

Released on 2026-09-28 at https://app.neev.so. Open **Settings → Shared library**. Validation and release evidence are recorded in `docs/superpowers/plans/2026-09-28-shared-library.md`.
