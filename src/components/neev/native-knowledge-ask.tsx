"use client"

import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { NativeClientError } from "@/lib/neev-client"
import {
  CITE_ANCHOR,
  KNOWLEDGE_PROVIDERS,
  PAGE_ANCHOR,
} from "@/lib/neev-knowledge"
import {
  ASK_HISTORY_LIMITS,
  ASK_POLL_MS,
  ASK_QUESTION_MAX,
  askHistory,
  loadKnowledgeAsk,
  prepareAskMarkdown,
  startKnowledgeAsk,
  stopKnowledgeAsk,
  type KnowledgeAsk,
} from "@/lib/neev-knowledge-ask"
import {
  loadAgentCapabilities,
  loadAgentConnections,
  NEEV_DEFAULT_MODEL,
  type NeevAgentProviderCatalog,
} from "@/lib/neev-agent-sessions-client"
import { useNativeAuth } from "./native-auth-provider"
import { NativeError } from "./native-boundary"
import { useOpenInWorkspace } from "./native-knowledge"
import { NativeMarkdown } from "./native-markdown"

/**
 * How often an answer on screen is re-checked. The server discards it the
 * moment a source it read stops being shared; this is how that reaches here.
 */
const REVALIDATE_MS = 30_000

type Notice =
  | "askExpired"
  | "askRevoked"
  | "askNotAdmin"
  | "askReconnect"
  | "askTimeout"
  | "askInvalidOutput"
  | "askTooLarge"
  | "askBusy"
  | "askFailed"

type Turn = {
  /** Local only, for React keys. */
  seq: number
  question: string
  /** Null once the server no longer has it (or no longer lets us read it). */
  ask: KnowledgeAsk | null
  notice: Notice | null
}

function failureNotice(code: string): Notice {
  switch (code) {
    case "source_revoked":
      return "askRevoked"
    case "not_admin":
      return "askNotAdmin"
    case "credential_missing":
    case "unauthorized":
      return "askReconnect"
    case "timeout":
      return "askTimeout"
    case "invalid_output":
      return "askInvalidOutput"
    case "too_large":
      return "askTooLarge"
    case "lanes_busy":
      return "askBusy"
    default:
      return "askFailed"
  }
}

/**
 * Whether `next` may replace what is on screen. A running ask may become
 * anything; a finished one only ever becomes failed (revoked, or no longer
 * admin) — a late poll can never bring back an answer that was discarded,
 * nor un-stop a stopped one.
 */
function accepts(held: KnowledgeAsk, next: KnowledgeAsk) {
  if (held.id !== next.id) return false
  if (held.state === "running") return true
  if (held.state === "answered" || held.state === "insufficient")
    return next.state === "failed" || next.state === held.state
  return false
}

/**
 * Settings → Knowledge → Test knowledge (admins only).
 *
 * A temporary test: a question answered from the saved pages this admin can
 * read right now, on their own connected agent, with server-checked citations.
 * Nothing is saved — the conversation lives in this component and is gone when
 * it unmounts (a running ask is stopped then). It never writes to the wiki and
 * is not what members or agents use.
 */
export function NativeKnowledgeAsk({
  onOpenPage,
}: {
  /** Show a saved page in the section's page view. */
  onOpenPage?: (pageId: string) => void
} = {}) {
  const t = useTranslations("NativeKnowledge")
  const auth = useNativeAuth()
  const { handleError } = auth
  const csrf = auth.session?.csrf
  const openInWorkspace = useOpenInWorkspace()
  const headingId = useId()
  const questionId = useId()

  const [agents, setAgents] = useState<string[] | null>(null)
  const [agentsFailed, setAgentsFailed] = useState(false)
  const [catalogs, setCatalogs] = useState<readonly NeevAgentProviderCatalog[]>(
    []
  )
  const [provider, setProvider] = useState("")
  const [model, setModel] = useState("")

  const [turns, setTurns] = useState<Turn[]>([])
  const [draft, setDraft] = useState("")
  const [sending, setSending] = useState(false)
  const [startNotice, setStartNotice] = useState<
    "askConflict" | "askStartBusy" | "askReconnect" | "askLimited" | null
  >(null)
  const [error, setError] = useState<unknown>(null)
  const seq = useRef(0)
  /** Bumped by "New test" and unmount: an answer to an older test is dropped. */
  const generation = useRef(0)
  const live = useRef(true)
  useEffect(() => {
    live.current = true
    return () => {
      live.current = false
    }
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    loadAgentConnections(controller.signal)
      .then((wiring) => {
        if (controller.signal.aborted) return
        const connected = wiring.connections
          .filter(
            (item) =>
              item.state === "connected" &&
              KNOWLEDGE_PROVIDERS.includes(item.provider)
          )
          .map((item) => item.provider as string)
        setAgents(connected)
        setProvider((old) => old || connected[0] || "")
      })
      .catch(() => {
        if (controller.signal.aborted) return
        setAgents([])
        setAgentsFailed(true)
      })
    loadAgentCapabilities(controller.signal)
      .then((catalog) => {
        if (!controller.signal.aborted) setCatalogs(catalog.providers)
      })
      .catch(() => undefined)
    return () => controller.abort()
  }, [])

  const models = (() => {
    const catalog = catalogs.find((entry) => entry.provider === provider)
    const ids = catalog?.models.map((entry) => entry.id) ?? []
    return ids.length ? ids : [NEEV_DEFAULT_MODEL]
  })()
  const preferred =
    catalogs
      .find((entry) => entry.provider === provider)
      ?.models.find((entry) => entry.isDefault)?.id ?? models[0]
  const chosenModel = models.includes(model) ? model : preferred

  /** Put the server's latest word about an ask on screen, if it may. */
  const apply = useCallback((next: KnowledgeAsk) => {
    if (!live.current) return
    setTurns((old) =>
      old.map((turn) =>
        turn.ask && accepts(turn.ask, next)
          ? {
              ...turn,
              ask: next,
              notice:
                next.state === "failed" && next.error
                  ? failureNotice(next.error.code)
                  : null,
            }
          : turn
      )
    )
  }, [])
  /** The server no longer has it, or no longer lets us read it. */
  const drop = useCallback((askId: string, notice: Notice) => {
    if (!live.current) return
    setTurns((old) =>
      old.map((turn) =>
        turn.ask?.id === askId ? { ...turn, ask: null, notice } : turn
      )
    )
  }, [])
  /**
   * Read an ask again and show what the server says now. Resolves to the
   * fresh ask, or null when it could not be read — gone, forbidden, or a
   * transient failure (the next tick asks again).
   */
  const check = useCallback(
    async (askId: string): Promise<KnowledgeAsk | null> => {
      try {
        const next = await loadKnowledgeAsk(askId)
        apply(next)
        return next
      } catch (cause) {
        if (!(cause instanceof NativeClientError)) return null
        if (cause.status === 404) drop(askId, "askExpired")
        else if (cause.status === 401 || cause.status === 403) {
          // Fail closed: an answer we may no longer read does not stay up.
          drop(askId, cause.status === 403 ? "askNotAdmin" : "askFailed")
          handleError(cause)
        }
        return null
      }
    },
    [apply, drop, handleError]
  )

  const running = turns.find((turn) => turn.ask?.state === "running")?.ask?.id
  const runningRef = useRef<string | undefined>(undefined)
  const csrfRef = useRef(csrf)
  useEffect(() => {
    runningRef.current = running
    csrfRef.current = csrf
  }, [running, csrf])

  // Poll the running ask, and only it, until it finishes.
  useEffect(() => {
    if (!running) return
    let inFlight = false
    const timer = setInterval(() => {
      if (inFlight) return
      inFlight = true
      void check(running).finally(() => {
        inFlight = false
      })
    }, ASK_POLL_MS)
    return () => clearInterval(timer)
  }, [running, check])

  // Re-check what is on screen, slowly, while it is on screen and visible.
  const shownIds = turns
    .filter(
      (turn) =>
        turn.ask?.state === "answered" || turn.ask?.state === "insufficient"
    )
    .map((turn) => turn.ask!.id)
    .join(" ")
  useEffect(() => {
    if (!shownIds) return
    const ids = shownIds.split(" ")
    const recheckAll = () => {
      if (typeof document !== "undefined" && document.hidden) return
      for (const id of ids) void check(id)
    }
    const timer = setInterval(recheckAll, REVALIDATE_MS)
    // Back from a hidden tab: re-check at once rather than up to 30 s later.
    document.addEventListener("visibilitychange", recheckAll)
    return () => {
      clearInterval(timer)
      document.removeEventListener("visibilitychange", recheckAll)
    }
  }, [shownIds, check])

  // Leaving stops a running test: nothing outlives the panel.
  useEffect(
    () => () => {
      generation.current += 1
      const id = runningRef.current
      const token = csrfRef.current
      if (id && token) void stopKnowledgeAsk(id, token).catch(() => undefined)
    },
    []
  )

  async function send() {
    const question = draft.trim()
    if (!question || sending || running || !provider || !csrf) return
    const mine = generation.current
    setSending(true)
    setStartNotice(null)
    setError(null)
    try {
      // Follow-up context is re-read first, never sent from what is on screen:
      // an answer revoked since the last re-check must not reach the agent
      // (review FB1). Only what the server still holds, unchanged, is sent;
      // anything that cannot be confirmed right now is left out.
      const held = turns
        .flatMap((turn) =>
          turn.ask?.state === "answered" || turn.ask?.state === "insufficient"
            ? [turn.ask]
            : []
        )
        .slice(-ASK_HISTORY_LIMITS.turns)
      const fresh = await Promise.all(held.map((item) => check(item.id)))
      if (!live.current || mine !== generation.current) return
      const confirmed = fresh.filter(
        (next, index): next is KnowledgeAsk =>
          !!next && next.state === held[index].state
      )
      const started = await startKnowledgeAsk(
        {
          provider,
          model: chosenModel,
          question,
          history: askHistory(confirmed),
        },
        csrf
      )
      if (mine !== generation.current) {
        // "New test" (or leaving) while it started: stop it, show nothing.
        if (started.state === "running")
          void stopKnowledgeAsk(started.id, csrf).catch(() => undefined)
        return
      }
      seq.current += 1
      setTurns((old) => [
        ...old,
        {
          seq: seq.current,
          question: started.question,
          ask: started,
          notice:
            started.state === "failed" && started.error
              ? failureNotice(started.error.code)
              : null,
        },
      ])
      setDraft("")
    } catch (cause) {
      if (!live.current || mine !== generation.current) return
      if (cause instanceof NativeClientError && cause.status === 409)
        setStartNotice(
          cause.code === "ask_busy"
            ? "askStartBusy"
            : cause.code === "credential_missing"
              ? "askReconnect"
              : "askConflict"
        )
      else if (cause instanceof NativeClientError && cause.status === 429)
        setStartNotice("askLimited")
      else {
        setError(cause)
        handleError(cause)
      }
    } finally {
      if (live.current) setSending(false)
    }
  }

  async function stop(askId: string) {
    if (!csrf) return
    try {
      apply(await stopKnowledgeAsk(askId, csrf))
    } catch (cause) {
      if (cause instanceof NativeClientError && cause.status === 404)
        drop(askId, "askExpired")
      else if (live.current) {
        setError(cause)
        handleError(cause)
      }
    }
  }

  function startOver() {
    generation.current += 1
    if (running && csrf)
      void stopKnowledgeAsk(running, csrf).catch(() => undefined)
    setTurns([])
    setStartNotice(null)
    setError(null)
  }

  const noAgent = agents !== null && agents.length === 0 && !agentsFailed
  const canAsk =
    !!agents?.length && !!provider && !!csrf && !sending && !running
  const muted = "text-sm text-muted-foreground"
  const select =
    "h-10 rounded-lg border bg-background px-2 text-base sm:h-8 sm:text-sm"

  return (
    <section
      className="space-y-3 rounded-xl border bg-card p-4"
      aria-labelledby={headingId}
    >
      <div className="flex items-center justify-between gap-3">
        <h3 id={headingId} className="text-sm font-medium">
          {t("askTitle")}
        </h3>
        {turns.length > 0 && (
          <Button size="sm" variant="ghost" onClick={startOver}>
            {t("askNew")}
          </Button>
        )}
      </div>
      <p className={muted}>{t("askIntro")}</p>
      <p className="text-xs text-muted-foreground">
        {t("askTemporary")} {t("askProviderCopies")}
      </p>

      {turns.length > 0 && (
        <ol className="space-y-4" aria-live="polite">
          {turns.map((turn) => (
            <li key={turn.seq} className="space-y-2">
              <p className="ms-auto w-fit max-w-[85%] whitespace-pre-wrap break-words rounded-lg bg-muted px-3 py-2 text-sm">
                <span className="sr-only">{t("askYourQuestion")}: </span>
                {turn.question}
              </p>
              <TurnResult
                turn={turn}
                onStop={stop}
                onOpenSource={(cite) =>
                  openInWorkspace({
                    kind: "resource",
                    resourceId: cite.sourceId,
                    nodeId: cite.nodeId,
                  })
                }
                onOpenPage={onOpenPage}
              />
            </li>
          ))}
        </ol>
      )}

      <NativeError error={error} />
      {startNotice && (
        <p role="alert" className="text-sm font-medium">
          {t(startNotice)}
        </p>
      )}
      {agentsFailed ? (
        <p role="alert" className="text-sm text-destructive">
          {t("connectionsFailed")}
        </p>
      ) : (
        noAgent && <p className={muted}>{t("askNoAgent")}</p>
      )}

      <form
        className="space-y-2"
        onSubmit={(event) => {
          event.preventDefault()
          void send()
        }}
      >
        <label htmlFor={questionId} className="sr-only">
          {t("askQuestion")}
        </label>
        <textarea
          id={questionId}
          value={draft}
          rows={2}
          maxLength={ASK_QUESTION_MAX}
          placeholder={t("askPlaceholder")}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (
              event.key === "Enter" &&
              !event.shiftKey &&
              !event.nativeEvent.isComposing
            ) {
              event.preventDefault()
              void send()
            }
          }}
          className="block min-h-16 w-full resize-y rounded-lg border bg-background px-3 py-2 text-base sm:text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        />
        <div className="flex flex-wrap items-center justify-end gap-2">
          {!!agents?.length && (
            <>
              <label className="flex items-center gap-1.5 text-xs text-muted-foreground">
                {t("agent")}
                <select
                  value={provider}
                  disabled={sending || !!running}
                  onChange={(event) => {
                    setProvider(event.target.value)
                    setModel("")
                  }}
                  className={select}
                >
                  {agents.map((id) => (
                    <option key={id} value={id}>
                      {catalogs.find((entry) => entry.provider === id)?.label ??
                        id}
                    </option>
                  ))}
                </select>
              </label>
              <label className="flex items-center gap-1.5 text-xs text-muted-foreground">
                {t("model")}
                <select
                  value={chosenModel}
                  disabled={sending || !!running}
                  onChange={(event) => setModel(event.target.value)}
                  className={select}
                >
                  {models.map((id) => (
                    <option key={id} value={id}>
                      {catalogs
                        .find((entry) => entry.provider === provider)
                        ?.models.find((entry) => entry.id === id)?.label ?? id}
                    </option>
                  ))}
                </select>
              </label>
            </>
          )}
          <Button size="sm" type="submit" disabled={!canAsk}>
            {t("askSend")}
          </Button>
        </div>
      </form>
    </section>
  )
}

type Cite = KnowledgeAsk["citations"][number]

function TurnResult({
  turn,
  onStop,
  onOpenSource,
  onOpenPage,
}: {
  turn: Turn
  onStop: (askId: string) => void
  onOpenSource: (cite: Cite) => void
  onOpenPage?: (pageId: string) => void
}) {
  const t = useTranslations("NativeKnowledge")
  const ask = turn.ask
  const markdown = useMemo(
    () =>
      ask?.state === "answered"
        ? prepareAskMarkdown(ask, t("withheldCite"))
        : "",
    [ask, t]
  )
  const muted = "text-sm text-muted-foreground"

  if (!ask)
    return (
      <p role="status" className={muted}>
        {t(turn.notice ?? "askExpired")}
      </p>
    )
  if (ask.state === "running")
    return (
      <div className="flex items-center gap-3">
        <p role="status" className={muted}>
          {t("askThinking")}
        </p>
        <Button size="sm" variant="outline" onClick={() => onStop(ask.id)}>
          {t("askStop")}
        </Button>
      </div>
    )
  if (ask.state === "canceled")
    return <p className={muted}>{t("askCanceled")}</p>
  if (ask.state === "failed")
    return (
      <div className="space-y-1">
        <p role="alert" className="text-sm font-medium">
          {t(turn.notice ?? failureNotice(ask.error?.code ?? ""))}
        </p>
        {/* The provider's own words, already redacted by the server. */}
        {ask.error?.code === "provider" && ask.error.message && (
          <p className="text-xs text-muted-foreground">{ask.error.message}</p>
        )}
      </div>
    )
  const considered = (
    <p className="text-xs text-muted-foreground">
      {t("askPagesConsidered", { number: ask.pagesConsidered })}
    </p>
  )
  if (ask.state === "insufficient") {
    const reason = ask.insufficient?.reason
    return (
      <div className="space-y-1">
        <p className="text-sm font-medium">
          {t(
            reason === "no_readable_pages"
              ? "askNoPages"
              : reason === "ungrounded"
                ? "askUngrounded"
                : "askNotCovered"
          )}
        </p>
        {reason === "agent" && ask.insufficient?.note && (
          <p className={muted}>{ask.insufficient.note}</p>
        )}
        {reason !== "no_readable_pages" && considered}
      </div>
    )
  }

  const slugs = new Map(ask.pages.map((page) => [page.slug, page.id]))
  function onClick(event: React.MouseEvent) {
    if (!ask) return
    const href =
      (event.target as HTMLElement).closest("a")?.getAttribute("href") ?? ""
    if (href.startsWith(CITE_ANCHOR)) {
      event.preventDefault()
      const cite = ask.citations.find(
        (item) => item.key === href.slice(CITE_ANCHOR.length)
      )
      if (cite) onOpenSource(cite)
    } else if (href.startsWith(PAGE_ANCHOR)) {
      event.preventDefault()
      const id = slugs.get(href.slice(PAGE_ANCHOR.length))
      if (id) onOpenPage?.(id)
    }
  }
  const chip =
    "rounded-md border px-2 py-0.5 text-xs hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
  return (
    <article
      aria-label={t("askAnswer")}
      className="space-y-2"
      onClick={onClick}
    >
      <NativeMarkdown markdown={markdown} />
      {ask.citations.length > 0 && (
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-xs text-muted-foreground">
            {t("askSources")}
          </span>
          <ul aria-label={t("askSources")} className="flex flex-wrap gap-1.5">
            {ask.citations.map((cite) => (
              <li key={cite.key}>
                <button
                  type="button"
                  className={chip}
                  onClick={() => onOpenSource(cite)}
                >
                  {cite.title}
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
      {ask.pages.length > 0 && (
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-xs text-muted-foreground">{t("askPages")}</span>
          <ul aria-label={t("askPages")} className="flex flex-wrap gap-1.5">
            {ask.pages.map((page) => (
              <li key={page.id}>
                {onOpenPage ? (
                  <button
                    type="button"
                    className={chip}
                    onClick={() => onOpenPage(page.id)}
                  >
                    {page.title}
                  </button>
                ) : (
                  <span className={cn(chip, "hover:bg-transparent")}>
                    {page.title}
                  </span>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}
      {considered}
    </article>
  )
}
