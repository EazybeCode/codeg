/**
 * Marking a mention read when the conversation is actually on screen.
 *
 * Following a notice is a REQUEST to go somewhere, not an arrival. The
 * workspace can ask an open panel first — an unsent message is a question, and
 * the reader may answer "stay" — and the strip can move while that question is
 * up. A receipt written when the link was clicked would therefore clear the one
 * pointer to something nobody ended up seeing.
 *
 * So the notice is STAGED here, and claimed by the chat panel once it is
 * showing that conversation. Nothing is claimed if the navigation is declined,
 * refused, or abandoned: the entry simply sits until that chat is opened, which
 * is the moment the reader genuinely sees what they were named in.
 *
 * Entries are keyed by session scope as well as by chat. A scope is one person
 * in one organization, so a receipt staged under one can never be claimed by
 * another — the same reason the indicator itself remounts per scope.
 */

/** scope → chat id → the messages waiting to be marked read there. */
const staged = new Map<string, Map<string, Set<string>>>()
const readListeners = new Set<() => void>()
const stageListeners = new Set<() => void>()
/**
 * Bumped whenever what is staged changes.
 *
 * A panel cannot re-check on a prop it does not have: following a notice for a
 * conversation ALREADY open and in front changes nothing about that panel, so
 * without a version to watch it would sit there with an unclaimed receipt until
 * something else re-rendered it.
 */
let version = 0

export function stagedVersion(): number {
  return version
}

export function subscribeStagedMentions(listener: () => void): () => void {
  stageListeners.add(listener)
  return () => {
    stageListeners.delete(listener)
  }
}

function changed(): void {
  version += 1
  for (const listener of [...stageListeners]) listener()
}

/** Remember a notice to mark read once its conversation is shown. */
export function stageMentionRead(
  scopeKey: string,
  chatId: string,
  messageId: string
): void {
  const byChat = staged.get(scopeKey) ?? new Map<string, Set<string>>()
  staged.set(scopeKey, byChat)
  const ids = byChat.get(chatId) ?? new Set<string>()
  byChat.set(chatId, ids)
  if (ids.has(messageId)) return
  ids.add(messageId)
  changed()
}

/** Forget one, for a navigation that was refused before it began. */
export function dropMentionRead(
  scopeKey: string,
  chatId: string,
  messageId: string
): void {
  const ids = staged.get(scopeKey)?.get(chatId)
  if (!ids?.delete(messageId)) return
  if (ids.size === 0) staged.get(scopeKey)?.delete(chatId)
  changed()
}

/**
 * Take what is waiting for this conversation. Taking is final: a receipt is
 * claimed once, and a failure to record it leaves the count stale rather than
 * marking a second conversation read by accident.
 */
export function claimMentionReads(scopeKey: string, chatId: string): string[] {
  const ids = staged.get(scopeKey)?.get(chatId)
  if (!ids || ids.size === 0) return []
  staged.get(scopeKey)!.delete(chatId)
  changed()
  return [...ids]
}

/** Tell any indicator that a receipt was recorded, so its count can catch up. */
export function notifyMentionsRead(): void {
  for (const listener of [...readListeners]) listener()
}

export function subscribeMentionsRead(listener: () => void): () => void {
  readListeners.add(listener)
  return () => {
    readListeners.delete(listener)
  }
}
