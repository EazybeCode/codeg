# Agent chat HTML tools and response boundaries — 2026-09-24

## Observed problem and change

A completed HTML review showed two failed commands: `python` was absent, then
`node` was absent during a fallback attempt. The agent recovered with `rg` and
answered, but the trace summary did not state the successful turn outcome.
Separate Codex progress and final messages also ran together in the saved reply.

- `b47f947d`: add distro `python-is-python3` and `python3-bs4`; distinguish whole
  Codex messages from fragments and add paragraph boundaries through one capped
  append, keeping live deltas, stored text, and activity offsets consistent.
- `5238dfad`: show Completed only for a completed turn; retain failed step counts
  and verbatim error details. Localized count wording uses ICU plurals.
- Node is not added. The observed HTML task now has its original Python parser.
- No data migration or rewrite of historical responses was performed.

## Verification

Claude implementation worker reported 308 agent tests and 40 HTTP agent tests
passing, with 22 and 3 ignored respectively. The focused UI/locales run passed
71 tests; changed-file ESLint and Node 22 production build passed. Root reviewed
the diff, caught and corrected an intermediate live/stored separator mismatch,
and verified the Linux build snapshot exactly matched committed Rust sources.
Linux release build passed.

Live synthetic Codex test (GPT-5.6-Luna) at 19:06 IST executed `python` with
BeautifulSoup inside the real agent sandbox, returned `HTML_TOOLS_OK`, and
completed at 19:06:11 (submitted 19:06:04). The UI showed one tool call and result,
Completed, and no failed steps. Progress and final answer appeared in separate
paragraphs. DOM paragraph contents matched exactly after reload.

Automated attachment selection was blocked by the browser extension file-URL
permission. Therefore this release did not re-test the upload transport; the
live smoke supplied synthetic HTML directly to the same parser command. It did
not re-run or modify the user's HTML review. Seven seconds is one small smoke
run, not a performance benchmark for larger tasks or provider latency.

## Deployment

- Runtime: `neev-native-agent:ux-docs-speed-smooth-20260924`
- Server SHA256: `a30ff21c45bf3f454136ad535022b5874ca6b8138285bd3a0265fca3beb2ca23`
- Workspace SHA256: `06e7cdfde08b39eb852e12639a83780a7b1385c234ede7d17892d5035c6b2526`
- Previous runtime: `neev-native-agent:ux-docs-speed-open-861c718c`
- Pre-deployment: zero active sign-ins and zero running turns.
- Post-deployment: health HTTP 200, expected UI and server hashes verified,
  document creation enabled. Only the compose image changed.
- Existing auth, sandbox, resource limits, loop-device pool, credentials,
  document engine, volumes, and cache-policy fix remain unchanged.
- Obsolete generated transfer archives/build contexts were removed for disk
  space. User data and rollback images were preserved.

Org memory supplied background only; current source and remote are authoritative:
[org-memory: wiki/engineering/workos.md]. Release uses the `workos` remote to
EazybeCode/neev, not the historical codeg origin. Direct deployment/push was
explicitly authorized by the user in this session.
