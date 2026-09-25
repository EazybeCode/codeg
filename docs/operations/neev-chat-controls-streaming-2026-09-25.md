# Chat controls and large tool output — 2026-09-25

A user uploaded a 21.7 MiB HTML successfully, but their Codex run failed on the first `rg` command. The streaming adapter rejected frames above 1 MiB, including full command-output notifications. A 22 MiB regression reproduced that failure. Incoming frame allowance is now 64 MiB; command output forwarded to chat is limited to a 4096-character preview. The protocol still has a bounded frame size, separate from uploaded-file policy.

Both chat surfaces now use one composer control: Send when idle, Stop while running. Stop keeps the existing cancellation API and does not submit the form. Automatic first-message naming now also handles reopened empty unnamed chats and attachment-only first messages; manual titles and populated older chats are preserved.

## Checks

- 31 adapter tests passed, including the new 22 MiB output regression and oversized-protocol rejection.
- Live Codex GPT-5.6-Luna test ran a command emitting 23,068,672 characters, completed in 8 seconds and replied OK. Disposable chat archived afterward.
- 164 focused UI tests across six files passed; lint, typecheck, production Next build passed.
- Attachment-only naming fallback is typechecked but lacks a dedicated regression test.
- Live health200 and exact UI build hash verified.

Image: `neev-native-agent:chat-controls-context-20260925`

UI SHA256: `df47de4f52bc124d9f1734869e793757c331997c058a32953a836704320ef23e`

Server SHA256 unchanged: `f59cd1d82be5bd7ef3a32527cef2e63b68dceb56373887b0432f4295543b9da6`.

Prior images retained: `neev-native-agent:large-tool-stream-20260925` and `neev-native-agent:uploads-no-file-cap-20260925`. No database migration for these changes.
