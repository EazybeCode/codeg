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

import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { useTranslations } from "next-intl"
import {
  ArrowUp,
  Square,
  Bot,
  FileText,
  Globe,
  Lock,
  MoreHorizontal,
  PenLine,
  PlugZap,
  Plus,
  Search,
  Terminal,
  Wrench,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { AgentRunIndicator } from "./agent-run-indicator"
import { usePublishRunningChat } from "@/lib/neev-running-chats"
import { Dialog, DialogDescription, DialogTitle } from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import {
  describeToolInput,
  ELAPSED_TICK_MS,
  elapsedMs,
  elapsedParts,
  mergeActivity,
  pairResults,
  runningTool,
  splitByActivity,
  summariseActivity,
  visibleActivity,
  type NeevToolResult,
} from "@/lib/neev-agent-activity"
import { type NeevActivityItem } from "@/lib/neev-agent-sessions-client"
import { cn } from "@/lib/utils"
import { NativeClientError } from "@/lib/neev-client"
import {
  cancelAgentRun,
  createAgentSession,
  setAgentSessionNode,
  followAgentRun,
  listAgentSessions,
  listAgentTurns,
  loadAgentCapabilities,
  loadAgentConnections,
  loadAgentSession,
  retryAgentRun,
  startAgentTurn,
  steerAgentRun,
  updateAgentSession,
  updateAgentSessionSettings,
  NEEV_DEFAULT_MODEL,
  NEEV_MAX_PROMPT_CHARS,
  type NeevAgentCapabilities,
  type NeevAgentConnections,
  type NeevAgentProvider,
  type NeevAgentSession,
  type NeevAgentSettings,
  type NeevAgentTurn,
  type NeevRunInput,
  type NeevRunPhase,
  type UpdateNeevAgentSessionInput,
} from "@/lib/neev-agent-sessions-client"
import { AgentControls } from "./native-agent-controls"
import { NativeRunFailure } from "./native-run-failure"
import { providerName, runFailure } from "@/lib/neev-run-failure"
import {
  AttachmentButton,
  SentPrompt,
  SentAttachments,
  useAttachmentTray,
  useFileDrop,
  type AttachmentUploader,
} from "./native-attachments"
import {
  NativeInlineComposer,
  remainingAttachments,
  type InlineComposerHandle,
} from "./native-inline-composer"
import {
  composeWirePrompt,
  promptIsEmpty,
  promptText,
  sameSegments,
  type PromptSegment,
} from "@/lib/neev-inline-prompt"
import { uploadSessionAttachment } from "@/lib/neev-attachments"
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
    // What the runner has actually been shown to accept. Read once per scope
    // rather than with every list refresh: it is a catalogue, not a state.
    [capabilities, setCapabilities] = useState<NeevAgentCapabilities | null>(
      null
    ),
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

  // The catalogue is separate from the list on purpose: an unreadable one must
  // not blank the sessions, and the controls it feeds simply offer nothing
  // rather than inventing a model menu of their own.
  useEffect(() => {
    const controller = new AbortController()
    loadAgentCapabilities(controller.signal)
      .then((value) => {
        if (!controller.signal.aborted) setCapabilities(value)
      })
      .catch(() => {
        // Not knowing what is on offer is not a claim that nothing is; the
        // composer falls back to showing what the session already runs as.
      })
    return () => controller.abort()
  }, [auth.scopeKey])

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
        capabilities={capabilities}
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
        capabilities={capabilities}
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
        capabilities={capabilities}
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
 * The provider arrives from the menu item that opened this, and the rest of the
 * run configuration is chosen in the same compact row the saved transcript uses.
 * The session is created by the first message so that an abandoned draft leaves
 * nothing behind — which also means the whole configuration goes up with the
 * creation, rather than being patched on afterwards.
 */
function NewAgentChat({
  provider: initialProvider,
  nodeId,
  connections,
  capabilities,
  connectionsLoading,
  onCreated,
  onCancel,
  onRequestSettings,
  onRequestNavigationGuard,
}: {
  provider: NeevAgentProvider
  nodeId: number | null
  connections: NeevAgentConnections | null
  capabilities: NeevAgentCapabilities | null
  connectionsLoading: boolean
  onCreated: (session: NeevAgentSession) => void
  onCancel: () => void
  onRequestSettings?: () => void
  onRequestNavigationGuard?: (guard: ContentNavigationGuard) => void
}) {
  const auth = useNativeAuth(),
    t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  // Per instance: several composers stay mounted at once, so a fixed id would
  // point every Connect control at the first one's description.
  const connectNote = useId()
  const [provider, setProvider] = useState<NeevAgentProvider>(initialProvider)
  // Once the session exists it has a provider, and it is the one it was created
  // with. Changing it here would send the next attempt into that session under a
  // provider it is not — and, worse, with the wrong credential. A different
  // provider is a different chat.
  const [frozen, setFrozen] = useState(false)
  const [draft, setDraft] = useState<PromptSegment[]>([]),
    [settings, setSettings] = useState<NeevAgentSettings>({
      model: NEEV_DEFAULT_MODEL,
      // Never a level nobody picked, and never fast: both are choices about
      // someone's token spend, and the defaults are the cheap ones.
      effort: null,
      fast: false,
    }),
    [busy, setBusy] = useState(false),
    [error, setError] = useState<unknown>(null)
  // The menu chose a provider; re-opening for another one must follow it.
  useEffect(() => setProvider(initialProvider), [initialProvider])
  const request = useRef<string | null>(null)
  // A session the first turn failed on. It EXISTS, so a retry sends into it
  // rather than creating a second one for the same draft.
  const created = useRef<NeevAgentSession | null>(null)
  const draftRef = useRef("")
  draftRef.current = promptText(draft)
  const composer = useRef<InlineComposerHandle>(null)

  // An unsent draft is work; leaving must ask, exactly as a document does.
  useEffect(() => {
    onRequestNavigationGuard?.((action) => {
      // Staged files are unsent work too, even with nothing typed.
      if (!draftRef.current.trim() && !stagedRef.current) return action()
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
  // There is nothing to upload TO until the first message creates the session,
  // so files wait here and go up inside `start` — after the session exists and
  // before the turn that carries them.
  const attachments = useAttachmentTray({ upload: null, disabled: !ready })
  const drop = useFileDrop({
    // Dropped ON the sentence the editor places it exactly where the pointer
    // was; this is the rest of the card, and the caret is the honest answer.
    onFiles: (files) => void composer.current?.addFiles(files),
    disabled: !ready,
  })
  const stagedRef = useRef(false)
  stagedRef.current = !attachments.empty

  async function start() {
    // The words and the files in the order they were written: each file leaves
    // a numbered marker where it sat, and the keys go up in that same order.
    const { prompt, ids } = composeWirePrompt(
      draft,
      new Map(attachments.items.map((item) => [item.id, item.name]))
    )
    if (
      (promptIsEmpty(draft) && attachments.empty) ||
      busy ||
      !auth.session ||
      !ready ||
      attachments.uploading
    )
      return
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
            // A chat started from files alone takes its name from the first
            // of them: a real thing the person chose, not a placeholder.
            title: deriveSessionTitle(
              promptText(draft).trim() || attachments.items[0]?.name || ""
            ),
            nodeId,
            provider,
            model: settings.model,
            effort: settings.effort,
            fast: settings.fast,
            requestId: request.current,
          },
          auth.session.csrf,
          auth.signal
        ))
      // Remembered BEFORE the turn: if the turn fails the session still
      // exists, and must never be created a second time for this draft.
      created.current = session
      setFrozen(true)
      // Now there is something to upload to. Each file goes up once: a retry
      // after a failed turn reuses the keys rather than sending the bytes again.
      const keys = await attachments.takeKeys(
        (file, signal) =>
          uploadSessionAttachment(
            session.id,
            file,
            auth.session!.csrf,
            signal ?? auth.signal
          ),
        ids
      )
      await startAgentTurn(
        session.id,
        prompt,
        auth.session.csrf,
        auth.signal,
        keys
      )
      setDraft([])
      draftRef.current = ""
      attachments.clear()
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
          setDraft([])
          draftRef.current = ""
          attachments.clear()
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
      <div className="flex min-h-0 flex-1 items-end justify-center px-4 pb-2 sm:px-8">
        <div className="w-full max-w-[832px] pb-4">
          <h1 className="text-xl font-semibold tracking-tight">
            {t("newAgentChat")}
          </h1>
          <p className="mt-1 text-sm leading-6 text-muted-foreground">
            {t("agentChatsPrivate")}
          </p>
        </div>
      </div>
      <div className="shrink-0 px-4 pb-3 pt-2 sm:px-8 sm:pb-5">
        <div
          {...drop.handlers}
          className={cn(
            // The in-chat composer's chrome, so the first prompt is typed into
            // the same control it will be edited in. Only the height differs:
            // an empty chat has room, and a taller box invites the first ask.
            "mx-auto w-full max-w-[832px] rounded-xl border border-border/70 bg-background p-2 shadow-sm focus-within:border-chat-accent/50 focus-within:ring-2 focus-within:ring-chat-accent/20",
            drop.over && "border-chat-accent ring-2 ring-chat-accent/25"
          )}
        >
          <NativeError error={error} />
          {created.current && error !== null && (
            <p role="status" className="text-xs text-muted-foreground">
              {t("agentChatKept")}
            </p>
          )}
          <NativeInlineComposer
            ref={composer}
            value={draft}
            onChange={(next) => {
              if (!sameSegments(next, draft)) setDraft(next)
            }}
            tray={attachments}
            label={t("message")}
            placeholder={t("agentPromptPlaceholder")}
            onSubmit={() => void start()}
            busy={busy || attachments.uploading}
            readOnly={!ready}
            attachDisabled={!ready}
            autoFocus
            maxChars={NEEV_MAX_PROMPT_CHARS}
            className="min-h-16"
          />
          {drop.over && (
            <p
              role="status"
              className="px-1 pb-1 text-xs text-muted-foreground"
            >
              {t("attachmentDrop")}
            </p>
          )}
          {attachments.uploading && (
            <p role="status" className="px-1 text-xs text-muted-foreground">
              {t("attachmentsPending")}
            </p>
          )}
          <div className="flex flex-wrap items-center justify-between gap-2">
            <AttachmentButton
              disabled={!ready || busy}
              remaining={remainingAttachments(attachments)}
              onFiles={(files) => void composer.current?.addFiles(files)}
            />
            {/* Picking a model under another provider's heading re-points this
                whole draft — nothing has been created yet, so it is still a
                choice about which chat to start rather than an edit to one. */}
            <AgentControls
              capabilities={capabilities}
              provider={provider}
              settings={settings}
              disabled={frozen}
              lockProvider={frozen}
              onChange={(next, nextProvider) => {
                setSettings(next)
                setProvider(nextProvider)
              }}
              className="min-w-0 flex-1"
            />
            <div className="flex items-center gap-2">
              {/* The same compact Connect as every other composer. It was a
                  full-width banner above the textarea, which put a standing
                  block of explanation between the heading and the box someone
                  came here to type in. The state is unchanged — Send is still
                  held while `ready` is false — only the way it asks. */}
              {!ready && !unknown && (
                <>
                  <p id={connectNote} className="sr-only">
                    {t("providerNotConnected", { name: provider })}
                  </p>
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    aria-describedby={connectNote}
                    title={t("providerNotConnected", { name: provider })}
                    className="h-7 shrink-0 gap-1.5 rounded-full px-2.5 text-xs font-normal"
                    onClick={onRequestSettings}
                  >
                    <PlugZap aria-hidden className="size-3.5" />
                    {t("connectProvider")}
                  </Button>
                </>
              )}
              <Button variant="ghost" size="sm" onClick={onCancel}>
                {n("cancel")}
              </Button>
              <Button
                size="sm"
                disabled={
                  (promptIsEmpty(draft) && attachments.empty) ||
                  busy ||
                  !ready ||
                  attachments.uploading
                }
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
  capabilities,
}: {
  open: boolean
  onClose: () => void
  onCreated: (session: NeevAgentSession) => void
  /** Started inside a project, so the server files it there on creation. */
  nodeId: number | null
  capabilities: NeevAgentCapabilities | null
}) {
  const auth = useNativeAuth(),
    t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  const [title, setTitle] = useState(""),
    [provider, setProvider] = useState<NeevAgentProvider>("claude"),
    [settings, setSettings] = useState<NeevAgentSettings>({
      model: NEEV_DEFAULT_MODEL,
      effort: null,
      fast: false,
    }),
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
            model: settings.model,
            effort: settings.effort,
            fast: settings.fast,
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
          <div className="space-y-1">
            <span className="text-sm font-medium">{t("agentModel")}</span>
            {/* One row rather than three fields: the provider heading in the
                model menu is the provider choice, and nothing offered here is
                a name this UI invented. */}
            <AgentControls
              capabilities={capabilities}
              provider={provider}
              settings={settings}
              onChange={(next, nextProvider) => {
                setSettings(next)
                setProvider(nextProvider)
              }}
            />
          </div>
          <NativeError error={error} />
          <div className="flex justify-end gap-2">
            <Button type="button" variant="ghost" onClick={onClose}>
              {n("cancel")}
            </Button>
            <Button type="submit" disabled={busy || !title.trim()}>
              {busy ? n("working") : t("create")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  )
}

/**
 * The clock on a running turn, and nothing else.
 *
 * Its own component because it is the only thing on screen that changes ten
 * times a second. The same state one level up, in `ActivityTimeline`, would
 * re-render every activity row on every tick; in the transcript above it, every
 * turn. Here a tick repaints one `<span>`.
 *
 * The wall clock is read ONCE, to learn how long this turn has already been
 * going. After that the count advances on `performance.now()`, which cannot be
 * set backwards — an NTP correction or a laptop waking up mid-run would
 * otherwise make the seconds jump, or run backwards, in front of the reader.
 */
function ElapsedClock({ startedAt }: { startedAt: number }) {
  const t = useTranslations("NativeContent")
  const [ms, setMs] = useState(() => elapsedMs(startedAt, Date.now()))
  useEffect(() => {
    const base = elapsedMs(startedAt, Date.now())
    const origin = performance.now()
    const timer = window.setInterval(
      () => setMs(base + (performance.now() - origin)),
      ELAPSED_TICK_MS
    )
    return () => window.clearInterval(timer)
  }, [startedAt])

  // `elapsedParts` returns components for localized display.
  // `tabular-nums` fixes the width so digits don't twitch on every tick.
  // `aria-hidden`: the team chat reads its agent reply from inside an
  // `aria-live` region, and a number that changes ten times a second would be
  // announced ten times a second. The turn's status is said in words beside it.
  // Tenths at every length: the live clock is asked to be precise, and past a
  // minute it reads minutes plus the remaining seconds ("2m 13.4s").
  const parts = elapsedParts(ms, true)
  const display = parts.hasMinutes
    ? t("activityElapsedMinutes", {
        minutes: parts.minutes,
        seconds: parts.seconds,
      })
    : t("activityElapsed", { number: parts.seconds })

  return (
    <span aria-hidden className="min-w-[5ch] shrink-0 tabular-nums opacity-70">
      {display}
    </span>
  )
}

/**
 * The progress footer for a turn: what it is doing, and what it cost.
 *
 * ONE small line, and deliberately not a trace. The trace itself is drawn by
 * `TranscriptBody`, inline and in order, where each tool actually ran — so
 * repeating those rows down here would be the same run described twice, once
 * in place and once in a drawer.
 *
 * What is left is only what the rows cannot say: that something is still
 * happening, how long it has been happening, how long it took, and whether the
 * turn as a whole answered. The provider's own bookkeeping — `end_turn`,
 * `success`, the model line — is filtered out upstream by `isNoise`.
 */
export function ActivityTimeline({
  items,
  running,
  startedAt,
  finishedAt = null,
  completed = false,
}: {
  /** The turn's activity, already merged with anything arriving live. */
  items: NeevActivityItem[]
  /** Whether this turn is still going, so the status shows at once. */
  running: boolean
  /** When the turn began, in epoch milliseconds; null if unknown. */
  startedAt: number | null
  /**
   * When the turn ended, in epoch milliseconds; null while it runs or if the
   * server never recorded one. With `startedAt` it is how long the turn took,
   * which is worth keeping on screen after the live clock stops — but both are
   * stored to the second, so what it can honestly say is whole seconds.
   */
  finishedAt?: number | null
  /**
   * Whether the turn itself finished and answered.
   *
   * A tool that failed is not a turn that failed. An agent that reached for
   * `python`, did not find it, and got the same answer for `node` before
   * finishing the job with `rg` ran two commands that failed and then did the
   * work — but the row said "2 failed" in red and nothing else, so a completed
   * review read as a broken one.
   *
   * The count is NOT softened: those two commands did fail, and each keeps its
   * own error on its own row above. What was missing is the outcome beside them.
   */
  completed?: boolean
}) {
  const t = useTranslations("NativeContent")

  const visible = visibleActivity(items)
  // What the turn ended up costing, once it is over. Whole seconds: the two
  // timestamps it is read from are stored that way; `elapsedParts` with
  // `precise: false` renders "37s" or "2m 13s" without decimal places.
  const tookParts =
    !running && startedAt !== null && finishedAt !== null
      ? elapsedParts(elapsedMs(startedAt, finishedAt), false)
      : null
  // A turn that ran no tools still took time and still finished, and that is
  // worth one quiet line. Only a turn with NOTHING to say — no duration, no
  // outcome, not running — renders nothing at all.
  if (!running && tookParts === null && !completed) return null
  const summary = summariseActivity(visible)
  const current = running ? runningTool(visible) : null

  const tookDisplay = tookParts
    ? tookParts.hasMinutes
      ? t("activityElapsedMinutes", {
          minutes: tookParts.minutes,
          seconds: tookParts.seconds,
        })
      : t("activityElapsed", { number: tookParts.seconds })
    : null

  return (
    <div className="my-1.5 flex min-w-0 max-w-full items-center gap-1.5 px-1.5 py-0.5 text-[13px] leading-5 text-muted-foreground">
      {/* The accent, on the one thing that is actually happening. */}
      {running && <AgentRunIndicator className="text-muted-foreground" />}
      {running && startedAt !== null && <ElapsedClock startedAt={startedAt} />}
      {/*
        Announced, not painted. The tool in flight already has a row of its own
        in the transcript above, so printing its name again here would put the
        same fact on screen twice — but a reader who cannot see that row still
        needs to be told the run is going, and what it is doing.
      */}
      {running && (
        <span className="sr-only" role="status">
          {current
            ? t("activityRunningTool", { name: current.name })
            : t("activityWorking")}
        </span>
      )}
      {tookDisplay !== null && (
        <span className="shrink-0 tabular-nums opacity-70">{tookDisplay}</span>
      )}
      {summary.errors > 0 && (
        <span className="shrink-0 text-destructive">
          {t("activityErrors", { number: summary.errors })}
        </span>
      )}
      {/*
        Last, and only once the turn is actually over. It is the answer to
        "did this work?", which the failed-step count was being read as.
      */}
      {!running && completed && (
        <span className="shrink-0">{t("activityCompleted")}</span>
      )}
    </div>
  )
}

/**
 * The icon for a tool, by what it does.
 *
 * Matched on the lowercased name, so one mapping covers providers that spell
 * the same job differently — `Bash`, `shell`, `run_command`. Anything not
 * recognised gets the neutral wrench rather than a near-miss: an icon claiming
 * a tool read a file when it wrote one is a statement about what ran, and this
 * is only allowed to decorate.
 *
 * Order matters. `write_file` and `edit_file` both contain "file", so the
 * writing tests come before the reading ones.
 */
function ToolIcon({ name }: { name: string }) {
  const key = name.toLowerCase()
  // Slate rather than the prose grey: a tool row is machinery, and the icon is
  // what says so before the name is read.
  const shared = {
    "aria-hidden": true,
    className: "size-3.5 shrink-0 text-chat-tool",
  } as const
  if (/bash|shell|command|terminal|exec|run/.test(key))
    return <Terminal {...shared} />
  if (/search|grep|glob|find|list/.test(key)) return <Search {...shared} />
  if (/fetch|http|web|url|browse|curl/.test(key)) return <Globe {...shared} />
  if (/write|edit|patch|apply|create|update/.test(key))
    return <PenLine {...shared} />
  if (/read|file|cat|view|open/.test(key)) return <FileText {...shared} />
  return <Wrench {...shared} />
}

/**
 * One tool invocation, as one compact row.
 *
 * Collapsed by default and to a single line: the tool's name, then the command
 * or path it was given, clipped, on a quiet pill. A run can make dozens of
 * calls and their raw output can be thousands of lines — dumped inline they
 * bury the answer they were gathered for, so the full input and output are one
 * click away and the click is a real, labelled, keyboard-reachable expander.
 *
 * `result` is the tool's own answer, folded in by `pairResults` so that one
 * invocation is one row. It is what makes a row able to say "this failed"
 * without a second nameless row underneath saying it.
 *
 * Everything rendered here is model-authored or tool-authored text. It is
 * displayed and nothing else: never parsed into a request, never put in a URL,
 * never executed.
 */
function ActivityRow({
  item,
  result,
}: {
  item: NeevActivityItem
  /** The result folded into this call, when it has one and this is a call. */
  result: NeevToolResult | null
}) {
  const t = useTranslations("NativeContent")
  const [open, setOpen] = useState(false)

  if (item.kind === "status")
    return (
      <li>
        <p role="status" className="px-1.5 py-1 text-xs text-muted-foreground">
          {item.state === "truncated"
            ? t("activityTruncated")
            : t("activityUnsupported", { name: item.detail ?? "" })}
        </p>
      </li>
    )

  // A result standing on its own: one whose call is not in this trace, or a
  // second result for a call that already had one. Kept rather than hidden —
  // dropping recorded output to shorten a list is the one thing a trace may
  // not do — but it has no name of its own, so it stays a plain line.
  if (item.kind === "tool_result") {
    const empty = !item.output.trim()
    return (
      <li>
        <button
          type="button"
          aria-expanded={open}
          disabled={empty}
          onClick={() => setOpen((value) => !value)}
          className="flex w-full min-w-0 items-baseline gap-2 rounded-md px-1.5 py-1 text-start hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring disabled:hover:bg-transparent"
        >
          <span
            className={cn(
              "truncate text-xs",
              item.isError ? "text-destructive" : "text-muted-foreground"
            )}
          >
            {item.isError
              ? t("activityFailed")
              : oneLineOf(item.output) || t("activityCompleted")}
          </span>
        </button>
        {open && !empty && (
          <OutputBlock text={item.output} truncated={item.outputTruncated} />
        )}
      </li>
    )
  }

  const output = result?.output.trim() ? result.output : ""
  // Nothing to open is not a control. The input is almost always there, so this
  // is the rare tool called with no arguments and no output recorded.
  const empty = !item.input.trim() && !output
  return (
    <li>
      <button
        type="button"
        aria-expanded={open}
        disabled={empty}
        onClick={() => setOpen((value) => !value)}
        className="flex w-full min-w-0 items-center gap-2 rounded-md px-1.5 py-1 text-start hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring disabled:hover:bg-transparent"
      >
        <ToolIcon name={item.name} />
        <span className="shrink-0 text-[13px] font-normal leading-5 text-muted-foreground">
          {item.name}
        </span>
        {/* The command as it was given, clipped to one line on a quiet pill.
            `min-w-0` so the pill is what shrinks when the column is narrow —
            without it the flex child refuses to go below its content width and
            the row overflows instead of the text ellipsing. */}
        {describeToolInput(item.input) && (
          <span className="min-w-0 truncate rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-chat-tool">
            {describeToolInput(item.input)}
          </span>
        )}
        {result?.isError && (
          <span className="shrink-0 text-xs text-destructive">
            {t("activityFailed")}
          </span>
        )}
      </button>
      {open && !empty && (
        <div className="space-y-1">
          {item.input.trim() && (
            <OutputBlock text={item.input} truncated={item.inputTruncated} />
          )}
          {output && (
            <OutputBlock
              text={result!.output}
              truncated={result!.outputTruncated}
            />
          )}
        </div>
      )}
    </li>
  )
}

/** The verbatim text behind a row, capped in height rather than in content. */
function OutputBlock({
  text,
  truncated,
}: {
  text: string
  truncated: boolean
}) {
  const t = useTranslations("NativeContent")
  return (
    <pre className="mx-1.5 mb-1 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md border border-border/60 bg-muted/40 p-2.5 font-mono text-xs leading-5">
      {text}
      {truncated && (
        <span className="block pt-1 text-muted-foreground">
          {t("activityTruncated")}
        </span>
      )}
    </pre>
  )
}

/**
 * A reply and the work that produced it, in the order both happened.
 *
 * The server records, for every activity item, the `offset` in the reply text
 * at which it happened — so the chronology here is READ, never reconstructed.
 * `splitByActivity` cuts the text at those offsets and hands back segments
 * whose `items` sit at the segment's leading boundary, which is why the rows
 * are drawn before the prose in each one.
 *
 * User follow-up inputs (steering) are interleaved at their `responseOffset`
 * positions, rendered as right-aligned bubbles to match the original prompt.
 *
 * Nothing is synthesised. A run that recorded no tools renders exactly the
 * markdown it always did; a segment exists only because an offset put it there.
 *
 * `streaming` applies to the LAST segment alone. Incomplete-markdown repair
 * appends closers to text that may end mid-token, which is right for the text
 * still arriving and wrong for text already settled behind a tool call — there
 * it could close a span the author left open on purpose.
 */
export function TranscriptBody({
  text,
  items,
  inputs = [],
  streaming = false,
}: {
  /** The reply so far, or the whole stored reply. */
  text: string
  /** The turn's activity, already merged with anything arriving live. */
  items: readonly NeevActivityItem[]
  /** Follow-up inputs sent via steering, sorted by responseOffset. */
  inputs?: readonly NeevRunInput[]
  /** Whether `text` is still arriving. */
  streaming?: boolean
}) {
  const visible = visibleActivity(items)
  const { resultOf, absorbed } = pairResults(visible)
  const segments = splitByActivity(text, visible)
  const finalTextIndex = segments.reduce(
    (last, segment, index) => (segment.text.trim() ? index : last),
    -1
  )
  // `responseOffset` counts Unicode scalar values, like activity offsets, so
  // the text is cut as scalars too — never with UTF-16 `slice`.
  const textLength = [...text].length
  // An offset past the text so far (the reply has not caught up yet) sits at
  // the end until it does.
  const pendingInputs = [...inputs]
    .map((input) => ({
      input,
      at: Math.max(0, Math.min(input.responseOffset, textLength)),
    }))
    .sort((left, right) => left.at - right.at)
  /*
   * Only the segments that will actually draw something.
   *
   * A result folded into its call leaves its own segment with no rows, and a
   * result recorded at the end of the text so far leaves it with no text
   * either — which is the ordinary shape of a live run, not an edge case. Left
   * in, each one is an empty `<div>` inside a `space-y-2` stack, so the reply
   * grows a blank gap every time a tool answers.
   *
   * `index` is kept from BEFORE the filter, for two reasons. It is what says
   * which segment holds the text still arriving (the last one, and only if it
   * has text: everything behind a recorded offset is settled, and repairing
   * settled text can close a span its author left open). And it is not the
   * React key — that is the seq of the segment's leading item, which does not
   * shift when an earlier segment appears or disappears, so an expanded row
   * stays expanded when its result lands.
   */
  // Each input goes to the first segment whose text reaches its offset, and
  // that segment's text is cut there: the reply written before the follow-up,
  // the follow-up, then the reply written after it. An input exactly on an
  // activity boundary closes the earlier segment, ahead of the next rows.
  type Part =
    | { kind: "text"; start: number; text: string }
    | { kind: "input"; input: NeevRunInput }
  const laid: {
    index: number
    parts: Part[]
    lastText: number
    key: number | "head"
    rows: NeevActivityItem[]
  }[] = []
  let cursor = 0
  for (const [index, segment] of segments.entries()) {
    const scalars = [...segment.text]
    const startOffset = cursor
    cursor += scalars.length
    const parts: Part[] = []
    let from = 0
    while (pendingInputs.length > 0 && pendingInputs[0].at <= cursor) {
      const { input, at } = pendingInputs.shift()!
      const to = at - startOffset
      if (to > from)
        parts.push({
          kind: "text",
          start: from,
          text: scalars.slice(from, to).join(""),
        })
      from = Math.max(from, to)
      parts.push({ kind: "input", input })
    }
    if (from < scalars.length)
      parts.push({
        kind: "text",
        start: from,
        text: scalars.slice(from).join(""),
      })
    const lastText = parts.reduce(
      (last, part, at) =>
        part.kind === "text" && part.text.trim() ? at : last,
      -1
    )
    laid.push({
      index,
      parts,
      lastText,
      // The item that opens this segment, drawn or not. Only the FIRST segment
      // can have none — every later one is created by crossing an item's
      // offset — so this is a unique, stable key even as segments come and go.
      key: segment.items[0]?.seq ?? "head",
      rows: segment.items.filter(
        (item) => !(item.kind === "tool_result" && absorbed.has(item.seq))
      ),
    })
  }
  const drawn = laid.filter(
    (segment) =>
      segment.rows.length > 0 ||
      segment.lastText >= 0 ||
      segment.parts.some((part) => part.kind === "input")
  )
  const bubble = (input: NeevRunInput) => (
    <div key={input.requestId} data-agent-input className="flex justify-end">
      <div className="max-w-[85%] rounded-2xl border border-chat-user-foreground/10 bg-chat-user-surface px-3.5 py-2 text-sm text-chat-user-foreground">
        {input.text}
      </div>
    </div>
  )
  return (
    <div className="space-y-2">
      {drawn.map((segment) => (
        <div key={segment.key} className="space-y-2">
          {segment.rows.length > 0 && (
            <ul className="space-y-0.5">
              {segment.rows.map((item) => (
                <ActivityRow
                  key={item.seq}
                  item={item}
                  result={
                    item.kind === "tool_call"
                      ? (resultOf.get(item.seq) ?? null)
                      : null
                  }
                />
              ))}
            </ul>
          )}
          {segment.parts.map((part, at) => {
            if (part.kind === "input") return bubble(part.input)
            if (!part.text.trim()) return null
            // Only the closing prose of the closing segment is the answer;
            // anything a follow-up came after is progress it redirected.
            const progress =
              streaming ||
              segment.index !== finalTextIndex ||
              at !== segment.lastText
            return (
              <div
                key={`text:${part.start}`}
                data-agent-prose={progress ? "progress" : "final"}
                className={cn(
                  "font-normal",
                  // Progress reads a size down from the answer. Set on the
                  // markdown root itself, which carries its own `text-sm`.
                  progress
                    ? "text-muted-foreground [&>div]:text-[13px] [&>div]:leading-relaxed"
                    : "text-foreground"
                )}
              >
                <NativeMarkdown
                  markdown={part.text}
                  streaming={
                    streaming &&
                    segment.index === segments.length - 1 &&
                    at === segment.parts.length - 1
                  }
                />
              </div>
            )
          })}
        </div>
      ))}
      {/* No text at all yet: follow-ups still show, in order. */}
      {pendingInputs.map(({ input }) => bubble(input))}
    </div>
  )
}

/** A tool's output as one readable line, for the collapsed row. */
function oneLineOf(output: string): string {
  const flat = output.replace(/\s+/g, " ").trim()
  const characters = [...flat]
  return characters.length <= 80
    ? flat
    : `${characters.slice(0, 79).join("").trimEnd()}…`
}

function AgentSessionView({
  session,
  connections,
  capabilities,
  // Kept on the props; no longer reached from a header control.
  onChanged,
  onGone,
  onRequestSettings,
  onRequestNavigationGuard,
}: {
  session: NeevAgentSession
  connections: NeevAgentConnections | null
  capabilities: NeevAgentCapabilities | null
  onBack: () => void
  onChanged: (session: NeevAgentSession) => void
  onGone: () => void
  onRequestSettings?: () => void
  onRequestNavigationGuard?: (guard: ContentNavigationGuard) => void
}) {
  const auth = useNativeAuth(),
    t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  // Per instance: several composers stay mounted at once, so a fixed id would
  // point every Connect control at the first one's description.
  const connectNote = useId()
  const stopHint = useId()
  const router = useRouter()
  const [turns, setTurns] = useState<NeevAgentTurn[] | null>(null),
    [draft, setDraft] = useState<PromptSegment[]>([]),
    [error, setError] = useState<unknown>(null),
    [sending, setSending] = useState(false),
    [renaming, setRenaming] = useState(false),
    // The run this view is attached to, and how that attachment is doing.
    [activeRun, setActiveRun] = useState<string | null>(null),
    [phase, setPhase] = useState<NeevRunPhase>("streaming"),
    [liveText, setLiveText] = useState(""),
    // The running turn's trace. Upserted by `seq`, never appended: a reattach
    // replays the stored prefix before streaming live, so the same item
    // arrives twice and appending would show every tool call twice.
    [liveActivity, setLiveActivity] = useState<NeevActivityItem[]>([]),
    // User inputs sent into the running turn via steering. Upserted by
    // `requestId` so a reattach never shows duplicates.
    [liveInputs, setLiveInputs] = useState<NeevRunInput[]>([]),
    // Whether the current run supports steering (provider acknowledged).
    [supportsSteering, setSupportsSteering] = useState(false),
    // A steering request in flight: the requestId, so the same id is retried
    // if the first attempt fails. Cleared on success or when the run ends.
    [steeringInFlight, setSteeringInFlight] = useState<string | null>(null),
    // Bumped to ask for a fresh attachment to the same run — an explicit
    // reattach, never a re-run.
    [attachNonce, setAttachNonce] = useState(0)
  // A configuration the reader has chosen but the server has not stored yet,
  // and whether the last attempt to store it lost a race. `null` means the
  // controls are showing the session's own row, which is the normal state.
  const [desired, setDesired] = useState<NeevAgentSettings | null>(null),
    [applying, setApplying] = useState(false)
  // Files for the next turn, uploaded to THIS session as they arrive. A key is
  // only ever staged into a run by a turn that carries it, so dropping a file
  // sends nothing to any provider.
  const uploader: AttachmentUploader | null = auth.session
    ? (file, signal) =>
        uploadSessionAttachment(
          session.id,
          file,
          auth.session!.csrf,
          signal ?? auth.signal
        )
    : null
  const attachments = useAttachmentTray({
    upload: uploader,
    disabled: session.archived || !auth.session,
  })
  const composer = useRef<InlineComposerHandle>(null)
  const drop = useFileDrop({
    // The editor places a file dropped onto the sentence itself; this is the
    // rest of the card, where the caret is the honest answer.
    onFiles: (files) => void composer.current?.addFiles(files),
    disabled: session.archived,
  })
  const draftRef = useRef("")
  // The requestId for the steering request to retry on failure. Kept across
  // failed sends so the same id is used, preventing duplicates.
  // Retry of a failed turn: which run is being retried, and the id that makes
  // a second press after a dropped response resolve to the same new run.
  const [retryingRun, setRetryingRun] = useState<string | null>(null)
  const retryRequest = useRef<{ runId: string; id: string } | null>(null)
  const steeringRetryId = useRef<{
    id: string
    text: string
    runId: string
  } | null>(null)
  // Read by the navigation guard, which is installed once and must not close
  // over an old tray.
  const stagedRef = useRef(false)
  stagedRef.current = !attachments.empty
  const history = useRef<HTMLOListElement>(null)
  /**
   * Whether the reader is still following the newest output.
   *
   * Scrolling up during a run is a decision, and the transcript used to
   * overrule it on the very next token: every change of `turns` or `liveText`
   * pinned the view back to the bottom, so reading anything earlier while an
   * agent worked was impossible. This is the same follow flag the team thread
   * has, with the same 48px tolerance, so "near enough to the bottom" still
   * counts as following and a run that ends where you left it does not jump.
   */
  const followHistory = useRef(true)
  // Opening a different chat must not carry this one's unsaved choice into it,
  // and an answer to a request made for the old one must not be applied to the
  // new one. The id at the time of asking is checked against the id now.
  const openSession = useRef(session.id)
  useEffect(() => {
    if (openSession.current === session.id) return
    openSession.current = session.id
    setDesired(null)
    setApplying(false)
    // Files were uploaded to the chat they were dropped into. Carrying them
    // across would attach them to a different conversation, so the tray is
    // emptied and anything still uploading is abandoned.
    attachments.clear()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session.id])
  // What the current attachment has streamed so far, readable from the settle
  // handler without making it depend on the rendered value.
  const liveRef = useRef("")
  const liveActivityRef = useRef<NeevActivityItem[]>([])
  const liveInputsRef = useRef<NeevRunInput[]>([])
  // A queued navigation held back by an unsent prompt.
  const [pending, setPending] = useState<(() => void) | null>(null)

  // An unsent prompt is work in progress: hold navigation the same way an
  // unsaved document does.
  useEffect(() => {
    if (!onRequestNavigationGuard) return
    const guard: ContentNavigationGuard = (action) => {
      // Staged files are unsent work too: leaving would discard them.
      if (draftRef.current || stagedRef.current) setPending(() => action)
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
          liveInputsRef.current = running.inputs
          setLiveInputs(running.inputs)
          setSupportsSteering(running.supportsSteering)
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
      onActivity: (item) => {
        if (controller.signal.aborted) return
        setLiveActivity((old) => {
          const next = mergeActivity(old, [item])
          liveActivityRef.current = next
          return next
        })
      },
      onInput: (input) => {
        if (controller.signal.aborted) return
        // Upsert by requestId: a reattach replays persisted inputs.
        setLiveInputs((old) => {
          if (old.some((item) => item.requestId === input.requestId)) return old
          const next = [...old, input].sort(
            (left, right) => left.responseOffset - right.responseOffset
          )
          liveInputsRef.current = next
          return next
        })
        // Clear pending steering request when our input is acknowledged.
        setSteeringInFlight((pending) =>
          pending === input.requestId ? null : pending
        )
      },
      onEvent: (event) => {
        if (controller.signal.aborted) return
        // A replay restarts the buffer: the server resends everything it stored.
        if (event.type === "started") {
          liveRef.current = ""
          setLiveText("")
          liveActivityRef.current = []
          setLiveActivity([])
          liveInputsRef.current = []
          setLiveInputs([])
          // Set supportsSteering from the SSE event on both initial and reconnect.
          setSupportsSteering(event.supportsSteering)
        } else if (event.type === "chunk") {
          liveRef.current += event.text
          setLiveText(liveRef.current)
        } else {
          // Run ended: clear steering state and merge inputs into stored turn.
          setSteeringInFlight(null)
          setSupportsSteering(false)
          setTurns((old) =>
            (old ?? []).map((turn) =>
              turn.runId === activeRun
                ? {
                    ...turn,
                    status: event.status,
                    errorCode: event.errorCode,
                    // The stream replays exactly what the server stored, so the
                    // accumulated text is the durable response — and the same
                    // is true of the trace and inputs beside it.
                    response: liveRef.current || turn.response,
                    activity: mergeActivity(
                      turn.activity,
                      liveActivityRef.current
                    ),
                    inputs: liveInputsRef.current.length
                      ? liveInputsRef.current
                      : turn.inputs,
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

  // Before paint, and only while the reader is still following: a tool row
  // appearing mid-reply changes the height of the transcript, and correcting
  // for that after the browser has painted is what a visible jolt looks like.
  useLayoutEffect(() => {
    const element = history.current
    if (element && followHistory.current)
      element.scrollTop = element.scrollHeight
  }, [turns, liveText, liveActivity, liveInputs])

  const runtimeEnabled = connections?.runtimeEnabled ?? false
  const connection = connections?.connections.find(
    (item) => item.provider === session.provider
  )
  const ready = runtimeEnabled && connection?.state === "connected"
  const running = activeRun !== null
  // The composer's one button is Stop for as long as a run has nothing typed
  // it could take. A follow-up waiting to go into the run makes it Send — but
  // only until it is sent: a follow-up in flight shows Stop again, so a steer
  // that never answers cannot leave a disabled Send where Stop should be.
  // Escape stops the run from the composer whichever it shows.
  const stopping =
    running &&
    (steeringInFlight !== null || !(supportsSteering && !promptIsEmpty(draft)))
  usePublishRunningChat(auth.session, "agent", session.id, running)
  /**
   * What this session runs as, from the SESSION — not from local state.
   *
   * That is what makes the row still correct after a reload: the run reads the
   * stored configuration, so the stored configuration is what is shown. A
   * choice not yet accepted by the server overlays it, and is labelled as not
   * yet applied rather than drawn as though it were.
   */
  const live: NeevAgentSettings = {
    model: session.model,
    effort: session.effort,
    fast: session.fast,
  }
  const shown = desired ?? live

  /**
   * Store the chosen configuration, and say whether it is stored.
   *
   * All three fields go up together in one revision: the server validates them
   * as a combination, so patching them one at a time could put the session
   * through a pairing no run could honour. A stale revision means someone else
   * changed this session while it was open — re-read it and try once against
   * what is actually there, then stop rather than fight a loop.
   */
  async function applySettings(next: NeevAgentSettings): Promise<boolean> {
    if (!auth.session) return false
    const target = session.id
    setApplying(true)
    try {
      let revision = session.revision
      for (let attempt = 0; attempt < 2; attempt++) {
        try {
          const stored = await updateAgentSessionSettings(
            target,
            next,
            revision,
            auth.session.csrf,
            auth.signal
          )
          // The chat moved on underneath us; this answer is about a chat the
          // reader is no longer looking at, so it changes nothing here.
          if (openSession.current !== target) return false
          onChanged(stored)
          setDesired(null)
          setError(null)
          return true
        } catch (cause) {
          const stale =
            cause instanceof NativeClientError && cause.kind === "conflict"
          if (!stale || attempt === 1) throw cause
          const fresh = await loadAgentSession(target, auth.signal)
          if (openSession.current !== target) return false
          onChanged(fresh)
          revision = fresh.revision
        }
      }
      return false
    } catch (cause) {
      if (openSession.current !== target) return false
      fail(cause)
      return false
    } finally {
      if (openSession.current === target) setApplying(false)
    }
  }

  async function send() {
    // Files alone are a complete ask: the prompt goes up empty and the server
    // is what says so, rather than this composer inventing a sentence.
    const { prompt, ids } = composeWirePrompt(
      draft,
      new Map(attachments.items.map((item) => [item.id, item.name]))
    )
    if (
      (!prompt && attachments.empty) ||
      sending ||
      running ||
      attachments.uploading ||
      !auth.session
    )
      return
    setSending(true)
    setError(null)
    // The run reads the SESSION, so an unstored choice would be silently
    // ignored by the very turn it was chosen for. Store it first, and if that
    // fails do not send: the prompt stays exactly where it was typed, with the
    // reason beside it, rather than running under a configuration nobody chose.
    if (desired && !(await applySettings(desired))) {
      setSending(false)
      return
    }
    try {
      // Bytes first: a turn only carries files the server already has, and a
      // failure here keeps both the prompt and the files for another try.
      const keys = await attachments.takeKeys(undefined, ids)
      const started = await startAgentTurn(
        session.id,
        prompt,
        auth.session.csrf,
        auth.signal,
        keys
      )
      setTurns((old) => [...(old ?? []), started.turn])
      setDraft([])
      draftRef.current = ""
      // Only now: the turn owns them from here.
      attachments.clear()
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

  /**
   * Run a failed turn's own ask again: the server re-reads its prompt and file
   * references, so nothing typed since can change it. The id is kept across a
   * failed request, so pressing Retry again after a dropped response resolves
   * to the run the first press started instead of starting a second.
   */
  async function retryTurn(turn: NeevAgentTurn) {
    if (sending || running || retryingRun || !auth.session) return
    const previous = retryRequest.current
    const request =
      previous?.runId === turn.runId
        ? previous
        : { runId: turn.runId, id: crypto.randomUUID() }
    retryRequest.current = request
    setRetryingRun(turn.runId)
    setError(null)
    try {
      const started = await retryAgentRun(
        turn.runId,
        request.id,
        auth.session.csrf,
        auth.signal
      )
      retryRequest.current = null
      setTurns((old) =>
        (old ?? []).some((item) => item.id === started.turn.id)
          ? old
          : [...(old ?? []), started.turn]
      )
      if (started.turn.status === "running") {
        liveRef.current = ""
        setLiveText("")
        setActiveRun(started.runId)
      }
    } catch (cause) {
      fail(cause)
    } finally {
      setRetryingRun(null)
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

  /**
   * Send a follow-up input into the active run via steering.
   *
   * The request is idempotent on requestId: on failure we keep the same id
   * so a retry cannot duplicate. Text is retained in the composer until
   * the server acknowledges receipt via the SSE input event.
   */
  async function steer() {
    if (!activeRun || !auth.session || !supportsSteering) return
    // Capture the draft at the start of the attempt for later clearing.
    const sentDraft = draft
    const { prompt } = composeWirePrompt(
      sentDraft,
      new Map(attachments.items.map((item) => [item.id, item.name]))
    )
    if (!prompt || steeringInFlight) return
    // Attachments are not supported mid-run; refuse honestly.
    if (!attachments.empty) return
    // Reuse the same requestId on retry so duplicates cannot happen.
    // The retry ref persists the id across failed attempts.
    const previous = steeringRetryId.current
    const requestId =
      previous?.text === prompt && previous.runId === activeRun
        ? previous.id
        : crypto.randomUUID()
    steeringRetryId.current = { id: requestId, text: prompt, runId: activeRun }
    setSteeringInFlight(requestId)
    setError(null)
    try {
      await steerAgentRun(
        activeRun,
        prompt,
        requestId,
        auth.session.csrf,
        auth.signal
      )
      // Success: clear only the captured draft if the composer still holds it.
      // If the user has edited while waiting, preserve their new text.
      steeringRetryId.current = null
      setSteeringInFlight((pending) => (pending === requestId ? null : pending))
      if (draftRef.current === promptText(sentDraft)) {
        setDraft([])
        draftRef.current = ""
      }
      // steeringInFlight is cleared by the SSE input event when it matches.
    } catch (cause) {
      // On conflict (finished/unsupported), the run ended while we were
      // typing. Keep text for a new turn. Clear retry id since a new turn
      // needs a new id. For network errors, keep text and requestId in the
      // ref so Enter retries with the same id.
      if (cause instanceof NativeClientError && cause.kind === "conflict") {
        // Run finished; steering no longer available. User can start new turn.
        setSupportsSteering(false)
        steeringRetryId.current = null
      }
      setSteeringInFlight(null)
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
      {/* A hairline under the header, matching the team chat: the transcript
          below scrolls and this does not, so without an edge a turn scrolling
          up disappeared into the title. */}
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-border/60 px-4 sm:gap-3 sm:px-8">
        <div className="flex min-w-0 flex-1 items-baseline gap-2">
          <h2
            className="shrink truncate text-sm font-medium"
            title={session.title}
          >
            {session.title}
          </h2>
          {/* The run configuration is CHOSEN a few lines below, in the
              composer. Repeating it up here in full cost a second header row
              to restate what the chips already say, so this keeps only what
              the chips cannot: who can see it, and what the run is doing. */}
          {/* An agent chat is always private, and says so at every width. */}
          <span
            role="img"
            aria-label={t("private")}
            title={t("private")}
            className="shrink-0 text-muted-foreground/80 sm:hidden"
          >
            <Lock aria-hidden className="size-3.5" />
          </span>
          <p className="hidden min-w-0 shrink-0 truncate text-xs text-muted-foreground/80 sm:block">
            {t("private")}
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
              {t(session.archived ? "restoreFromTrash" : "moveToTrash")}
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
          onScroll={(event) => {
            const element = event.currentTarget
            followHistory.current =
              element.scrollHeight - element.scrollTop - element.clientHeight <
              48
          }}
          aria-label={t("agentTurns")}
          // Asymmetric on purpose: the first ask starts where it always did,
          // and the last paragraph of a reply gets room to breathe before the
          // composer instead of ending against its top edge.
          className="mx-auto min-h-0 w-full max-w-[832px] flex-1 space-y-6 overflow-y-auto overscroll-contain px-4 pb-14 pt-5 text-sm sm:px-8"
        >
          {turns.map((turn) => {
            const live = turn.runId === activeRun
            /*
             * ONE tree for a turn, whether it is running or finished.
             *
             * Running and finished used to be two different branches, and React
             * reconciles by position and type: the live branch wrapped the body
             * in a live region and the finished one did not, so the moment a run
             * ended the body was a different element in that slot. React
             * unmounted it and mounted a new one — which threw away every open
             * tool row under the reader, and changed the height of the turn as
             * the wrapper's own text size went with it.
             *
             * So the shape is fixed and only the PROPS change. `aria-live` is an
             * attribute on the same element rather than a reason to draw a
             * different one, and the footer keeps its slot and its spacing and
             * merely stops running.
             */
            const items = live
              ? mergeActivity(turn.activity, liveActivity)
              : turn.activity
            // Follow-up inputs for this turn: live during the run, stored after.
            const inputs = live ? liveInputs : turn.inputs
            const generatedAttachments = turn.attachments.filter(
              (file) =>
                turn.response.includes(`/api/neev/attachments/${file.key}`) &&
                !turn.prompt.includes(file.key)
            )
            const inputAttachments = turn.attachments.filter(
              (file) => !generatedAttachments.includes(file)
            )
            return (
              <li
                key={turn.id}
                className="mx-auto w-full max-w-3xl space-y-2.5"
              >
                {/* The ask, right-aligned and only as wide as it needs to be:
                    this chat has exactly two participants and one of them is the
                    reader, so the side of the column says who is speaking. Safe
                    here and NOT in team chat, where several people share the
                    thread. Files stay where they were put in the sentence. */}
                <div className="flex justify-end">
                  <div className="max-w-[85%] rounded-2xl border border-chat-user-foreground/10 bg-chat-user-surface px-3.5 py-2 text-chat-user-foreground">
                    <SentPrompt
                      body={turn.prompt}
                      attachments={inputAttachments}
                    />
                  </div>
                </div>
                <div className="space-y-2">
                  {/* Prose and tool rows in one body, in the order the server's
                      offsets record them. Live text while the run is in flight,
                      the stored reply once it is not — the same component in the
                      same place, so an expanded row stays expanded across the
                      handover. */}
                  <div
                    aria-live={live ? "polite" : undefined}
                    className="text-sm"
                  >
                    <TranscriptBody
                      text={live ? liveText : turn.response}
                      items={items}
                      inputs={inputs}
                      streaming={live}
                    />
                  </div>
                  <SentAttachments attachments={generatedAttachments} />
                  {/* Progress after the newest output, near the composer. The
                      clock stays outside the response live region, and the trace
                      is NOT repeated here — it is above, in place. */}
                  <div className="pt-3">
                    <ActivityTimeline
                      items={items}
                      running={live}
                      startedAt={turn.createdAt * 1000}
                      finishedAt={
                        live || turn.completedAt === null
                          ? null
                          : turn.completedAt * 1000
                      }
                      completed={!live && turn.status === "completed"}
                    />
                  </div>
                  {turn.status === "failed" && !live
                    ? failedTurn(turn)
                    : turn.status !== "completed" &&
                      !live && (
                        <p className="text-xs text-muted-foreground">
                          {outcomeLabel(turn)}
                        </p>
                      )}
                </div>
              </li>
            )
          })}
        </ol>
      )}

      <form
        onSubmit={(event) => {
          event.preventDefault()
          // When a run is active and supports steering, Enter sends into
          // the same run. When running without steering, Enter does nothing
          // (text is kept for the next turn). Otherwise it starts a new turn.
          if (running && supportsSteering) void steer()
          else if (!running) void send()
        }}
        onKeyDown={(event) => {
          // Stop is always one key away, even while the corner shows Send.
          // Not when something inside claimed Escape first (the mention
          // picker), and not from a portalled menu opened from this row,
          // whose Escape only closes that menu.
          if (
            event.key !== "Escape" ||
            !running ||
            event.defaultPrevented ||
            !event.currentTarget.contains(event.target as Node)
          )
            return
          event.preventDefault()
          void cancel()
        }}
        className="mx-auto w-full max-w-[832px] shrink-0 space-y-3 px-4 pb-3 pt-2 sm:px-8 sm:pb-5 sm:pt-3"
      >
        <div
          {...drop.handlers}
          className={cn(
            // Same geometry and same focus accent as the team-chat composer:
            // one control should not change shape between two chat surfaces.
            "rounded-xl border border-border/70 bg-background p-2 shadow-sm focus-within:border-chat-accent/50 focus-within:ring-2 focus-within:ring-chat-accent/20",
            drop.over && "border-chat-accent ring-2 ring-chat-accent/25"
          )}
        >
          {drop.over && (
            <p
              role="status"
              className="px-1 pb-1 text-xs text-muted-foreground"
            >
              {t("attachmentDrop")}
            </p>
          )}
          <NativeInlineComposer
            ref={composer}
            value={draft}
            onChange={(next) => {
              // Re-reporting what it already holds is not an edit.
              if (sameSegments(next, draft)) return
              draftRef.current = promptText(next)
              setDraft(next)
            }}
            tray={attachments}
            label={
              running && supportsSteering
                ? t("agentFollowUp")
                : t("agentPrompt")
            }
            placeholder={
              running && supportsSteering
                ? t("agentFollowUp")
                : t("agentPrompt")
            }
            onSubmit={() => {
              if (running && supportsSteering) void steer()
              else if (!running) void send()
            }}
            busy={sending || steeringInFlight !== null || attachments.uploading}
            // Typing is always allowed during a run (for the next turn, or for
            // steering when supported). Entering submits when not running or
            // when steering is supported.
            readOnly={sending || session.archived || !ready}
            // Attachments are not supported mid-run.
            attachDisabled={session.archived || !ready || running}
            maxChars={NEEV_MAX_PROMPT_CHARS}
            className="min-h-14"
          />
          {/* Sending is held while bytes are still going up; say so rather than
              leaving a Send button that does nothing when pressed. */}
          {attachments.uploading && (
            <p role="status" className="px-1 text-xs text-muted-foreground">
              {t("attachmentsPending")}
            </p>
          )}
          {running && !attachments.empty && (
            <p role="status" className="text-xs text-muted-foreground">
              {t("agentFollowUpTextOnly")}
            </p>
          )}
          <NativeError error={error} />
          {/* Agents off for the whole server is not something this person can
              connect their way out of, so it stays a sentence. Everything that
              IS actionable is the Connect control in the row below. */}
          {!runtimeEnabled && (
            <p role="status" className="px-1 text-xs text-muted-foreground">
              {t("agentsOff")}
            </p>
          )}
          <div className="flex min-w-0 items-center gap-2">
            <Bot
              aria-hidden
              className="size-3.5 shrink-0 text-muted-foreground"
            />
            <AttachmentButton
              disabled={sending || session.archived || !ready}
              remaining={remainingAttachments(attachments)}
              onFiles={(files) => void composer.current?.addFiles(files)}
            />
            {/* The provider is immutable here — a turn may not contradict its
                session's, and a team holder's id is derived from it — so only
                this provider's models are offered. */}
            <AgentControls
              capabilities={capabilities}
              provider={session.provider as NeevAgentProvider}
              settings={shown}
              lockProvider
              disabled={applying || session.archived || !ready}
              onChange={(next) => {
                // Stored straight away so it is already in force by the time
                // the next prompt is sent; `send` re-checks in case this is
                // still in flight or was refused.
                setDesired(next)
                void applySettings(next)
              }}
              className="min-w-0 flex-1"
            />
            {/* Announced, not printed. Both states are momentary — a write in
                flight, or one that has not landed — and giving each a visible
                word made the row jump width every time a chip was touched. The
                chips themselves are disabled while `applying`, which is the
                visible half of the same fact. */}
            <span role="status" className="sr-only">
              {applying
                ? t("settingsPending")
                : desired !== null
                  ? t("settingsUnsaved")
                  : ""}
            </span>
            {/* Honest, compact, and the only thing here that fixes it. The
                sentence and trailing link this replaces sat under the composer
                whenever the provider was not connected — a full line, every
                time, saying the state rather than offering the remedy. Send
                stays disabled either way: nothing here pretends a
                disconnected provider will run. */}
            {runtimeEnabled &&
              connection &&
              connection.state !== "connected" && (
                <>
                  <p id={connectNote} className="sr-only">
                    {t("providerNotConnected", { name: session.provider })}
                  </p>
                  <Button
                    asChild
                    size="sm"
                    variant="outline"
                    className="h-7 shrink-0 gap-1.5 rounded-full px-2.5 text-xs font-normal"
                  >
                    <Link
                      href="/settings?section=agents"
                      aria-describedby={connectNote}
                      title={t("providerNotConnected", {
                        name: session.provider,
                      })}
                      onClick={(event) => {
                        if (onRequestSettings) {
                          event.preventDefault()
                          onRequestSettings()
                        }
                      }}
                    >
                      <PlugZap aria-hidden className="size-3.5" />
                      {connection.state === "reconnect_required"
                        ? t("reconnectProvider")
                        : t("connectProvider")}
                    </Link>
                  </Button>
                </>
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
            {/* ONE control, always in the same corner. A run shows Stop; typing
                a follow-up the run can take turns it into Send, and clearing
                the text turns it back. Two buttons side by side moved Stop
                left the moment steering was on. */}
            {running && (
              <p id={stopHint} className="sr-only">
                {t("stopShortcutHint")}
              </p>
            )}
            <Button
              type={stopping ? "button" : "submit"}
              size="icon"
              variant="default"
              className="ms-auto rounded-md"
              aria-label={stopping ? t("stopGeneration") : t("send")}
              // During a run the key that stops it is announced on the one
              // control, and repeated in the tooltip when that control is Send.
              aria-keyshortcuts={running ? "Escape" : undefined}
              aria-describedby={running && !stopping ? stopHint : undefined}
              title={
                stopping
                  ? t("stopGeneration")
                  : running
                    ? `${t("send")} · ${t("stopShortcutHint")}`
                    : t("send")
              }
              disabled={
                stopping
                  ? false
                  : sending ||
                    steeringInFlight !== null ||
                    session.archived ||
                    !ready ||
                    attachments.uploading ||
                    (promptIsEmpty(draft) && attachments.empty)
              }
              onClick={stopping ? () => void cancel() : undefined}
            >
              {stopping ? (
                <Square aria-hidden className="size-3.5 fill-current" />
              ) : (
                <ArrowUp aria-hidden />
              )}
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
                setDraft([])
                draftRef.current = ""
                attachments.clear()
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

  function runStatusLabel() {
    // A healthy run is NOT reported here. The activity line beside the turn
    // already says it, counts it, and names the tool — so this header repeated
    // it in a second place, one that could not say anything more. What is kept
    // is what the activity line cannot know: a run whose stream dropped.
    if (running && phase !== "streaming")
      return phase === "reconnecting"
        ? t("agentReattaching")
        : t("agentRunLost")
    if (!runtimeEnabled) return t("agentsOff")
    return t(`connectionState_${connection?.state ?? "unavailable"}`)
  }

  /**
   * A failed turn, in the provider's own words. Actions only on the NEWEST
   * turn: retrying an older one would answer a question the conversation has
   * already moved past.
   */
  function failedTurn(turn: NeevAgentTurn) {
    const failure = runFailure(turn)
    if (!failure) return null
    const newest = turns !== null && turns[turns.length - 1]?.id === turn.id
    const reconnected =
      connection?.state === "connected" &&
      connection.checkedAt !== null &&
      connection.checkedAt >= (turn.completedAt ?? turn.createdAt)
    return (
      <NativeRunFailure
        failure={failure}
        provider={providerName(turn.provider)}
        retrying={retryingRun === turn.runId}
        reconnected={reconnected}
        onRetry={newest && !running ? () => void retryTurn(turn) : undefined}
        onReconnect={
          newest
            ? () => {
                if (onRequestSettings) onRequestSettings()
                else router.push("/settings?section=agents")
              }
            : undefined
        }
      />
    )
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
