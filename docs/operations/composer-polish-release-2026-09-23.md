# Composer polish release — 2026-09-23

Source commit: `324110a4`. Live image: `neev-native-agent:ux-docs-speed-394d22a6`.
Workspace HTML SHA256: `394d22a67a74535b43921a8c33312043a8ca6dbed42da1fe2eb6d118fe5b2ee6`.

## Changes

- Fix staged image previews: blob URLs were blocked by the existing CSP. Bounded data URLs now render under that same policy. Images above 2 MiB retain file chips without local thumbnails; uploads are unaffected.
- Compact attachment references inside the composer and messages, with named error fallbacks for invalid images.
- One model menu with More models, reasoning, Fast, and a browser-local editable shortlist. Compact mode and Connect controls across composers.

## Verification

- 148 tests across 10 focused suites passed; typecheck and changed-file lint passed.
- Node 22 webpack production build passed. Full suite was not rerun; known unrelated failures remain.
- Canary browser: pasted PNG rendered (naturalWidth=1); Enter sent file-only message; same-origin preview survived reload. Invalid prior fixture images show a named fallback.
- Live browser: dataURL preview rendered (naturalWidth=1), then disposable draft removed without sending a message. More models, reasoning and Edit shortlist submenus opened successfully.
- Model discovery currently unavailable for disconnected Codex, so only its Default entry is offered; Claude catalog entries remain visible. This release does not resolve subscription sign-in.
- Deployment checked no active sign-ins/runs, health200, document creation enabled and exact UI hash.
- UI-only image: server and capability-helper binary hashes match previous release. Schema15 and La Suite backend unchanged.

Rollback image: `neev-native-agent:ux-docs-speed-3d010ec6` (same backend/schema).
