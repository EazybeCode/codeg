import { expect, it } from "vitest"
import { sendsOnEnter } from "./neev-composer-keys"

const press = (over: Partial<Parameters<typeof sendsOnEnter>[0]> = {}) => ({
  key: "Enter",
  shiftKey: false,
  nativeEvent: {},
  ...over,
})

it("sends on Enter and makes a new line on Shift+Enter", () => {
  expect(sendsOnEnter(press())).toBe(true)
  expect(sendsOnEnter(press({ shiftKey: true }))).toBe(false)
  expect(sendsOnEnter(press({ key: "a" }))).toBe(false)
})

it("never sends while an input method is composing", () => {
  // Enter means "accept this candidate" to someone typing Japanese, Chinese or
  // Korean. Sending then fires off a half-finished word and empties the box.
  expect(sendsOnEnter(press({ isComposing: true }))).toBe(false)
  expect(sendsOnEnter(press({ nativeEvent: { isComposing: true } }))).toBe(
    false
  )
  // Safari reports an in-progress composition only as this legacy key code.
  expect(sendsOnEnter(press({ nativeEvent: { keyCode: 229 } }))).toBe(false)
})

it("does not send while something is already being sent", () => {
  expect(sendsOnEnter(press(), { busy: true })).toBe(false)
  expect(sendsOnEnter(press(), { busy: false })).toBe(true)
})
