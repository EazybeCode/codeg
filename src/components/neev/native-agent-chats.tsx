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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { NativeClientError } from "@/lib/neev-client"
import {
  cancelAgentRun,
  createAgentSession,
  setAgentSessionNode,
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
  /** The project this surface was opened from; null outside one. */
  nodeId?: number | null
  nodeName?: string
  /** Open this saved session directly, as the sidebar does. */
  sessionId?: string | null
  /**
   * Start a NEW chat with this provider: straight to a composer, no dialog.
   * The session is created by the first message, titled from what was typed.
   */
  draftProvider?: NeevAgentProvider | null
  /** A session the composer just created, so the tree can select it. */
  onSessionCreated?: (session: NeevAgentSession) => void
  onDraftCancelled?: () => void
  /**
   * Leave the open chat.
   *
   * The sidebar owns the selection, so clearing it here would change nothing:
   * `sessionId` would still name the same session and it would re-open on the
   * next render. Only the parent can actually go back.
   */
  onClosed?: () => void
}

export function NativeAgentChats(props: Props) {
  const auth = useNativeAuth()
  if (!auth.session?.organization) return null
  return <AgentChats key={auth.scopeKey} {...props} />
}

function AgentChats({
  onRequestSettings,
  onRequestNavigationGuard,
  nodeId = null,
  nodeName,
  sessionId = null,
  draftProvider = null,
  onSessionCreated,
  onDraftCancelled,
  onClosed,
}: Props) {
  const auth = useNativeAuth(),
    t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  const [sessions, setSessions] = useState<NeevAgentSession[] | null>(null),
    [connections, setConnections] = useState<NeevAgentConnections | null>(null),
    [showArchived, setShowArchived] = useState(false),
    [open, setOpen] = useState<NeevAgentSession | null>(null),
    [showUnfiled, setShowUnfiled] = useState(false),
    [filing, setFiling] = useState<string | null>(null),
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

  /**
   * What this project shows, and what is not in any project yet.
   *
   * Outside a project every session is shown, so nothing is ever unreachable.
   * Inside one, only its own — showing the same global list under every project
   * would say each of them contains all of it, which is not true.
   */
  const shown = (sessions ?? []).filter(
    (session) => nodeId === null || session.nodeId === nodeId
  )
  const unfiled = (sessions ?? []).filter((session) => session.nodeId === null)

  // Keep the freshly-changed session visible without a round trip; the server's
  // own row is what we store, so nothing here is optimistic.
  const absorb = useCallback((session: NeevAgentSession) => {
    setSessions((old) => {
      const rest = (old ?? []).filter((item) => item.id !== session.id)
      return [session, ...rest].sort((a, b) => b.updatedAt - a.updatedAt)
    })
    setOpen((current) => (current?.id === session.id ? session : current))
  }, [])

  /**
   * File an unfiled session under this project.
   *
   * `expectedRevision` is what makes this safe: the server refuses the move if
   * the session changed underneath, so a concurrent rename is never silently
   * overwritten. The server's own row comes back and replaces ours.
   */
  const file = useCallback(
    async (session: NeevAgentSession) => {
      if (nodeId === null || !auth.session || filing) return
      setFiling(session.id)
      setError(null)
      try {
        absorb(
          await setAgentSessionNode(
            session.id,
            nodeId,
            session.revision,
            auth.session.csrf,
            auth.signal
          )
        )
      } catch (cause) {
        setError(cause)
        auth.handleAuthorityError(cause)
      } finally {
        setFiling(null)
      }
    },
    [auth, nodeId, filing, absorb]
  )

  // A new chat is a composer, not a form: the provider came from the menu that
  // opened it, and the first message is what creates the session.
  if (draftProvider)
    return (
      <NewAgentChat
        key={`${nodeId ?? "none"}:${draftProvider}`}
        provider={draftProvider}
        nodeId={nodeId}
        connections={connections}
        connectionsLoading={connections === null && error === null}
        onRequestNavigationGuard={onRequestNavigationGuard}
        onRequestSettings={onRequestSettings}
        onCancel={() => onDraftCancelled?.()}
        onCreated={(session) => {
          absorb(session)
          setOpen(session)
          onSessionCreated?.(session)
        }}
      />
    )

  // The sidebar owns selection, so a named session opens straight into its
  // transcript rather than through this surface's own list.
  const selected = sessionId
    ? (sessions ?? []).find((item) => item.id === sessionId)
    : null
  if (sessionId && !selected)
    return sessions === null ? (
      <p role="status" className="px-6 py-10 text-sm text-muted-foreground">
        {n("loading")}
      </p>
    ) : (
      // The list has resolved and this is not in it: deleted, archived, or
      // filed elsewhere. An endless "Loading" would be a lie.
      <div className="space-y-3 px-6 py-10">
        <p role="status" className="text-sm text-muted-foreground">
          {t("agentChatMissing")}
        </p>
        <Button size="sm" variant="outline" onClick={reload}>
          {n("retry")}
        </Button>
      </div>
    )
  if (selected || open)
    return (
      <AgentSessionView
        session={(selected ?? open) as NeevAgentSession}
        connections={connections}
        onBack={() => {
          setOpen(null)
          // The parent holds the pointer that decides what is shown.
          onClosed?.()
        }}
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

      <NativeError error={error} />

      <div className="flex items-center justify-between">
        <h2 className="text-sm font-medium">
          {nodeName
            ? t("agentChatsIn", { name: nodeName })
            : t("agentSessions")}
        </h2>
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
      ) : shown.length === 0 ? (
        <p className="rounded-xl border p-5 text-sm text-muted-foreground">
          {t("emptyAgentChats")}
        </p>
      ) : (
        <ul aria-label={t("agentSessions")} className="space-y-2">
          {shown.map((session) => (
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

      {nodeId !== null && unfiled.length > 0 && (
        <div className="rounded-xl border p-4">
          <Button
            variant="ghost"
            size="sm"
            aria-expanded={showUnfiled}
            onClick={() => setShowUnfiled((value) => !value)}
          >
            {t("unfiledAgentChats", { number: unfiled.length })}
          </Button>
          {showUnfiled && (
            <ul
              aria-label={t("unfiledAgentSessions")}
              className="mt-2 space-y-1"
            >
              {unfiled.map((session) => (
                <li
                  key={session.id}
                  className="flex items-center justify-between gap-3 text-sm"
                >
                  <span className="min-w-0 flex-1 truncate">
                    {session.title}
                  </span>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={filing !== null}
                    onClick={() => void file(session)}
                  >
                    {t("moveAgentChatHere")}
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      <CreateSessionDialog
        open={creating}
        onClose={() => setCreating(false)}
        nodeId={nodeId}
        onCreated={(session) => {
          absorb(session)
          setCreating(false)
          setOpen(session)
        }}
      />
    </div>
  )
}

/**
 * Derive a session title from the first message.
 *
 * The user came here to type, not to name something. A title still has to exist
 * — it is how the chat is found again — so it is taken from the opening line,
 * trimmed to the server's limit on whole characters rather than bytes.
 */
export function deriveSessionTitle(prompt: string): string {
  const line = prompt.trim().split(/\r?\n/, 1)[0]?.trim() ?? ""
  const characters = [...line]
  if (characters.length === 0) return ""
  if (characters.length <= 60) return line
  return `${characters.slice(0, 59).join("").trimEnd()}…`
}

/**
 * A new agent chat: an empty transcript and a composer, and nothing else.
 *
 * The provider is already decided — it came from the menu item that opened this
 * — so the only decision left here is the model, and `default` is the answer
 * unless the operator has a specific one. The session is created by the first
 * message so that an abandoned draft leaves nothing behind.
 */
function NewAgentChat({
  provider: initialProvider,
  nodeId,
  connections,
  connectionsLoading,
  onCreated,
  onCancel,
  onRequestSettings,
  onRequestNavigationGuard,
}: {
  provider: NeevAgentProvider
  nodeId: number | null
  connections: NeevAgentConnections | null
  connectionsLoading: boolean
  onCreated: (session: NeevAgentSession) => void
  onCancel: () => void
  onRequestSettings?: () => void
  onRequestNavigationGuard?: (guard: ContentNavigationGuard) => void
}) {
  const auth = useNativeAuth(),
    t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  const [provider, setProvider] = useState<NeevAgentProvider>(initialProvider)
  const [draft, setDraft] = useState(""),
    [model, setModel] = useState(NEEV_DEFAULT_MODEL),
    [advanced, setAdvanced] = useState(false),
    [busy, setBusy] = useState(false),
    [error, setError] = useState<unknown>(null)
  // The menu chose a provider; re-opening for another one must follow it.
  useEffect(() => setProvider(initialProvider), [initialProvider])
  const request = useRef<string | null>(null)
  // A session the first turn failed on. It EXISTS, so a retry sends into it
  // rather than creating a second one for the same draft.
  const created = useRef<NeevAgentSession | null>(null)
  const draftRef = useRef("")
  draftRef.current = draft

  // An unsent draft is work; leaving must ask, exactly as a document does.
  useEffect(() => {
    onRequestNavigationGuard?.((action) => {
      if (!draftRef.current.trim()) return action()
      if (window.confirm(t("discardDraftPrompt"))) action()
    })
    return () => onRequestNavigationGuard?.((action) => action())
  }, [onRequestNavigationGuard, t])

  const connection = connections?.connections.find(
    (row) => row.provider === provider
  )
  // Three different things, said differently: still asking the server, asking
  // and been told there is no runtime, and a provider the user must connect.
  const ready = connection?.state === "connected"
  const unknown = connectionsLoading || connections === null

  async function start() {
    const prompt = draft.trim()
    if (!prompt || busy || !auth.session || !ready) return
    setBusy(true)
    setError(null)
    // One id per draft: a retry after a lost response returns the SAME session
    // rather than creating a second one.
    request.current ??= crypto.randomUUID()
    try {
      const session =
        created.current ??
        (await createAgentSession(
          {
            title: deriveSessionTitle(prompt),
            nodeId,
            provider,
            model: advanced ? model : NEEV_DEFAULT_MODEL,
            requestId: request.current,
          },
          auth.session.csrf,
          auth.signal
        ))
      // Remembered BEFORE the turn: if the turn fails the session still
      // exists, and must never be created a second time for this draft.
      created.current = session
      await startAgentTurn(session.id, prompt, auth.session.csrf, auth.signal)
      setDraft("")
      draftRef.current = ""
      onCreated(session)
    } catch (cause) {
      auth.handleAuthorityError(cause)
      if (created.current) {
        // The session exists and the turn MAY already have run — a lost
        // response is not a failed request. Never resend blindly: ask the
        // server what actually happened.
        const session = created.current
        let landed = false
        try {
          const history = await listAgentTurns(session.id, null, auth.signal)
          landed = history.items.some((turn) => turn.prompt === prompt)
        } catch {
          // Could not find out. Treat it as not sent, which keeps the words
          // the user typed rather than discarding them on a guess.
          landed = false
        }
        if (landed) {
          // The work was done. Hand over to the transcript that holds it.
          setDraft("")
          draftRef.current = ""
          onCreated(session)
          return
        }
        // Nothing was recorded: the chat exists but is empty. Keep the prompt
        // where the user can see it and send it again deliberately — an empty
        // chat and a lost message would explain nothing.
        setError(cause)
        return
      }
      setError(cause)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex min-h-0 flex-1 items-center justify-center px-6">
        <div className="w-full max-w-[760px] space-y-2 text-center">
          <h1 className="text-2xl font-semibold tracking-tight">
            {t(provider === "claude" ? "newClaudeChat" : "newCodexChat")}
          </h1>
          <p className="text-sm leading-6 text-muted-foreground">
            {t("agentChatsPrivate")}
          </p>
        </div>
      </div>
      <div className="border-t px-6 py-4">
        <div className="mx-auto w-full max-w-[760px] space-y-3">
          <NativeError error={error} />
          {created.current && error !== null && (
            <p role="status" className="text-xs text-muted-foreground">
              {t("agentChatKept")}
            </p>
          )}
          {!ready && !unknown && (
            <div
              role="status"
              className="flex flex-wrap items-center justify-between gap-3 rounded-lg border px-3 py-2 text-sm"
            >
              <span className="text-muted-foreground">
                {t("providerNotConnected", { name: provider })}
              </span>
              <Button size="sm" variant="outline" onClick={onRequestSettings}>
                {t("connectProvider")}
              </Button>
            </div>
          )}
          <label className="sr-only" htmlFor="neev-new-agent-chat">
            {t("message")}
          </label>
          <textarea
            id="neev-new-agent-chat"
            value={draft}
            autoFocus
            rows={3}
            disabled={!ready}
            maxLength={NEEV_MAX_PROMPT_CHARS}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                event.preventDefault()
                void start()
              }
            }}
            placeholder={t("agentPromptPlaceholder")}
            className="w-full resize-none rounded-xl border bg-background px-3 py-2 text-sm focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-60"
          />
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="flex min-w-0 flex-wrap items-center gap-2">
              <Select
                value={provider}
                onValueChange={(next) => setProvider(next as NeevAgentProvider)}
              >
                <SelectTrigger size="sm" aria-label={t("agentProvider")}>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {NEEV_AGENT_PROVIDERS.map((option) => (
                    <SelectItem key={option} value={option}>
                      {t(option === "claude" ? "claudeLabel" : "codexLabel")}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {advanced ? (
                <>
                  <label className="sr-only" htmlFor="neev-new-agent-model">
                    {t("agentModel")}
                  </label>
                  <input
                    id="neev-new-agent-model"
                    value={model}
                    onChange={(event) => setModel(event.target.value)}
                    className="h-8 w-44 rounded-lg border bg-background px-2 text-xs focus-visible:ring-2 focus-visible:ring-ring"
                  />
                </>
              ) : (
                <Button
                  variant="ghost"
                  size="sm"
                  className="text-xs text-muted-foreground"
                  onClick={() => setAdvanced(true)}
                >
                  {t("providerDefaultModel")}
                </Button>
              )}
            </div>
            <div className="flex items-center gap-2">
              <Button variant="ghost" size="sm" onClick={onCancel}>
                {n("cancel")}
              </Button>
              <Button
                size="sm"
                disabled={!draft.trim() || busy || !ready}
                onClick={() => void start()}
              >
                {t("send")}
              </Button>
            </div>
          </div>
        </div>
      </div>
    </div>
  )
}

function CreateSessionDialog({
  open,
  onClose,
  onCreated,
  nodeId,
}: {
  open: boolean
  onClose: () => void
  onCreated: (session: NeevAgentSession) => void
  /** Started inside a project, so the server files it there on creation. */
  nodeId: number | null
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
            nodeId,
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
      <div className="flex shrink-0 items-start gap-2 px-4 py-3 sm:gap-3 sm:px-8 sm:py-5">
        <Button
          variant="ghost"
          size="icon"
          aria-label={t("back")}
          title={t("back")}
          onClick={onBack}
        >
          <ArrowLeft aria-hidden />
        </Button>
        <div className="min-w-0 flex-1 space-y-0.5">
          <h2
            className="truncate text-base font-semibold sm:text-lg"
            title={session.title}
          >
            {session.title}
          </h2>
          <p className="truncate text-xs text-muted-foreground">
            {session.provider} · {session.model} · {t("private")}
            {session.archived ? ` · ${t("archived")}` : ""} ·{" "}
            <span role="status">{runStatusLabel()}</span>
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
          className="min-h-0 flex-1 space-y-8 overflow-y-auto overscroll-contain px-4 py-4 sm:px-8 sm:py-6"
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
        className="mx-auto w-full max-w-[832px] shrink-0 space-y-3 px-4 pb-3 pt-2 sm:px-8 sm:pb-5 sm:pt-3"
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
                href="/settings?section=agents"
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
