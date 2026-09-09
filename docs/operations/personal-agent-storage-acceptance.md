# Personal-agent storage acceptance

Date: September 9, 2026. Status: implementation verification and independent review
in progress. This is a storage checkpoint, not a working provider connection or
release approval.

## Scope

The new connection, attempt and operation tables store ownership and lifecycle
metadata only. They provide restrictive parent references, scoped uniqueness,
generation/lease fields, bounded values and fixed state/error allowlists. No
provider code, token, credential payload or challenge URL column is added.

The migration does not backfill from legacy credentials, mount routes, launch
clients or enable agent execution. Authorization, final account verification,
cleanup execution and per-author prompt admission remain later service/runtime
requirements; database constraints alone do not establish those properties.

Task1 was accepted through `09c78148` after independent re-review resolved exact
length-endpoint and retained-parent rollback test gaps. Controller verification
at that source passed 36 personal-agent, credential and team-access migration
tests, including isolated PostgreSQL fixtures. Historical fixes are implemented at
`9bcf9d50`. Task2 and final whole-plan reviews are still pending acceptance.

Fresh controller verification at `9bcf9d50`, using the test binary rebuilt from
that exact source (from `src-tauri`):

```text
./target/debug/deps/codeg_lib-732148cb61475fb4 team_access personal_agent_connections_tests team_credentials_tests team_server::http_tests --include-ignored --test-threads=1
174 passed; 0 failed; 0 ignored; 16.28s

./target/debug/deps/codeg_lib-732148cb61475fb4 team_server --include-ignored --skip team_browser_fixture_server --test-threads=1
37 passed; 0 failed; 0 ignored; 2.06s
```

Both commands exited0; their nine HTTP tests overlap. This is 202 distinct backend
checks, including guarded PostgreSQL, not a full-project suite. The opt-in actual
browser fixture was not rerun for this storage change. Compiler output retained
the pre-existing unused import, linker frame-size and proc-macro compatibility
warnings; no new scoped warning was reported. Whitespace checks pass.

## Migration recovery boundary

The personal-agent down migration checks all three owned tables before dropping
any of them. Populated state is refused; empty named down preserves exact parent
organization/user rows. Forward rerun preserves data.

Full empty-chain testing exposed two older assumptions: catalog down selected a
newer repository model after its extra column was removed; initial down expected
a legacy table that the opened-tabs migration had already removed. Narrow fixes
and SQLite/PostgreSQL round-trip regressions are under review.

An empty-schema round trip is not permission to downgrade a populated production
database. Historical forward migrations may discard legacy UI state. Production
recovery must preserve tenant data through reviewed forward fixes or verified
isolated backup restoration; no live down migration was run here.

## Rulings carried forward

1. Deliver the approved connection subsystem through consecutive storage,
   lifecycle, supervisor/adapters and HTTP/UI plans under the same approved spec.
   Each boundary is independently testable; no release requirement is dropped.
   Cost if wrong: interface/plan rework. Runtime remains disabled meanwhile.
2. Keep Task1's down acceptance scoped to its owned tables, and add sequential
   Task2 for the independently reproduced historical catalog guard bug and full
   empty-chain coverage. Cost if wrong: historical rollback regression; populated
   refusal and empty round trips must both be verified.
3. Extend Task2 only by adding `IF EXISTS` to the initial migration's drop of the
   legitimately absent legacy opened-conversation table, with a direct regression.
   Cost if wrong: this one missing table no longer signals drift during initial
   down. No other legacy migration rewrite or populated downgrade is authorized.

## Release status

The owner separately confirmed the Claude hosted-use terms after the specific
terms question. See [provider contract record](personal-agent-provider-contracts.md).
That confirmation does not establish encrypted storage, client compatibility,
provider authentication or tenant isolation. No live provider account was connected,
no GitHub data was deleted, and no deployment/publication was performed for this
storage stage.

Context: [org-memory: wiki/engineering/workos.md]. Legacy operator-mode assumptions
are not used as the new multi-tenant authorization boundary.
