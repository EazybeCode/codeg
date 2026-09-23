# Conductor-style agent chat release — 2026-09-24

## Released

Live image: `neev-native-agent:ux-docs-speed-0ede0a3d`.
UI SHA-256: `c841db72c2bb774fcb9a2a7a9fea6c387bbc4b35ffbe56b2da03028036ee0f55`.
Schema remains 15. Existing La Suite document engine retained.

- ChatGPT subscription device login now admits the required login endpoint and Prolite plan. Credentials persist across reload; no API key used.
- Removed nested connection/catalogue lock; fixed metadata-probe workdir/stdin, optional catalogue default effort parsing, and Codex non-Git workdir execution.
- Production Codex commands use the existing outer sandbox rather than attempting an incompatible nested sandbox. Outer namespaces, tenant UID, capability removal, no-new-privileges, egress controls and cleanup remain. Model commands can reach the container root filesystem and permitted endpoints inside that boundary; this is intentional command functionality.
- Codex V8 virtual-address limit is finite 2 TiB to accommodate reservations. Physical container memory remains 2 GiB, no additional swap, read-only rootfs and 512 PID cap. Claude's address limit and explicit operator overrides remain unchanged.
- Production prompt is a literal tail after `--`; option-looking prompts cannot become CLI flags.
- Inline attachment chips preserve text before/after, paste/drop/caret position, removal and Undo. Sent messages retain attachment position. Enter sends. Preview avoids CSP-blocked blob URLs.
- Compact model/effort/speed controls support discovered models and shortlist. Connection checks show a timestamp. Trash sits at the sidebar bottom.

## Live browser evidence (IST)

- Subscription connection test succeeded 01:41:06 after full reload.
- Actual command at 01:52:20 completed 01:52:29: expanded trace displayed `/usr/bin/bash -lc 'printf NEEV_OK'` and `NEEV_OK`, one tool call and one result.
- 01:57:52 valid synthetic 64x64 red PNG sent between prompt text and trailing instruction using Enter; 01:57:56 Codex answered `Red`. A prior malformed tiny fixture was correctly reported unreadable; replaced with a CRC-valid fixture.
- Discovered GPT models, supported reasoning levels and Fast control visible. Fast not enabled during tests.
- Same-host isolated browser verified inline image preview, sent order, chip removal/Undo and restored-file send. Fresh La Suite doc opened and renamed; title survived reload.
- Disposable production verification conversation archived recoverably; existing user conversation preserved.
- Final production health returned `status: ok`, schema 15, expected image.

## Automated validation and review

- Launcher Python regressions: 34 passed. Relevant earlier Python suite: 501 passed.
- Four targeted Rust regressions passed, including actual catalogue-producer workdir/handshake; Linux release build passed.
- TypeScript, lint, Next export passed. Library/agent-control suite: 2485 passed.
- Component suite: 379 passed, one settings timeout; isolated settings rerun 15/15 passed. Inline attachment regressions and browser Undo/send checks passed.
- Isolated same-host content verifier passed schema 15; manifest `8766ffac3231c7a6447d7db43f0acde3d9c039b81b6cf260155fca260d21c6ce`.
- Claude implementation and independent review used. Review gaps for control validation, probe producer/relay wiring and production option-looking prompts were closed with regressions.

## Remaining engineering limitations

This is a focused release, not a claim of complete Conductor parity or sustained-load validation. Unsandboxed development runtime retains Codex's own sandbox and still needs separate literal-prompt normalization. Catalogue probe stderr capture could improve future diagnostics. Unsupported future effort labels fall back to raw names. Production command compatibility is live-tested against this pinned image; recheck when upgrading Codex.

Laptop build caches and completed Rust target removed; about 19 GiB available afterwards. Source, user files and deployment data preserved.

Deployment/repository context: [org-memory: wiki/engineering/workos.md].
