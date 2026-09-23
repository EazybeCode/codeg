# Hosted Codex device-auth: why a completed device code left GPT disconnected

Status: root cause confirmed, both fixes landed with regression tests, and
**verified end to end on the live deployment** — a real ChatGPT device login now
completes, persists, and reports `prolite`. See "Live verification" for the trace
and "What is still unverified" for what this does *not* prove.

Scope: two Python files that ship inside the agent image
(`Dockerfile.native.agent`). No Rust and no frontend change; `codeg-server` and the
Next.js bundle are untouched, and the pinned Codex CLI version is not implicated.

## Symptom

The user opened the device-verification URL, entered the code, and the provider
panel still showed Codex disconnected — across refreshes, repeatedly, for days.

## Root cause 1 — the login CONNECT allowlist was missing `chatgpt.com:443`

`("codex", "login")` in `scripts/operations/neev_provider_endpoints.py` named a
single authority, `auth.openai.com:443`. The pinned CLI also calls `chatgpt.com`
during the same device flow, and the per-slot proxy fails closed
(`neev_provider_egress.parse_connect` → `EgressError("denied")`).

Live evidence, production container `neev-native-https-20260913-neev-1`
(read-only; no mutation, nothing cancelled):

* `neev_agent_attempts` gen 7 — created `2026-09-23 18:21:00Z`, state `pending`,
  `updated_at == created_at`, i.e. the attempt never advanced after creation.
  `neev_agent_connections`: `codex | connecting | error_code NULL | storage_handle
  NULL`. The attempt then aged out on its own at `18:31:46Z` → `expired`, with no
  error code, *after* the user had entered the code.
* The only five egress diagnostics in 24h, all inside that attempt:

  ```
  18:21:01  stage=parse category=denied CONNECT authority=chatgpt.com:443(known)
  18:21:01  stage=parse category=denied CONNECT authority=api.github.com:443(other)
  18:21:01  stage=parse category=denied CONNECT authority=chatgpt.com:443(known)
  18:21:38  stage=parse category=denied CONNECT authority=chatgpt.com:443(known)  ×2
  ```

  `(known)` marks the host as in the diagnostic vocabulary, not as allowed;
  `category=denied` is the allowlist rejection. `api.github.com` is the CLI's own
  update check — unrelated, correctly denied, and left denied.

Every Codex attempt in the table ends `expired` or `canceled`; Codex has never
completed once. Claude is `connected`, and its login allowlist carries five hosts
including the post-exchange profile fetch it needs to *finish* login. Codex's
carried one. Same bug class, already fixed once for Claude.

Fix: add `Endpoint("chatgpt.com", 443, True)` to the `("codex","login")` tuple.
Exact host, login purpose only, still no wildcard and no all-Internet fallback.

## Root cause 2 — the plan profile rejected a real paid tier

`_SUPPORTED_PLANS` in `scripts/operations/neev_codex_login_protocol.py` admitted
`plus/pro/team/business/enterprise/edu`. The account in question reports
**`prolite`** (the local CLI shows "Plan: Prolite, Auth: ChatGPT login"), so
`_project_account` returned `unsupported`, and `CodexLoginDriver` turned that into
`failed / unsupported_account` — a paying subscriber rendered as disconnected.

This would have fired the moment root cause 1 was fixed, so both were needed. The
gate exists only in Python; no Rust file references plan strings.

Fix: admit `prolite`. Everything else — `free`, `go`, `unknown`, the
`self_serve_business_*` and `edu_plus`/`edu_pro` variants, and any future string —
still fails closed.

## Regression tests

`scripts/tests/` — `python3 -m unittest discover -p 'test_neev*.py'`, 501 pass.

* `test_neev_provider_endpoints.py` — the login host set is exactly
  `{auth.openai.com, chatgpt.com}`; subdomains, suffix-confusion hosts
  (`chatgpt.com.evil.invalid`), the runtime-only `api.openai.com` and the CLI's
  `api.github.com` are all absent; every authority matches `^[a-z0-9.-]+:443$`; the
  login scope stays a strict subset of the runtime scope.
* `test_neev_provider_egress.py` — at the proxy parser, `CONNECT chatgpt.com:443`
  now tunnels under the *login* policy, while `sub.chatgpt.com:443`,
  `chatgpt.com:80`, `chatgpt.com:8443`, `api.openai.com:443`, `api.github.com:443`
  and `evil.invalid:443` all raise `denied`.
* `test_neev_codex_login_protocol.py` — a full initialize → device start →
  `account/login/completed` → refreshed `account/read` walk with `planType:
  "prolite"` projects `supported` (and still keeps the email out of reprs); the
  same completed-login walk still projects `unsupported` for `free`, `go`,
  `unknown`, `self_serve_business_prolite`, `edu_plus`, the case variant
  `Prolite`, the prefix variant `prolite_future`, and an unseen future string.

## Live verification (image `neev-native-agent:ux-docs-speed-547c72e1`)

A real sign-in, driven by hand in a browser — the device code had to be typed on a
physical keyboard, since the OpenAI form ignores programmatic fill/paste. Observed
read-only; nothing was cancelled or mutated. Times are the container's UTC clock.

```
18:34:45  attempt gen 9 created            connection codex: connecting, handle=false, label=-
18:34:46  denied  api.github.com:443       (CLI update check)
18:34:46  denied  files.openai.com:443     (see below)
          chatgpt.com:443 — NOT denied, for the first time
18:37:50  attempt gen 9 -> completed       operation connect -> completed
18:37:58  connection codex: connected, storage_handle=true, account_label=prolite, checked_at set
```

Both fixes are exercised by that trace, and each was load-bearing:

* `chatgpt.com` no longer appears in the deny stream, and the flow got past the
  point where every previous attempt stalled. Fix 1 is what let it complete.
* the row landed `connected` with `account_label=prolite`. Under the old profile
  this same successful login would have projected `unsupported` and been published
  as `reconnect_required` / `unsupported_account`. Fix 2 is what let it stick.

Persistence is real, not a UI state: `storage_handle` is non-null on the row, an
encrypted credential volume exists under `/var/lib/neev-supervisor/credentials`
(existence and count only were inspected — never contents), and the connection
survived a full browser reload as `connected / prolite`.

`files.openai.com:443` and `api.github.com:443` were denied and **stay denied**.
Neither blocked completion, so neither is a login host; adding them would widen the
allowlist for no proven need.

### The Test-connection control is a real probe

Two clicks on "Test" against the connected row advanced the server's `checked_at`
to `18:38:13Z` and `18:38:30Z`. That value is written in exactly one place —
`finalize_connection_check` in `neev/agents/status.rs`, and only on a `Supported`
outcome — which is reached only after `adopt_status_epoch`, an admission bound to
the exact generation and storage handle, a supervisor round-trip that verifies the
stored credential against the provider, and a re-proof that the row has not moved.
A check that cannot be performed is a `503`. So the two advances are the probe's own
receipt: the control does what it says.

Two things to record so this is not re-litigated:

* the route deliberately writes **no operation row**, so an absent
  `neev_agent_operations` entry is not evidence a check did not run, and
  `neev_agent_status_checks` holds single-use admission tickets that are consumed
  on finalize. `checked_at` is the durable signal.
* the check produced **no visible UI change**, because a successful check on an
  already-connected row renders the same string the row already showed. The probe
  is honest; its feedback is invisible. Fixing that needs a new i18n key, so it is
  left to a coordinated UI change rather than done here.

## What is still unverified

**Runtime is not proven.** This establishes sign-in, credential persistence and
account verification. It does not establish that a Codex *turn* runs: no agent turn
has been executed against this connection, and `("codex","run")` endpoints remain
`confirmed=False` pending an isolated probe. A connected row is not a claim about
quota, entitlement, or runtime egress — `prolite` in particular is accepted as a
paid tier for display and connect only.

The Test-connection probe verifies a stored credential; it is not a turn either, so
"checked" must not be reported as "runtime verified".
