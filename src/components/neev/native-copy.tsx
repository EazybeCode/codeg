"use client"

import { useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { Check, Copy } from "lucide-react"
import { cn } from "@/lib/utils"
import { codeFromPre, copyToClipboard } from "@/lib/neev-copy"

/** How long "Copied" stays before the button goes back to "Copy". */
export const COPIED_MS = 2000

export interface CopyLabels {
  copy: string
  copied: string
  failed: string
}

const ENGLISH: Record<"message" | "code", CopyLabels> = {
  message: { copy: "Copy", copied: "Copied", failed: "Copy failed" },
  code: { copy: "Copy code", copied: "Copied", failed: "Copy failed" },
}

/**
 * The labels for one kind of copy: translated when the locale has them
 * (`NativeContent.copy…`), English until those strings are merged.
 */
export function useCopyLabels(kind: "message" | "code"): CopyLabels {
  const t = useTranslations("NativeContent")
  const pick = (key: string, fallback: string) =>
    t.has(key as never) ? t(key as never) : fallback
  const english = ENGLISH[kind]
  return {
    copy: pick(kind === "code" ? "copyCode" : "copyMessage", english.copy),
    copied: pick("copied", english.copied),
    failed: pick("copyFailed", english.failed),
  }
}

/**
 * A small Copy button: an icon with an accessible name, a polite live
 * announcement of the result, and no text of its own that could be copied
 * along with the content. Discreet until hovered or focused; always shown on
 * a device that cannot hover.
 */
export function CopyButton({
  text,
  labels,
  className,
  reveal = "group",
}: {
  /** The exact text to copy, read at click time. */
  text: () => string
  labels: CopyLabels
  className?: string
  /**
   * Hidden until the surrounding `group` (or `group/message`) is hovered or
   * focused; `false` keeps it always visible. A result (Copied / failed) is
   * always visible while it is announced.
   */
  reveal?: "group" | "message" | false
}) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle")
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current)
    },
    []
  )
  async function copy() {
    const ok = await copyToClipboard(text())
    setState(ok ? "copied" : "failed")
    if (timer.current) clearTimeout(timer.current)
    timer.current = setTimeout(() => setState("idle"), COPIED_MS)
  }
  const label =
    state === "copied"
      ? labels.copied
      : state === "failed"
        ? labels.failed
        : labels.copy
  return (
    <button
      type="button"
      onClick={() => void copy()}
      aria-label={label}
      title={label}
      data-copy-state={state}
      className={cn(
        "inline-flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-opacity hover:bg-muted hover:text-foreground focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring select-none",
        state === "idle" &&
          reveal === "group" &&
          "opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 [@media(hover:none)]:opacity-100",
        state === "idle" &&
          reveal === "message" &&
          "opacity-0 group-hover/message:opacity-100 group-focus-within/message:opacity-100 [@media(hover:none)]:opacity-100",
        className
      )}
    >
      {state === "copied" ? (
        <Check aria-hidden className="size-3.5" />
      ) : (
        <Copy aria-hidden className="size-3.5" />
      )}
      <span className="sr-only" aria-live="polite">
        {state === "idle" ? "" : label}
      </span>
    </button>
  )
}

/**
 * A fenced code block with a Copy button for its raw code. The text comes from
 * the parsed node, so it is exactly what was fenced — never line numbers,
 * labels or the button's own name.
 */
export function CopyableCodeBlock({
  node,
  children,
}: {
  node?: unknown
  children?: React.ReactNode
}) {
  const labels = useCopyLabels("code")
  return (
    <div className="group relative" data-copyable-code="">
      <pre>{children}</pre>
      <CopyButton
        text={() => codeFromPre(node)}
        labels={labels}
        className="absolute end-1.5 top-1.5 bg-muted"
      />
    </div>
  )
}

/** The whole reply's Markdown source, copied from a button after it. */
export function CopyMessageButton({
  markdown,
  className,
}: {
  markdown: () => string
  className?: string
}) {
  const labels = useCopyLabels("message")
  return (
    <CopyButton
      text={markdown}
      labels={labels}
      className={className}
      reveal="message"
    />
  )
}
