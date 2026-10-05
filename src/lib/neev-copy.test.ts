import { afterEach, expect, it, vi } from "vitest"
import {
  codeFromPre,
  copyToClipboard,
  messageSource,
  nodeText,
} from "./neev-copy"

const text = (value: string) => ({ type: "text", value })
const pre = (code: string, language = "ts") => ({
  type: "element",
  tagName: "pre",
  children: [
    {
      type: "element",
      tagName: "code",
      properties: { className: [`language-${language}`] },
      children: [text(code)],
    },
  ],
})

afterEach(() => {
  vi.unstubAllGlobals()
})

it("copies a fenced block's raw code exactly, minus the fence's own newline", () => {
  expect(codeFromPre(pre("const a = 1\n  const b = `x`\n\tend\n"))).toBe(
    "const a = 1\n  const b = `x`\n\tend"
  )
  // Only ONE trailing newline belongs to the fence; a blank last line stays.
  expect(codeFromPre(pre("x\n\n"))).toBe("x\n")
  expect(codeFromPre(pre(""))).toBe("")
})

it("reads text in document order and never anything but text nodes", () => {
  const node = {
    type: "element",
    children: [
      text("a"),
      {
        type: "element",
        children: [text("b"), { type: "comment", value: "no" }],
      },
      text("c"),
    ],
  }
  expect(nodeText(node)).toBe("abc")
  expect(codeFromPre(null)).toBe("")
  expect(codeFromPre({ children: "not an array" })).toBe("")
})

it("is bounded on a pathological tree", () => {
  const children = Array.from({ length: 50 }, () => text("x".repeat(100)))
  expect(nodeText({ children }, 120).length).toBe(120)
})

it("keeps a reply's Markdown source as written, trimming only the edges", () => {
  expect(
    messageSource("\n\n# Title\n\n- **one**\n\n```js\nx()\n```\n\n  ")
  ).toBe("# Title\n\n- **one**\n\n```js\nx()\n```")
  expect(messageSource("   ")).toBe("")
})

it("reports whether the clipboard actually took the text", async () => {
  const writeText = vi.fn().mockResolvedValue(undefined)
  vi.stubGlobal("navigator", { clipboard: { writeText } })
  expect(await copyToClipboard("hello")).toBe(true)
  expect(writeText).toHaveBeenCalledWith("hello")

  vi.stubGlobal("navigator", {
    clipboard: { writeText: vi.fn().mockRejectedValue(new Error("denied")) },
  })
  expect(await copyToClipboard("hello")).toBe(false)
})
