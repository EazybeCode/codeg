# Hosted Claude Connect: why every sign-in died one second after the click

Status: root cause **confirmed by reproduction inside the running production
container**, capacity restored live by an operator device change, and the source
defect that caused it fixed with regression tests. Scope: two Python files that ship
inside the agent image and one frontend component. **No Rust change**; `codeg-server`
is untouched, and neither provider CLI nor the login allowlists are implicated.

Follows [`hosted-codex-device-auth-2026-09-24.md`](./hosted-codex-device-auth-2026-09-24.md),
which fixed Codex sign-in the day before. Fixing Codex is what exhausted the
resource this document is about — see "Why it worked until it didn't".

## Symptom

Pressing **Connect** on the Claude row failed about one second later with
`Native.error_unavailable` — "This service is temporarily unavailable. Please try
again." Retrying eventually produced `Native.error_conflict` instead — "This
information changed, or the sign-in method is linked to another account" — which
reads as a provider account problem and is not one. The panel showed Claude as
*Not signed in* while Codex stayed *Connected*.

## The database signature was the server's, not the user's

Three attempts, each followed within a second by a cancel (times UTC):

```
attempt   pending    03:15:46      03:16:03      03:18:21
connect   abandoned  03:15:47      03:16:04      03:18:21
cancel    completed  03:15:48      03:16:06      03:18:21
```

No user pressed Cancel. `connect()` in `src-tauri/src/neev/http/agent_sessions.rs`
records the attempt via `intents::begin`, then calls `supervisor::start_login`; when
the supervisor refuses it deliberately cancels the attempt it just recorded
(`intents::cancel(..., "<key>-abort")`) rather than leaving it pending for ten
minutes. That produces exactly an abandoned connect plus a completed cancel, one
second apart. So the cancels were a *downstream effect*, and the whole question was
why `start_login` refused.

## Root cause: the credential store ran out of loop devices

`Supervisor._start_login` calls `self._volumes.create(store_scope)` first, and
`CredentialVolumes.create` runs `cryptsetup luksFormat` and then **`cryptsetup
open`**. Opening a file-backed LUKS image requires a free loop device. The container
had two, and both were already attached:

```
/dev/loop1 -> /credentials/44fd51e3…/volume.luks   codex,  org 2 / user 2  (connected)
/dev/loop0 -> /credentials/e5b919c1…/volume.luks   claude, org 3 / user 2  (connected)
```

The host had `loop0`–`loop7`; `compose.yml` enumerated only `loop0` and `loop1`.
`LOOP_CTL_GET_FREE` returns the kernel's next free number, but with no matching
device node inside the container the attach cannot proceed.

### Reproduced in the live container

A throwaway 32 MiB image under `/tmp`, a throwaway key from `/dev/urandom` (never
printed), a unique mapping name, all removed afterwards — no store lock, no
credential, no existing volume, nothing mutated:

```
format exit: 0
Attaching loopback device failed (loop device with autoclear flag is required).
/dev/mapper/neev-loopprobe-tmp is inactive.
```

`luksFormat` succeeds — cryptsetup formats a regular file without a loop device — and
`cryptsetup open` is the step that fails. That is exactly the step `create()` runs
next.

### Corroboration on disk

The three failed attempts each left a half-provisioned volume whose manifest has
`luks_uuid: null`, i.e. `create()` died after `allocate()` and before
`write_manifest(..., "ready", luks_uuid)`. Their timestamps match the three attempts
exactly. Retaining a failed create's image and manifest for inspection is documented
behaviour (`neev-credential-volumes.md`) and was left alone; the entries cost 100 MiB
each and are an operator cleanup decision, not an automatic one.

## Why it worked until it didn't

Capacity for a *new* sign-in was `loop_nodes − volumes_already_open`. It reached
zero when the Codex device login completed at 2026-09-23 18:37 and took the second
loop device; the first had been held by a Claude volume since 2026-09-21. From that
moment every new sign-in, for either provider, was dead on arrival. Codex's **Test
connection** kept working throughout because it reuses its already-open mapping
instead of allocating one — which is why the panel showed one provider healthy and
the other unable to start.

## The source defect: `open` without `close`

`neev_agent_credentials.deliver()` opened the caller's volume and never closed it.
Its `finally` restored canonical ownership only, so every volume ever lent kept its
dm mapping — and its autoclear loop device — for the life of the container.
`CredentialVolumes.close()` exists, is careful, and is **non-destructive** (`revoke`
is the destructive one), but in the whole tree it was reachable only from
`Supervisor._revoke_volume`, i.e. teardown. Nothing called it on a success path, and
`_check_capacity()` guards free disk only. Nothing anywhere sized a loop pool against
the number of connected users.

This was not a designed invariant. The one artefact that looked like one,
`assertEqual(volumes.closed, [])` in `test_neev_agent_credentials.py`, was justified
as "the launcher must NEVER unmount/delete" — a statement about *non-destructiveness*,
which closing does not violate. The test conflated unmap with delete.

Left as it was, the ceiling grows with the number of users who have ever signed in,
which does not survive a multi-user deployment.

## Fixes

### 1. The lend gives the mapping back (`neev_agent_credentials.py`)

`deliver()` now closes the volume it opened. Two properties carry the safety
argument:

* **It happens under the per-scope run lock.** `_scope_run_lock` is exclusive per
  `(org, user, provider)` and is held across the whole lend, so no other legitimate
  consumer of that volume can be reading the mapping as it goes away. A concurrent
  probe or run for the same account waits for the lock or reports `busy`, exactly as
  before. `test_release_runs_under_the_scope_run_lock` proves this rather than
  asserting it: the injected `close` tries to take the same lockfile on a fresh
  descriptor and must fail to.
* **The release is owed from the moment `open` returns**, not from the moment the run
  starts. Everything between a successful open and the lend — the shape and mode
  checks, the ownership handoff, the provider-home creation — can refuse, and each of
  those exits previously abandoned a mapped volume. They are now inside the same
  `finally`. A refusal *before* open closes nothing: there is no mapping of ours, and
  closing on a handle we were refused would be a cross-scope action.

A close that cannot complete is deliberately not fatal — a busy unmount is documented
to leave the volume for an administrator, the store's metadata lock is non-blocking
so a concurrent create can make this transiently unavailable, and `open` revalidates
and reuses a surviving mapping — but it is never silent: `NEEV-CRED-DIAG stage=release
code=<allowlisted>`, where the code comes from the store's fixed vocabulary and
anything else degrades to `unknown`.

### 2. A sign-in that dies before the provider says so (`neev_agent_supervisor.py`)

`_start_login` mapped every exception to a bare `UNAVAILABLE`. It now also writes one
sanitized line — `NEEV-LOGIN-DIAG provider=<claude|codex> stage=<volume_create|driver_start>
code=<allowlisted>` — before failing. Provider, stage and code are all drawn from
fixed sets; no message, path, handle or exception text can reach the log. Writing the
line can never affect the outcome. This failure produced no trace anywhere, which is
most of why it took as long as it did to find.

### 3. A spent connect key is retired (`native-agent-connections.tsx`)

The panel keeps one connect `request_key` per provider so a retry after a lost
response replays the same attempt instead of starting a second one. It cleared that
key on success, on cancel and on reconnect — but not when the connect itself failed.
After the server self-cancelled an attempt, every later click replayed the spent key,
matched the recorded operation, found its attempt canceled, and earned `409` forever.
That is where "the sign-in method is linked to another account" came from.

The key is now dropped on `409` **and only on `409`**: it is the application's own
answer and the only one that proves the key is spent. A `502`/`503`/`504` may come
from the reverse proxy *after* the backend admitted the request, and a timeout, abort
or transport failure carries no answer at all — in every one of those the attempt may
be live, so the key is kept and the retry resumes it. Both halves are pinned by
tests, and the over-broad "any HTTP status" rule fails the gateway test.

## Deployment

**Operator change (applied live during this investigation, by root).** Enumerate the
additional loop device nodes in `/opt/neev-native-https-20260913/compose.yml`
alongside the existing entries:

```yaml
    devices:
    - /dev/mapper/control:/dev/mapper/control
    - /dev/loop-control:/dev/loop-control
    - /dev/loop0:/dev/loop0
    …
    - /dev/loop7:/dev/loop7
```

Strictly scoped: named device nodes only. No `privileged`, no `/dev` bind mount, and
no change to `device_cgroup_rules` — the existing `b 252:* rmw` covers device-mapper,
and loop devices are granted per node by `devices:`. Recreating the container drops
the live mappings; both connected rows re-open on next use, which is the supported
restart path. **This was verified live on 2026-09-24: Connect then reached "Waiting
for sign-in" with a working Claude sign-in link and code prompt.**

**Image change.** The fix is Python and static assets only:

* `scripts/operations/neev_agent_credentials.py`
* `scripts/operations/neev_agent_supervisor.py`
* the `out/` static export (for the frontend change)

No Rust rebuild is required or wanted; `codeg-server` is byte-identical to
`neev-native-agent:ux-docs-speed-0ede0a3d`.

The two changes are complementary, not alternatives. The device pool sets the
concurrency ceiling; the lifecycle fix is what keeps the requirement proportional to
*lends in flight* rather than to the number of users who have ever signed in.

## Verification

* `scripts/tests` — `python3 -m unittest discover -p 'test_neev*.py'`: **531 passed, 3 skipped** (534 total) (was 501 before this change).
* Load-bearing check, credential lifecycle: with `_release_volume` neutralised to the
  pre-fix behaviour, **8** tests fail —
  `test_recover_lend_and_restore_ownership_around_the_child`,
  `test_release_runs_under_the_scope_run_lock`,
  `test_release_runs_when_the_run_fails`,
  `test_release_runs_when_the_lend_is_refused_after_open`,
  `test_release_runs_when_preparing_the_provider_home_raises`,
  `test_a_close_that_fails_is_reported_and_does_not_fail_the_run`,
  `test_a_close_failure_reports_only_an_allowlisted_code`,
  `test_repeated_connections_never_accumulate_open_volumes`.
* `src/components/neev/native-agent-connections.test.tsx` — **33 pass** (31 before).
  Load-bearing in both directions: neutralising `forgetSpentKey` fails
  "retires a connect key the server has told us is spent", and widening it to any
  numeric HTTP status fails "keeps the connect key when a gateway answers for the
  backend".
* `pnpm eslint` clean on both changed frontend files.

## What this does not establish

The capacity failure is fixed and sign-in reaches the provider again. This document
does not claim a completed Claude login on the current account: the only completed
Claude login on this deployment belongs to a different account and predates the
outage. A connected row remains a statement about a stored, verified credential —
not about quota, entitlement, or a runtime turn.

## Final deployed verification

- Commit `10aa9db3`, image `neev-native-agent:ux-docs-speed-claude-5e00323a`.
- UI `workspace.html` SHA-256 `5e00323af6f4a1f3816cf2c8cbcf086fe091dc78f07016e40167ce40cc59e7c0`.
- Native server SHA-256 remains `ca8928567353882884deef861a706dc719a90ccf595ee227e8ac8741cc63bb27`; helper capabilities unchanged.
- Live health 200; document creation remains enabled. Deployment retains explicit loop0..7 device nodes and prior image/config backups.
- Browser after reload: Codex Test succeeded, Checked 09:11:14 IST. Claude Connect reached Waiting for sign-in with provider link and code field, expiry 09:21:38 IST. No temporary-unavailable error. Completing provider authorization requires the user's approval/code; not claimed complete.

[org-memory: wiki/engineering/workos.md]
