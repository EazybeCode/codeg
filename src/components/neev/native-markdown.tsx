"use client"

import Markdown from "react-markdown"
import remarkGfm from "remark-gfm"
import { useTranslations } from "next-intl"

function safeUrl(value: string): string | undefined {
  if (value.startsWith("#")) return value
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

export function NativeMarkdown({ markdown }: { markdown: string }) {
  const t = useTranslations("NativeContent")
  return (
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
     */
    <div className="space-y-3 break-words text-sm leading-[1.65] [&_blockquote]:border-s-2 [&_blockquote]:ps-4 [&_blockquote]:text-muted-foreground [&_code]:text-[0.925em] [&_h1]:text-[17px] [&_h1]:font-semibold [&_h1]:leading-snug [&_h2]:text-[15px] [&_h2]:font-semibold [&_h2]:leading-snug [&_h3]:text-sm [&_h3]:font-semibold [&_h3]:leading-snug [&_h4]:text-sm [&_h4]:font-medium [&_h5]:font-medium [&_h6]:font-medium [&_li:has(>input)]:ms-1 [&_li:has(>input)]:list-none [&_li]:ms-5 [&_ol]:list-decimal [&_pre]:overflow-x-auto [&_pre]:rounded [&_pre]:bg-muted [&_pre]:p-3 [&_pre]:text-[13px] [&_pre]:leading-[1.5] [&_pre_code]:text-[1em] [&_td]:border [&_td]:px-2 [&_td]:py-1 [&_td]:align-top [&_th]:border [&_th]:px-2 [&_th]:py-1 [&_th]:text-start [&_th]:font-medium [&_thead]:bg-muted/50 [&_ul]:list-disc">
      <Markdown
        skipHtml
        /*
         * Tables, strikethrough, task lists and bare autolinks — the dialect
         * every one of these providers actually writes. Without it a reply
         * that reached for a table printed its own pipes and dashes down the
         * column, which is the source, not the answer.
         *
         * This widens the grammar, not the trust: `skipHtml` still drops raw
         * HTML and `urlTransform` still refuses every scheme but http(s), so
         * the new elements below can only come from Markdown this parser
         * built.
         */
        remarkPlugins={[remarkGfm]}
        allowedElements={[
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
          // Only ever a task-list checkbox: GFM emits no other input, and raw
          // HTML cannot reach here to emit one.
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
        ]}
        urlTransform={safeUrl}
        components={{
          table: ({ children }) => (
            // The scroll belongs to the table, not to the reply: a wide table
            // must not widen the column every other message is read in.
            <div className="overflow-x-auto">
              <table className="w-full border-collapse text-[13px] leading-[1.5]">
                {children}
              </table>
            </div>
          ),
          // Rendered from `checked`, never from the reader: a reply reports a
          // list's state and is not a form to fill in.
          input: ({ checked }) => (
            <input
              type="checkbox"
              checked={!!checked}
              disabled
              readOnly
              className="me-1.5 align-[-0.1em]"
            />
          ),
          a: ({ href, children }) =>
            href ? (
              <a
                href={href}
                rel="noopener noreferrer"
                className="rounded underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
              >
                {children}
              </a>
            ) : (
              <span>{children}</span>
            ),
          img: ({ src, alt }) => (
            <span className="text-muted-foreground">
              {t("imageHidden")}{" "}
              {typeof src === "string" &&
              safeUrl(src) &&
              !src.startsWith("#") ? (
                <a
                  href={safeUrl(src)}
                  rel="noopener noreferrer"
                  className="underline"
                >
                  {alt || t("openImage")}
                </a>
              ) : (
                alt
              )}
            </span>
          ),
        }}
      >
        {markdown}
      </Markdown>
    </div>
  )
}
