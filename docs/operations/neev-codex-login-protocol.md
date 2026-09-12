# Neev Codex managed-login protocol

Status: protocol component implemented against invented frames only. This is not
provider acceptance, a connected application feature, or an enabled agent runtime.

## Purpose and ownership

`scripts/operations/neev_codex_login_protocol.py` is a memory-only boundary around
the pinned Codex app-server account protocol. It emits only initialize,
`account/read`, managed device-login start/cancel, and logout requests. It parses
bounded NDJSON into frozen, allowlisted projections and correlates one response at
a time. It has no transport, subprocess, filesystem, database, logging, credential,
container, browser, OAuth, network, or arbitrary-RPC interface.

The caller must write the fixed `initialized` notification returned by the
successful handshake event. No general outbound message builder is exposed.
Device codes, login identifiers, verification URLs, partial frames, and private
account labels live only in the protocol instance or returned transient event.
Secret-bearing event fields are excluded from representations. `close()` clears
internal transient state and is irreversible; restart requires a new instance and
cannot recover a pending code.

The approved architecture and lifecycle are defined by
[`2026-09-09-personal-agent-connections-design.md`](../superpowers/specs/2026-09-09-personal-agent-connections-design.md).
The exact response shapes implemented here come from the local, non-Git schema
record at
`/Users/air1/conductor/workspaces/new-idea-v1/rio-de-janeiro/.context/neev-provider-schema-20260913.md`,
which records a credential-free probe of the provenance-verified Codex 0.154.0
Linux musl binary. The public protocol background is the official
[app-server documentation](https://learn.chatgpt.com/docs/app-server) and
[authentication documentation](https://learn.chatgpt.com/docs/auth).

## State and validation rules

Initialization is one-shot per instance and must finish before account operations.
Only one request may be in flight. Request IDs are monotonically increasing
integers, and responses settle only the exact pending integer ID; boolean or string
IDs, unsolicited or duplicate responses, and result/error combinations fail
closed. Provider error messages and data are discarded and surface only the fixed
`provider_failed` code.

Input chunks and complete frames are capped at 65,536 bytes. A frame's newline is
part of that limit. Parsing rejects invalid UTF-8, duplicate object keys,
non-finite numbers (including exponent overflow), excessive JSON nesting,
malformed envelopes, unknown notifications, server-initiated requests, and
unexpected object keys or variants. Fatal framing or protocol errors close the
instance and clear pending, partial, and login state. `account/updated` is validated
and ignored; it never establishes authentication. Its pinned object has no required
fields and permits only optional nullable `authMode` and `planType`. `authMode` is
limited to `apikey`, `chatgpt`, `chatgptAuthTokens`, `headers`, `agentIdentity`,
`personalAccessToken`, `bedrockApiKey`, and `bedrockAccessKeys`; `planType` is
limited to the pinned plan enum listed below. No notification value is projected.

Only the literal managed flow `chatgptDeviceCode` is accepted. Its verification
URL must be exactly `https://auth.openai.com/codex/device`; queries, fragments,
userinfo, ports, redirects, alternate hosts, schemes, or trailing slashes are
rejected. Login and user-code syntax and sizes are bounded. Completion must match
the current login ID. Foreign or late completion is harmless and cannot change
state. Cancellation and logout invalidate the current ID before their requests
leave the component. A matched successful completion emits only
`account_check_required`; the caller must issue a new `account/read` with
`refreshToken=true` before considering the account projection.

## Conservative subscription profile

A supported projection requires all of the following:

- `requiresOpenaiAuth` is exactly `true`;
- the account is the strict `chatgpt` variant;
- the email is null or a printable private display label of at most 254 Unicode
  codepoints; and
- `planType` is one of `plus`, `pro`, `team`, `business`, `enterprise`, or `edu`.

Absent/null accounts project as disconnected. Exact API-key and Amazon Bedrock
accounts, `requiresOpenaiAuth=false`, and well-formed ChatGPT plans outside the six
recognized values project as unsupported, never connected. Account-read
`planType` requires only a JSON string and is not restricted to the notification
enum; the global frame cap bounds its encoded size. Unsupported projections do
not retain the raw plan string. Unsupported values explicitly include future plan
strings and the pinned schema's `free`, `go`, `prolite`,
`self_serve_business_prolite`, `self_serve_business_usage_based`, `ent26`,
`enterprise_cbp_automation`, `enterprise_cbp_usage_based`, `edu_plus`, `edu_pro`,
and `unknown` values. A profile extension requires review; enum membership alone
does not prove subscription entitlement. Email is a label, not identity or
authorization. A supported projection does not claim quota, runtime readiness, or
permission to execute.

## Remaining acceptance gates

The tests use invented frames and establish only deterministic protocol behavior.
Before real login can be enabled, separate work must verify the pinned provider
binary and image, the restricted supervisor and timeouts, exact provider behavior,
enforced egress, isolated encrypted credential storage and recovery, restart and
cleanup behavior, and real-user acceptance. The application must also recheck the
current organization, member, connection generation, and native account status.
Those checks are outside this component.

No real provider call, credential access, supervisor integration, storage
integration, or runtime activation was performed for this implementation. In
particular, personal-agent connections are not thereby available in the
application, and `runtimeEnabled:false` remains the delivery boundary.

Context: [org-memory: wiki/engineering/workos.md]. The page describes the shared
WorkOS architecture and its legacy runtime boundary; current code and the approved
personal-agent specification remain authoritative for this protocol.
