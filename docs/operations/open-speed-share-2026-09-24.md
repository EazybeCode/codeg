# Chat/document opening and Share label — 2026-09-24

Live image: `neev-native-agent:ux-docs-speed-open-861c718c`.
Source commits: `861c718c`, `6583d510`.
UI workspace SHA-256: `42bd7b17286574e5dd68cc8fe6c5f2db96a4b28d132cf983bc05dc92a6f33d9b`.
Server SHA-256: `7e48ae018402f43b8fc0a1434124c57f8e501aad7f0034c5b969b49b05a15a1b`.

## Changes

- Versioned `_next/static` app/editor assets allow private browser caching for one year. HTML shells and sensitive responses retain their previous cache policy. Resolved engine paths prevent shell traversal from gaining immutable caching.
- Initial chat metadata and first history page start concurrently, removing one serial request. Metadata must be authorized before history renders. Subsequent polls preserve metadata-first ordering and revocation behavior.
- Chat/native document visible action reads Share; accessible name, tooltip and confirmation retain the organization audience. Engine document retains its compact icon action with an explicit tooltip.
- La Suite, authentication fixes, credential cleanup, helper capabilities and deployment device pool retained. Agent configuration caching and archived-state redesign deferred.

## Verification

- Linux release backend build and Node22 frontend build/typecheck passed.
- Rust engine asset tests 6/6; actual static HTTP route regression 1/1 passed.
- Relevant frontend assertions 188/188 passed sequentially. Seven BlockNote/jsdom uncaught errors were also reproduced at unchanged HEAD (32/32 workspace assertions, seven errors), so that subset is not a clean process exit.
- Full suite was not green: 6782 passed, 54 failed, eight errors under concurrent compile load; no whole-suite baseline established. Changed-file lint clean.
- Live health200 and exact UI hash verified. Actual app JS responds `private, max-age=31536000, immutable`; workspace HTML remains `no-store`.
- Live browser verified Share text, messages, and La Suite editor on initial and repeated openings. Warm document click plus automation snapshot took ~3.1s; this includes automation overhead, is not a controlled benchmark, and does not establish a percentage speedup. No precise user-visible latency claim.
- Removed obsolete build staging directories only to make release space; runtime/rollback images and all user data retained.

[org-memory: wiki/engineering/workos.md]
