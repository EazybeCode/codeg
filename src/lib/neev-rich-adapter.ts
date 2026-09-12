import type {
  PartialBlock,
  InlineContent,
  DefaultInlineContentSchema,
  DefaultStyleSchema,
} from "@blocknote/core"
import { fromMarkdown } from "mdast-util-from-markdown"
import {
  parseRichDocument,
  type RichBlock,
  type RichDocument,
  type RichInlineRun,
  type RichStyle,
} from "./neev-rich-document"

function unsupported(): never {
  throw new Error("Unsupported rich document content")
}
function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value))
    return unsupported()
  return value as Record<string, unknown>
}
function keys(value: Record<string, unknown>, allowed: string[]) {
  if (Object.keys(value).some((key) => !allowed.includes(key))) unsupported()
}
const styleOrder: RichStyle[] = ["bold", "italic", "strike", "code"]
function fromRuns(
  runs: RichInlineRun[]
): InlineContent<DefaultInlineContentSchema, DefaultStyleSchema>[] {
  return runs.map((run) => {
    const text = {
      type: "text" as const,
      text: run.text,
      styles: Object.fromEntries(run.styles.map((style) => [style, true])),
    }
    return run.href === null
      ? text
      : { type: "link", href: run.href, content: [text] }
  })
}
export function fromNeev(document: RichDocument): PartialBlock[] {
  const block = (value: RichBlock): PartialBlock => {
    const common = { id: value.id, children: value.children.map(block) }
    switch (value.type) {
      case "codeBlock":
        return {
          ...common,
          type: value.type,
          props: { language: value.language },
          content: [{ type: "text", text: value.text, styles: {} }],
        }
      case "table":
        return {
          ...common,
          type: value.type,
          content: {
            type: "tableContent",
            rows: value.rows.map((row) => ({ cells: row.map(fromRuns) })),
          },
        }
      case "heading":
        return {
          ...common,
          type: value.type,
          props: { level: value.level },
          content: fromRuns(value.content),
        }
      case "checkListItem":
        return {
          ...common,
          type: value.type,
          props: { checked: value.checked },
          content: fromRuns(value.content),
        }
      default:
        return { ...common, type: value.type, content: fromRuns(value.content) }
    }
  }
  return parseRichDocument(document).blocks.map(block)
}
function toRuns(value: unknown, href: string | null = null): RichInlineRun[] {
  if (!Array.isArray(value)) return unsupported()
  return value.flatMap((item): RichInlineRun[] => {
    const run = object(item)
    if (run.type === "link" && href === null) {
      keys(run, ["type", "href", "content"])
      if (typeof run.href !== "string") return unsupported()
      return toRuns(run.content, run.href)
    }
    keys(run, ["type", "text", "styles"])
    if (run.type !== "text" || typeof run.text !== "string")
      return unsupported()
    const styles = object(run.styles)
    keys(styles, styleOrder)
    if (Object.values(styles).some((value) => value !== true))
      return unsupported()
    return [
      {
        text: run.text,
        styles: styleOrder.filter((style) => styles[style] === true),
        href,
      },
    ]
  })
}
function neutralProps(value: Record<string, unknown>, extras: string[] = []) {
  keys(value, ["textColor", "backgroundColor", "textAlignment", ...extras])
  for (const name of ["textColor", "backgroundColor"])
    if (value[name] !== undefined && value[name] !== "default") unsupported()
  if (value.textAlignment !== undefined && value.textAlignment !== "left")
    unsupported()
}
export function toNeev(value: unknown): RichDocument {
  if (!Array.isArray(value)) return unsupported()
  const block = (input: unknown): unknown => {
    const item = object(input)
    keys(item, ["id", "type", "props", "content", "children"])
    const props = item.props === undefined ? {} : object(item.props)
    if (!Array.isArray(item.children)) return unsupported()
    const common = {
      id: item.id,
      type: item.type,
      children: item.children.map(block),
    }
    switch (item.type) {
      case "codeBlock": {
        keys(props, ["language"])
        const runs = toRuns(item.content)
        if (runs.some((run) => run.styles.length || run.href !== null))
          return unsupported()
        return {
          ...common,
          text: runs.map((run) => run.text).join(""),
          language: props.language ?? "",
        }
      }
      case "table": {
        neutralProps(props)
        const content = object(item.content)
        keys(content, [
          "type",
          "rows",
          "columnWidths",
          "headerRows",
          "headerCols",
        ])
        if (
          content.type !== "tableContent" ||
          !Array.isArray(content.rows) ||
          (content.headerRows !== undefined && content.headerRows !== 0) ||
          (content.headerCols !== undefined && content.headerCols !== 0)
        )
          return unsupported()
        if (
          content.columnWidths !== undefined &&
          (!Array.isArray(content.columnWidths) ||
            content.columnWidths.some((width) => width !== undefined))
        )
          return unsupported()
        return {
          ...common,
          rows: content.rows.map((row) => {
            const parsed = object(row)
            keys(parsed, ["cells"])
            if (!Array.isArray(parsed.cells)) return unsupported()
            return parsed.cells.map((cell) => {
              if (Array.isArray(cell)) return toRuns(cell)
              const parsedCell = object(cell)
              keys(parsedCell, ["type", "props", "content"])
              if (parsedCell.type !== "tableCell") return unsupported()
              const cellProps = object(parsedCell.props)
              neutralProps(cellProps, ["colspan", "rowspan"])
              if (
                [cellProps.colspan, cellProps.rowspan].some(
                  (span) => span !== undefined && span !== 1
                )
              )
                return unsupported()
              return toRuns(parsedCell.content)
            })
          }),
        }
      }
      case "heading":
        neutralProps(props, ["level"])
        return {
          ...common,
          content: toRuns(item.content),
          level: props.level ?? 1,
        }
      case "checkListItem":
        neutralProps(props, ["checked"])
        return {
          ...common,
          content: toRuns(item.content),
          checked: props.checked ?? false,
        }
      case "numberedListItem":
        neutralProps(props, ["start"])
        if (props.start !== undefined && props.start !== 1) return unsupported()
        return { ...common, content: toRuns(item.content) }
      case "paragraph":
      case "bulletListItem":
      case "quote":
        neutralProps(props)
        return { ...common, content: toRuns(item.content) }
      default:
        return unsupported()
    }
  }
  return parseRichDocument({ version: 1, blocks: value.map(block) })
}

/** Conservative CommonMark import: unsupported syntax is a visible refusal. */
export function importMarkdown(markdown: string): RichDocument {
  if (new TextEncoder().encode(markdown).length > 1_048_576)
    return unsupported()
  // These extensions would otherwise be parsed as ordinary text by CommonMark.
  if (/~~|^\s*\|.*\||^\s*[-+*]\s+\[[ xX]\]/m.test(markdown))
    return unsupported()
  type Node = {
    type: string
    value?: string
    depth?: number
    url?: string
    title?: string | null
    lang?: string | null
    meta?: string | null
    ordered?: boolean
    start?: number | null
    children?: Node[]
  }
  const inline = (
    nodes: Node[],
    styles: RichStyle[] = [],
    href: string | null = null
  ): RichInlineRun[] =>
    nodes.flatMap((node): RichInlineRun[] => {
      switch (node.type) {
        case "text":
          return [{ text: node.value ?? "", styles, href }]
        case "break":
          return [{ text: "\n", styles, href }]
        case "strong":
          return inline(
            node.children ?? [],
            styleOrder.filter(
              (style) => style === "bold" || styles.includes(style)
            ),
            href
          )
        case "emphasis":
          return inline(
            node.children ?? [],
            styleOrder.filter(
              (style) => style === "italic" || styles.includes(style)
            ),
            href
          )
        case "inlineCode":
          if (styles.length) return unsupported()
          return [{ text: node.value ?? "", styles: ["code"], href }]
        case "link":
          if (href || node.title) return unsupported()
          return inline(node.children ?? [], styles, node.url ?? "")
        default:
          return unsupported()
      }
    })
  const convert = (nodes: Node[]): RichBlock[] =>
    nodes.flatMap((node): RichBlock[] => {
      const base = { id: crypto.randomUUID(), children: [] as RichBlock[] }
      switch (node.type) {
        case "paragraph":
          return [
            {
              ...base,
              type: "paragraph",
              content: inline(node.children ?? []),
            },
          ]
        case "heading":
          if (![1, 2, 3].includes(node.depth ?? 0)) return unsupported()
          return [
            {
              ...base,
              type: "heading",
              level: node.depth as 1 | 2 | 3,
              content: inline(node.children ?? []),
            },
          ]
        case "code":
          if (node.meta) return unsupported()
          return [
            {
              ...base,
              type: "codeBlock",
              text: node.value ?? "",
              language: node.lang ?? "",
            },
          ]
        case "blockquote": {
          const children = convert(node.children ?? [])
          if (children[0]?.type !== "paragraph") return unsupported()
          return [
            {
              ...base,
              type: "quote",
              content: children[0].content,
              children: children.slice(1),
            },
          ]
        }
        case "list":
          if (node.ordered && node.start !== 1) return unsupported()
          return (node.children ?? []).map((item): RichBlock => {
            if (item.type !== "listItem") return unsupported()
            const children = convert(item.children ?? [])
            if (children[0]?.type !== "paragraph") return unsupported()
            return {
              id: crypto.randomUUID(),
              type: node.ordered ? "numberedListItem" : "bulletListItem",
              content: children[0].content,
              children: children.slice(1),
            }
          })
        default:
          return unsupported()
      }
    })
  const blocks = convert(fromMarkdown(markdown).children as Node[])
  return parseRichDocument({
    version: 1,
    blocks: blocks.length
      ? blocks
      : [
          {
            id: crypto.randomUUID(),
            type: "paragraph",
            content: [],
            children: [],
          },
        ],
  })
}
