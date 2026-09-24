# A shared response style for both providers (2026-09-24)

Replies from the two subscription providers were formatted inconsistently —
the observed cases were tables written with raw pipes instead of real newlines,
and prose whose structure varied from turn to turn. This adds one policy, owned
by us, carried into every paid turn on each provider's own instruction channel.
The rest of the policy (no invented links, no claimed test runs) is preventive
guidance, not a record of incidents.

## What was researched first

Conductor documents how an agent's behaviour is shaped in a repository —
`AGENTS.md` / `CLAUDE.md`, repository preferences, skills. We did not find an
exact universal response prompt in the public docs we reviewed; the bare domain
does not serve, `https://www.conductor.build/docs/reference/agent-behavior`
does. Claude's output-styles documentation describes session-level style
instructions, which guide a reply rather than guarantee its shape. So what
ships here is our own policy rather than a copy of an upstream one.

Sources:

- <https://www.conductor.build/docs/reference/agent-behavior>
- <https://code.claude.com/docs/en/output-styles>
- <https://learn.chatgpt.com/docs/app-server>
- <https://learn.chatgpt.com/docs/config-file/config-reference>

## The policy

`scripts/operations/neev_response_style.md` — about 300 words, UTF-8, packaged
into the agent image beside the launcher (`COPY` + `chmod 0444`). It asks for
the answer first, readable paragraphs, structure chosen to fit the request,
blank lines around headings/lists/fences, real-newline GFM tables only for
genuinely tabular comparisons, fenced code with a language label, links only to
sources that exist, and no fabricated results. It states explicitly that an
explicit request about language, length or format overrides every default, that
supplied code and quotations are not restyled, and that **nothing in it limits
how much work is done or how long an answer may be**.

## How it reaches each provider

| Provider | Channel | Where |
| --- | --- | --- |
| Claude | `--append-system-prompt` | appended after the workspace briefing |
| Codex (app-server) | `thread/start` `developerInstructions` | `neev_codex_stream_adapter.py` |
| Codex (`exec` fallback) | `-c developer_instructions=<JSON string>` | mapped to the parameter above |

`launch.response_style_argv` does this once, in `build_spec`, before the
transport rewrite — so both Codex transports derive from a single injection.
`baseInstructions` is never set: the provider's own base prompt stays and ours
is added alongside it. The Codex override is a JSON string, so newlines, quotes
and backslashes survive verbatim.

## What it will not do

- **The prompt is never parsed.** Only the canonical paid-turn shapes are
  touched (`claude -p <prompt> …`, `codex exec --json <prompt> …`) and the
  options are *walked* from a known table, never searched. A user prompt that
  is literally `--append-system-prompt`, or shaped like
  `developer_instructions="…"`, stays the prompt.
- **Workspace data is never elevated.** The briefing keeps its place and its
  content; the policy is appended after it. A briefing that merely *contains* a
  copy of the policy still receives the trusted one as a suffix.
- **Applied once.** An argv that already carries our suffix, or an existing
  `developer_instructions` override, is extended at most once — and an existing
  developer instruction is kept, not replaced.
- **Probes and logins are untouched.** The catalogue probe (`codex app-server`)
  and anything that is not a canonical turn pass through unchanged, stdin
  handling included.
- **Missing or oversized fails open, bounded.** The policy is read only from the
  launcher's own directory with an 8 KiB cap; missing, oversized or undecodable
  all mean "no policy" and the turn runs exactly as it did before. Nothing is
  read from the run workdir, and no credential volume is written.
- **An unknown argv is left alone.** An option this launcher does not know
  means the style is skipped rather than an argv rewritten on a guess.

## Limitations

Prompt-level guidance improves consistency; it does not guarantee it. A model
can still emit a malformed table or an unfenced block, so the renderer remains
the thing that has to cope — this reduces how often it must. No claim is made
that this matches any Conductor prompt: we did not find one in the public docs
we reviewed, which is not the same as establishing that none exists.

Verified by the focused suites — `test_neev_agent_launch.py`,
`test_neev_codex_stream_adapter.py`, `test_neev_agent_sandbox.py`: 107 passed.
Not deployed or validated against a live provider turn as part of this change.
