"use client"

import { Streamdown, type Components } from "streamdown"
import { useTranslations } from "next-intl"
import { messageSource } from "@/lib/neev-copy"
import { CopyableCodeBlock, CopyMessageButton } from "./native-copy"

function safeUrl(value: string): string | undefined {
  if (value.startsWith("#")) return value
  // Exact attachment route only; the server enforces the conversation ACL.
  if (
    /^\/api\/neev\/attachments\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(
      value
    )
  )
    return value
  // No base URL: relative, protocol-relative and custom-protocol links stay inert.
  if (!/^https?:\/\//i.test(value) || /[\u0000-\u0020\u007f]/.test(value))
    return
  try {
    const url = new URL(value)
    if (url.protocol === "https:" || url.protocol === "http:") return url.href
  } catch {
    /* Keep malformed links as plain text. */
  }
}

/**
 * Everything a reply is allowed to become.
 *
 * GFM's own grammar — tables, strikethrough, task lists, bare autolinks — and
 * nothing else. `input` is only ever a task-list checkbox: GFM emits no other
 * input, and with `skipHtml` on, raw HTML cannot reach here to emit one.
 */
const ALLOWED = [
  "p",
  "br",
  "hr",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "strong",
  "em",
  "del",
  "blockquote",
  "ul",
  "ol",
  "li",
  "input",
  "table",
  "thead",
  "tbody",
  "tr",
  "th",
  "td",
  "pre",
  "code",
  "a",
  "img",
]

function ScrollTable({ children }: { children?: React.ReactNode }) {
  // The scroll belongs to the table, not to the reply: a wide table must not
  // widen the column every other message is read in.
  return (
    <div className="overflow-x-auto">
      <table className="w-full border-collapse text-[0.8125rem] leading-[1.5]">
        {children}
      </table>
    </div>
  )
}

// Rendered from `checked`, never from the reader: a reply reports a list's
// state and is not a form to fill in.
function TaskBox({ checked }: { checked?: boolean }) {
  return (
    <input
      type="checkbox"
      checked={!!checked}
      disabled
      readOnly
      className="me-1.5 align-[-0.1em]"
    />
  )
}

function SafeLink({
  href,
  children,
}: {
  href?: string
  children?: React.ReactNode
}) {
  return href ? (
    <a
      href={href}
      rel="noopener noreferrer"
      className="rounded underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
    >
      {children}
    </a>
  ) : (
    <span>{children}</span>
  )
}

/** Named, never fetched: an image in a reply is a link the reader may follow. */
function HiddenImage({ src, alt }: { src?: string | Blob; alt?: string }) {
  const t = useTranslations("NativeContent")
  return (
    <span className="text-muted-foreground">
      {t("imageHidden")}{" "}
      {typeof src === "string" && safeUrl(src) && !src.startsWith("#") ? (
        <a href={safeUrl(src)} rel="noopener noreferrer" className="underline">
          {alt || t("openImage")}
        </a>
      ) : (
        alt
      )}
    </span>
  )
}

/*
 * One object, defined once.
 *
 * Streamdown splits the reply into blocks and memoises each one; that memo
 * compares `components` BY REFERENCE. Built inline in the render body it would
 * be a new object on every chunk, every block would fail the comparison, and
 * the whole transcript would re-render per token — which is the thing block
 * splitting exists to avoid.
 */
const COMPONENTS = {
  // Streamdown draws `strong` as a `span` with a bold class. That is the right
  // weight and the wrong element: emphasis a screen reader cannot hear is not
  // emphasis. Every other tag it produces is already the semantic one.
  strong: ({ children }: { children?: React.ReactNode }) => (
    <strong className="font-semibold">{children}</strong>
  ),
  table: ScrollTable,
  input: TaskBox,
  a: SafeLink,
  img: HiddenImage,
  // Cast once, at the boundary. Streamdown's `Components` carries an index
  // signature of `Record<string, unknown>`, which no precisely-typed component
  // can satisfy; widening each one instead would give up the prop types that
  // make them readable.
} as Components

/*
 * The same set plus a Copy button on every fenced code block. Also defined
 * once, and only ever used for COMPLETE text: a streaming reply keeps the set
 * above, so block memoisation is untouched while it grows, and switches to this
 * one exactly once, when it settles.
 */
const COMPONENTS_WITH_CODE_COPY = {
  ...(COMPONENTS as Record<string, unknown>),
  pre: CopyableCodeBlock,
} as Components

/** Off by default and passed explicitly, for the same reason. */
const NO_LINK_SAFETY = { enabled: false }

/**
 * What a reader may copy from this text once it is complete.
 * `none`: nothing (documents, knowledge pages — the default, unchanged).
 * `code`: each fenced code block's raw code.
 * `message`: the code blocks, plus the whole Markdown source from one button
 * after the text.
 */
export type NativeMarkdownCopy = "none" | "code" | "message"

export function NativeMarkdown({
  markdown,
  /**
   * Whether this text is still arriving.
   *
   * It decides one thing: whether a half-written span is repaired before it is
   * parsed. Streaming text ends mid-token — `**bold`, a table whose delimiter
   * row has not arrived, an unclosed fence — and read literally that renders
   * as asterisks and pipes which then snap into place a chunk later. Repairing
   * it renders the emphasis, the table and the code block from the first
   * chunk, so the reply only ever grows.
   *
   * It must be FALSE for stored text, and not merely because the repair is
   * unnecessary there. The repair appends closers, and on finished text it can
   * append one after a span that is already complete — a glob inside code, an
   * identifier like `_meta` — putting a character in a reply that its author
   * never wrote.
   */
  streaming = false,
  /**
   * Copy controls for COMPLETE text only: while `streaming`, nothing is added
   * (no buttons that would copy half an answer, and no change to the
   * components Streamdown memoises on).
   */
  copy = "none",
}: {
  markdown: string
  streaming?: boolean
  copy?: NativeMarkdownCopy
}) {
  const copying = !streaming && copy !== "none"
  const body = (
    /*
     * 14px on a 23px line, and a heading scale that stays close to it.
     *
     * The rhythm was 14/28 — a ratio of 2, which reads as a draft double-spaced
     * for margin notes rather than as an answer. Headings were worse than loose:
     * Tailwind's preflight resets them to `font-weight: inherit`, so an `h3`
     * came out at 400 on that same 28px line and differed from body text by
     * nothing but size. Weight carries the hierarchy here and size barely moves,
     * which is what lets a heading sit inside a reply instead of interrupting it.
     *
     * `pre`/`code` step down to 13px: at a shared size, monospace reads larger
     * than the sans beside it.
     *
     * No per-heading top margin: `space-y-3` already spaces every child, and a
     * `mt-*` on a heading would be competing with it at the same specificity —
     * a fight decided by stylesheet order rather than by intent.
     *
     * Table cells step down to 13px for the same reason `pre` does: a grid of
     * short strings reads denser than the sentences around it. A task-list
     * item drops its bullet — the box already marks the line, and a disc
     * beside it is two markers for one item.
     *
     * `[&>div]:space-y-3` reaches Streamdown's own block wrapper, which brings
     * a `space-y-4` of its own. Left alone it would set the rhythm of every
     * reply, since our `space-y-3` then has exactly one child to space.
     */
    <div className="space-y-3 [&>div]:space-y-3 break-words text-sm leading-[1.65] [&_blockquote]:border-s-2 [&_blockquote]:ps-4 [&_blockquote]:text-muted-foreground [&_code]:text-[0.925em] [&_h1]:text-[1.0625rem] [&_h1]:font-semibold [&_h1]:leading-snug [&_h2]:text-[0.9375rem] [&_h2]:font-semibold [&_h2]:leading-snug [&_h3]:text-sm [&_h3]:font-semibold [&_h3]:leading-snug [&_h4]:text-sm [&_h4]:font-medium [&_h5]:font-medium [&_h6]:font-medium [&_li:has(>input)]:ms-1 [&_li:has(>input)]:list-none [&_li]:ms-5 [&_ol]:list-decimal [&_pre]:overflow-x-auto [&_pre]:rounded [&_pre]:bg-muted [&_pre]:p-3 [&_pre]:text-[0.8125rem] [&_pre]:leading-[1.5] [&_pre_code]:text-[1em] [&_td]:border [&_td]:px-2 [&_td]:py-1 [&_td]:align-top [&_th]:border [&_th]:px-2 [&_th]:py-1 [&_th]:text-start [&_th]:font-medium [&_thead]:bg-muted/50 [&_ul]:list-disc">
      <Streamdown
        /*
         * Streamdown for the block splitting and the incomplete-Markdown
         * repair, and for nothing else it offers.
         *
         * No `plugins`, so no Shiki, Mermaid or KaTeX is pulled in behind a
         * chat reply. `controls` off: a reply is read, not a widget. Link
         * safety off because this already has a stricter answer — `skipHtml`
         * drops raw HTML outright and `urlTransform` refuses every scheme but
         * http(s), so a link that reaches the reader has already been vetted
         * and an interstitial would be asking about a decision made upstream.
         *
         * All three default ON, so all three are passed.
         */
        mode={streaming ? "streaming" : "static"}
        parseIncompleteMarkdown={streaming}
        plugins={undefined}
        controls={false}
        linkSafety={NO_LINK_SAFETY}
        skipHtml
        allowedElements={ALLOWED}
        urlTransform={safeUrl}
        components={copying ? COMPONENTS_WITH_CODE_COPY : COMPONENTS}
      >
        {markdown}
      </Streamdown>
    </div>
  )
  if (!(copying && copy === "message") || !messageSource(markdown)) return body
  return (
    <div className="group/message">
      {body}
      {/* One button for the whole reply, after it — not one per paragraph or
          list item. */}
      <div className="mt-1 flex">
        <CopyMessageButton markdown={() => messageSource(markdown)} />
      </div>
    </div>
  )
}
