# Pinned Codex login protocol implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Execute the scoped task with TDD and review.

**Goal:** Implement the narrow, memory-only Codex managed-login protocol boundary needed by the approved personal-agent supervisor.

**Architecture:** A Python protocol module validates bounded provider frames and emits only fixed account operations. It owns request correlation and transient login state, not authentication authority, containers, credential files, HTTP, or execution. A future supervisor supplies the confined transport and current organization/member/generation checks.

**Tech Stack:** Python 3.9-compatible standard library, unittest; official Codex 0.154.0 generated schema.

**Spec:** `docs/superpowers/specs/2026-09-09-personal-agent-connections-design.md`, approved personal-agent connection stage. The pinned schema discovery is recorded outside Git at `/Users/air1/conductor/workspaces/new-idea-v1/rio-de-janeiro/.context/neev-provider-schema-20260913.md`.

## Global Constraints

- Preserve native identity/privacy and `runtimeEnabled:false`; no legacy runtime.
- No network, subprocess, container, real account, credential, database, cloud or push operation from this task.
- No API-key, external-token, arbitrary-method, thread, turn, command, plugin or filesystem interface.
- Codes and account labels remain memory-only, absent from exception messages, repr, logs, test output and documentation examples containing real data.
- Maximum provider frame and retained partial buffer: 65,536 bytes. Reject invalid UTF-8, duplicate JSON keys, non-finite numbers and malformed/non-object envelopes.
- Native account authority and final generation/session verification remain outside this component; parsed provider state never grants Neev membership or permission.
- Preserve unrelated edits, operator artifacts and existing provider/custody modules.

### Task 1: Implement the bounded protocol and managed-login state machine

**Files:**
- Create `scripts/operations/neev_codex_login_protocol.py`.
- Create `scripts/tests/test_neev_codex_login_protocol.py`.
- Create `docs/operations/neev-codex-login-protocol.md`.

**Interfaces:**
- `ProtocolError(code)` exposes only fixed `invalid`, `overflow`, `unsupported`, `stale`, `conflict`, `provider_failed` error codes.
- `CodexLoginProtocol()` starts uninitialized, with no persisted state or I/O.
- `initialize()` returns one newline-terminated JSON bytes request with monotonically increasing integer ID; fixed clientInfo `name=neev`, `title=Neev`, `version=0.1.0`, no experimental capabilities.
- `read_account(refresh=False)`, `start_login()`, `cancel_login()`, `logout()` each return a fixed typed request. A single request may be in flight. `refresh` must be a real bool. Only initialized sessions may send these operations. `start_login` uses only `type=chatgptDeviceCode`; cancel uses the current internal loginId.
- `receive(chunk: bytes) -> list[Event]` incrementally parses NDJSON. Events are frozen dataclasses or equivalent typed projections with secret-bearing fields excluded from repr. A successful initialize response produces an event carrying the exact `initialized` notification bytes for its caller to write; no general outbound builder is exposed.
- `close()` irreversibly invalidates the protocol instance and clears pending request, partial bytes and transient login state. A new instance represents a fresh transport; it cannot recover previous codes.
- Outbound or inbound invalid state fails with a fixed error, never echoes provider content. Fatal framing/protocol errors close the instance. Harmless mismatched/stale login completion is ignored or produces a non-sensitive stale event and cannot change its current login state.

The pinned schema is the authority for response parsing, not today's broader client schema:

```python
# Invented fixtures only, not real login/account evidence.
ACCOUNT = {"requiresOpenaiAuth": True,
           "account": {"type": "chatgpt", "email": "person@example.invalid", "planType": "plus"}}
DEVICE = {"type": "chatgptDeviceCode", "loginId": "12345678-1234-4234-8234-123456789abc",
          "verificationUrl": "https://auth.openai.com/codex/device", "userCode": "ABCD-1234"}
COMPLETED = {"method": "account/login/completed", "params":
             {"loginId": DEVICE["loginId"], "success": True, "error": None}}
```

Response requirements:

- Integer request IDs must match the single pending request exactly; bool/string IDs, unsolicited responses, simultaneous result/error, and duplicate replies cannot settle a request. Never expose raw error messages or data.
- Initialize validates an object result with bounded nonempty `userAgent` string; permit optional bounded `platformFamily` and `platformOs` strings according to the documented schema, reject other result keys. This is handshake validation, not binary provenance proof.
- Account read requires `requiresOpenaiAuth` bool. `account` may be absent/null (disconnected), exact `apiKey` object (unsupported), exact `amazonBedrock` object with optional bool `usesCodexManagedCredentials` (unsupported), or `chatgpt` with required email string/null and planType string. Reject malformed/unknown variants. Do not forward provider-specific fields.
- Only `requiresOpenaiAuth=True` plus a verified chatgpt account with plan in `plus`, `pro`, `team`, `business`, `enterprise`, `edu` produces a supported subscription projection. Other well-formed modes/plans yield unsupported, not a connected state. This conservative initial profile is not a claim of quota, enabled execution, or exhaustive provider plans; document unsupported plans explicitly.
- Email null is valid; otherwise a bounded printable string, maximum254 Unicode codepoints, no control characters. It is a private display label, never authority. Labels are not logged/repr'd.
- Device result has exactly type/loginId/verificationUrl/userCode. loginId is a bounded nonempty ASCII opaque string (maximum128; letters/digits/underscore/hyphen only). userCode is1–64 uppercase ASCII letters/digits/hyphens. verificationUrl must equal the literal fixture URL, with no query/fragment/userinfo/port/trailing slash or redirects.
- Completion permits success bool, optional/null loginId and error, optional/null onboardingEntrypoint=`life_sciences`; all unexpected fields/types reject. Raw error text is discarded. Only exact current loginId may settle completion. A matched success emits `account_check_required`, NEVER `connected`; subsequent explicit `read_account(refresh=True)` supplies independent status. Cancellation/logout invalidates current login immediately when the request is generated, before provider acknowledgement; late completion cannot revive it. Preserve the canceled ID internally only to encode the fixed cancel request.
- Cancel response is exact `status=canceled|notFound`; logout response is exactly an empty object. Failure leaves the login unusable, not resurrected.
- `account/updated` notifications may be safely ignored after bounded envelope/schema validation; they do not prove login. Unknown notifications and server-initiated requests must fail closed without echoing content. No unbounded queued events or retained successful frame history: process a bounded chunk and return current events. If a caller provides a chunk over65,536 bytes, fail overflow rather than allocate an arbitrarily large event list.

- [ ] **Step 1: Write behavioral tests before implementation.**

Use the existing importlib loading style in credential-store tests. Exercise the real protocol with invented byte frames, without mocking its parsers. For example:

```python
protocol = module.CodexLoginProtocol()
request = json.loads(protocol.initialize())
self.assertEqual(request["method"], "initialize")
events = protocol.receive(json.dumps({"id": request["id"], "result": {"userAgent": "invented-profile"}}).encode() + b"\n")
self.assertEqual(json.loads(events[0].outbound), {"method": "initialized", "params": {}})
login = json.loads(protocol.start_login())
self.assertEqual(login["params"], {"type": "chatgptDeviceCode"})
```

Cover initialization order/repetition, supported/null/unsupported accounts, strict bool/shape rules, URL attacks, fragmented UTF-8/NDJSON, duplicate keys, exact and over-limit frames/chunks, raw-error secret canary exclusion, foreign response IDs, provider error, two concurrent requests, correct/foreign completion, success requiring independent account read, cancel-before-late-success, logout-before-late-success, and irreversible close/restart loss of pending code. No version-string-only tests and no weakened assertions.

- [ ] **Step 2: Run new tests and record the missing implementation failures.**

```sh
python3 -m unittest discover -s scripts/tests -p 'test_neev_codex_login_protocol.py' -v
```

- [ ] **Step 3: Implement the smallest memory-only protocol satisfying these interfaces.**

Use `json.loads(..., object_pairs_hook=..., parse_constant=...)`, incremental bytes buffering bounded before decode, exact DTO validation and fixed error codes. Clear secret-bearing internal state on terminal invalidation. No subprocess/async transport or dependency is required for this component.

- [ ] **Step 4: Verify new and adjacent credential tests, then document exact boundaries.**

```sh
python3 -m unittest discover -s scripts/tests -p 'test_neev_codex_login_protocol.py' -v
python3 -m unittest discover -s scripts/tests -p 'test_neev_credential_*.py'
git diff --check
```

Operations documentation must distinguish protocol fixture acceptance from real provider, supervisor, egress, encrypted-storage integration and live runtime acceptance. Cite the approved spec, schema probe and official documentation `https://learn.chatgpt.com/docs/app-server` and `https://learn.chatgpt.com/docs/auth`. No fresh real provider call is required or allowed.

- [ ] **Step 5: Self-review and commit only the three task files.**

```sh
git add scripts/operations/neev_codex_login_protocol.py scripts/tests/test_neev_codex_login_protocol.py docs/operations/neev-codex-login-protocol.md
git commit -m "feat(neev): add bounded managed Codex login protocol"
```

Report exact tests/TDD evidence and commit, any deliberately unsupported schema/profile cases, and remaining integration gates. This task must not claim personal-agent connections are usable in the application.
