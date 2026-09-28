import type { Command } from "@tiptap/pm/state"

/** Continue a numbered paragraph without consuming Enter outside a list. */
export const continueNumberedList: Command = (state, dispatch) => {
  const { selection } = state
  if (!selection.empty || !selection.$from.parent.isTextblock) return false
  const paragraph = selection.$from.parent
  const text = paragraph.textBetween(0, paragraph.content.size, "", "\uFFFC")
  const prefix = /^(\d+)\.[ \t]/.exec(text)
  if (!prefix) return false
  const next = Number(prefix[1]) + 1
  if (!Number.isSafeInteger(next)) return false
  if (!dispatch) return true

  const tr = state.tr
  if (!text.slice(prefix[0].length).trim()) {
    const start = selection.$from.start()
    tr.delete(start, start + prefix[0].length)
  } else {
    tr.split(selection.from)
    tr.insertText(`${next}. `)
  }
  dispatch(tr.scrollIntoView())
  return true
}
