"use client"

import Markdown from "react-markdown"
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
    <div className="space-y-3 break-words text-sm leading-7 [&_blockquote]:border-s-2 [&_blockquote]:ps-4 [&_h1]:text-2xl [&_h2]:text-xl [&_h3]:text-lg [&_li]:ms-5 [&_ol]:list-decimal [&_pre]:overflow-x-auto [&_pre]:rounded [&_pre]:bg-muted [&_pre]:p-3 [&_ul]:list-disc">
      <Markdown
        skipHtml
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
          "blockquote",
          "ul",
          "ol",
          "li",
          "pre",
          "code",
          "a",
          "img",
        ]}
        urlTransform={safeUrl}
        components={{
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
