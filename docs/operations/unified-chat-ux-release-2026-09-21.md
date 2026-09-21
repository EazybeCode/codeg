# Unified chat UX release — 2026-09-21

## Behavior
- One New agent chat action in project/folder menus. Agent and Team modes share a conversation; provider selection is inside the composer. Mode/provider preferences survive reload, scoped to user/org/chat.
- Creation opens immediately and the first message supplies a default name. Existing standalone private sessions remain accessible; no private history was migrated or shared.
- Document titles are directly editable. Enter/blur saves, Escape cancels. Manual naming is serialized with autosave and stops automatic naming.
- The composer stays writable during execution and preserves text typed after sending.
- Real tool starts, current command, elapsed time and expandable results appear during execution and replay from saved turns. No hidden reasoning is displayed.
- Bounded workspace/project orientation and same-thread context replace coding-only greetings. Shared-thread context excludes private resources; user-authored context is delimited and bounded.
- Independent authentication reads and document body/history reads run concurrently. Trace refresh is incremental.
- Transient revalidation failures preserve drafts in a hidden, inert workspace. Retry revalidates the held session. Actual access refusal or identity/scope changes still clear it; autosave resumes after successful verification.

## Verification
Backend 6c4b971f; frontend 093c07f7 plus compatibility fix ada28f1b; integration 7bafcc1b.
- Native harness: 233 agent + 74 HTTP tests passed; 15 external/environment-dependent tests ignored.
- Frontend at 093c07f7: 221 tests passed, lint and TypeScript clean.
- At ada28f1b: 120 affected tests passed with actual server-shaped deterministic session IDs; lint and TypeScript clean.
- Node22.23.2 webpack production export passed, all34 routes generated.
- Same-host candidate browser verified inline rename, Escape cancellation, autosave/reload, folder creation menu, Team Enter-send, automatic naming and mode/provider persistence.
- Live: the exact previously rejected team session reopened with its saved response and trace. Claude identified the correct workspace/project without volunteering a repository path.
- A live Bash sleep/printf test displayed Running Bash, elapsed time and command before completion, then rendered its saved result. Draft text typed during the run survived.
- Live document title field rendered. Codex is not connected; authenticated Codex execution is not claimed.

The first live test caught a contract mismatch masked by UUID-shaped fixtures: team session IDs are hash-derived canonical hex strings, without guaranteed RFC UUID version/variant bits. The browser rejected successful server responses. The previous image was temporarily restored; the client now accepts this shape only for agent-session IDs. Resource/chat/turn/message/request checks remain strict. Tests include the actual failing ID shape.

## Deployment
Image: neev-native-agent:ux-unified-ada28f1b
Builder: neev-build:unified-6c4b971f
Workspace SHA256: 143e60112fbc92d54af54c4f669ca747fd0764f6f3cb2f18f1461a57bb187b93
Schema:13 unchanged. Exact canary/live artifact and schema verified.
Database backup: neev-before-unified-ux-20260921-182430.sql
Compose backup: compose.yml.before-unified-ux-20260921-182430
Compatible rollback: neev-native-agent:ux-2d4608ff
Runtime isolation, credential mounts, DNS binding and database settings preserved. Docmost was not rebuilt.

## Limits
Known request waterfalls were removed; no broad latency reduction percentage is claimed. Per-row authorization queries and organization admission locking were not redesigned.
Shared-chat traces remain private to the invoker. Stored turns attach to replies only on a unique completed owner/provider/body match; ambiguous identical replies do not receive a potentially incorrect trace.
Workspace orientation includes authorized titles and bounded chat history, not a mounted repository or complete document corpus.
