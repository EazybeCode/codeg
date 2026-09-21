import { expect, it } from "vitest"
import { titleFromBody } from "@/components/neev/native-document"

/**
 * The shape a rich document actually has: `{ version, blocks }`, each block
 * keeping its text in `content` runs. Traversing only `content`/`children` —
 * never `blocks` — made every rich document derive an empty name, so every one
 * of them stayed `Untitled`.
 */
it("names a rich document from its first block", () => {
  expect(
    titleFromBody({
      format: "blocks",
      document: {
        version: 1,
        blocks: [
          {
            type: "heading",
            props: { level: 1 },
            content: [{ text: "Launch plan" }],
          },
          { type: "paragraph", content: [{ text: "Body text" }] },
        ],
      },
    } as never)
  ).toBe("Launch plan")
})

it("keeps formatted runs on one line", () => {
  // A heading split across a bold run is still one heading. Joining runs with
  // a newline would name the document after its first word.
  expect(
    titleFromBody({
      format: "blocks",
      document: {
        version: 1,
        blocks: [
          {
            type: "heading",
            content: [
              { text: "Launch " },
              { text: "plan", styles: { bold: true } },
              { text: " Q3" },
            ],
          },
        ],
      },
    } as never)
  ).toBe("Launch plan Q3")
})

it("skips empty leading blocks and reads nested ones", () => {
  expect(
    titleFromBody({
      format: "blocks",
      document: {
        version: 1,
        blocks: [
          { type: "paragraph", content: [] },
          {
            type: "bulletListItem",
            content: [{ text: "" }],
            children: [
              { type: "paragraph", content: [{ text: "Nested first words" }] },
            ],
          },
        ],
      },
    } as never)
  ).toBe("Nested first words")
})

it("derives nothing from a document with no text", () => {
  expect(
    titleFromBody({
      format: "blocks",
      document: { version: 1, blocks: [{ type: "paragraph", content: [] }] },
    } as never)
  ).toBe("")
})
