# Typography and elapsed timer — 2026-09-24

Source: `a25be2b3`.

## Changes

- Retain the existing Inter Variable UI font and appearance preferences.
  Native chat body changes from 14px/28px to 14px/23.1px, with a compact
  semibold heading scale. Small shell labels get explicit line heights.
- The same-origin La Suite frame defaults its editor body to the already
  available Inter Variable family, matching its title. One idempotent scoped
  style is applied on load and readiness; no content writes, engine rebuild,
  font download, or change to explicit descendant font choices.
- Running turns display tenths of seconds, including trailing zero, at 100ms
  cadence. Only the small clock component updates. Monotonic elapsed time
  avoids backwards wall-clock adjustments. Timer is excluded from live-region
  announcements and unmounts when a turn stops.
- Completed turns retain a duration, including turns without tools. Stored
  timestamps are whole seconds, so historical/final duration does not invent
  subsecond precision.

## Validation

- Root independently ran the four directly affected component/helper suites:
  107 tests passed, process exit 0.
- Broader Neev run: 24 files, 401 assertions passed, but process exit 1 due to
  seven previously reproduced BlockNote/jsdom unhandled errors. One earlier
  assertion flake passed rerun. Do not describe this as a clean full suite.
- ESLint exit 0, Node 22 production export exit 0, diff whitespace check clean.
- Root inspected native Conductor screenshot: live timer showed `37.2s`.
  Exact Conductor font family was not measured or claimed.
- Live computed styles verified chat paragraphs 14px/23.1px and h3 14px/600;
  document editor body now starts with `Inter Variable`, size 16px.
- A synthetic live GPT-5.6-Luna turn ran `sleep 8`, with timer observations
  `8.1s`, `16.5s`, then `18s Completed` and `TIMER_OK`. This is a functional
  timer check, not a provider latency benchmark. User content was not edited.

## Deployment

Image `neev-native-agent:ux-docs-speed-type-timer-20260924`.
Workspace SHA256 `541ebe48723c9a54c160d8d84620797b7d7db56ba37dbe6f370d62fa7b02f810`.
Server SHA256 unchanged: `a30ff21c45bf3f454136ad535022b5874ca6b8138285bd3a0265fca3beb2ca23`.
Previous image `neev-native-agent:ux-docs-speed-smooth-20260924` retained.
Predeploy zero active sign-ins/running turns; postdeploy health 200 and exact
UI hash verified. Only compose image changed; creation remains enabled.

User explicitly authorized direct deploy/push. Correct remote is workos to
EazybeCode/neev. Background: [org-memory: wiki/engineering/workos.md]; current
source/deployment take precedence over the older codeg reference there.
