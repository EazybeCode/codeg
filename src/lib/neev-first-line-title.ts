/**
 * A name taken from the first line of what was written.
 *
 * Shared by documents and team chats so the two never disagree about what
 * "named from its content" means. Markdown heading markers are stripped — a
 * document opening with `# Launch plan` is called "Launch plan" — and the
 * result is cut to the server's 100-character limit measured in whole
 * characters, not UTF-16 units, so an emoji counts once.
 */
export function firstLineTitle(text: string, limit = 60): string {
  for (const raw of text.split(/\r?\n/)) {
    const line = raw
      .replace(/^\s*#{1,6}\s+/, "")
      .replace(/\s+/g, " ")
      .trim()
    if (!line) continue
    const characters = [...line]
    return characters.length <= limit
      ? line
      : `${characters
          .slice(0, limit - 1)
          .join("")
          .trimEnd()}…`
  }
  return ""
}
