import { Fragment, type ReactNode } from "react"
import { BrowserLink } from "@/components/ui/browser-link"
import {
  parseRichDocument,
  type RichBlock,
  type RichDocument,
  type RichInlineRun,
} from "@/lib/neev-rich-document"

function runs(content: RichInlineRun[]): ReactNode {
  return content.map((run, index) => {
    let text: ReactNode = run.text
    for (const style of run.styles) {
      if (style === "bold") text = <strong>{text}</strong>
      if (style === "italic") text = <em>{text}</em>
      if (style === "strike") text = <s>{text}</s>
      if (style === "code")
        text = <code className="rounded bg-muted px-1">{text}</code>
    }
    if (run.href)
      text = run.href.startsWith("#") ? (
        <a className="underline" href={run.href}>
          {text}
        </a>
      ) : (
        <BrowserLink className="underline" href={run.href}>
          {text}
        </BrowserLink>
      )
    return <Fragment key={index}>{text}</Fragment>
  })
}
function block(value: RichBlock): ReactNode {
  let content: ReactNode
  switch (value.type) {
    case "heading": {
      const Heading = `h${value.level}` as "h1" | "h2" | "h3"
      content = (
        <Heading className="text-lg font-semibold">
          {runs(value.content)}
        </Heading>
      )
      break
    }
    case "codeBlock":
      content = (
        <pre className="overflow-x-auto rounded bg-muted p-3">
          <code>{value.text}</code>
        </pre>
      )
      break
    case "table":
      content = (
        <div className="overflow-x-auto">
          <table className="w-full border-collapse">
            <tbody>
              {value.rows.map((row, index) => (
                <tr key={index}>
                  {row.map((cell, cellIndex) => (
                    <td className="border p-2 align-top" key={cellIndex}>
                      {runs(cell)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )
      break
    case "quote":
      content = (
        <blockquote className="border-l-2 pl-3">
          {runs(value.content)}
        </blockquote>
      )
      break
    case "checkListItem":
      content = (
        <div className="flex items-start gap-2">
          <input
            type="checkbox"
            checked={value.checked}
            disabled
            aria-label={value.content.map((run) => run.text).join("")}
          />
          <span>{runs(value.content)}</span>
        </div>
      )
      break
    default:
      content = <p className="min-h-6">{runs(value.content)}</p>
  }
  return (
    <div key={value.id}>
      {content}
      {value.children.length > 0 && (
        <div className="space-y-2 border-l pl-5">{blocks(value.children)}</div>
      )}
    </div>
  )
}
function blocks(values: RichBlock[]): ReactNode[] {
  const rendered: ReactNode[] = []
  for (let index = 0; index < values.length; index++) {
    const value = values[index]
    if (value.type !== "numberedListItem" && value.type !== "bulletListItem") {
      rendered.push(block(value))
      continue
    }
    const List = value.type === "numberedListItem" ? "ol" : "ul"
    const items: ReactNode[] = []
    do {
      const item = values[index] as Extract<
        RichBlock,
        { content: RichInlineRun[] }
      >
      items.push(
        <li key={item.id}>
          {runs(item.content)}
          {item.children.length > 0 && blocks(item.children)}
        </li>
      )
      index++
    } while (index < values.length && values[index].type === value.type)
    index--
    rendered.push(
      <List
        key={value.id}
        className={List === "ol" ? "list-decimal pl-5" : "list-disc pl-5"}
      >
        {items}
      </List>
    )
  }
  return rendered
}
export function NativeRichView({ document }: { document: RichDocument }) {
  return (
    <div className="min-w-0 space-y-3 whitespace-pre-wrap break-words text-sm leading-6">
      {blocks(parseRichDocument(document).blocks)}
    </div>
  )
}
