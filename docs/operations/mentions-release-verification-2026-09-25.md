# Mentions (schema 16) — release verification

Source: `b600f071`, unchanged. The combined release image was built from that
production source with **no source changes** for this verification and needs no
rebuild: UI `36152a…`, server `cabc2f…`. This records what was tested, what passed,
and how the one failing result was classified.

## What passed

| Surface | Result |
|---|---|
| SQLite, every suite this work touches | **255 passed, 0 failed** — `neev::db::` 94, `neev::content::` 80, `neev::http::` 81, each run from the compiled binary, exit 0 |
| PostgreSQL mention service (`postgres_mentions`) | **PASS** against the isolated test database |
| PostgreSQL schema-16 migration (`postgres_mention_migration`) | reported `FAILED`; classified below as an **infrastructure failure, not a migration defect** |
| Schema-16 reopen, real server binary, same host | **3 consecutive clean starts** on the database that had reported the failure |

## The one failure, and why it is not a migration defect

`postgres_mention_migration` panicked at the **first reopen** after a successful
15→16 upgrade, with `left: Err(Unavailable)`, `right: Ok(())`. The upgrade itself had
already been asserted good (marker 16, mentions table present, accounts preserved).

`AuthError::Unavailable` is what `database_error()` returns for *every* `DbErr`,
including "the socket is gone", so that assertion cannot distinguish a schema refusal
from a lost connection. Three independent lines of evidence say it was the latter:

1. **The database was valid.** The harness panicked without cleanup, so the failing
   schema survived. Every check the reopen performs was replayed against it at zero
   latency: inventory exactly the schema-16 table set, one marker row at 16, mention
   and attachment storage classes correct, the raised revision-cap CHECK present, the
   two singleton rows valid, and all 61 applicable validator queries reporting zero
   violations. Nothing there can make a schema-16 validator refuse.
2. **PostgreSQL logged no error.** For the entire run window the server log holds only
   checkpoints — no `ERROR`, no `FATAL`, no client EOF. Error-with-statement logging was
   verified working in the same session.
3. **The transaction was never closed.** The backend that ran the reopen was still
   `idle in transaction`, its last statement (uniqueness check 4 of 6 in the session
   validator) having *completed successfully*, with the client simply never sending the
   next one. A validator refusal rolls back; a severed client cannot. Two sibling
   connections also outlived the test process, and no tunnel process remained.

Conclusion: the test lost its database connection mid-validation. The run used a
network tunnel to a remote host; the release path does not.

## Direct proof: the candidate reopens that same database three times

The candidate server image was run on the PostgreSQL host itself — no tunnel, no test
harness — pointed at the exact schema whose reopen had failed, with only the database
target overridden and no published ports. `initialize()` runs before anything binds,
so a refusal exits with `Neev native server failed to start`.

Start, then restart, then restart again — the reopen under test, three times:

| | process | stderr | `/healthz` | marker | tables | account rows | init lock released |
|---|---|---|---|---|---|---|---|
| start | running, exit 0 | empty | ok | 16 | 39 | preserved | yes |
| restart 1 | running, exit 0 | empty | ok | 16 | 39 | preserved | yes |
| restart 2 | running, exit 0 | empty | ok | 16 | 39 | preserved | yes |

**Preservation** held throughout: the account created before the downgrade survived the
15→16 upgrade and all three reopens, the mentions table stayed present and empty, and
the table count never moved off 39.

The one way this proof could have been hollow is a search-path override that silently
did not apply, which would have sent startup down its empty-inventory fresh-install
branch somewhere else. Ruled out: all 39 tables of the target schema were read, no
`neev_%` table was read or created in any other schema, and the public schema stayed
empty before and after. PostgreSQL logged no error for the whole canary window.

## Operational notes worth keeping

- **The schema init advisory lock is per-database, not per-schema.** Two PostgreSQL
  tests sharing one database therefore serialize every `initialize()` — including each
  multi-minute fresh install — on that single lock, which is most of why the remote run
  took 26 minutes. Run the PostgreSQL tests with `--test-threads=1`.
- **A severed init connection can leave that lock held by a backend PostgreSQL still
  believes is alive.** Every later start would then block on it indefinitely rather than
  fail. Pre-existing, unrelated to mentions, and worth a bounded acquire
  (`pg_try_advisory_xact_lock` with retry, or a `lock_timeout` on that one statement) if
  it is ever seen in the wild. Not changed here.
- **A schema-15 binary refuses a schema-16 database**: its inventory guard has no arm
  for the newer table set. Do not point an older backend at a database a schema-16
  build has already opened.
- **Next time this test fails**, check for an `idle in transaction` backend before
  suspecting the schema. A refusal rolls back; a dropped connection does not.

## Cleanup

The verification container and its credential-bearing env file were removed. A dump of
the failing schema, taken before the canary wrote anything, was kept as evidence. The
shared test PostgreSQL container, its network, and the test server were deliberately
left running for the release owner, who owns final cleanup.
