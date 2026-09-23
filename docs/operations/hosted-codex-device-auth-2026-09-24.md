# Hosted Codex device-auth: why a completed device code left GPT disconnected

Status: root cause confirmed against the live deployment; both fixes landed with
regression tests. **Not yet proven end to end** — see "What is still unverified".

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

## What is still unverified

No successful hosted login has been observed. The denial and the code path it
blocks are directly evidenced, but "adding `chatgpt.com` makes the flow complete"
is unproven until an attempt runs on a rebuilt agent image. A further host may
surface after the `chatgpt.com` call succeeds — that is exactly how the Claude
allowlist grew to five. The diagnostic line names any next denied host, so one live
attempt resolves it.

Also unverified: whether `prolite` carries Codex runtime quota. The projection gates
display and connect only; it is not a claim about usage rights, and a supported
projection has never implied entitlement.
