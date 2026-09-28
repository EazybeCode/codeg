# Transcript text hierarchy — 2026-09-28

In-progress prose and text preceding the final response use muted text color;
the final prose block of a completed turn uses normal foreground color and
normal body weight. Tool labels use muted color and normal weight. Markdown
structure/size and tool-error styling remain unchanged. Shared TranscriptBody
covers standalone agent sessions and team-chat agent replies.

The current protocol records text and tool offsets, not explicit reasoning/final
channels. Styling uses live/completed status and the last nonempty prose block;
it does not claim to identify or expose private model reasoning.

Regression failed before the change; 35 transcript/session tests passed afterward.
