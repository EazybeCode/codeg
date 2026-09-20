"use client"

// Conductor-style saved Agent Chats (spec §5): a private, owner-scoped session
// list and a full-canvas session view with server-owned turn history.
//
// Two honesty rules shape this file:
//  - Nothing claims a capability the server has not confirmed. Connection
//    labels come from `GET /api/neev/agent/connections`, run outcomes from the
//    turn's stored `status`/`errorCode`. There is no optimistic "Connected" and
//    no invented queue position.
//  - Reconnecting REATTACHES, never re-runs. Every view of a run — the first
//    one and one after a refresh — is the same GET on `…/runs/{id}/events`, so
//    a dropped connection can never repeat a prompt's side effects. A run that
//    cannot be followed is reported as such and needs an explicit new turn.

import { useCallback, useEffect, useRef, useState } from "react"
import Link from "next/link"
import { useTranslations } from "next-intl"
import { ArrowLeft, ArrowUp, Bot, MoreHorizontal, Plus } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogDescription, DialogTitle } from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { NativeClientError } from "@/lib/neev-client"
import {
  cancelAgentRun,
  createAgentSession,
  followAgentRun,
  listAgentSessions,
  listAgentTurns,
  loadAgentConnections,
  startAgentTurn,
  updateAgentSession,
  NEEV_AGENT_PROVIDERS,
  NEEV_DEFAULT_MODEL,
  isSupportedModel,
  NEEV_MAX_PROMPT_CHARS,
  type NeevAgentConnections,
  type NeevAgentProvider,
  type NeevAgentSession,
  type NeevAgentTurn,
  type NeevRunPhase,
  type UpdateNeevAgentSessionInput,
} from "@/lib/neev-agent-sessions-client"
import { NativeContentNotFoundError } from "@/lib/neev-content-client"
import type { ContentNavigationGuard } from "./native-content"
import { useNativeAuth } from "./native-auth-provider"
import { NativeError } from "./native-boundary"
import { NativeMarkdown } from "./native-markdown"
import { NativeAgentConnectionsPanel } from "./native-agent-connections"
import {
  NativeDialogContent as DialogContent,
  NativeDropdownMenuContent as DropdownMenuContent,
} from "./native-suspended-surface"

/** Error codes the server can store on a turn; anything else reads as unknown. */
const TURN_ERROR_CODES = [
  "disabled",
  "unauthorized",
  "isolation",
  "provider",
  "unavailable",
  "unsupported",
  "canceled",
  "io",
  "interrupted",
] as const
type TurnErrorCode = (typeof TURN_ERROR_CODES)[number]

function isTurnErrorCode(value: string | null): value is TurnErrorCode {
  return (
    value !== null && (TURN_ERROR_CODES as readonly string[]).includes(value)
  )
}

type Props = {
  onRequestSettings?: () => void
  onRequestNavigationGuard?: (guard: ContentNavigationGuard) => void
}

export function NativeAgentChats(props: Props) {
  const auth = useNativeAuth()
  if (!auth.session?.organization) return null
  return <AgentChats key={auth.scopeKey} {...props} />
}

function AgentChats({ onRequestSettings, onRequestNavigationGuard }: Props) {
  const auth = useNativeAuth(),
    t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  const [sessions, setSessions] = useState<NeevAgentSession[] | null>(null),
    [connections, setConnections] = useState<NeevAgentConnections | null>(null),
    [showArchived, setShowArchived] = useState(false),
    [open, setOpen] = useState<NeevAgentSession | null>(null),
    [creating, setCreating] = useState(false),
    [error, setError] = useState<unknown>(null)

  // Bumped to re-read the list and the connection states from the server.
  const [refreshToken, setRefreshToken] = useState(0)
  const reload = useCallback(() => setRefreshToken((value) => value + 1), [])

  useEffect(() => {
    const controller = new AbortController()
    const load = async () => {
      try {
        const [list, wiring] = await Promise.all([
          listAgentSessions(showArchived, controller.signal),
          loadAgentConnections(controller.signal),
        ])
        if (controller.signal.aborted) return
        setSessions(list)
        setConnections(wiring)
        setError(null)
      } catch (cause) {
        if (controller.signal.aborted) return
        setError(cause)
        setSessions((old) => old ?? [])
        auth.handleAuthorityError(cause)
      }
    }
    void load()
    return () => controller.abort()
  }, [showArchived, refreshToken, auth])

  // Keep the freshly-changed session visible without a round trip; the server's
  // own row is what we store, so nothing here is optimistic.
  const absorb = useCallback((session: NeevAgentSession) => {
    setSessions((old) => {
      const rest = (old ?? []).filter((item) => item.id !== session.id)
      return [session, ...rest].sort((a, b) => b.updatedAt - a.updatedAt)
    })
    setOpen((current) => (current?.id === session.id ? session : current))
  }, [])

  if (open)
    return (
      <AgentSessionView
        session={open}
        connections={connections}
        onBack={() => setOpen(null)}
        onChanged={absorb}
        onGone={() => {
          setOpen(null)
          reload()
        }}
        onRequestSettings={onRequestSettings}
        onRequestNavigationGuard={onRequestNavigationGuard}
      />
    )

  return (
    <div className="mx-auto w-full max-w-3xl space-y-6 overflow-y-auto px-6 py-10">
      <div className="flex items-start gap-3">
        <div className="flex size-11 shrink-0 items-center justify-center rounded-xl bg-muted">
          <Bot aria-hidden className="size-5 text-muted-foreground" />
        </div>
        <div className="min-w-0 flex-1">
          <h1 className="text-2xl font-semibold tracking-tight">
            {t("agentChats")}
          </h1>
          <p className="text-sm leading-6 text-muted-foreground">
            {t("agentChatsPrivate")}
          </p>
        </div>
        <Button onClick={() => setCreating(true)}>
          <Plus aria-hidden />
          {t("newAgentChat")}
        </Button>
      </div>

      <NativeAgentConnectionsPanel
        connections={connections}
        onRefresh={reload}
        onRequestSettings={onRequestSettings}
      />

      <NativeError error={error} />

      <div className="flex items-center justify-between">
        <h2 className="text-sm font-medium">{t("agentSessions")}</h2>
        <Button
          variant="ghost"
          size="sm"
          aria-pressed={showArchived}
          onClick={() => setShowArchived((value) => !value)}
        >
          {t("showArchived")}
        </Button>
      </div>

      {sessions === null ? (
        <p role="status">{n("loading")}</p>
      ) : sessions.length === 0 ? (
        <p className="rounded-xl border p-5 text-sm text-muted-foreground">
          {t("emptyAgentChats")}
        </p>
      ) : (
        <ul aria-label={t("agentSessions")} className="space-y-2">
          {sessions.map((session) => (
            <li key={session.id}>
              <button
                type="button"
                onClick={() => setOpen(session)}
                className="flex w-full items-center gap-3 rounded-lg border px-4 py-3 text-start hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
              >
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-medium">
                    {session.title}
                  </span>
                  <span className="block text-xs text-muted-foreground">
                    {session.provider} · {session.model}
                    {session.archived ? ` · ${t("archived")}` : ""}
                  </span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}

      <CreateSessionDialog
        open={creating}
        onClose={() => setCreating(false)}
        onCreated={(session) => {
          absorb(session)
          setCreating(false)
          setOpen(session)
        }}
      />
    </div>
  )
}

function CreateSessionDialog({
  open,
  onClose,
  onCreated,
}: {
  open: boolean
  onClose: () => void
  onCreated: (session: NeevAgentSession) => void
}) {
  const auth = useNativeAuth(),
    t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  const [title, setTitle] = useState(""),
    [provider, setProvider] = useState<NeevAgentProvider>("claude"),
    [model, setModel] = useState(NEEV_DEFAULT_MODEL),
    [busy, setBusy] = useState(false),
    [error, setError] = useState<unknown>(null)
  const request = useRef<string | null>(null)
  useEffect(() => {
    if (!open) {
      setTitle("")
      setError(null)
      request.current = null
    }
  }, [open])
  async function submit() {
    if (!title.trim() || busy || !auth.session) return
    setBusy(true)
    setError(null)
    // A retry after a lost response reuses the id, so a double submit cannot
    // create two sessions.
    request.current ??= crypto.randomUUID()
    try {
      onCreated(
        await createAgentSession(
          {
            title: title.trim(),
            provider,
            model,
            requestId: request.current,
          },
          auth.session.csrf,
          auth.signal
        )
      )
    } catch (cause) {
      setError(cause)
      auth.handleAuthorityError(cause)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent>
        <DialogTitle>{t("newAgentChat")}</DialogTitle>
        <DialogDescription>{t("agentChatsPrivate")}</DialogDescription>
        <form
          className="space-y-4"
          onSubmit={(event) => {
            event.preventDefault()
            void submit()
          }}
        >
          <label className="block space-y-1">
            <span className="text-sm font-medium">{t("title")}</span>
            <input
              value={title}
              autoFocus
              maxLength={100}
              onChange={(event) => setTitle(event.target.value)}
              className="w-full rounded-md border bg-background px-3 py-2 text-sm outline-none focus-visible:ring-1 focus-visible:ring-ring"
            />
          </label>
          <label className="block space-y-1">
            <span className="text-sm font-medium">{t("agentProvider")}</span>
            <select
              value={provider}
              onChange={(event) => {
                const next = event.target.value as NeevAgentProvider
                setProvider(next)
                setModel(NEEV_DEFAULT_MODEL)
              }}
              className="w-full rounded-md border bg-background px-3 py-2 text-sm"
            >
              {/* Only providers the runtime can actually dispatch are offered. */}
              {NEEV_AGENT_PROVIDERS.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
            </select>
          </label>
          <label className="block space-y-1">
            <span className="text-sm font-medium">{t("agentModel")}</span>
            <input
              value={model}
              maxLength={80}
              onChange={(event) => setModel(event.target.value)}
              className="w-full rounded-md border bg-background px-3 py-2 text-sm outline-none focus-visible:ring-1 focus-visible:ring-ring"
            />
            {/* `default` is the provider's own default (no --model at all). The
                UI does not invent a model menu: the runtime passes any other
                value straight through, so only the provider knows what exists. */}
            <span className="block text-xs text-muted-foreground">
              {t("agentModelHint")}
            </span>
          </label>
          <NativeError error={error} />
          <div className="flex justify-end gap-2">
            <Button type="button" variant="ghost" onClick={onClose}>
              {n("cancel")}
            </Button>
            <Button
              type="submit"
              disabled={busy || !title.trim() || !isSupportedModel(model)}
            >
              {busy ? n("working") : t("create")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function AgentSessionView({
  session,
  connections,
  onBack,
  onChanged,
  onGone,
  onRequestSettings,
  onRequestNavigationGuard,
}: {
  session: NeevAgentSession
  connections: NeevAgentConnections | null
  onBack: () => void
  onChanged: (session: NeevAgentSession) => void
  onGone: () => void
  onRequestSettings?: () => void
  onRequestNavigationGuard?: (guard: ContentNavigationGuard) => void
}) {
  const auth = useNativeAuth(),
    t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  const [turns, setTurns] = useState<NeevAgentTurn[] | null>(null),
    [draft, setDraft] = useState(""),
    [error, setError] = useState<unknown>(null),
    [sending, setSending] = useState(false),
    [renaming, setRenaming] = useState(false),
    // The run this view is attached to, and how that attachment is doing.
    [activeRun, setActiveRun] = useState<string | null>(null),
    [phase, setPhase] = useState<NeevRunPhase>("streaming"),
    [liveText, setLiveText] = useState(""),
    // Bumped to ask for a fresh attachment to the same run — an explicit
    // reattach, never a re-run.
    [attachNonce, setAttachNonce] = useState(0)
  const draftRef = useRef("")
  const history = useRef<HTMLOListElement>(null)
  // What the current attachment has streamed so far, readable from the settle
  // handler without making it depend on the rendered value.
  const liveRef = useRef("")
  // A queued navigation held back by an unsent prompt.
  const [pending, setPending] = useState<(() => void) | null>(null)

  // An unsent prompt is work in progress: hold navigation the same way an
  // unsaved document does.
  useEffect(() => {
    if (!onRequestNavigationGuard) return
    const guard: ContentNavigationGuard = (action) => {
      if (draftRef.current) setPending(() => action)
      else action()
    }
    onRequestNavigationGuard(guard)
    return () => onRequestNavigationGuard((action) => action())
  }, [onRequestNavigationGuard])

  const fail = useCallback(
    (cause: unknown) => {
      setError(cause)
      auth.handleAuthorityError(cause)
      if (
        cause instanceof NativeContentNotFoundError ||
        (cause instanceof NativeClientError &&
          (cause.kind === "forbidden" || cause.kind === "unauthorized"))
      )
        onGone()
    },
    [auth, onGone]
  )

  // Load the server-owned history. A turn still marked `running` means a run
  // outlived the page: attach to it rather than asking the user to re-send.
  useEffect(() => {
    const controller = new AbortController()
    const load = async () => {
      try {
        const collected: NeevAgentTurn[] = []
        let after: string | null = null
        do {
          const page = await listAgentTurns(
            session.id,
            after,
            controller.signal
          )
          if (controller.signal.aborted) return
          collected.push(...page.items)
          after = page.nextAfter
        } while (after)
        setTurns(collected)
        const running = collected.find((turn) => turn.status === "running")
        if (running) {
          liveRef.current = ""
          setLiveText("")
          setActiveRun(running.runId)
        }
      } catch (cause) {
        if (controller.signal.aborted) return
        fail(cause)
        setTurns((old) => old ?? [])
      }
    }
    void load()
    return () => controller.abort()
  }, [session.id, fail])

  // Attach to the active run. `followAgentRun` only ever GETs the run, so this
  // effect re-running (mount, refresh, explicit reconnect) cannot re-execute
  // the prompt.
  useEffect(() => {
    if (!activeRun) return
    const controller = new AbortController()
    void followAgentRun({
      runId: activeRun,
      signal: controller.signal,
      onPhase: (next) => {
        if (!controller.signal.aborted) setPhase(next)
      },
      onEvent: (event) => {
        if (controller.signal.aborted) return
        // A replay restarts the buffer: the server resends everything it stored.
        if (event.type === "started") {
          liveRef.current = ""
          setLiveText("")
        } else if (event.type === "chunk") {
          liveRef.current += event.text
          setLiveText(liveRef.current)
        } else {
          setTurns((old) =>
            (old ?? []).map((turn) =>
              turn.runId === activeRun
                ? {
                    ...turn,
                    status: event.status,
                    errorCode: event.errorCode,
                    // The stream replays exactly what the server stored, so the
                    // accumulated text is the durable response.
                    response: liveRef.current || turn.response,
                  }
                : turn
            )
          )
          setActiveRun(null)
        }
      },
    })
    return () => controller.abort()
    // `attachNonce` is the explicit reconnect signal: re-running this effect
    // reattaches to the same run and never starts a new one.
  }, [activeRun, attachNonce])

  useEffect(() => {
    const element = history.current
    if (element) element.scrollTop = element.scrollHeight
  }, [turns, liveText])

  const runtimeEnabled = connections?.runtimeEnabled ?? false
  const connection = connections?.connections.find(
    (item) => item.provider === session.provider
  )
  const ready = runtimeEnabled && connection?.state === "connected"
  const running = activeRun !== null

  async function send() {
    if (!draft.trim() || sending || running || !auth.session) return
    setSending(true)
    setError(null)
    try {
      const started = await startAgentTurn(
        session.id,
        draft.trim(),
        auth.session.csrf,
        auth.signal
      )
      setTurns((old) => [...(old ?? []), started.turn])
      setDraft("")
      draftRef.current = ""
      // A refused turn comes back already settled with its real reason; only a
      // genuinely running one gets an attachment.
      if (started.turn.status === "running") {
        liveRef.current = ""
        setLiveText("")
        setActiveRun(started.runId)
      }
    } catch (cause) {
      fail(cause)
    } finally {
      setSending(false)
    }
  }

  async function cancel() {
    if (!activeRun || !auth.session) return
    try {
      const outcome = await cancelAgentRun(
        activeRun,
        auth.session.csrf,
        auth.signal
      )
      // Honest: a run this instance does not hold is not cancelled by asking.
      if (!outcome.canceled && outcome.status === "running")
        setError(new NativeClientError("unavailable"))
    } catch (cause) {
      fail(cause)
    }
  }

  async function mutate(input: { title: string } | { archived: boolean }) {
    if (!auth.session) return
    try {
      const change: UpdateNeevAgentSessionInput =
        "title" in input
          ? { title: input.title, expectedRevision: session.revision }
          : { archived: input.archived, expectedRevision: session.revision }
      onChanged(
        await updateAgentSession(
          session.id,
          change,
          auth.session.csrf,
          auth.signal
        )
      )
      setError(null)
    } catch (cause) {
      fail(cause)
    }
  }

  return (
    <section
      className="flex min-h-0 flex-1 flex-col"
      aria-label={session.title}
    >
      <div className="flex shrink-0 items-start gap-3 px-5 py-5 sm:px-8">
        <Button
          variant="ghost"
          size="icon"
          aria-label={t("back")}
          title={t("back")}
          onClick={onBack}
        >
          <ArrowLeft aria-hidden />
        </Button>
        <div className="min-w-0 flex-1 space-y-1">
          <h2 className="break-words text-lg font-semibold">{session.title}</h2>
          <p className="text-xs text-muted-foreground">
            {session.provider} · {session.model} · {t("private")}
            {session.archived ? ` · ${t("archived")}` : ""}
          </p>
          <p role="status" className="text-xs text-muted-foreground">
            {runStatusLabel()}
          </p>
        </div>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="icon"
              aria-label={t("resourceActions")}
            >
              <MoreHorizontal aria-hidden />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem onSelect={() => setRenaming(true)}>
              {t("rename")}
            </DropdownMenuItem>
            <DropdownMenuItem
              onSelect={() => void mutate({ archived: !session.archived })}
            >
              {t(session.archived ? "reopen" : "archive")}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      {turns === null ? (
        <p role="status" className="px-5 sm:px-8">
          {n("loading")}
        </p>
      ) : (
        <ol
          ref={history}
          aria-label={t("agentTurns")}
          className="min-h-0 flex-1 space-y-8 overflow-y-auto overscroll-contain px-5 py-6 sm:px-8"
        >
          {turns.map((turn) => (
            <li key={turn.id} className="mx-auto w-full max-w-3xl space-y-3">
              <div className="rounded-lg bg-muted/60 px-4 py-3 text-sm">
                {turn.prompt}
              </div>
              <div className="space-y-2">
                {turn.runId === activeRun ? (
                  <div aria-live="polite" className="text-sm">
                    {liveText ? (
                      <NativeMarkdown markdown={liveText} />
                    ) : (
                      <span className="text-muted-foreground">
                        {n("working")}
                      </span>
                    )}
                  </div>
                ) : (
                  turn.response && <NativeMarkdown markdown={turn.response} />
                )}
                {turn.status !== "completed" && turn.runId !== activeRun && (
                  <p className="text-xs text-muted-foreground">
                    {outcomeLabel(turn)}
                  </p>
                )}
              </div>
            </li>
          ))}
        </ol>
      )}

      <form
        onSubmit={(event) => {
          event.preventDefault()
          void send()
        }}
        className="mx-auto w-full max-w-[832px] shrink-0 space-y-3 px-5 pb-5 pt-3 sm:px-8"
      >
        <div className="rounded-lg border bg-background p-3 shadow-sm focus-within:ring-1 focus-within:ring-ring">
          <label className="block">
            <span className="sr-only">{t("agentPrompt")}</span>
            <textarea
              placeholder={t("agentPrompt")}
              value={draft}
              maxLength={NEEV_MAX_PROMPT_CHARS}
              disabled={sending || running || session.archived || !ready}
              onChange={(event) => {
                draftRef.current = event.target.value
                setDraft(event.target.value)
              }}
              className="h-20 max-h-40 min-h-12 w-full resize-y border-0 bg-transparent px-1 py-2 text-base outline-none sm:text-sm"
            />
          </label>
          <NativeError error={error} />
          {!ready && (
            <p role="status" className="px-1 text-xs text-muted-foreground">
              {connectionBlocker()}{" "}
              <Link
                href="/settings"
                onClick={(event) => {
                  if (onRequestSettings) {
                    event.preventDefault()
                    onRequestSettings()
                  }
                }}
                className="rounded underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
              >
                {n("personalAgents")}
              </Link>
            </p>
          )}
          <div className="flex items-center gap-2">
            {running && (
              <Button
                type="button"
                variant="ghost"
                onClick={() => void cancel()}
              >
                {n("cancel")}
              </Button>
            )}
            {phase === "lost" && running && (
              <Button
                type="button"
                variant="outline"
                onClick={() => setAttachNonce((value) => value + 1)}
              >
                {t("agentReconnect")}
              </Button>
            )}
            <Button
              type="submit"
              size="icon"
              className="ms-auto rounded-md"
              aria-label={sending ? n("working") : t("send")}
              title={sending ? n("working") : t("send")}
              disabled={
                sending ||
                running ||
                session.archived ||
                !ready ||
                !draft.trim()
              }
            >
              <ArrowUp aria-hidden />
            </Button>
          </div>
        </div>
      </form>

      <Dialog
        open={pending !== null}
        onOpenChange={(next) => !next && setPending(null)}
      >
        <DialogContent>
          <DialogTitle>{t("discardTitle")}</DialogTitle>
          <DialogDescription>{t("discardDescription")}</DialogDescription>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setPending(null)}>
              {t("keepEditing")}
            </Button>
            <Button
              onClick={() => {
                const action = pending
                setPending(null)
                setDraft("")
                draftRef.current = ""
                action?.()
              }}
            >
              {t("discard")}
            </Button>
          </div>
        </DialogContent>
      </Dialog>

      {renaming && (
        <RenameDialog
          current={session.title}
          onClose={() => setRenaming(false)}
          onRename={async (title) => {
            await mutate({ title })
            setRenaming(false)
          }}
        />
      )}
    </section>
  )

  function connectionBlocker() {
    if (!runtimeEnabled) return t("agentsOff")
    return t(`connectionState_${connection?.state ?? "unavailable"}`)
  }

  function runStatusLabel() {
    if (running)
      return phase === "streaming"
        ? t("agentRunning")
        : phase === "reconnecting"
          ? t("agentReattaching")
          : t("agentRunLost")
    if (!runtimeEnabled) return t("agentsOff")
    return t(`connectionState_${connection?.state ?? "unavailable"}`)
  }

  function outcomeLabel(turn: NeevAgentTurn) {
    if (turn.status === "running") return t("agentRunLost")
    return isTurnErrorCode(turn.errorCode)
      ? t(`agentError_${turn.errorCode}`)
      : t("agentError_unknown")
  }
}

/** Mounted only while renaming, so the field seeds from the current title. */
function RenameDialog({
  current,
  onClose,
  onRename,
}: {
  current: string
  onClose: () => void
  onRename: (title: string) => Promise<void>
}) {
  const t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  const [value, setValue] = useState(current)
  return (
    <Dialog open onOpenChange={(next) => !next && onClose()}>
      <DialogContent>
        <DialogTitle>{t("rename")}</DialogTitle>
        <DialogDescription>{t("agentChatsPrivate")}</DialogDescription>
        <form
          className="space-y-4"
          onSubmit={(event) => {
            event.preventDefault()
            void onRename(value.trim())
          }}
        >
          <label className="block space-y-1">
            <span className="text-sm font-medium">{t("title")}</span>
            <input
              value={value}
              autoFocus
              maxLength={100}
              onChange={(event) => setValue(event.target.value)}
              className="w-full rounded-md border bg-background px-3 py-2 text-sm outline-none focus-visible:ring-1 focus-visible:ring-ring"
            />
          </label>
          <div className="flex justify-end gap-2">
            <Button type="button" variant="ghost" onClick={onClose}>
              {n("cancel")}
            </Button>
            <Button type="submit" disabled={!value.trim()}>
              {n("save")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  )
}
