# Personal agent provider protocol findings

Checked September9,2026. Read-only documentation research, not a tested client
profile, provider-terms acceptance or enabled integration. No login was started.

## Codex

The official app-server defaults to stdio. Initialize with truthful Neev client
metadata, then send the initialized notification before account methods. Leave
experimental capabilities disabled. Managed ChatGPT device login returns a
login identifier, verification URL and user code. Correlate the completion event
to that identifier; independently check account/read afterward. Requesting
refreshToken=true forces refresh in managed mode. A chatgpt account reports its
plan type; email can be null. A false requiresOpenaiAuth flag is not evidence of a
personal ChatGPT subscription. Cancel uses the matching login ID. Logout and its
account update do not prove provider-wide token revocation.
[Official app-server documentation](https://learn.chatgpt.com/docs/app-server#auth-endpoints).

Implementation decisions under the approved spec: only initialization and the four
account/login/status/logout operations belong to this stage. Never forward general
RPC, external-token refresh requests or account-credit/email operations. Validate
the device verification URL against the pinned auth.openai.com/codex/device
profile, with no userinfo, fragment, port override or unexpected query. Completion
alone is insufficient; require the reviewed managed subscription account shape.
Use a safe generic account label if a verified account has no email. Unknown plan
types require profile review rather than assuming paid entitlement.

## Claude Code

The dedicated auth login/logout/status commands are documented; status emits JSON
and exits0 when logged in or1 otherwise. The native login command also supports
Console billing and SSO options. These built-in choices must not be modified or
hidden; an unsupported account mode does not become a Neev subscription connection.
[Official CLI reference](https://code.claude.com/docs/en/cli-reference).

The retrieved reference does not define the complete status JSON schema or all
terminal login events. Do not invent parser fields from these docs. Before enabling
a profile, inspect an exact verified official binary in an isolated credential-free
environment, capture invented/scrubbed protocol fixtures, and then perform separate
real-user acceptance. A broad banner regex is not an authentication check.

## Remaining profile gates

Both providers still need pinned Linux client versions/image digests, exact output
schemas, timeout/cancellation behavior, controlled environment/config, encrypted
private storage and enforced provider-only egress tested with actual containers.
Do not inherit or inspect the Mac operator's authenticated home to obtain fixtures.
Read-only research does not demonstrate hosted-use compliance or tenant isolation.

Context: [org-memory: wiki/engineering/workos.md]. The legacy ACP registry is not
the selected personal native-client authentication boundary.
