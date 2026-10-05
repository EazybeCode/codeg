import { act, fireEvent, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, beforeEach, expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"
import { NativeMarkdown } from "./native-markdown"
import { COPIED_MS } from "./native-copy"

const writeText = vi.fn()

beforeEach(() => {
  writeText.mockReset().mockResolvedValue(undefined)
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  })
})

afterEach(() => {
  vi.useRealTimers()
})

function show(ui: React.ReactNode) {
  return render(
    <NextIntlClientProvider locale="en" messages={messages}>
      {ui}
    </NextIntlClientProvider>
  )
}

const REPLY =
  "Here is the fix:\n\n```ts\nconst a = 1\n  return `x`\n```\n\n- first\n- second with `inline`\n\n```\nplain\n```\n"

it("adds nothing by default, so documents and knowledge pages are unchanged", () => {
  show(<NativeMarkdown markdown={REPLY} />)
  expect(screen.queryAllByRole("button")).toHaveLength(0)
})

it("adds nothing while the reply is streaming", () => {
  show(<NativeMarkdown markdown={REPLY} streaming copy="message" />)
  expect(screen.queryAllByRole("button")).toHaveLength(0)
})

it("gives each code block one Copy button that copies its raw code", async () => {
  show(<NativeMarkdown markdown={REPLY} copy="code" />)
  const buttons = screen.getAllByRole("button", { name: "Copy code" })
  expect(buttons).toHaveLength(2)
  await act(async () => fireEvent.click(buttons[0]))
  expect(writeText).toHaveBeenLastCalledWith("const a = 1\n  return `x`")
  await act(async () => fireEvent.click(buttons[1]))
  expect(writeText).toHaveBeenLastCalledWith("plain")
  // No whole-reply button in `code` mode, and none per list item.
  expect(screen.queryByRole("button", { name: "Copy" })).toBeNull()
  for (const item of screen.getAllByRole("listitem"))
    expect(item.querySelector("button")).toBeNull()
})

it("copies the whole reply as its Markdown source from one button after it", async () => {
  show(<NativeMarkdown markdown={`\n${REPLY}\n\n`} copy="message" />)
  const all = screen.getAllByRole("button")
  expect(all).toHaveLength(3) // two code blocks + one for the reply
  await act(async () =>
    fireEvent.click(screen.getByRole("button", { name: "Copy" }))
  )
  const copied = writeText.mock.calls[
    writeText.mock.calls.length - 1
  ]?.[0] as string
  expect(copied).toBe(REPLY.trimEnd())
  // Source, not the rendered UI: no button names, no "Copied" text.
  expect(copied).not.toMatch(/Copy code|Copied/)
  // Never a button inside a button.
  for (const button of all) expect(button.querySelector("button")).toBeNull()
})

it("says Copied, announces it, and goes back to Copy", async () => {
  vi.useFakeTimers()
  show(<NativeMarkdown markdown={"```\nx\n```"} copy="code" />)
  const button = screen.getByRole("button", { name: "Copy code" })
  await act(async () => fireEvent.click(button))
  expect(button).toHaveAccessibleName("Copied")
  expect(button).toHaveAttribute("data-copy-state", "copied")
  expect(button.querySelector("[aria-live=polite]")).toHaveTextContent("Copied")
  act(() => vi.advanceTimersByTime(COPIED_MS))
  expect(button).toHaveAccessibleName("Copy code")
})

it("says so when the browser refuses, instead of claiming it copied", async () => {
  writeText.mockRejectedValue(new Error("denied"))
  show(<NativeMarkdown markdown={"```\nx\n```"} copy="code" />)
  const button = screen.getByRole("button", { name: "Copy code" })
  await act(async () => fireEvent.click(button))
  expect(button).toHaveAccessibleName("Copy failed")
})

it("gains its buttons once, when a streamed reply completes", () => {
  const { rerender } = show(
    <NativeMarkdown markdown={REPLY} streaming copy="message" />
  )
  expect(screen.queryAllByRole("button")).toHaveLength(0)
  rerender(
    <NextIntlClientProvider locale="en" messages={messages}>
      <NativeMarkdown markdown={REPLY} copy="message" />
    </NextIntlClientProvider>
  )
  expect(screen.getAllByRole("button")).toHaveLength(3)
})

it("offers no reply button for an empty reply", () => {
  show(<NativeMarkdown markdown={"  \n"} copy="message" />)
  expect(screen.queryAllByRole("button")).toHaveLength(0)
})
