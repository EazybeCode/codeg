"use client"

import { useEffect, useMemo, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { NativeClientError } from "@/lib/neev-client"
import {
  CITE_ANCHOR,
  downloadKnowledgeExport,
  KNOWLEDGE_PAGE_SIZE,
  knowledgeExportUrl,
  loadKnowledgeLog,
  loadKnowledgePage,
  loadKnowledgePages,
  PAGE_ANCHOR,
  prepareKnowledgeMarkdown,
  type KnowledgeLogEntry,
  type KnowledgePage,
  type KnowledgePageSummary,
} from "@/lib/neev-knowledge"
import { useNativeAuth } from "./native-auth-provider"
import { NativeError } from "./native-boundary"
import { NativeMarkdown } from "./native-markdown"
import { NativeSkeletonRows } from "./native-skeleton"
import { useOpenInWorkspace } from "./native-knowledge"

function Chip({
  children,
  tone,
}: {
  children: React.ReactNode
  tone?: "warn"
}) {
  return (
    <span
      className={cn(
        "rounded px-1.5 py-0.5 text-2xs leading-4",
        tone === "warn"
          ? "bg-neev-warn-tint text-neev-warn"
          : "bg-muted text-muted-foreground"
      )}
    >
      {children}
    </span>
  )
}

/**
 * The compiled pages, their preview, the log and the export.
 *
 * A withheld page is shown as exactly what the server says it is: an opaque
 * entry and a count. No title, slug or body is shown for it — none is received.
 * Citations and page links inside a readable page are in-app anchors handled
 * here; the markdown renderer only ever sees `#…` links it already allows.
 */
export function NativeKnowledgePages({
  version = 0,
  onRefresh,
  focus = null,
}: {
  /**
   * Show this page (from a test answer's link). A new nonce shows it again,
   * re-read, even if it is already the one selected.
   */
  focus?: { id: string; nonce: number } | null
  /** Refresh was pressed: the section's status should be read again too. */
  onRefresh?: () => void
  /**
   * Bumped when pages may have changed under us — a job wrote or withheld
   * some. The list and the open page are both read again, so a body whose
   * source was revoked does not stay on screen until a remount.
   */
  version?: number
} = {}) {
  const t = useTranslations("NativeKnowledge")
  const { handleError } = useNativeAuth()
  const openInWorkspace = useOpenInWorkspace()
  const [pages, setPages] = useState<KnowledgePageSummary[] | null>(null)
  const [selected, setSelected] = useState<string | null>(null)
  const [page, setPage] = useState<KnowledgePage | null>(null)
  const [log, setLog] = useState<KnowledgeLogEntry[]>([])
  const [more, setMore] = useState(false)
  const [error, setError] = useState<unknown>(null)
  /** "Refresh": the list and the open page. */
  const [reload, setReload] = useState(0)
  /** Choosing the open page again re-reads just that page. */
  const [pageReload, setPageReload] = useState(0)
  const [loadingEarlier, setLoadingEarlier] = useState(false)
  const earlierInFlight = useRef(false)
  const [exportFailed, setExportFailed] = useState(false)
  const exporting = useRef(false)
  const live = useRef(true)
  useEffect(() => {
    live.current = true
    return () => {
      live.current = false
    }
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    loadKnowledgePages(controller.signal).then(
      (list) => {
        if (controller.signal.aborted) return
        setPages(list)
        setError(null)
      },
      (cause: unknown) => {
        if (controller.signal.aborted) return
        setError(cause)
        handleError(cause)
      }
    )
    return () => controller.abort()
  }, [handleError, version, reload])

  // The log restarts from its newest entries on an explicit refresh only, so
  // a running job's updates do not discard earlier pages someone loaded.
  useEffect(() => {
    const controller = new AbortController()
    loadKnowledgeLog(null, controller.signal).then(
      (entries) => {
        if (controller.signal.aborted) return
        setLog(entries)
        setMore(entries.length === KNOWLEDGE_PAGE_SIZE)
      },
      (cause: unknown) => {
        if (controller.signal.aborted) return
        setError(cause)
        handleError(cause)
      }
    )
    return () => controller.abort()
  }, [handleError, reload])

  // The page on screen is always the one selected: an older answer is dropped.
  useEffect(() => {
    if (!selected) return
    const controller = new AbortController()
    loadKnowledgePage(selected, controller.signal).then(
      (next) => {
        if (controller.signal.aborted) return
        setPage(next)
        setError(null)
      },
      (cause: unknown) => {
        if (controller.signal.aborted) return
        // Nothing stale stays on screen when the page can no longer be read.
        setPage(null)
        // Gone (a rebuild archived it): drop the selection, quietly and once,
        // rather than re-reporting it on every poll while a job runs.
        if (cause instanceof NativeClientError && cause.status === 404) {
          setSelected(null)
          return
        }
        setError(cause)
        handleError(cause)
      }
    )
    return () => controller.abort()
  }, [selected, version, reload, pageReload, handleError])

  const readable = useMemo(() => {
    const bySlug = new Map<string, string>()
    for (const item of pages ?? [])
      if (item.status === "readable") bySlug.set(item.slug, item.id)
    return bySlug
  }, [pages])

  const shown = page && page.id === selected ? page : null
  const markdown = useMemo(
    () =>
      shown?.status === "readable"
        ? prepareKnowledgeMarkdown(
            shown.markdown,
            shown.citations,
            new Set(readable.keys()),
            t("withheldCite")
          )
        : "",
    [shown, readable, t]
  )

  function onPreviewClick(event: React.MouseEvent) {
    const anchor = (event.target as HTMLElement).closest("a")
    const href = anchor?.getAttribute("href") ?? ""
    if (href.startsWith(CITE_ANCHOR) && shown?.status === "readable") {
      event.preventDefault()
      const key = href.slice(CITE_ANCHOR.length)
      const cite = shown.citations.find((item) => item.key === key)
      if (cite && !cite.withheld)
        openInWorkspace({
          kind: "resource",
          resourceId: cite.sourceId,
          nodeId: cite.nodeId,
        })
    } else if (href.startsWith(PAGE_ANCHOR)) {
      event.preventDefault()
      const target = readable.get(href.slice(PAGE_ANCHOR.length))
      if (target) setSelected(target)
    }
  }

  async function exportNow(event: React.MouseEvent) {
    event.preventDefault()
    if (exporting.current) return
    exporting.current = true
    setExportFailed(false)
    try {
      const { blob, filename } = await downloadKnowledgeExport()
      const url = URL.createObjectURL(blob)
      const link = document.createElement("a")
      link.href = url
      link.download = filename
      link.click()
      // Later, never in the click's own tick: some browsers cancel a download
      // whose URL is revoked before it has started.
      setTimeout(() => URL.revokeObjectURL(url), 1_000)
    } catch {
      if (live.current) setExportFailed(true)
    } finally {
      exporting.current = false
    }
  }

  async function loadEarlier() {
    const before = log[log.length - 1]?.seq
    if (before === undefined || earlierInFlight.current) return
    earlierInFlight.current = true
    setLoadingEarlier(true)
    try {
      const entries = await loadKnowledgeLog(before)
      if (!live.current) return
      setLog((old) => [
        ...old,
        ...entries.filter((entry) => entry.seq < before),
      ])
      setMore(entries.length === KNOWLEDGE_PAGE_SIZE)
    } catch (cause) {
      if (live.current) {
        setError(cause)
        handleError(cause)
      }
    } finally {
      earlierInFlight.current = false
      if (live.current) setLoadingEarlier(false)
    }
  }

  const view = useRef<HTMLElement | null>(null)
  const focusNonce = focus?.nonce
  const focusId = focus?.id
  useEffect(() => {
    if (!focusId) return
    setSelected(focusId)
    setPageReload((value) => value + 1)
    view.current?.scrollIntoView?.({ block: "start", behavior: "smooth" })
  }, [focusId, focusNonce])

  const card = "space-y-3 rounded-xl border bg-card p-4"
  return (
    <div className="space-y-4">
      <NativeError error={error} />
      <section className={card} ref={view}>
        <div className="flex items-center justify-between gap-3">
          <h3 className="text-sm font-medium">{t("pagesTitle")}</h3>
          <div className="flex items-center gap-3">
            <Button
              size="sm"
              variant="ghost"
              onClick={() => {
                setReload((value) => value + 1)
                onRefresh?.()
              }}
            >
              {t("refresh")}
            </Button>
            {/* Fetched, not followed: a refusal is reported, never saved. */}
            <a
              href={knowledgeExportUrl()}
              download
              onClick={(event) => void exportNow(event)}
              className="rounded text-sm underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
            >
              {t("export")}
            </a>
          </div>
        </div>
        {exportFailed && (
          <p role="alert" className="text-sm text-destructive">
            {t("exportFailed")}
          </p>
        )}
        {pages === null ? (
          <NativeSkeletonRows label={t("loading")} />
        ) : pages.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("noPages")}</p>
        ) : (
          <div className="grid gap-4 md:grid-cols-[minmax(0,14rem)_minmax(0,1fr)]">
            <ul aria-label={t("pagesTitle")} className="space-y-0.5">
              {pages.map((item) => (
                <li key={item.id}>
                  <button
                    type="button"
                    aria-current={selected === item.id ? "true" : undefined}
                    onClick={() => {
                      // Choosing it again re-reads it: access may have changed.
                      setSelected(item.id)
                      setPageReload((value) => value + 1)
                    }}
                    className={cn(
                      "flex w-full flex-wrap items-center gap-1.5 rounded-lg px-2 py-1.5 text-start text-sm hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring",
                      selected === item.id && "bg-muted"
                    )}
                  >
                    {item.status === "readable" ? (
                      <>
                        <span className="min-w-0 truncate">{item.title}</span>
                        {item.stale && (
                          <Chip tone="warn">{t("chipStale")}</Chip>
                        )}
                        {item.lint.length > 0 && (
                          <Chip>
                            {t("lintCount", { number: item.lint.length })}
                          </Chip>
                        )}
                      </>
                    ) : (
                      <>
                        <span className="text-muted-foreground">
                          {t("withheldPage")}
                        </span>
                        <Chip tone="warn">{t("chipWithheld")}</Chip>
                      </>
                    )}
                  </button>
                </li>
              ))}
            </ul>
            <div className="min-w-0">
              {!selected ? (
                <p className="text-sm text-muted-foreground">
                  {t("choosePage")}
                </p>
              ) : !shown ? (
                <NativeSkeletonRows label={t("loading")} rows={4} />
              ) : shown.status === "withheld" ? (
                <p role="status" className="text-sm font-medium">
                  {t("withheldNotice", { number: shown.unreadableSources })}
                </p>
              ) : (
                <article
                  aria-label={shown.title}
                  className="space-y-3"
                  onClick={onPreviewClick}
                >
                  {shown.stale && (
                    <p className="text-sm text-neev-warn">{t("staleNotice")}</p>
                  )}
                  {shown.lint.length > 0 && (
                    <ul className="flex flex-wrap gap-1">
                      {shown.lint.map((code) => (
                        <li key={code}>
                          <Chip>{code}</Chip>
                        </li>
                      ))}
                    </ul>
                  )}
                  <NativeMarkdown markdown={markdown} />
                </article>
              )}
            </div>
          </div>
        )}
        {/* The log is part of the pages card, collapsed by default so the
            section stays compact (review M8). */}
        <details className="group border-t pt-3">
          <summary className="cursor-pointer text-sm font-medium">
            {t("logTitle")}
          </summary>
          <div className="mt-3 space-y-3">
            {log.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t("noLog")}</p>
            ) : (
              <ol className="space-y-1 text-sm">
                {log.map((entry) => (
                  <li key={entry.seq} className="flex flex-wrap gap-x-2">
                    <time
                      className="text-xs text-muted-foreground tabular-nums"
                      dateTime={new Date(entry.at * 1000).toISOString()}
                    >
                      {new Date(entry.at * 1000).toLocaleString()}
                    </time>
                    <code className="text-xs">{entry.event}</code>
                    {entry.pageTitle && <span>{entry.pageTitle}</span>}
                    {entry.detail && (
                      <span className="text-xs text-muted-foreground">
                        {entry.detail}
                      </span>
                    )}
                  </li>
                ))}
              </ol>
            )}
            {more && (
              <Button
                size="sm"
                variant="outline"
                disabled={loadingEarlier}
                onClick={() => void loadEarlier()}
              >
                {t("loadEarlier")}
              </Button>
            )}
          </div>
        </details>
      </section>
    </div>
  )
}
