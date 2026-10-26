# Neev Run Reliability and OpenUI Business Surfaces Design

**Date:** 2026-10-16
**Status:** Approved for implementation
**Scope:** Neev native agent chat responses and run outcome reporting

## Problem

Live testing found two separate problems:

1. An agent run can finish with failed commands while the footer says `Completed`, and the generated answer can claim that files were created or verified even when the required generation step failed. This makes a recoverable run look successful and makes a failed run hard to diagnose.
2. Marketing and sales work often needs an interface rather than a wall of prose: KPIs, funnels, comparisons, campaign variants, timelines, and next actions. Neev currently renders assistant output as Markdown, so the reader has to reconstruct that structure themselves.

The existing Neev chat already owns conversation persistence, streamed agent activity, command traces, attachments, permissions, routing, and follow-up steering. Replacing it with a second chat surface would duplicate those responsibilities and risk losing the behavior users depend on.

## Goals

- Make run outcomes unambiguous when one or more tool steps fail.
- Keep exact command output, stderr, exit codes, and retry behavior visible.
- Prevent a final answer from presenting required artifacts as verified when the required checks failed.
- Render useful, responsive business interfaces inside the current assistant reply area.
- Stream structured UI progressively when the agent produces it.
- Fall back to the existing Markdown renderer for normal answers, unsupported agents, malformed output, and parser errors.
- Keep all generated components deterministic, allowlisted, and subject to Neev permissions.
- Preserve current threads, tool traces, attachments, copy actions, and follow-ups.

## Non-goals

- Replacing Neev's chat with OpenUI `AgentInterface`.
- Allowing model-generated arbitrary HTML, JavaScript, CSS, network requests, or browser APIs.
- Changing agent selection, Jev routing, provider authentication, or file storage in this change.
- Converting existing historical messages in bulk.

## Design

### 1. Run outcome semantics

The activity footer will compute a terminal outcome from the turn state and activity summary:

- Running: show the existing live indicator and elapsed clock.
- Finished with no failed tool results: show `Completed`.
- Finished with one or more failed tool results: show `Completed with errors` plus the existing singular/plural failure count.
- Provider/turn failure with no completed answer: show the existing failure card and exact provider detail.

The failed-step count remains visible on individual command rows. The new footer wording distinguishes “the runner stopped” from “all requested work succeeded.” Existing successful turns keep the same quiet presentation.

### 2. Verification claims

The run transcript will expose a derived `hasFailedSteps`/`failedStepCount` signal to the final-answer guard. The guard will be applied at the boundary that assembles the persisted assistant reply, not by trying to edit arbitrary prose in the client.

When required artifact steps fail, the final response must:

- state that the run completed with errors;
- include the exact failed command and stderr when available;
- identify which requested artifacts were not verified;
- label any fallback artifact explicitly as a fallback;
- avoid claims such as “created,” “verified,” or “all checks passed” for failed requirements.

If a run has only incidental non-required command failures and the requested result is demonstrably verified by later checks, the answer may describe the verified result but must still disclose the failed steps. The implementation will encode this as structured run metadata so the prompt/response layer does not need to infer it from rendered text.

### 3. Runtime preflight for artifact generation

Before an agent writes a multi-step artifact script, the runtime guidance will prefer available standard-library or installed browser tools. It will not assume Pillow, Node, Chromium, or another optional binary exists. If a dependency is needed, the agent must check for it first and either use an available fallback or report the blocker before claiming success.

This change does not install packages inside runs. It improves preflight and final verification so an unavailable dependency produces a useful failure instead of a cascade of malformed repair commands.

### 4. OpenUI integration boundary

Neev will use OpenUI's React renderer and component primitives inside the existing assistant-message component. It will not use OpenUI's full `AgentInterface` or its thread storage.

The integration will add a presentation discriminator to an assistant reply/turn:

```ts
type AssistantPresentation =
  | { kind: "markdown"; content: string }
  | { kind: "openui"; content: string; version: 1 }
```

The discriminator may be inferred during streaming for new runs, but persisted messages must carry the selected kind so historical rendering is deterministic. Existing rows without the field render as Markdown.

### 5. Controlled business component library

The first library is intentionally small and focused on marketing and sales:

- `MetricCard` and `MetricGrid`
- `Funnel`
- `ComparisonTable`
- `TrendChart`/`BarChart` with bounded data sizes
- `Timeline`
- `NextActions`
- `Callout`
- `AttachmentGallery`

Components use Zod schemas and explicit renderers. Actions are allowlisted and call existing Neev callbacks for safe operations such as opening an attachment, copying a value, or opening a related document. No component can execute a command or make an arbitrary network request.

The library will be usable in two ways:

- a prompt-generated OpenUI response for eligible marketing/sales requests;
- a deterministic renderer for stored OpenUI messages.

### 6. Streaming, errors, and fallback

OpenUI content will be fed to a streaming parser as chunks arrive. While parsing:

- already valid content remains visible;
- a compact Neev loading state is shown for unresolved content;
- parser/query errors are shown with a retry affordance only when a retry is safe;
- any unsupported or malformed response falls back to the raw Markdown answer without dropping the message.

OpenUI errors are recorded for diagnostics and do not turn a successfully completed agent run into a provider failure. If the model emits Markdown, the renderer is never invoked.

### 7. Theme and accessibility

OpenUI styles will be imported once and wrapped or overridden so Neev's existing theme remains authoritative. The component library will use Neev colors, compact spacing, keyboard-accessible controls, visible focus states, and text alternatives for charts and images. Dark and light themes must both remain readable.

## Data flow

```text
agent stream
   ├─ activity/tool events ──▶ existing trace timeline
   └─ assistant chunks ──────▶ presentation classifier
                                  ├─ markdown ──▶ NativeMarkdown
                                  └─ openui ────▶ OpenUI streaming Renderer
                                                     └─ fallback ─▶ NativeMarkdown
turn completion + tool summary ─▶ outcome/verification metadata
                                  └─ persisted with reply/turn
```

## API and compatibility

- Existing API responses remain valid when `presentation` is absent.
- New presentation metadata is optional on read and write until all clients understand it.
- The server must not accept arbitrary component names or schemas from the client; the server/client share a fixed library version and component allowlist.
- Existing attachment URLs and permission checks remain the only way to access generated files.

## Testing

Unit tests will cover:

- footer wording for zero, one, and multiple failed steps;
- failed-step metadata propagation into the final-answer guard;
- no false verification claim when required commands fail;
- Markdown fallback for malformed/unknown OpenUI components;
- streaming OpenUI rendering with partial chunks;
- action allowlisting and attachment permission checks;
- existing Markdown, trace, copy, retry, and attachment behavior.

Live smoke tests will repeat:

- text, PDF, PNG, and attachment preview generation;
- a missing-command negative path with exact stderr and recovery;
- an image-provider failure with an explicitly labelled fallback;
- a marketing prompt that renders a KPI/funnel interface;
- a sales prompt that renders a comparison/next-actions interface.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| OpenUI output is malformed during streaming | permissive parser, visible fallback, raw response retained |
| Business data leaks into generated UI | allowlisted components/actions and existing resource permissions |
| Bundle size or CSS conflicts | import the smallest packages needed, load styles once, use a scoped wrapper |
| Existing messages change appearance | absent presentation metadata always uses current Markdown renderer |
| Agent overclaims after a partial failure | structured failed-step metadata and explicit final-answer guard |

## Decision

Implement the reliability patch first, then add OpenUI behind a controlled feature path for marketing and sales responses. Promote it to the default only after the live smoke tests and regression suite pass.
