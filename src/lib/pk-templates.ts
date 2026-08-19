/**
 * Built-in PK task templates — famous one-line prompts that are visually
 * verifiable and proven conversation starters in the AI community.
 *
 * Each template fills the launcher's task textarea (and optionally other
 * fields) in one click, lowering the barrier to starting a match.
 *
 * Sources are documented in docs/PK-ROADMAP.md (功能 1).
 */

export interface PkTaskTemplate {
  /** Stable id for the template. */
  id: string
  /** Short display label (i18n key under PkArena.templates.<id>). */
  labelKey: string
  /** Emoji shown on the template chip. */
  emoji: string
  /** The task text to fill into the textarea. */
  task: string
}

/** Built-in templates, ordered by visual impact / viral potential. */
export const PK_TEMPLATES: readonly PkTaskTemplate[] = [
  {
    id: "pelican",
    labelKey: "pelican",
    emoji: "🦤",
    task: "Generate an SVG of a pelican riding a bicycle.",
  },
  {
    id: "bouncingBall",
    labelKey: "bouncingBall",
    emoji: "⚽",
    task: "Create an HTML animation: a ball starts in the center of a triangle. Every time it hits a side it speeds up, and the shape gains an extra side (Triangle → Square → Pentagon → Hexagon …).",
  },
  {
    id: "jellyBlob",
    labelKey: "jellyBlob",
    emoji: "🫧",
    task: "Build a tiny browser toy: a jelly blob. You poke, grab, stretch it. No scoring, no level, just a satisfying blob.",
  },
  {
    id: "snake",
    labelKey: "snake",
    emoji: "🐍",
    task: "Write a Snake game in a single HTML file with keyboard controls.",
  },
  {
    id: "flappyBird",
    labelKey: "flappyBird",
    emoji: "🐤",
    task: "Write a Flappy Bird clone in a single HTML file with Canvas rendering.",
  },
  {
    id: "voiceChat",
    labelKey: "voiceChat",
    emoji: "🎙️",
    task: "Create a voice-enabled chatbot web app using the Web Speech API.",
  },
] as const
