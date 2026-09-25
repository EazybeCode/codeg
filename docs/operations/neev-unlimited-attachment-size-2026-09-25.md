# Chat attachment size policy — 2026-09-25

Removed the independent 10 MiB per-file and 25 MiB per-turn attachment caps from browser validation, upload/download storage, chat-message linking, private-session linking and agent staging resolution. Removed the baked database upper-bound CHECK with an idempotent migration; schema remains 16. Existing rows and references are preserved. The organization attachment storage quota remains 256 MiB, along with five files per message, content sniffing and ACLs. Allocation guards follow the organization quota. Document-editor image limits are unchanged.

File transfers no longer use the ordinary 65-second request timeout; abort signals still cancel them. Only the server's explicit 429/content_quota response maps to storage exhaustion; 409 conflicts and ordinary rate limits do not.

## Verification

- Frontend admission regressions failed at both old size boundaries before the fix; 57 focused client/attachment tests passed afterward.
- ESLint passed; final Next.js production build passed.
- Linux release server and content verifier compiled successfully.
- Isolated PostgreSQL upgraded from the prior schema16 image, removed the old constraint, and restarted successfully.
- Actual user HTML: 44,778,046 bytes. HTTP upload returned 201; downloaded bytes matched SHA256; attaching it to a chat message returned 201.
- Rust unit suite incomplete: first test compilation lacked fixture files; after supplying them, rustc was killed by the 4 GiB test-container memory ceiling. Do not report this suite as passing.

## Release

Image: `neev-native-agent:uploads-no-file-cap-20260925`

UI SHA256: `c13226398651f2fccd9b2b289f64b2161bc8d9bcf4a0603982948058e4d53a98`

Server SHA256: `f59cd1d82be5bd7ef3a32527cef2e63b68dceb56373887b0432f4295543b9da6`

Verified database dump before deployment: `before-upload-20260925T161801Z.dump`, 219750 bytes, SHA256 `1015a71a753534c289b66e90c4731a842ca4001f0c3cd8cd04aeaadbb0c8ad50`. No active sign-ins or running turns at deployment. HTTPS health200 and exact UI hash verified after recreation.

Previous image retained: `neev-native-agent:ux-warm-canvas-20260925`. Once larger attachments exist, the old server's snapshot validation rejects them; do not blindly roll back the server or restore an old database over new user data. Prefer a forward fix.
