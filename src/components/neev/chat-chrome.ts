/**
 * The chrome both chat surfaces share — the team chat and the agent chats —
 * so a person's words and the composer look the same wherever they appear.
 */

/** A person's words: the warm brown paper, never the brand purple. */
export const CHAT_USER_SURFACE =
  "rounded-2xl border border-chat-user-foreground/10 bg-chat-user-surface px-3.5 py-2 text-chat-user-foreground"

/**
 * The composer box. A focus accent in the agent mark's hue: a soft halo, not
 * an outline, because this box is on screen at all times.
 */
export const CHAT_COMPOSER =
  "rounded-xl border border-border/70 bg-background p-2 shadow-sm focus-within:border-chat-accent/50 focus-within:ring-2 focus-within:ring-chat-accent/20"

/** The composer while a file is dragged over it. */
export const CHAT_COMPOSER_DROP =
  "border-chat-accent ring-2 ring-chat-accent/25"
