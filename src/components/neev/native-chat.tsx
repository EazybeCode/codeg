"use client"

import {
  ArrowLeft,
  ArrowUp,
  Lock,
  MoreHorizontal,
  PlugZap,
  Sparkles,
  Users,
} from "lucide-react"
import { NativeMarkdown } from "./native-markdown"

import { useEffect, useId, useLayoutEffect, useRef, useState } from "react"
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
  loadAgentCapabilities,
  loadAgentConnections,
  loadAgentSession,
  loadTeamAgentSession,
  updateAgentSessionSettings,
  NEEV_AGENT_PROVIDERS,
  NEEV_DEFAULT_MODEL,
  type NeevActivityItem,
  type NeevAgentCapabilities,
  type NeevAgentConnections,
  type NeevAgentProvider,
  type NeevAgentSession,
  type NeevAgentSettings,
  type NeevAgentTurn,
} from "@/lib/neev-agent-sessions-client"
import { AgentControls, reconcileAgentSettings } from "./native-agent-controls"
import {
  AttachmentButton,
  SentPrompt,
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
import { uploadChatAttachment } from "@/lib/neev-attachments"
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

/** Whether two sends carry the same files, in the same order. */
function sameKeys(
  left: readonly string[] | undefined,
  right: readonly string[]
): boolean {
  return (
    (left ?? []).length === right.length &&
    (left ?? []).every((key, index) => key === right[index])
  )
}

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
  /** Draw a way back to the listing. Only the standalone folder view needs one. */
  showBack?: boolean
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
  showBack = false,
}: Props) {
  const auth = useNativeAuth(),
    t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  // Per instance: every open chat tab stays mounted, so a fixed id here would
  // point every Ask agent toggle at the first chat's description.
  const postsNote = useId()
  // Same reasoning, for the Connect control's honest description of WHICH
  // provider is not connected.
  const connectNote = useId()
  /**
   * Opens when this mount's first metadata read has settled.
   *
   * Drawing the shell early means the composer exists before the read that
   * proves this chat is still readable. Disabling Send for that window was the
   * first answer and it was the wrong lever: it makes a button that does
   * nothing when pressed, for a race the reader cannot see. Sending waits on
   * this instead — the post is issued after the read either way, and nothing
   * about the composer lies in the meantime.
   */
  const [gate] = useState(() => {
    let open!: () => void
    const promise = new Promise<void>((resolve) => {
      open = resolve
    })
    return { promise, open }
  })
  const [latest, setLatest] = useState(resource),
    [messages, setMessages] = useState<NativeMessage[]>([]),
    [draft, setDraft] = useState<PromptSegment[]>([]),
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
    [connections, setConnections] = useState<NeevAgentConnections | null>(null),
    [capabilities, setCapabilities] = useState<NeevAgentCapabilities | null>(
      null
    ),
    // The caller's own holder for this chat, one per provider. It is where the
    // run configuration lives, so it is what the controls read and write.
    [holders, setHolders] = useState<NeevAgentSession[]>([]),
    // A configuration chosen here but not yet stored on the holder, and whether
    // storing it is in flight. Cleared whenever the provider changes, so a
    // level only Codex advertises can never ride along into Claude.
    [desired, setDesired] = useState<NeevAgentSettings | null>(null),
    [applying, setApplying] = useState(false)
  /**
   * Files staged for the next send, uploaded to THIS chat as they arrive.
   *
   * Uploading on arrival is what makes a preview honest — the bytes are already
   * where the run will read them — and it is not a transmission to any
   * provider: an attachment only reaches an agent when a turn that carries its
   * key is actually started. The attachment inherits the chat's audience, so it
   * is exactly as private as the conversation it was dropped into.
   */
  const uploader: AttachmentUploader | null = auth.session
    ? (file, signal) =>
        uploadChatAttachment(
          resource.id,
          file,
          auth.session!.csrf,
          signal ?? auth.signal
        )
    : null
  const attachments = useAttachmentTray({
    upload: uploader,
    disabled: resource.archived || !auth.session,
  })
  const composer = useRef<InlineComposerHandle>(null)
  const drop = useFileDrop({
    // Dropped onto the sentence itself, the editor lands it under the pointer.
    // This is the rest of the card, where the caret is the honest answer.
    onFiles: (files) => void composer.current?.addFiles(files),
    disabled: resource.archived || !auth.session,
  })
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
  /**
   * Point this composer at a mode and a provider.
   *
   * It deliberately does NOT touch the pending configuration. The model menu
   * sets that and switches provider in the same gesture, and a reset in here
   * would land after it and wipe the choice that caused the switch.
   */
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
    // prompts — the same idempotency the plain composer uses. What the id was
    // minted FOR is kept with it: the server hashes the prompt and the keys
    // into the receipt, so re-using an id after removing a file or editing the
    // words is a permanent conflict rather than a retry.
    invokeRequest = useRef<{
      id: string
      prompt: string
      keys: readonly string[]
    } | null>(null)
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
  // A staged file is unsent work in exactly the way typed text is, so leaving
  // with one asks the same question.
  useEffect(() => {
    onDirtyChange?.(!!draftRef.current || !attachments.empty)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [attachments.empty])
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
      setDraft([])
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
  /**
   * Re-read the metadata, then the history that follows this reader's cursor.
   *
   * `opening` is the first read after mounting, and the ONLY one that starts
   * the two together. The first page of history reads nothing off the metadata
   * — it is keyed on the chat id and on this reader's own cursor — so awaiting
   * one before asking for the other put a whole round trip in front of every
   * chat open, with someone watching a spinner for it.
   *
   * Every LATER refresh keeps the strict order. Those are polls, and a poll's
   * metadata read is the authority check: it is how a revocation is noticed. A
   * chat this reader has just lost access to must not have its history asked
   * for again, and on a poll there is no one waiting on the round trip to
   * justify asking early. So the concurrency is spent exactly where it is felt
   * and nowhere else.
   */
  async function refresh(opening = false) {
    let after = sequence.current
    // Started before the metadata is awaited. The authority check is still the
    // FIRST thing that happens to either answer: nothing reaches the transcript
    // until the metadata has come back and been accepted, so a reader whose
    // access was refused still sees nothing. Issuing the read early grants
    // nothing either — the server authorizes this endpoint on its own, and the
    // response is dropped unread when the metadata refuses.
    const firstPage = opening
      ? listNativeMessages(resource.id, after, scope.signal)
      : null
    // Observed the moment it exists. When the metadata read fails, the code
    // below throws before it ever awaits this promise, and a rejection nobody
    // looked at surfaces as an unhandled one. The sink does not swallow it: the
    // `await` further down still sees the same rejection and still reports it.
    firstPage?.catch(() => {})
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
      // The first iteration consumes the read already in flight; the rest are
      // genuinely sequential, since each one needs the cursor the last returned.
      let inflight = firstPage
      do {
        const page = await (inflight ??
          listNativeMessages(resource.id, after, scope.signal))
        inflight = null
        if (!scope.isCurrent()) return
        absorb(page.items)
        if (page.items.length)
          sequence.current = page.items[page.items.length - 1].sequence
        after = page.nextAfter
      } while (after)
      setLoading(false)
      gate.open()
    } catch (e) {
      fail(e)
      if (scope.isCurrent()) setLoading(false)
      gate.open()
    }
  }
  useEffect(() => {
    sending.current = false
    busy.current = false
    setSaving(false)
    void refresh(true)
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
      // Kept, not just walked: the holder carries the model, effort and speed
      // this chat's agent mode actually runs at, which is what the controls
      // show and change.
      setHolders(sessions)
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
  useEffect(() => {
    if (!runtimeEnabled) return
    let live = true
    loadAgentCapabilities(auth.signal)
      .then((value) => {
        if (live) setCapabilities(value)
      })
      .catch(() => {
        // An unreadable catalogue offers nothing rather than a menu of guesses.
      })
    return () => {
      live = false
    }
  }, [runtimeEnabled, auth.signal])
  const connection =
    connections?.connections.find((item) => item.provider === agentProvider) ??
    null
  /** This chat's holder for one provider, once that provider has one. */
  const holderFor = (name: NeevAgentProvider) =>
    holders.find((item) => item.provider === name) ?? null
  /**
   * What a holder runs as, or the defaults when there is not one yet.
   *
   * Read from the holder rather than from local state, which is what keeps it
   * true after a reload. Each provider has its OWN holder, so switching
   * provider shows that provider's stored configuration and never the other
   * one's.
   */
  const settingsOf = (item: NeevAgentSession | null): NeevAgentSettings =>
    item
      ? { model: item.model, effort: item.effort, fast: item.fast }
      : // No holder yet: the defaults stand, and they are the cheap ones.
        { model: NEEV_DEFAULT_MODEL, effort: null, fast: false }
  const holder = holderFor(agentProvider)
  const live = settingsOf(holder)
  const shown = desired ?? live
  // A held choice is released when the SERVER'S OWN ROW reports it — which
  // arrives either as the response to `applySettings` or, for a choice that
  // travelled with an ask, on the next holder read. Clearing it on a successful
  // request instead would claim the server had stored something before it said
  // so, and if it stored something else the row would quietly start describing
  // a configuration nobody chose.
  useEffect(() => {
    if (
      desired &&
      holder &&
      holder.model === desired.model &&
      holder.effort === desired.effort &&
      holder.fast === desired.fast
    )
      setDesired(null)
  }, [desired, holder])

  /**
   * Store the chosen configuration on this chat's holder.
   *
   * Only reachable once the holder exists, because a holder is created by the
   * first invocation. That is not a limit on what can be CHOSEN: every control
   * is live from the start, and before there is a holder the whole triple
   * travels with the ask itself. This path is what keeps a LATER change in
   * force, since a run reads the holder rather than the request that follows.
   *
   * Returns whether the holder now says what the reader chose, so the caller
   * can decline to run under something else.
   */
  async function applySettings(next: NeevAgentSettings): Promise<boolean> {
    if (!holder || !auth.session) return false
    const target = holder.id
    setApplying(true)
    try {
      let revision = holder.revision
      for (let attempt = 0; attempt < 2; attempt++) {
        try {
          const stored = await updateAgentSessionSettings(
            target,
            next,
            revision,
            auth.session.csrf,
            auth.signal
          )
          if (!scope.isCurrent()) return false
          setHolders((old) =>
            old.map((item) => (item.id === stored.id ? stored : item))
          )
          setDesired(null)
          return true
        } catch (cause) {
          // Someone changed this holder while it was open — including this same
          // reader in another tab. Re-read and try once against what is really
          // stored, then stop rather than loop against a moving target.
          const stale =
            cause instanceof NativeClientError && cause.kind === "conflict"
          if (!stale || attempt === 1) throw cause
          const fresh = await loadAgentSession(target, auth.signal)
          if (!scope.isCurrent()) return false
          setHolders((old) =>
            old.map((item) => (item.id === fresh.id ? fresh : item))
          )
          revision = fresh.revision
        }
      }
      return false
    } catch (cause) {
      if (scope.isCurrent()) fail(cause)
      return false
    } finally {
      if (scope.isCurrent()) setApplying(false)
    }
  }
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
    // Files and no words is a complete ask. The prompt goes up empty and the
    // server says what such a turn was asked to do — this composer does not
    // invent a sentence and attribute it to the person who sent it.
    // The words and the files in the order they were written: each file leaves
    // a numbered marker where it sat, and the keys go up in that same order, so
    // what the provider reads has the reference in the sentence it belongs to.
    const written = draft
    const { prompt, ids } = composeWirePrompt(
      written,
      new Map(attachments.items.map((item) => [item.id, item.name]))
    )
    if ((promptIsEmpty(written) && attachments.empty) || !scope.isCurrent())
      return
    // Pressing Enter with no runtime bound must SAY so. Failing silently here
    // is how a prompt disappears into nothing, and the draft stays put so
    // nothing typed is lost to it.
    if (!runtimeEnabled) {
      setError(new NativeClientError("unavailable"))
      return
    }
    // A settings write already in flight owns the holder's revision. Starting
    // a run underneath it would race: the turn could launch against the
    // configuration being replaced, which is precisely the turn the reader was
    // configuring.
    if (
      agentBusy ||
      saving ||
      applying ||
      latest.archived ||
      attachments.uploading ||
      !auth.session
    )
      return
    // The run reads the HOLDER, so an unstored choice would be ignored by the
    // very turn it was chosen for. Store it first, and if that fails do not
    // invoke: the prompt stays exactly where it was typed, with the reason
    // beside it, rather than running under a configuration nobody chose.
    if (desired && holder && !(await applySettings(desired))) return
    setAgentBusy(true)
    setAgentText("")
    setError(null)
    try {
      // Bytes first: a key only exists once the server has the file, so an ask
      // can never mention a file that is not there. A failure here leaves the
      // prompt and the files exactly where they are, so pressing Send again is
      // a retry rather than a second ask.
      const keys = await attachments.takeKeys(undefined, ids)
      if (!scope.isCurrent()) return
      // The same ask keeps its id; a changed one gets a new id, because after a
      // failure the natural next move is to remove a file or reword — and the
      // old id would then be refused forever instead of retried.
      const previous = invokeRequest.current
      const request =
        previous && previous.prompt === prompt && sameKeys(previous.keys, keys)
          ? previous
          : { id: crypto.randomUUID(), prompt, keys }
      invokeRequest.current = request
      const invocation = await invokeTeamAgent(
        latest.id,
        {
          prompt,
          provider: agentProvider,
          // The whole configuration travels with the ask, because the holder
          // for (chat, provider) is CREATED by the first one. Sent every time
          // so the first turn is configured the same way the tenth is, and so
          // there is never a turn that ran as something nobody chose.
          model: shown.model,
          effort: shown.effort,
          fast: shown.fast,
          // Persisted onto the message the server posts for this ask, and
          // staged for the run from that message — one list, not two.
          attachmentKeys: keys,
          requestId: request.id,
          expectedAuthRevision: pinned.current,
        },
        auth.session.csrf,
        auth.signal
      )
      if (!scope.isCurrent()) return
      // The ask is durable now: clear the composer and show it immediately.
      // The files go with it, and only now — the message the server returned is
      // what carries them from here on.
      invokeRequest.current = null
      clearSent(written)
      attachments.clear()
      retry.current = null
      followHistory.current = true
      absorb([invocation.message])
      void nameFromFirstMessage(promptText(written))
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
    // Empty body + files is a message the server accepts and labels; empty
    // body with nothing attached is not a message at all.
    const written = draft
    const { prompt: body, ids } = composeWirePrompt(
      written,
      new Map(attachments.items.map((item) => [item.id, item.name]))
    )
    if (
      (promptIsEmpty(written) && attachments.empty) ||
      sending.current ||
      latest.archived ||
      attachments.uploading ||
      !auth.session ||
      !scope.isCurrent()
    )
      return
    // Nothing is posted before this mount's metadata read has settled. The
    // guards are re-checked afterwards, because the read is exactly what may
    // have revoked this chat.
    await gate.promise
    if (revoked || !auth.session || !scope.isCurrent()) return
    sending.current = true
    setSaving(true)
    setError(null)
    let keys: string[]
    try {
      // Nothing is posted until every staged file has a key. A failure here
      // keeps the draft AND the files, so the next Send is the same message
      // again rather than one with its attachments quietly missing.
      keys = await attachments.takeKeys(undefined, ids)
    } catch (cause) {
      if (scope.isCurrent()) {
        setError(cause)
        sending.current = false
        setSaving(false)
      }
      return
    }
    if (!scope.isCurrent()) {
      sending.current = false
      return
    }
    const candidate = {
      body,
      expectedAuthRevision: pinned.current,
      attachmentKeys: keys,
    }
    const previous = retry.current
    const input =
      previous &&
      previous.body === candidate.body &&
      previous.expectedAuthRevision === candidate.expectedAuthRevision &&
      // Same words but a different set of files is a different message, and
      // reusing the request id would make the server return the first one.
      sameKeys(previous.attachmentKeys, keys)
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
      clearSent(written)
      attachments.clear()
      retry.current = null
      pinned.current = latest.authRevision
      void nameFromFirstMessage(promptText(written))
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
  function clearSent(sent: readonly PromptSegment[]) {
    // Compared as the whole draft, files included: the same words with a
    // different file in them is a different draft, and emptying the box would
    // take that file out of a message nobody has sent yet.
    setDraft((current) => (sameSegments(current, sent) ? [] : current))
    if (draftRef.current === promptText(sent)) {
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
  /*
   * Nothing waits on the history read to draw the conversation.
   *
   * This used to return a single line — "Loading your workspace…" — in place of
   * the whole panel until `refresh()` had finished. `refresh()` reads the
   * resource and THEN pages the history, nose to tail, so opening a brand new
   * chat with no messages in it still spent two round trips showing a blank
   * region and a sentence about the workspace, which was not what was loading.
   *
   * The shell is drawable before either read returns: `latest` starts as the
   * `resource` the parent passed in, which the parent had already fetched and
   * been authorized for. So the title, the audience and the composer are
   * honest from the first frame.
   *
   * What is NOT drawn early is any message. The history list stays empty until
   * `refresh()` has re-read the metadata and been satisfied by it, which is the
   * check that ordering was there to enforce — it is kept exactly as it was.
   * Only the paint is unblocked, not the read.
   */
  const traces = attachTurns(messages, turns, auth.session?.user.id ?? -1)
  // `applying` is in here too, so Enter and the Send button are both held
  // while the configuration is being stored. The textarea stays writable.
  // `loading` is in here, so the held state is VISIBLE rather than a button
  // that looks ready and does nothing. The textarea stays writable, as it does
  // for every other busy state; only sending waits, and `gate` below is what
  // actually holds the POST.
  const composerBusy =
    saving || agentBusy || applying || loading || attachments.uploading
  // Files with no words are a complete message; words with a file still
  // uploading are not, because sending now would drop the file.
  const sendable =
    (!promptIsEmpty(draft) || !attachments.empty) &&
    !composerBusy &&
    !latest.archived
  return (
    <section className="flex min-h-0 flex-1 flex-col" aria-label={latest.title}>
      {/* ONE row, title and standing included. It was a title stacked over a
          metadata line, which cost two rows of height at the top of every
          conversation to say two short things. The conversation is the page. */}
      <div className="flex h-9 shrink-0 items-center gap-2 px-4 sm:gap-3 sm:px-8">
        {showBack && (
          <Button
            variant="ghost"
            size="icon"
            aria-label={t("back")}
            title={t("back")}
            onClick={onBack}
          >
            <ArrowLeft aria-hidden />
          </Button>
        )}
        <div className="flex min-w-0 flex-1 items-baseline gap-2">
          <h2
            className="shrink truncate text-sm font-medium"
            title={latest.title}
          >
            {latest.title}
          </h2>
          {/* The audience never collapses to nothing. At narrow widths the
              words go and the icon stays: hiding the whole line on mobile meant
              a shared conversation looked exactly like a private one, which is
              the one thing here that changes what writing in this box means. */}
          <span
            role="img"
            aria-label={t(latest.visibility)}
            title={t(latest.visibility)}
            className="shrink-0 text-muted-foreground/80 sm:hidden"
          >
            {latest.visibility === "private" ? (
              <Lock aria-hidden className="size-3.5" />
            ) : (
              <Users aria-hidden className="size-3.5" />
            )}
          </span>
          <p className="hidden min-w-0 shrink-0 truncate text-xs text-muted-foreground/80 sm:block">
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
                // accessible name stays the full sentence either way — and now
                // also when the label is showing, where it reads just "Share".
                // Who that shares with is the whole consequence of pressing it,
                // so it stays in the accessible name and in the tooltip rather
                // than being dropped along with the width.
                aria-label={t(
                  actions.visibility === "private" ? "share" : "makePrivate"
                )}
                title={t(
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
                    actions.visibility === "private"
                      ? "shareShort"
                      : "makePrivate"
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
        {/* Says what is actually being waited for. The conversation's own
            history, not "your workspace" — the workspace is already here. */}
        {loading && (
          <li
            role="status"
            className="mx-auto w-full max-w-3xl text-sm text-muted-foreground"
          >
            {t("loadingConversation")}
          </li>
        )}
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
                  // How long it took, from the two stored timestamps. They are
                  // whole seconds, so this says whole seconds — the live clock
                  // is the only thing that measured a tenth.
                  startedAt={traces.get(message.id)!.createdAt * 1000}
                  finishedAt={
                    traces.get(message.id)!.completedAt === null
                      ? null
                      : traces.get(message.id)!.completedAt! * 1000
                  }
                  // `attachTurns` pairs a reply only with a COMPLETED turn, so
                  // a trace reaching this row is by construction a turn that
                  // answered. Read off the turn anyway rather than passed as a
                  // literal, so it stays true if that ever loosens.
                  completed={traces.get(message.id)!.status === "completed"}
                />
              )}
              {/* A message written here keeps its files where they were put;
                  anything else — an agent's reply, a message from before this
                  existed — is the markdown it always was. */}
              {message.attachments.length > 0 ? (
                <SentPrompt
                  body={message.body}
                  attachments={message.attachments}
                />
              ) : (
                <NativeMarkdown markdown={message.body} />
              )}
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
            <div className="min-w-0 flex-1 space-y-1.5">
              <p className="text-xs font-medium">{agentProvider}</p>
              {/* Outside the live region below: the clock on this line changes
                  ten times a second, and inside one it would be announced
                  that often. The reply itself is what is worth hearing. */}
              <ActivityTimeline
                items={liveActivity}
                running={agentBusy}
                startedAt={runStartedAt}
              />
              <div aria-live="polite">
                {agentText && <NativeMarkdown markdown={agentText} />}
                {agentBusy && <span className="sr-only">{n("working")}</span>}
              </div>
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
        <div
          {...drop.handlers}
          className={cn(
            "rounded-xl border bg-background p-2 shadow-sm focus-within:ring-1 focus-within:ring-ring",
            drop.over && "border-primary ring-1 ring-primary"
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
          {/* Writable while a run is in flight. A long run would otherwise
              lock the box for minutes, and what is typed during one is kept:
              the ask was cleared the moment it became durable, so nothing the
              run finishes doing touches this draft.

              Writable during the first read too, now that the composer is drawn
              before it finishes. Only sending waits: a box that refuses the
              first thing typed into a chat that looks open is worse than a Send
              button that says it is not ready yet. */}
          <NativeInlineComposer
            ref={composer}
            value={draft}
            onChange={(next) => {
              // An editor that re-reports what it already holds — remounting
              // after a hidden tab is shown again does this — is not an edit,
              // and must not re-mint the request id of a send in flight.
              if (sameSegments(next, draft)) return
              if (!draftRef.current) pinned.current = latest.authRevision
              draftRef.current = promptText(next)
              setDraft(next)
              retry.current = null
              onDirtyChange?.(!promptIsEmpty(next) || !attachments.empty)
            }}
            tray={attachments}
            label={t("message")}
            placeholder={
              mode === "agent" ? t("agentPromptPlaceholder") : t("message")
            }
            onSubmit={submit}
            busy={composerBusy}
            readOnly={latest.archived}
            attachDisabled={latest.archived || !auth.session}
            autoFocus={fresh}
            className="min-h-14"
          />
          {/* Send is held while bytes are still going up. Holding it without
              saying so is the "button that looks ready and does nothing" this
              composer refuses everywhere else. */}
          {attachments.uploading && (
            <p role="status" className="px-1 text-xs text-muted-foreground">
              {t("attachmentsPending")}
            </p>
          )}
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
          <div className="flex min-w-0 items-center gap-1.5">
            {/* The mode lives with the composer, not in a header or a side
                panel, because it changes what pressing Send does. */}
            <p id={postsNote} className="sr-only">
              {t("agentPostsToTeam")}
            </p>
            <AttachmentButton
              disabled={latest.archived || !auth.session}
              remaining={remainingAttachments(attachments)}
              onFiles={(files) => void composer.current?.addFiles(files)}
            />
            {/* A segmented pair, not two standing pills. Only the CHOSEN side
                spells itself out; the other is its icon, with the same
                accessible name and the same tooltip. Both words were on the
                row at all times before, and between them they cost more width
                than the model, the level and the speed control together —
                while saying something that only changes when it is clicked.
                Team chat is one keyboard stop away, exactly as it was. */}
            <div
              role="group"
              aria-label={t("agentPanel")}
              className="flex shrink-0 items-center rounded-full border p-0.5"
            >
              {(["agent", "team"] as const).map((value) => (
                <Button
                  key={value}
                  type="button"
                  size="sm"
                  variant={mode === value ? "secondary" : "ghost"}
                  aria-pressed={mode === value}
                  // The accessible name never collapses with the label: a
                  // screen reader gets "Team chat" whether or not the word is
                  // painted.
                  aria-label={t(value === "agent" ? "askAgent" : "teamChat")}
                  // Both the ask and the reply become team-visible messages, so
                  // the control that does that carries the promise (spec §5.3) —
                  // on hover for a mouse, and as its description for everyone.
                  aria-describedby={value === "agent" ? postsNote : undefined}
                  title={
                    value === "agent" ? t("agentPostsToTeam") : t("teamChat")
                  }
                  className="h-6 shrink-0 gap-1.5 rounded-full px-2 text-xs font-normal"
                  onClick={() => choose(value)}
                >
                  {value === "agent" ? (
                    <Sparkles aria-hidden className="size-3.5" />
                  ) : (
                    <Users aria-hidden className="size-3.5" />
                  )}
                  {mode === value && (
                    <span className="truncate">
                      {t(value === "agent" ? "askAgent" : "teamChat")}
                    </span>
                  )}
                </Button>
              ))}
            </div>
            {mode === "agent" && (
              <>
                {/* One grouped menu, not a provider select beside a model
                    select: the provider heading a model sits under IS the
                    provider choice, so which agent runs and what it runs as are
                    one decision in one place. */}
                <AgentControls
                  capabilities={capabilities}
                  provider={agentProvider}
                  settings={shown}
                  disabled={agentBusy || applying || latest.archived}
                  onChange={(next, nextProvider) => {
                    if (nextProvider !== agentProvider) {
                      // Each provider keeps its OWN holder. Switching shows
                      // that holder's stored configuration with the model just
                      // picked laid over it — carrying the old provider's level
                      // across would describe a configuration the new holder
                      // does not have.
                      setDesired(
                        reconcileAgentSettings(capabilities, nextProvider, {
                          ...settingsOf(holderFor(nextProvider)),
                          model: next.model,
                        })
                      )
                      // Deliberately after: `choose` no longer touches the
                      // pending choice, so this cannot be undone by the switch
                      // that caused it.
                      choose("agent", nextProvider)
                      return
                    }
                    setDesired(next)
                    // An existing holder is written to now, so the change is in
                    // force before the next ask. Before the first ask there is
                    // nothing to write to yet — the choice is held and travels
                    // WITH that invocation, so the first and costliest turn is
                    // configurable like every other one.
                    if (holder) void applySettings(next)
                  }}
                />
                {applying && (
                  <span role="status" className="text-xs text-muted-foreground">
                    {t("settingsPending")}
                  </span>
                )}
                {/* The one thing that is actually broken, as the one thing
                    that fixes it. This was a sentence and a link under the
                    composer — permanent, a full line wide, and describing a
                    state rather than offering a way out of it. It is now a
                    button that says Connect and goes where connecting
                    happens. Nothing here claims the provider IS connected:
                    the chip appears precisely because it is not, and the run
                    still fails honestly if one is started anyway. */}
                {runtimeEnabled &&
                  connection &&
                  connection.state !== "connected" && (
                    <>
                      <p id={connectNote} className="sr-only">
                        {t("providerNotConnected", { name: agentProvider })}
                      </p>
                      <Button
                        asChild
                        size="sm"
                        variant="outline"
                        className="h-7 shrink-0 gap-1.5 rounded-full px-2.5 text-xs font-normal"
                      >
                        {/* Still a link, so it opens in a new tab the way a link
                          does and works with the panel closed. */}
                        <Link
                          href="/settings?section=agents"
                          aria-describedby={connectNote}
                          title={t("providerNotConnected", {
                            name: agentProvider,
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
              </>
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
          {/*
            Only what is ACTIONABLE, and only when it applies.
            The standing "your question and its reply are posted to the team"
            line lived here permanently. It never changed, it was true of every
            send, and it sat under the composer forever — so it stopped being
            read. It is now the accessible description of the Ask agent toggle
            that makes it true, which is where someone deciding between Agent
            and Team is actually looking.
          */}
          {/* Agents switched off for the whole server is NOT a thing this
              person can connect their way out of, so it stays a sentence and
              does not become a button that leads somewhere useless. Everything
              that IS actionable moved up into the row as the Connect
              control. */}
          {mode === "agent" && !runtimeEnabled && (
            <p
              role="status"
              className="px-1 pt-1.5 text-xs text-muted-foreground"
            >
              {t("agentsOff")}
            </p>
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
