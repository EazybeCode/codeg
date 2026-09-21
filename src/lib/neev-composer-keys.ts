/**
 * Enter sends, Shift+Enter starts a new line.
 *
 * The one subtlety worth a shared helper is composition. While an input method
 * is open — Japanese kana being converted to kanji, Chinese pinyin being
 * chosen, Korean jamo being assembled — Enter means "accept this candidate",
 * not "send". Acting on it would fire off a half-finished word and leave the
 * writer looking at an empty box. Browsers say so in two different ways
 * depending on the event, so both are checked.
 */
export function sendsOnEnter(
  event: {
    key: string
    shiftKey: boolean
    isComposing?: boolean
    nativeEvent?: { isComposing?: boolean; keyCode?: number }
  },
  options: { busy?: boolean } = {}
): boolean {
  if (event.key !== "Enter" || event.shiftKey) return false
  if (options.busy) return false
  if (event.isComposing || event.nativeEvent?.isComposing) return false
  // Safari and older WebKit report an in-progress composition only as this
  // legacy key code, with `isComposing` unset.
  if (event.nativeEvent?.keyCode === 229) return false
  return true
}
