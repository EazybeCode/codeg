import { describe, expect, it } from "vitest"
import { fromNeev, toNeev, importMarkdown } from "./neev-rich-adapter"
import { parseRichDocument } from "./neev-rich-document"

const id = (n: number) =>
  `00000000-0000-0000-0000-${String(n).padStart(12, "0")}`
const runs = [
  { text: "plain", styles: [], href: null },
  {
    text: "bold italic strike",
    styles: ["bold", "italic", "strike"],
    href: "https://example.test/a",
  },
  { text: "code", styles: ["code"], href: "#here" },
]
const fixture = parseRichDocument({
  version: 1,
  blocks: [
    ...["paragraph", "bulletListItem", "numberedListItem", "quote"].map(
      (type, i) => ({ type, id: id(i), content: runs, children: [] })
    ),
    ...[1, 2, 3].map((level) => ({
      type: "heading",
      id: id(10 + level),
      content: runs,
      level,
      children: [],
    })),
    {
      type: "checkListItem",
      id: id(20),
      content: runs,
      checked: true,
      children: [{ type: "paragraph", id: id(21), content: [], children: [] }],
    },
    {
      type: "codeBlock",
      id: id(30),
      text: "const x = 1\n",
      language: "typescript",
      children: [],
    },
    {
      type: "table",
      id: id(40),
      rows: [
        [runs, []],
        [[], runs],
      ],
      children: [],
    },
  ],
})

describe("lossless bounded BlockNote adapter", () => {
  it("retains every canonical block, IDs, nesting, table cells, links and style combinations", () => {
    expect(toNeev(fromNeev(fixture))).toEqual(fixture)
  })
  it.each([
    {
      type: "image",
      props: { url: "https://example.test/a.png" },
      children: [],
    },
    {
      type: "paragraph",
      props: { textColor: "red" },
      content: [],
      children: [],
    },
    { type: "heading", props: { level: 4 }, content: [], children: [] },
    {
      type: "paragraph",
      props: {},
      content: [{ type: "text", text: "x", styles: { underline: true } }],
      children: [],
    },
    {
      type: "paragraph",
      props: {},
      content: [
        { type: "text", text: "x", styles: { code: true, bold: true } },
      ],
      children: [],
    },
    {
      type: "table",
      props: {},
      content: {
        type: "tableContent",
        rows: [
          {
            cells: [{ type: "tableCell", props: { colspan: 2 }, content: [] }],
          },
        ],
      },
      children: [],
    },
  ])("refuses unsupported content instead of stripping it: %j", (block) => {
    expect(() => toNeev([{ id: id(1), ...block }])).toThrow()
  })
})

describe("deliberate Markdown conversion", () => {
  it("imports headings, nested lists, quotes, text formatting, safe links and code", () => {
    const result = importMarkdown(
      "# Title\n\nHello **bold** and *italic* [link](https://example.test).\n\n- one\n  - two\n\n> quote\n\n```js\nx()\n```"
    )
    expect(result.blocks.map((block) => block.type)).toEqual([
      "heading",
      "paragraph",
      "bulletListItem",
      "quote",
      "codeBlock",
    ])
    expect(result.blocks[2].children[0].type).toBe("bulletListItem")
    expect(result.blocks[1]).toMatchObject({
      content: expect.arrayContaining([
        { text: "bold", styles: ["bold"], href: null },
        { text: "link", styles: [], href: "https://example.test" },
      ]),
    })
    expect(toNeev(fromNeev(result))).toEqual(result)
  })
  it.each([
    "<b>HTML</b>",
    "![image](https://example.test/a)",
    "[link][ref]\n\n[ref]: https://example.test",
    "#### four",
    "---",
    "~~strike~~",
    "| a | b |\n| --- | --- |",
    "a | b\n--- | ---\nc | d",
    "a | b |\n--- | --- |\nc | d |",
    "- [ ] task",
    "1. [ ] task",
    "1) [x] task",
    "> 1. [ ] nested task",
    "[bad](javascript:alert)",
  ])("refuses unsupported Markdown: %s", (markdown) => {
    expect(() => importMarkdown(markdown)).toThrow()
  })
  it.each([
    [
      "```txt\n~~literal~~\n1. [ ] task\na | b\n--- | ---\n```",
      "~~literal~~\n1. [ ] task\na | b\n--- | ---",
    ],
    ["    ~~literal~~", "~~literal~~"],
  ])("preserves GFM-looking literal code: %s", (markdown, text) => {
    expect(importMarkdown(markdown).blocks).toMatchObject([
      { type: "codeBlock", text },
    ])
  })
  it("preserves GFM-looking inline code and escaped literal punctuation", () => {
    expect(
      importMarkdown("`~~literal~~` and \\~\\~escaped\\~\\~").blocks
    ).toMatchObject([
      {
        type: "paragraph",
        content: [
          { text: "~~literal~~", styles: ["code"] },
          { text: " and ~~escaped~~", styles: [] },
        ],
      },
    ])
  })
})
