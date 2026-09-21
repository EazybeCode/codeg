import { expect, it } from "vitest"
import { firstLineTitle } from "./neev-first-line-title"

it("names a document from its first written line", () => {
  expect(firstLineTitle("Launch plan\nrest")).toBe("Launch plan")
  // A heading names the document by its text, not by its markup.
  expect(firstLineTitle("# Launch plan")).toBe("Launch plan")
  expect(firstLineTitle("###### Deep")).toBe("Deep")
  // Leading blank lines are not the first line.
  expect(firstLineTitle("\n\n  Actual start")).toBe("Actual start")
  expect(firstLineTitle("   \n\t\n")).toBe("")
  expect(firstLineTitle("")).toBe("")
})

it("cuts long names by characters, not by UTF-16 units", () => {
  const long = "a".repeat(80)
  expect([...firstLineTitle(long)]).toHaveLength(60)
  expect(firstLineTitle(long).endsWith("…")).toBe(true)
  // An emoji is one character to a reader and two units to JavaScript; the
  // cut must not split it in half.
  const emoji = "🙂".repeat(80)
  const cut = firstLineTitle(emoji)
  expect([...cut]).toHaveLength(60)
  expect(cut.includes("\uFFFD")).toBe(false)
})

it("flattens whitespace so a name is one line", () => {
  expect(firstLineTitle("A   spaced\tname")).toBe("A spaced name")
})
