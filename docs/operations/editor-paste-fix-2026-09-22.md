# Editor paste and notice repair — 2026-09-22

## Findings and changes
- Live reproduction: Shift+Enter in a table cell raises the generic unsupported-content error; subsequent typing succeeds but the old error remains visible.
- Accepted edits now clear stale notices. Table Shift+Enter gives a specific single-line-cell explanation.
- Paste uses the cursor/selection instead of appending after the enclosing block. Multi-block replacement preserves surrounding text in order.
- Pasting line breaks into a single-line table cell keeps the text in that cell, flattens breaks, and states that transformation. LF, CR, U+2028 and U+2029 are covered.
- Code blocks use BlockNote pasteText to preserve literal multi-line content. A mounted-editor regression and browser check are required: pure paste-decision tests missed a real insertion failure.
- Whitespace-only paste preserves whitespace; linked text containing line breaks is handled for table cells.

## Evidence
Runtime head 51874cee (includes ca0bc84b, 8759690d and tests-only 93c1984a). Final affected tests: 100 passed on Node22; lint and TypeScript clean. Production webpack export passed, 34 routes. Independent review findings were addressed, and the first browser candidate was withheld after code paste failed; 51874cee supersedes it.

Not full Notion parity: table cells remain single-line under the existing native document/export contract. External rich clipboard formatting may be flattened with a notice. Existing document validation and storage limits remain enforced.

## Candidate
Image neev-native-agent:ux-editor-51874cee; backend unchanged, neev-build:unified-6c4b971f.
Workspace HTML SHA256 bd0d2cc81001555fd19ee320a3c8eed7b08a5d055e4e52e9f2e8101c8f464971.
Schema13, no migration. Rollback image neev-native-agent:ux-login-ba9b3588.

## Deployment
Deployed after same-host candidate browser checks. Confirmed multi-block selection replacement, cell paste placement and explicit transformation notice, notice clearing, multi-line code paste, replacement inside existing code, and autosave.
Live exact artifact hash verified; schema13 unchanged.
Database backup: neev-before-editor-fix-20260922-054538.sql
Compose backup: compose.yml.before-editor-fix-20260922-054538

Post-deploy live browser reproduced Shift+Enter on the test document: precise single-line status appeared; subsequent typing cleared it and entered autosave. User documents were not edited.
