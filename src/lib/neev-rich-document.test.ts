import { describe, expect, it } from "vitest"
import { parseRichDocument } from "./neev-rich-document"

const run = { text: "Hello", styles: [], href: null }
const block = {
  type: "paragraph",
  id: "00000000-0000-0000-0000-000000000000",
  content: [run],
  children: [],
}
const doc = (value: unknown = block) => ({ version: 1, blocks: [value] })
const uuid = "11111111-1111-4111-8111-111111111111"
describe("bounded rich document parsing", () => {
  it("accepts a divider and an internal-image block", () => {
    expect(
      parseRichDocument(doc({ type: "divider", id: uuid, children: [] }))
        .blocks[0]
    ).toEqual({ type: "divider", id: uuid, children: [] })
    const image = {
      type: "image",
      id: uuid,
      url: "/api/neev/images/22222222-2222-4222-8222-222222222222",
      caption: "A chart",
      children: [],
    }
    expect(parseRichDocument(doc(image)).blocks[0]).toEqual(image)
  })
  it("refuses external, data, and traversal image URLs and non-empty children", () => {
    const image = (extra: Record<string, unknown>) => ({
      type: "image",
      id: uuid,
      url: "/api/neev/images/22222222-2222-4222-8222-222222222222",
      caption: "",
      children: [],
      ...extra,
    })
    for (const url of [
      "https://evil.example/x.png",
      "data:image/png;base64,AAAA",
      "/api/neev/images/../secret",
      "/other/x",
    ])
      expect(() => parseRichDocument(doc(image({ url })))).toThrow()
    expect(() => parseRichDocument(doc(image({ children: [block] })))).toThrow()
    // A divider carries no children.
    expect(() =>
      parseRichDocument(doc({ type: "divider", id: uuid, children: [block] }))
    ).toThrow()
  })
  it("accepts all native shapes and returns canonical field order", () => {
    expect(
      JSON.stringify(
        parseRichDocument(
          doc({
            children: [],
            content: [{ href: null, styles: [], text: "Hello" }],
            id: block.id,
            type: "paragraph",
          })
        )
      )
    ).toBe(
      '{"version":1,"blocks":[{"type":"paragraph","id":"00000000-0000-0000-0000-000000000000","content":[{"text":"Hello","styles":[],"href":null}],"children":[]}]}'
    )
    for (const type of [
      "paragraph",
      "bulletListItem",
      "numberedListItem",
      "quote",
    ])
      expect(parseRichDocument(doc({ ...block, type }))).toBeTruthy()
    expect(
      parseRichDocument(doc({ ...block, type: "heading", level: 3 }))
    ).toBeTruthy()
    expect(
      parseRichDocument(
        doc({ ...block, type: "checkListItem", checked: false })
      )
    ).toBeTruthy()
    expect(
      parseRichDocument(
        doc({
          type: "codeBlock",
          id: block.id,
          children: [],
          text: "x\n",
          language: "c++",
        })
      )
    ).toBeTruthy()
    expect(
      parseRichDocument(
        doc({ type: "table", id: block.id, children: [], rows: [[[run]]] })
      )
    ).toBeTruthy()
  })
  it("rejects unknown fields, unsupported blocks, controls, invalid styles and links", () => {
    for (const value of [
      { ...doc(), version: 2 },
      { ...doc(), extra: true },
      { version: 1, blocks: [] },
      doc({ ...block, type: "image" }),
      doc({ ...block, extra: 1 }),
      doc({ ...block, id: block.id.toUpperCase().replace(/^0/, "A") }),
      doc({ ...block, type: "heading", level: 4 }),
      doc({ ...block, type: "checkListItem", checked: 1 }),
      ...["\u0000", "\u0085", "\ud800"].map((text) =>
        doc({ ...block, content: [{ ...run, text }] })
      ),
      ...[
        ["italic", "bold"],
        ["bold", "bold"],
        ["code", "italic"],
        ["underline"],
      ].map((styles) => doc({ ...block, content: [{ ...run, styles }] })),
      ...[
        "javascript:alert(1)",
        "//example.com",
        "https://user@example.com",
        "https://example.com/%2520",
        "#",
        "#a%250a",
        "https://example.com/\\x",
      ].map((href) => doc({ ...block, content: [{ ...run, href }] })),
      doc({ ...block, content: [{ text: "x", styles: [] }] }),
      doc({
        type: "codeBlock",
        id: block.id,
        children: [block],
        text: "x",
        language: "",
      }),
      doc({ type: "table", id: block.id, children: [], rows: [[[run]], []] }),
      doc({
        type: "table",
        id: block.id,
        children: [],
        rows: [[[{ ...run, text: "a\u2028b" }]]],
      }),
    ])
      expect(() => parseRichDocument(value)).toThrow()
    for (const href of [
      "#section",
      "HTTPS://example.com/path?q=1",
      "https://example.com/%E2%9C%93",
    ])
      expect(
        parseRichDocument(doc({ ...block, content: [{ ...run, href }] }))
      ).toBeTruthy()
  })
  it("bounds nesting, total blocks, inline runs, tables and canonical bytes", () => {
    const id = (n: number) =>
      `00000000-0000-0000-0000-${String(n).padStart(12, "0")}`
    expect(() =>
      parseRichDocument({ version: 1, blocks: [block, block] })
    ).toThrow()
    expect(() =>
      parseRichDocument({
        version: 1,
        blocks: Array.from({ length: 1001 }, (_, n) => ({
          ...block,
          id: id(n),
        })),
      })
    ).toThrow()
    expect(() =>
      parseRichDocument(doc({ ...block, content: Array(10001).fill(run) }))
    ).toThrow()
    expect(() =>
      parseRichDocument(
        doc({ ...block, content: [{ ...run, text: "x".repeat(1048576) }] })
      )
    ).toThrow()
    expect(() =>
      parseRichDocument(
        doc({
          type: "table",
          id: block.id,
          children: [],
          rows: Array(101).fill([[run]]),
        })
      )
    ).toThrow()
    expect(() =>
      parseRichDocument(
        doc({
          type: "table",
          id: block.id,
          children: [],
          rows: [Array(21).fill([run])],
        })
      )
    ).toThrow()
    let nested: unknown = { ...block, id: id(9) }
    for (let n = 8; n >= 1; n--)
      nested = { ...block, id: id(n), children: [nested] }
    expect(() => parseRichDocument(doc(nested))).toThrow()
  })
})
