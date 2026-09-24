# Agent streaming — 2026-09-24

The user requested Conductor-like streaming: readable partial replies, visible running tools,
correct Markdown, and no disappearance when a reply becomes saved history. This implements
those behaviors; it does not claim to copy Conductor's private implementation.

## Changes

- Native replies use the existing Streamdown dependency, with incomplete-Markdown handling
  only while streaming. Saved replies render in static mode. Tables, task lists and
  strikethrough render correctly; existing HTML, URL and remote-image restrictions remain.
- A completed preview stays visible until its own saved reply arrives. Run generations and
  the invocation's message sequence protect subsequent turns from late cleanup.
- Codex uses its pinned 0.155.1 App Server through a Python adapter inside the existing
  sandbox. Actual `item/agentMessage/delta` notifications reach the existing SSE stream;
  no synthetic typing delay is added. Command starts appear before their results.
- The adapter preserves model, reasoning effort, tier, attachment paths and subscription
  credentials. Outer stdin, credential handling, network restrictions and resource limits
  are unchanged. Failed/interrupted provider turns exit unsuccessfully; cancellation is
  not displayed as a service outage.
- `NEEV_CODEX_STREAM=app-server` enables this transport in the agent image. Setting it to
  `exec` restores the old transport. The launcher defaults to `exec` when unspecified.

## Validation

- Rust agent suite: **281 passed, 12 ignored**, exit 0. Ignored tests require separate
  integration infrastructure. One old argv assertion was corrected to inspect provider
  arguments after the launcher separator; no production Rust changed for that correction.
- Python adapter, launcher and sandbox: **88 passed**, ResourceWarning treated as error.
  Independent adapter + launcher rerun: **70 passed**, exit 0.
- Independent final frontend suite: **57 passed**, exit 0. ESLint and static build passed.
  The cancellation regression test failed before the one-line fix and passed afterward.
- Linux release build passed. No database migration or document-engine rebuild.
- Live subscription test on the same production host: a preliminary reply appeared before
  completion; command activity appeared while `sleep 4; printf STREAM_OK` ran; final output
  contained STREAM_OK and a real three-row table. One saved answer, no duplicated final text.
- A second command was canceled through the UI. The server reported no running turns;
  subsequent inspection found **zero sandbox-lane processes and zero run directories**.
- After the final UI patch, a running `sleep 30` was canceled again: the composer
  recovered without the service-unavailable message. The verification chat was moved to
  trash and the original conversation remained available.
- A subsequent text-only reply completed successfully. The existing user conversation's
  previously raw-pipe table now renders as a real table without editing stored messages.
- Attachment mapping is schema- and unit-tested; a new live image upload was not exercised
  in this release. Claude account connectivity was not changed or claimed to be verified.
- An offline provider probe without network or credentials timed out; it is not counted
  as successful provider verification. Real subscription verification above succeeded.

## Release

Source commits: `fa4d8ab7`, `c65f98c4`, `e0a34d11`, `e6aaa6ba`.

Final image: `neev-native-agent:ux-docs-speed-streaming-final-20260924`.
Server SHA-256: `6d20bbcdfa268a83830ff7ffe0d436ec329aa747992090f66019611046921239`.
UI SHA-256: `99571fe50739f1b69d9cc462e2cafcb1ea9717aaa461396bdf6ea566b96a5784`.

The compose change only selects the image; auth volumes, document creation, isolation and
La Suite services are retained. Previous image retained for rollback. Final health returned HTTP 200, deployed UI hash
matched, and document creation remained enabled. Generated build contexts and transfer
archives were removed; credential volumes and user content were not part of cleanup.

Reference: <https://learn.chatgpt.com/docs/app-server>; protocol schemas generated from the
pinned installed binary. Repository/toolchain context: [org-memory: wiki/engineering/workos.md].
Current Neev source and deployment were verified directly because that wiki describes the
older WorkOS fork.
