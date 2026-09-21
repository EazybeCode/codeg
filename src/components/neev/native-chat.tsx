"use client"

import {
  ArrowLeft,
  ArrowUp,
  Lock,
  MoreHorizontal,
  Sparkles,
  Users,
} from "lucide-react"
import { NativeMarkdown } from "./native-markdown"

import { useEffect, useLayoutEffect, useRef, useState } from "react"
import Link from "next/link"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { Dialog, DialogTitle, DialogDescription } from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { cn } from "@/lib/utils"
import { NativeClientError } from "@/lib/neev-client"
import { sendsOnEnter } from "@/lib/neev-composer-keys"
import { firstLineTitle } from "@/lib/neev-first-line-title"
import { mergeActivity } from "@/lib/neev-agent-activity"
import type { ResourceActions } from "./native-document"
import {
  appendNativeMessage,
  listNativeMessages,
  loadNativeResource,
  updateNativeResource,
  NativeContentNotFoundError,
  type NativeResource,
  type NativeMessage,
  type AppendNativeMessageInput,
} from "@/lib/neev-content-client"
import {
  cancelAgentRun,
  followAgentRun,
  invokeTeamAgent,
  listAgentTurns,
  loadAgentConnections,
  loadTeamAgentSession,
  NEEV_AGENT_PROVIDERS,
  NEEV_DEFAULT_MODEL,
  type NeevActivityItem,
  type NeevAgentConnections,
  type NeevAgentProvider,
  type NeevAgentSession,
  type NeevAgentTurn,
} from "@/lib/neev-agent-sessions-client"
import { useNeevContentPoll } from "@/hooks/use-neev-content-poll"
import { useNeevMessageStream } from "@/hooks/use-neev-message-stream"
import { useNativeAuth } from "./native-auth-provider"
import {
  NativeDialogContent as DialogContent,
  NativeDropdownMenuContent as DropdownMenuContent,
} from "./native-suspended-surface"
import { NativeError } from "./native-boundary"
import { ActivityTimeline, attachTurns } from "./native-chat-activity"

/**
 * One conversation, two ways of speaking into it.
 *
 * `team` posts an ordinary message. `agent` posts the same prompt AND invokes
 * the runtime on it, so the ask and the reply both land in this thread. It is
 * the same conversation either way: there is no separate agent thread, no
 * private history travelling with a prompt, and no second place to look.
 */
type ChatMode = "agent" | "team"

/**
 * How this person has been using this conversation.
 *
 * A view preference and nothing else — it decides which composer opens, never
 * who may read anything — so it lives in local storage, keyed by the reader and
 * their organization so two accounts on one machine cannot see each other's.
 * It is remembered because forgetting it is dangerous in one specific way:
 * someone who chose Agent, reloaded, and pressed Enter would post a plain
 * message to their team instead of invoking, and only find out afterwards.
 */
type ComposerPreference = { mode: ChatMode; provider: NeevAgentProvider }

function preferenceKey(owner: string, chatId: string): string {
  return `neev:chat-composer:${owner}:${chatId}`
}

function readPreference(key: string): ComposerPreference | null {
  if (typeof window === "undefined") return null
  try {
    const raw = window.localStorage.getItem(key)
    if (!raw) return null
    const value = JSON.parse(raw) as Record<string, unknown>
    if (value.mode !== "agent" && value.mode !== "team") return null
    return {
      mode: value.mode,
      provider: NEEV_AGENT_PROVIDERS.includes(
        value.provider as NeevAgentProvider
      )
        ? (value.provider as NeevAgentProvider)
        : "claude",
    }
  } catch {
    return null
  }
}

/**
 * How far one session can be considered read.
 *
 * The longest prefix of settled turns: everything up to it will never change
 * again, so the next read can start after it. A running turn stops the prefix,
 * because it will be re-read — under the same sequence — once it finishes.
 */
function settledThrough(
  turns: ReadonlyMap<string, NeevAgentTurn>,
  sessionId: string
): string | null {
  const ordered = [...turns.values()]
    .filter((turn) => turn.sessionId === sessionId)
    .sort((left, right) =>
      BigInt(left.sequence) < BigInt(right.sequence) ? -1 : 1
    )
  // `null` means "from the beginning": the server's cursor is a sequence, and
  // sequences start at 1, so there is no zero to ask for.
  let cursor: string | null = null
  for (const turn of ordered) {
    if (turn.status === "running") break
    cursor = turn.sequence
  }
  return cursor
}

function writePreference(key: string, value: ComposerPreference): void {
  if (typeof window === "undefined") return
  try {
    window.localStorage.setItem(key, JSON.stringify(value))
  } catch {
    // Storage can be full or refused; a view preference is not worth failing.
  }
}

type Props = {
  resource: NativeResource
  onBack: () => void
  onChanged: (resource: NativeResource) => void
  onDirtyChange?: (dirty: boolean) => void
  onRequestSettings?: () => void
  actions?: ResourceActions
  /** Just created and still called "New chat": the first message may name it. */
  fresh?: boolean
}
export function NativeChat(props: Props) {
  const auth = useNativeAuth()
  if (
    !auth.session?.organization ||
    props.resource.orgId !== auth.session.organization.id
  )
    return null
  return <Chat key={`${auth.scopeKey}:${props.resource.id}`} {...props} />
}
function Chat({
  resource,
  onBack,
  onChanged,
  onDirtyChange,
  onRequestSettings,
  actions,
  fresh = false,
}: Props) {
  const auth = useNativeAuth(),
    t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  const [latest, setLatest] = useState(resource),
    [messages, setMessages] = useState<NativeMessage[]>([]),
    [draft, setDraft] = useState(""),
    [error, setError] = useState<unknown>(null),
    [loading, setLoading] = useState(true),
    [saving, setSaving] = useState(false),
    [revoked, setRevoked] = useState(false),
    [review, setReview] = useState(false),
    [agentText, setAgentText] = useState(""),
    [agentBusy, setAgentBusy] = useState(false),
    [agentProvider, setAgentProvider] = useState<NeevAgentProvider>("claude"),
    [agentRunId, setAgentRunId] = useState<string | null>(null),
    // The trace of the run in flight, and when it began, so the composer can
    // say what is happening now rather than only that something is.
    [liveActivity, setLiveActivity] = useState<NeevActivityItem[]>([]),
    [runStartedAt, setRunStartedAt] = useState<number | null>(null),
    // The caller's own stored turns for this chat: what makes a reopened
    // conversation show the trace it showed before.
    [turns, setTurns] = useState<NeevAgentTurn[]>([]),
    [connections, setConnections] = useState<NeevAgentConnections | null>(null)
  // A new conversation starts in agent mode; an existing one opens the way
  // this person last used it. Either is one click from the other.
  const [mode, setMode] = useState<ChatMode>(fresh ? "agent" : "team")
  const chose = useRef(false)
  const storageKey = auth.session
    ? preferenceKey(
        `${auth.session.user.id}:${auth.session.organization?.id ?? "identity"}`,
        resource.id
      )
    : null
  // Read on mount, not during render: the server has no local storage, and a
  // value read during SSR would hydrate into a mismatch.
  useEffect(() => {
    if (!storageKey) return
    const stored = readPreference(storageKey)
    if (!stored) return
    chose.current = true
    setMode(stored.mode)
    setAgentProvider(stored.provider)
  }, [storageKey])
  function choose(next: ChatMode, provider = agentProvider) {
    chose.current = true
    setMode(next)
    setAgentProvider(provider)
    if (storageKey) writePreference(storageKey, { mode: next, provider })
  }
  const runtimeEnabled = auth.capabilities?.runtimeEnabled ?? false
  // With no runtime bound, agent mode can only ever refuse; a chat nobody has
  // put in that mode ON PURPOSE opens where it can actually be used. A chosen
  // mode is never overridden — silently turning Agent into Team would make the
  // next Enter post to the team instead of invoking.
  useEffect(() => {
    if (
      !chose.current &&
      auth.capabilities &&
      !auth.capabilities.runtimeEnabled
    )
      setMode("team")
  }, [auth.capabilities])
  // Absent capability reads as "no push": the client then polls and says so.
  const pushAdvertised = auth.capabilities?.realtime ?? false
  const historyElement = useRef<HTMLOListElement>(null)
  const followHistory = useRef(true)
  useLayoutEffect(() => {
    const history = historyElement.current
    if (history && followHistory.current)
      history.scrollTop = history.scrollHeight
  }, [messages, loading, liveActivity, agentText])
  const pinned = useRef(resource.authRevision),
    draftRef = useRef(""),
    sequence = useRef<string | null>(null),
    retry = useRef<AppendNativeMessageInput | null>(null),
    sending = useRef(false),
    busy = useRef(false),
    // Reopening resumes a run that is still going; it must never resume the
    // same one twice, which would double its trace.
    resumed = useRef(new Set<string>()),
    // Per-session read cursors and the turns behind them, so a reload after a
    // completed run reads what is new rather than the whole history again.
    cursors = useRef(new Map<string, string | null>()),
    turnsRef = useRef<NeevAgentTurn[]>([]),
    // A retry after a lost response reuses the id, so one ask cannot post two
    // prompts — the same idempotency the plain composer uses.
    invokeRequest = useRef<string | null>(null)
  // Push delivery replaces the interval while it is genuinely live; every other
  // state keeps polling, so a dropped or unavailable stream still delivers.
  const stream = useNeevMessageStream({
    chatId: resource.id,
    enabled: pushAdvertised && !revoked && !loading,
    signal: auth.signal,
    onNotify: () => void refresh(),
  })
  const scope = useNeevContentPoll({
    signal: auth.signal,
    scopeKey: auth.scopeKey,
    resourceScope: resource.id,
    enabled: !revoked && !loading && stream.status !== "live",
    onPoll: refresh,
  })
  useEffect(() => {
    setLatest(resource)
    if (!draftRef.current && !retry.current)
      pinned.current = resource.authRevision
  }, [resource])
  // Merge server rows into the thread by id, ordered by sequence. Callers never
  // invent a message: everything here came back from the server.
  function absorb(incoming: NativeMessage[]) {
    setMessages((old) =>
      [
        ...new Map(
          [...old, ...incoming].map((item) => [item.id, item])
        ).values(),
      ].sort((a, b) =>
        BigInt(a.sequence) < BigInt(b.sequence)
          ? -1
          : BigInt(a.sequence) > BigInt(b.sequence)
            ? 1
            : 0
      )
    )
  }
  function fail(e: unknown) {
    if (!scope.isCurrent()) return
    if (
      e instanceof NativeContentNotFoundError ||
      (e instanceof NativeClientError &&
        (e.kind === "forbidden" || e.kind === "unauthorized"))
    ) {
      setMessages([])
      turnsRef.current = []
      cursors.current.clear()
      setTurns([])
      setDraft("")
      draftRef.current = ""
      retry.current = null
      setReview(false)
      setRevoked(true)
      onDirtyChange?.(false)
      scope.stop()
      onBack()
    }
    setError(e)
    auth.handleAuthorityError(e)
  }
  async function refresh() {
    try {
      const metadata = await loadNativeResource(
        resource.orgId,
        resource.id,
        scope.signal
      )
      if (!scope.isCurrent()) return
      if (metadata.kind !== "chat")
        throw new NativeClientError("invalid_response")
      setLatest(metadata)
      onChanged(metadata)
      if (!draftRef.current && !retry.current)
        pinned.current = metadata.authRevision
      // Metadata authority is checked before every incremental history read.
      let after = sequence.current
      do {
        const page = await listNativeMessages(resource.id, after, scope.signal)
        if (!scope.isCurrent()) return
        absorb(page.items)
        if (page.items.length)
          sequence.current = page.items[page.items.length - 1].sequence
        after = page.nextAfter
      } while (after)
      setLoading(false)
    } catch (e) {
      fail(e)
      if (scope.isCurrent()) setLoading(false)
    }
  }
  useEffect(() => {
    sending.current = false
    busy.current = false
    setSaving(false)
    void refresh()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope])
  /**
   * Replay this reader's own agent history for this chat, and rejoin a run that
   * is still going.
   *
   * The holder is looked up per provider and is the CALLER'S: the server scopes
   * it to the current actor, and a holder owned by anyone else is dropped here
   * as well, so a shared thread never borrows a teammate's private trace to
   * decorate their reply. Rejoining is a GET on the run, so nothing re-executes
   * and no prompt is re-sent (§5.2).
   *
   * It reads INCREMENTALLY. Every completed run calls this again, and re-reading
   * the whole history each time would grow without bound in exactly the chats
   * that are used most. The cursor only advances past turns that have SETTLED:
   * a running turn keeps its sequence when it completes, so advancing past it
   * would mean never seeing the trace it finished with.
   */
  async function loadTraces() {
    if (!runtimeEnabled || !auth.session) return
    const owner = auth.session.user.id
    try {
      // One lookup per provider, concurrently: a holder the reader does not own
      // is discarded here as well as server-side.
      const sessions = (
        await Promise.all(
          NEEV_AGENT_PROVIDERS.map((provider) =>
            loadTeamAgentSession(resource.id, provider, scope.signal)
          )
        )
      ).filter(
        (session): session is NeevAgentSession =>
          session !== null && session.ownerId === owner
      )
      if (!scope.isCurrent()) return
      const merged = new Map(turnsRef.current.map((turn) => [turn.id, turn]))
      for (const session of sessions) {
        let after: string | null = cursors.current.get(session.id) ?? null
        do {
          const page = await listAgentTurns(session.id, after, scope.signal)
          if (!scope.isCurrent()) return
          // Upsert: a turn read while running comes back settled, under the
          // same id and the same sequence.
          for (const turn of page.items) merged.set(turn.id, turn)
          after = page.nextAfter
        } while (after)
        cursors.current.set(session.id, settledThrough(merged, session.id))
      }
      if (!scope.isCurrent()) return
      const found = [...merged.values()]
      turnsRef.current = found
      setTurns(found)
      const live = found.find((turn) => turn.status === "running")
      if (live && !busy.current && !resumed.current.has(live.runId)) {
        resumed.current.add(live.runId)
        // Show the provider that is ACTUALLY running, not whichever the
        // composer happened to be pointed at.
        if (NEEV_AGENT_PROVIDERS.includes(live.provider as NeevAgentProvider))
          setAgentProvider(live.provider as NeevAgentProvider)
        await follow(live.runId, live.createdAt * 1000, live.activity)
      }
    } catch (cause) {
      if (!scope.isCurrent()) return
      // Replay decorates a conversation that has already loaded, so an ordinary
      // failure is left alone. Losing the authority that permitted it is not
      // ordinary: private activity must not outlive it on screen.
      if (
        cause instanceof NativeContentNotFoundError ||
        (cause instanceof NativeClientError &&
          (cause.kind === "forbidden" || cause.kind === "unauthorized"))
      ) {
        turnsRef.current = []
        cursors.current.clear()
        setTurns([])
        auth.handleAuthorityError(cause)
      }
    }
  }
  useEffect(() => {
    if (loading) return
    void loadTraces()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope, loading, runtimeEnabled])
  /**
   * With nothing stored, read the conversation itself.
   *
   * A thread that already contains agent replies, or that this reader has
   * already invoked in, is one being used with an agent; opening it in team
   * mode would be the same silent downgrade a forgotten preference causes. A
   * thread of purely human messages opens in team mode, which is what it is.
   */
  useEffect(() => {
    if (chose.current || fresh || !runtimeEnabled) return
    if (
      turns.length > 0 ||
      messages.some((item) => item.authorKind === "agent")
    )
      setMode("agent")
  }, [fresh, runtimeEnabled, turns, messages])
  useEffect(() => {
    if (!runtimeEnabled) return
    let live = true
    loadAgentConnections(auth.signal)
      .then((value) => {
        if (live) setConnections(value)
      })
      .catch(() => {
        // The composer states what it knows; an unreadable list is simply not
        // knowing, never a claim that the provider is connected.
      })
    return () => {
      live = false
    }
  }, [runtimeEnabled, auth.signal])
  const connection =
    connections?.connections.find((item) => item.provider === agentProvider) ??
    null
  /**
   * Follow one run to its end, live.
   *
   * The same path serves a run just started and a run rejoined on reopen, so
   * the two can never diverge. The text streamed here is a preview; what the
   * team keeps is the persisted reply, which is re-read once the run settles.
   */
  async function follow(
    runIdValue: string,
    startedAt: number,
    seed: readonly NeevActivityItem[]
  ) {
    busy.current = true
    setAgentBusy(true)
    setAgentRunId(runIdValue)
    setRunStartedAt(startedAt)
    setLiveActivity(mergeActivity([], seed))
    followHistory.current = true
    try {
      await followAgentRun({
        runId: runIdValue,
        signal: scope.signal,
        onPhase: (phase) => {
          if (scope.isCurrent() && phase === "lost")
            setError(new NativeClientError("unavailable"))
        },
        onActivity: (item) => {
          if (!scope.isCurrent()) return
          // `seq` is the identity: a rejoin replays the stored prefix, so this
          // upserts rather than appends and the trace cannot double.
          setLiveActivity((old) => mergeActivity(old, [item]))
        },
        onEvent: (event) => {
          if (!scope.isCurrent()) return
          if (event.type === "started") setAgentText("")
          else if (event.type === "chunk")
            setAgentText((current) => current + event.text)
          else if (event.status !== "completed")
            setError(new NativeClientError("unavailable"))
        },
      })
    } finally {
      busy.current = false
      if (scope.isCurrent()) {
        setAgentBusy(false)
        setAgentRunId(null)
        setAgentText("")
        setLiveActivity([])
        setRunStartedAt(null)
      }
    }
    if (!scope.isCurrent()) return
    await refresh()
    await loadTraces()
  }
  // Explicit team invocation (spec §5.3). The prompt is posted as an ordinary
  // human message by the server, the reply is written by the run driver as an
  // agent-authored message, and this view re-reads the thread once the run
  // settles — so what everyone sees is what was actually persisted. No private
  // Agent Chat history travels with the prompt; only this prompt does.
  async function askAgent() {
    if (!draft.trim() || !scope.isCurrent()) return
    // Pressing Enter with no runtime bound must SAY so. Failing silently here
    // is how a prompt disappears into nothing, and the draft stays put so
    // nothing typed is lost to it.
    if (!runtimeEnabled) {
      setError(new NativeClientError("unavailable"))
      return
    }
    if (agentBusy || saving || latest.archived || !auth.session) return
    const prompt = draft.trim()
    setAgentBusy(true)
    setAgentText("")
    setError(null)
    invokeRequest.current ??= crypto.randomUUID()
    try {
      const invocation = await invokeTeamAgent(
        latest.id,
        {
          prompt,
          provider: agentProvider,
          model: NEEV_DEFAULT_MODEL,
          requestId: invokeRequest.current,
          expectedAuthRevision: pinned.current,
        },
        auth.session.csrf,
        auth.signal
      )
      if (!scope.isCurrent()) return
      // The ask is durable now: clear the composer and show it immediately.
      invokeRequest.current = null
      clearSent(prompt)
      retry.current = null
      followHistory.current = true
      absorb([invocation.message])
      void nameFromFirstMessage(prompt)
      if (invocation.turn.status !== "running") {
        // Refused before it ever ran; the ask stays visible and the reason is
        // named rather than left as a fake pending state.
        setAgentBusy(false)
        setError(new NativeClientError("unavailable"))
        await refresh()
        return
      }
      resumed.current.add(invocation.runId)
      await follow(invocation.runId, Date.now(), invocation.turn.activity)
    } catch (cause) {
      if (scope.isCurrent()) {
        setAgentBusy(false)
        setAgentRunId(null)
        fail(cause)
      }
    }
  }
  async function cancelAgent() {
    const runId = agentRunId
    if (!runId || !auth.session) return
    try {
      await cancelAgentRun(runId, auth.session.csrf, auth.signal)
    } catch {
      // Best-effort: the run stream ends with a canceled `finished` event.
    }
  }
  async function send() {
    if (
      !draft.trim() ||
      sending.current ||
      latest.archived ||
      !auth.session ||
      !scope.isCurrent()
    )
      return
    sending.current = true
    setSaving(true)
    setError(null)
    const candidate = { body: draft, expectedAuthRevision: pinned.current }
    const previous = retry.current
    const input =
      previous &&
      previous.body === candidate.body &&
      previous.expectedAuthRevision === candidate.expectedAuthRevision
        ? previous
        : { ...candidate, requestId: crypto.randomUUID() }
    retry.current = input
    try {
      const message = await appendNativeMessage(
        resource.id,
        input,
        auth.session.csrf,
        scope.signal
      )
      if (!scope.isCurrent()) return
      followHistory.current = true
      absorb([message])
      // Leave the incremental cursor unchanged: another author's intervening messages may exist.
      clearSent(input.body)
      retry.current = null
      pinned.current = latest.authRevision
      void nameFromFirstMessage(input.body)
    } catch (e) {
      fail(e)
    } finally {
      if (scope.isCurrent()) {
        sending.current = false
        setSaving(false)
      }
    }
  }
  /**
   * Clear what was just sent, and only that.
   *
   * The composer stays usable while a send is in flight — a run can take
   * minutes, and locking the box for its duration is worse than useless — so by
   * the time the server answers, the reader may already be typing the next
   * thing. Clearing unconditionally would delete it in front of them. If the
   * box still holds exactly what was sent, it is emptied; if it holds anything
   * else, that is newer than the send and is left alone.
   */
  function clearSent(sent: string) {
    setDraft((current) => (current === sent ? "" : current))
    if (draftRef.current === sent) {
      draftRef.current = ""
      onDirtyChange?.(false)
    }
  }
  /** Whichever way this composer is pointed. */
  function submit() {
    if (mode === "agent") void askAgent()
    else void send()
  }
  /**
   * A chat nobody named takes its name from what was said first.
   *
   * Once, and only while it still carries the placeholder: a name someone
   * chose is never overwritten. It runs after the message landed, so the auth
   * revision it uses is the current one, and it takes the send mutex so a
   * second message cannot race the rename into a conflict.
   */
  const pendingName = useRef(fresh)
  async function nameFromFirstMessage(body: string) {
    if (!pendingName.current || !auth.session) return
    // A chat already named by hand is never renamed by us.
    if (latest.title !== t("untitledChat")) {
      pendingName.current = false
      return
    }
    const derived = firstLineTitle(body)
    // An empty message names nothing; keep the chance for the next one.
    if (!derived || derived === latest.title) return
    pendingName.current = false
    sending.current = true
    try {
      const renamed = await updateNativeResource(
        resource.orgId,
        resource.id,
        {
          nodeId: latest.nodeId,
          title: derived,
          visibility: latest.visibility,
          archived: latest.archived,
          expectedAuthRevision: latest.authRevision,
        },
        auth.session.csrf,
        scope.signal
      )
      if (!scope.isCurrent()) return
      pinned.current = renamed.authRevision
      setLatest(renamed)
      onChanged(renamed)
    } catch {
      // Naming is a convenience; a failure must not disturb the conversation.
    } finally {
      sending.current = false
    }
  }

  if (revoked)
    return (
      <div className="space-y-3">
        <p role="alert">{t("unavailable")}</p>
        <Button variant="ghost" onClick={onBack}>
          {t("back")}
        </Button>
      </div>
    )
  if (loading) return <p role="status">{n("loading")}</p>
  const traces = attachTurns(messages, turns, auth.session?.user.id ?? -1)
  const composerBusy = saving || agentBusy
  const sendable = !!draft.trim() && !composerBusy && !latest.archived
  return (
    <section className="flex min-h-0 flex-1 flex-col" aria-label={latest.title}>
      {/* One compact row. The conversation is the page; the header only has to
          name it, say who can see it, and hold the few actions on it. */}
      <div className="flex shrink-0 items-center gap-2 px-4 py-1.5 sm:gap-3 sm:px-8">
        <Button
          variant="ghost"
          size="icon"
          aria-label={t("back")}
          title={t("back")}
          onClick={onBack}
        >
          <ArrowLeft aria-hidden />
        </Button>
        <div className="min-w-0 flex-1">
          <h2 className="truncate text-sm font-medium" title={latest.title}>
            {latest.title}
          </h2>
          <p className="truncate text-xs text-muted-foreground">
            {t(latest.visibility)}
            {latest.archived ? ` · ${t("archived")}` : ""} ·{" "}
            <span role="status">
              {t(
                stream.status === "live"
                  ? "liveUpdates"
                  : stream.status === "reconnecting"
                    ? "reconnectingUpdates"
                    : "polledUpdates"
              )}
            </span>
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-0.5 sm:gap-1">
          {actions?.isCreator && actions.shareable && (
            <>
              <Button
                variant="ghost"
                size="sm"
                disabled={actions.disabled}
                // The label collapses to its icon on narrow viewports; the
                // accessible name stays the full sentence either way.
                aria-label={t(
                  actions.visibility === "private" ? "share" : "makePrivate"
                )}
                aria-describedby={
                  actions.visibility === "organization"
                    ? "neev-make-private-warning"
                    : undefined
                }
                onClick={
                  actions.visibility === "private"
                    ? actions.onShare
                    : actions.onMakePrivate
                }
              >
                {actions.visibility === "private" ? (
                  <Users aria-hidden />
                ) : (
                  <Lock aria-hidden />
                )}
                <span className="hidden sm:inline">
                  {t(
                    actions.visibility === "private" ? "share" : "makePrivate"
                  )}
                </span>
              </Button>
              {actions.visibility === "organization" && (
                <p id="neev-make-private-warning" className="sr-only">
                  {t("makePrivateWarning")}
                </p>
              )}
            </>
          )}
          {actions?.isCreator && (
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
                <DropdownMenuItem
                  disabled={actions.disabled}
                  onSelect={actions.onRename}
                >
                  {t("rename")}
                </DropdownMenuItem>
                <DropdownMenuItem
                  disabled={actions.disabled}
                  onSelect={actions.onArchiveToggle}
                >
                  {t(actions.archived ? "reopen" : "archive")}
                </DropdownMenuItem>
                <DropdownMenuItem
                  disabled={actions.disabled}
                  onSelect={actions.onMove}
                >
                  {t("move")}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </div>
      </div>
      <ol
        ref={historyElement}
        onScroll={(event) => {
          const history = event.currentTarget
          followHistory.current =
            history.scrollHeight - history.scrollTop - history.clientHeight < 48
        }}
        aria-label={t("messages")}
        className="min-h-0 flex-1 space-y-6 overflow-y-auto overscroll-contain px-4 py-3 sm:px-8"
      >
        {messages.map((message) => (
          <li key={message.id} className="mx-auto flex w-full max-w-3xl gap-3">
            <span
              aria-hidden
              className="flex size-7 shrink-0 items-center justify-center rounded-md bg-muted text-xs font-medium"
            >
              {message.authorKind === "agent" ? (
                <Sparkles className="size-3.5" />
              ) : (
                message.authorName.slice(0, 1)
              )}
            </span>
            <div className="min-w-0 flex-1 space-y-1.5">
              <div className="flex flex-wrap items-baseline gap-2 text-xs text-muted-foreground">
                {/* An agent reply names the provider that wrote it and the
                    human who invoked it. `authorId` is still the invoker,
                    so the agent never impersonates a teammate. */}
                <span className="font-medium text-foreground">
                  {message.authorKind === "agent"
                    ? message.agentLabel
                    : message.authorName}
                </span>
                {message.authorKind === "agent" && (
                  <span className="rounded bg-muted px-1.5 py-0.5">
                    {t("agentAuthored", { name: message.authorName })}
                  </span>
                )}
                <time
                  dateTime={new Date(message.createdAt * 1000).toISOString()}
                >
                  {new Date(message.createdAt * 1000).toLocaleString()}
                </time>
              </div>
              {/* The stored trace, replayed: only ever from this reader's own
                  sessions, and only on a reply they invoked. */}
              {traces.has(message.id) && (
                <ActivityTimeline
                  items={traces.get(message.id)!.activity}
                  running={false}
                  startedAt={null}
                />
              )}
              <NativeMarkdown markdown={message.body} />
            </div>
          </li>
        ))}
        {(agentBusy || agentText) && (
          <li className="mx-auto flex w-full max-w-3xl gap-3">
            <span
              aria-hidden
              className="flex size-7 shrink-0 items-center justify-center rounded-md bg-muted"
            >
              <Sparkles className="size-3.5" />
            </span>
            <div className="min-w-0 flex-1 space-y-1.5" aria-live="polite">
              <p className="text-xs font-medium">{agentProvider}</p>
              <ActivityTimeline
                items={liveActivity}
                running={agentBusy}
                startedAt={runStartedAt}
              />
              {agentText && <NativeMarkdown markdown={agentText} />}
              {agentBusy && <span className="sr-only">{n("working")}</span>}
            </div>
          </li>
        )}
      </ol>
      {/* One anchored composer for both modes: the audience of what you are
          about to say never depends on which panel you happened to type in. */}
      <form
        onSubmit={(event) => {
          event.preventDefault()
          submit()
        }}
        className="mx-auto w-full max-w-[832px] shrink-0 px-4 pb-3 pt-1 sm:px-8 sm:pb-5"
      >
        <div className="rounded-xl border bg-background p-2 shadow-sm focus-within:ring-1 focus-within:ring-ring">
          <label className="block">
            <span className="sr-only">{t("message")}</span>
            <textarea
              autoFocus={fresh}
              placeholder={
                mode === "agent" ? t("agentPromptPlaceholder") : t("message")
              }
              value={draft}
              // Writable while a run is in flight. A long run would otherwise
              // lock the box for minutes, and what is typed during one is kept:
              // the ask was cleared the moment it became durable, so nothing
              // the run finishes doing touches this draft.
              disabled={loading || latest.archived}
              onChange={(event) => {
                if (!draftRef.current) pinned.current = latest.authRevision
                draftRef.current = event.target.value
                setDraft(event.target.value)
                retry.current = null
                onDirtyChange?.(!!event.target.value)
              }}
              onKeyDown={(event) => {
                if (!sendsOnEnter(event, { busy: composerBusy })) return
                event.preventDefault()
                submit()
              }}
              className="h-16 max-h-40 min-h-12 w-full resize-y border-0 bg-transparent px-1 py-1.5 text-base outline-none sm:text-sm"
            />
          </label>
          <NativeError error={error} />
          {(latest.authRevision !== pinned.current ||
            (error instanceof NativeClientError &&
              error.kind === "conflict")) && (
            <Button
              type="button"
              variant="outline"
              onClick={async () => {
                await refresh()
                if (scope.isCurrent()) setReview(true)
              }}
            >
              {t("reviewAudience")}
            </Button>
          )}
          <div className="flex flex-wrap items-center gap-1.5">
            {/* The mode lives with the composer, not in a header or a side
                panel, because it changes what pressing Send does. */}
            {(["agent", "team"] as const).map((value) => (
              <Button
                key={value}
                type="button"
                size="sm"
                variant={mode === value ? "secondary" : "ghost"}
                aria-pressed={mode === value}
                className={cn("rounded-full", mode !== value && "opacity-70")}
                onClick={() => choose(value)}
              >
                {value === "agent" ? (
                  <Sparkles aria-hidden />
                ) : (
                  <Users aria-hidden />
                )}
                {t(value === "agent" ? "askAgent" : "teamChat")}
              </Button>
            ))}
            {mode === "agent" && (
              <label className="flex items-center gap-1">
                <span className="sr-only">{t("agentProvider")}</span>
                <select
                  value={agentProvider}
                  disabled={agentBusy}
                  onChange={(event) =>
                    choose("agent", event.target.value as NeevAgentProvider)
                  }
                  className="rounded-md border bg-background px-1.5 py-1 text-xs"
                >
                  {NEEV_AGENT_PROVIDERS.map((name) => (
                    <option key={name} value={name}>
                      {name}
                    </option>
                  ))}
                </select>
              </label>
            )}
            {agentBusy && (
              <Button
                type="button"
                size="sm"
                variant="ghost"
                onClick={() => void cancelAgent()}
              >
                {n("cancel")}
              </Button>
            )}
            <Button
              type="submit"
              size="icon"
              className="ms-auto rounded-md"
              aria-label={composerBusy ? n("working") : t("send")}
              title={composerBusy ? n("working") : t("send")}
              disabled={
                loading || !sendable || (mode === "agent" && !runtimeEnabled)
              }
            >
              <ArrowUp aria-hidden />
            </Button>
          </div>
          {mode === "agent" && (
            <div className="space-y-1 px-1 pt-1.5 text-xs text-muted-foreground">
              {/* Both the ask and the reply become team-visible messages, so
                  the composer must say so before send (spec §5.3). */}
              <p>{t("agentPostsToTeam")}</p>
              {!runtimeEnabled ? (
                <p role="status">{t("agentsOff")}</p>
              ) : (
                connection &&
                connection.state !== "connected" && (
                  // Honest, and actionable: the one place this can be fixed is
                  // one link away.
                  <p role="status">
                    {t("providerNotConnected", { name: agentProvider })}{" "}
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
                )
              )}
            </div>
          )}
        </div>
      </form>
      <Dialog open={review} onOpenChange={setReview}>
        <DialogContent>
          <DialogTitle>{t("reviewAudience")}</DialogTitle>
          <DialogDescription>
            {t("audienceReview", {
              organization: auth.session?.organization?.name ?? "",
              audience: t(latest.visibility),
            })}
          </DialogDescription>
          <Button
            onClick={() => {
              pinned.current = latest.authRevision
              retry.current = null
              setError(null)
              setReview(false)
            }}
          >
            {t("useAudience")}
          </Button>
        </DialogContent>
      </Dialog>
    </section>
  )
}
