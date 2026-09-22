# Collaborative document foundation

## Accepted direction

The user selected [La Suite Docs](https://github.com/suitenumerique/docs) for evaluation as the document subsystem, preserving Neev Projects and Agent Chat. It already combines BlockNote, Yjs and Hocuspocus with live collaboration, presence, sharing, granular permissions and subpages. It is a full Django/React service, not a drop-in React editor. Its core is MIT licensed; the upstream README identifies optional GPL BlockNote XL features and provides a MIT-only build option.

[Docmost](https://github.com/docmost/docmost) also supplies collaboration, permissions, history and comments. The current instruction is to leave Docmost unchanged. Its core is AGPL-3.0 with separately licensed enterprise features.

No claim is made that either implements every Notion feature. Databases, formulas, automations and exact UI parity need a separate feature inventory.

## Current implementation

Neev already uses BlockNote, but converts it to a deliberately limited native rich-document format. That adapter rejects unsupported features; numeric table column widths caused the reported resize error. Autosave uses optimistic revisions and conflict handling in `native-document.tsx`; this is not simultaneous edit merging. A new-looking editor alone would not fix that storage/concurrency architecture.

## Integration to prove before migration

1. Shared identity and organization/document authorization, including view-only access and immediate revocation.
2. Link a Neev project resource to the upstream document without duplicate navigation or a second user-facing sign-in.
3. Import existing documents, tables, links and attachments without losing content; preserve existing revisions as an archive until migration is verified.
4. Use the upstream collaboration protocol as the canonical edit state. Avoid a parallel whole-document autosave writer competing with it.
5. Preserve search, agent document context, export/backup and restore.

La Suite Docs exposes an [OIDC resource-server API](https://github.com/suitenumerique/docs/blob/main/documentation/resource_server.md). The documentation marks the API as evolving and controls which external actions are enabled. Shared identity and permission mapping therefore need a working prototype, not an assumption that an embed supplies access control.

## Multi-user acceptance

Use two separate authenticated users, not two tabs sharing a login. Edit the same paragraph and table concurrently, resize/add/delete columns, undo one's own edit, disconnect/reconnect, reload both clients and restart the collaboration service. Confirm convergence with no missing changes. Also verify a viewer cannot write, another organization cannot subscribe, and revoked access ends an active connection.

## Status

An isolated La Suite Docs v5.7.0 preview is running on the same host, accessible only through a loopback SSH tunnel. Two separate test accounts completed login; concurrent edits converged and an offline edit merged after reconnecting. The browser sample's text, 3×3 table and resized first column were independently decoded from the server's stored content. See [preview setup and evidence](../lasuite-docs-preview/README.md) for test boundaries and limitations.

Shared Neev identity, project permission projection, attachments, active-connection revocation and migration acceptance still need integration work. Existing documents have not been migrated. Docmost is unchanged. The immediate table-width and compact UI fixes are deployed separately (release `90d1da2a`).
