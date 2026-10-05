"use client"

import {
  ArrowLeft,
  ArrowUp,
  Square,
  Lock,
  MoreHorizontal,
  PlugZap,
  Sparkles,
  Users,
} from "lucide-react"
import { NativeMarkdown } from "./native-markdown"
import { usePublishRunningChat } from "@/lib/neev-running-chats"

import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react"
import { NativeAgentConnectionsPanel } from "./native-agent-connections"
import {
  CHAT_COMPOSER,
  CHAT_COMPOSER_DROP,
  CHAT_USER_SURFACE,
} from "./chat-chrome"
import { NativeSkeletonRows } from "./native-skeleton"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { Dialog, DialogTitle, DialogDescription } from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { cn } from "@/lib/utils"
import {
  NativeClientError,
  loadMembers,
  type NativeMember,
} from "@/lib/neev-client"
import { firstLineTitle } from "@/lib/neev-first-line-title"
import { mergeActivity } from "@/lib/neev-agent-activity"
import type { ResourceActions } from "./native-document"
import {
  appendNativeMessage,
  listNativeMessages,
  loadNativeResource,
  markNativeMentionsRead,
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
  retryAgentRun,
  steerAgentRun,
  updateAgentSessionSettings,
  NEEV_AGENT_PROVIDERS,
  NEEV_CORE_AGENT_PROVIDERS,
  NEEV_DEFAULT_MODEL,
  type NeevActivityItem,
  type NeevAgentCapabilities,
  type NeevAgentConnections,
  type NeevAgentProvider,
  type NeevAgentSession,
  type NeevAgentSettings,
  type NeevAgentTurn,
  type NeevRoute,
  type NeevRunInput,
} from "@/lib/neev-agent-sessions-client"
import { AgentControls, reconcileAgentSettings } from "./native-agent-controls"
import { RouteNote, useAutoRouting } from "./native-agent-auto"
import {
  ReplyActions,
  useForkAction,
  useOpenFork,
} from "./native-reply-actions"
import { forkChat } from "@/lib/neev-fork"
import { messageSource } from "@/lib/neev-copy"
import {
  autoAccount,
  recalledRoute,
  rememberRoute,
} from "@/lib/neev-auto-route"
import { NativeRunFailure } from "./native-run-failure"
import { providerName, runFailure } from "@/lib/neev-run-failure"
import {
  AttachmentButton,
  OutgoingPrompt,
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
  attachmentIds,
  composeWirePrompt,
  promptIsEmpty,
  promptText,
  sameSegments,
  type PromptSegment,
  type WireMention,
} from "@/lib/neev-inline-prompt"
import { matchCandidates } from "@/lib/neev-mentions"
import {
  claimMentionReads,
  notifyMentionsRead,
  stagedVersion,
  subscribeStagedMentions,
} from "@/lib/neev-mention-reads"
import { MentionedText } from "./native-mentions"
import { uploadChatAttachment } from "@/lib/neev-attachments"
import { useNeevContentPoll } from "@/hooks/use-neev-content-poll"
import { useNeevMessageStream } from "@/hooks/use-neev-message-stream"
import { useNativeAuth } from "./native-auth-provider"
import {
  NativeDialogContent as DialogContent,
  NativeDropdownMenuContent as DropdownMenuContent,
  useTabVisible,
} from "./native-suspended-surface"
import { NativeError } from "./native-boundary"
import {
  ActivityTimeline,
  TranscriptBody,
  attachTurns,
} from "./native-chat-activity"

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

/**
 * Whether two sends name the same people in the same places.
 *
 * Not implied by the body being identical: two coworkers can share a display
 * name, so "@Ana, can you look?" addressed to one Ana and to the other is the
 * same text and a different message. The server folds the triples into the
 * receipt hash for exactly that reason, and reusing a request id across the
 * difference would be a permanent conflict rather than a retry.
 */
function sameMentions(
  left: readonly WireMention[] | undefined,
  right: readonly WireMention[]
): boolean {
  const before = left ?? []
  return (
    before.length === right.length &&
    before.every(
      (span, index) =>
        span.userId === right[index].userId &&
        span.start === right[index].start &&
        span.length === right[index].length
    )
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

/** Neutral author marks; the icon and label distinguish people from agents. */
function AuthorMark({ agent, initial }: { agent: boolean; initial: string }) {
  return (
    <span
      aria-hidden
      className={cn(
        "flex size-7 shrink-0 items-center justify-center rounded-md text-xs font-medium",
        agent
          ? "bg-muted text-muted-foreground ring-1 ring-inset ring-border"
          : "bg-muted text-muted-foreground"
      )}
    >
      {agent ? <Sparkles className="size-3.5" /> : initial}
    </span>
  )
}

/**
 * Who wrote a message and when, on one quiet line. ONE component for the stored
 * row and the live preview, so the two cannot drift and move the reply down the
 * screen when a run ends. Nothing is dropped to shorten it: the invoker keeps
 * the chip, and the timestamp keeps its exact value in `dateTime` and the
 * tooltip while the painted form is short.
 */
function MessageMeta({
  author,
  invoker,
  at,
}: {
  /**
   * Whoever wrote it: a person, or the provider. Nullable because a stored
   * agent reply may carry no label, and inventing a provider name for it would
   * be claiming which agent answered.
   */
  author: string | null
  /** The human who invoked an agent reply; null on a person's own message. */
  invoker: string | null
  /** When it was written, in epoch milliseconds; null if not known yet. */
  at: number | null
}) {
  const t = useTranslations("NativeContent")
  return (
    <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1 text-xs leading-5 text-muted-foreground">
      {author !== null && (
        <span className="font-medium text-foreground">{author}</span>
      )}
      {invoker !== null && (
        <span className="rounded-full bg-muted px-1.5 text-2xs">
          {t("agentAuthored", { name: invoker })}
        </span>
      )}
      {at !== null && (
        <time
          dateTime={new Date(at).toISOString()}
          title={new Date(at).toLocaleString()}
          className="tabular-nums text-muted-foreground/70"
        >
          {messageTime(at)}
        </time>
      )}
    </div>
  )
}

/**
 * A timestamp as short as it can be without losing which day it means: today is
 * the clock alone, an earlier day adds the date, an earlier year adds the year —
 * a bare "Mar 3" would read as this March.
 */
function messageTime(at: number): string {
  const date = new Date(at)
  const now = new Date()
  const time = { hour: "numeric", minute: "2-digit" } as const
  if (date.getFullYear() !== now.getFullYear())
    return date.toLocaleString([], {
      year: "numeric",
      month: "short",
      day: "numeric",
      ...time,
    })
  const sameDay =
    date.getMonth() === now.getMonth() && date.getDate() === now.getDate()
  return sameDay
    ? date.toLocaleTimeString([], time)
    : date.toLocaleString([], { month: "short", day: "numeric", ...time })
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
  actions,
  fresh = false,
  showBack = false,
}: Props) {
  const auth = useNativeAuth(),
    t = useTranslations("NativeContent")
  // Per instance: every open chat tab stays mounted, so a fixed id here would
  // point every Ask agent toggle at the first chat's description.
  const postsNote = useId()
  // Same reasoning, for the Connect control's honest description of WHICH
  // provider is not connected.
  const connectNote = useId()
  const stopHint = useId()
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
  // A View grant: the chat is readable, nothing in it is writable. Decided
  // from the server's access level by the caller, never from the list row.
  const viewOnly = actions?.viewOnly === true
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
    // When it ended, and whether it answered. Both are null/false while it
    // runs; they are set for the moment between the run settling and its
    // stored row arriving, so the preview reads like that row already.
    [runFinishedAt, setRunFinishedAt] = useState<number | null>(null),
    [runCompleted, setRunCompleted] = useState(false),
    // User inputs sent into the running turn via steering. Upserted by
    // `requestId` so a reattach never shows duplicates.
    [liveInputs, setLiveInputs] = useState<NeevRunInput[]>([]),
    // Whether the current run supports steering (provider acknowledged).
    [supportsSteering, setSupportsSteering] = useState(false),
    // A steering request in flight: the requestId, so the same id is retried
    // if the first attempt fails. Cleared on success or when the run ends.
    [steeringInFlight, setSteeringInFlight] = useState<string | null>(null),
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
    [applying, setApplying] = useState(false),
    // The coworkers this conversation may name. Fetched from the organization's
    // own directory, which the server already scopes to the caller's
    // organization and its ACTIVE members — so there is nobody in here who
    // could not already read this chat.
    [directory, setDirectory] = useState<NativeMember[]>([])
  // What the sidebar reads to mark this conversation as running. Published
  // from the state the run already sets here rather than polled for, so it
  // clears the moment the run finishes or is cancelled.
  usePublishRunningChat(auth.session, "chat", resource.id, agentBusy)
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
  const providerChosen = useRef(false)
  const checkingConnection = useRef(false)
  const [connectOpen, setConnectOpen] = useState(false)
  // Retry of the newest failed run: in flight, and the id that makes pressing
  // it again after a dropped response resolve to the same new run.
  const [retrying, setRetrying] = useState(false)
  const retryRequest = useRef<{ runId: string; id: string } | null>(null)
  // Auto routing: the same per-device switch Agent Chats uses, and the last
  // decision for THIS chat so the composer can say what the ask ran on.
  const autoRouting = useAutoRouting(auth.scopeKey, autoAccount(auth.session))
  const forkAction = useForkAction(auth.scopeKey, useOpenFork())
  /**
   * Copy this chat up through one agent reply into a new private chat, and
   * open that in a new tab. The server copies; nothing is run.
   */
  function forkReply(messageId: string, nodeId?: number) {
    const session = auth.session
    if (!session) return
    const chatId = latest.id
    void forkAction.fork(messageId, async (requestId) => {
      const made = await forkChat(
        chatId,
        messageId,
        requestId,
        session.csrf,
        undefined,
        nodeId
      )
      return {
        target: { kind: "resource", resourceId: made.chatId },
        archived: made.archivedAttachmentsNotCopied,
        skipped: 0,
      }
    })
  }
  const [lastRoute, setLastRoute] = useState<NeevRoute | null>(() =>
    recalledRoute(resource.id)
  )
  useEffect(() => setLastRoute(recalledRoute(resource.id)), [resource.id])
  const refreshConnections = useCallback(async () => {
    const value = await loadAgentConnections(auth.signal)
    if (!auth.signal?.aborted) setConnections(value)
    return value
  }, [auth.signal])
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
    if (provider !== agentProvider) {
      providerChosen.current = true
      const target = connections?.connections.find(
        (row) => row.provider === provider
      )
      if (target && target.state !== "connected") {
        setError(null)
        setConnectOpen(true)
      }
    }
    setAgentProvider(provider)
    if (storageKey) writePreference(storageKey, { mode: next, provider })
  }
  const runtimeEnabled = auth.capabilities?.runtimeEnabled ?? false
  // The providers the server offers, as it lists them. Until it has said, only
  // the ones it always offers may be asked about.
  const offered: readonly NeevAgentProvider[] = connections
    ? connections.connections.map((row) => row.provider)
    : NEEV_CORE_AGENT_PROVIDERS
  const offeredKey = offered.join(",")
  const offeredRef = useRef(offered)
  offeredRef.current = offered
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
  // The message on its way: shown at once, settled by the server's answer to
  // THIS request (its message id), or put back in the box if it fails.
  const [outgoing, setOutgoing] = useState<{
    segments: readonly PromptSegment[]
    names: ReadonlyMap<string, string>
    files: ReadonlySet<string>
    after: string | null
  } | null>(null)
  // The tray as of the latest render, for a restore that runs after awaits.
  const stagedNow = useRef(attachments.items)
  useEffect(() => {
    stagedNow.current = attachments.items
  })
  // While this reader's own message is in flight, their NEWER messages wait
  // off screen: one may be this very message arriving by the stream before
  // the response, and which one it is is known only from the response. Held
  // for display only, and only until the request settles — then everything
  // shows, so a same-text message from another tab is never lost or merged.
  const ownId = auth.session?.user.id
  const shownMessages = outgoing
    ? messages.filter(
        (message) =>
          !(
            message.authorKind === "human" &&
            message.authorId === ownId &&
            (outgoing.after === null ||
              Number(message.sequence) > Number(outgoing.after))
          )
      )
    : messages
  useLayoutEffect(() => {
    const history = historyElement.current
    if (history && followHistory.current)
      history.scrollTop = history.scrollHeight
  }, [messages, loading, liveActivity, agentText, outgoing])
  const pinned = useRef(resource.authRevision),
    messagesRef = useRef<NativeMessage[]>([]),
    // Which run the live preview belongs to. A handover still in flight when
    // the next run starts must not clear the next run's text.
    generation = useRef(0),
    // How far the thread had got when the current run began, and the provider
    // it runs as. A sequence rather than a set of ids: a reply from an EARLIER
    // run can still be unread when this one starts, and it sorts below this
    // run's own ask, so a high-water mark excludes it where "not seen yet"
    // would not.
    previewFloor = useRef(0n),
    previewProvider = useRef<string>("claude"),
    // The requestId for the steering request to retry on failure. Kept across
    // failed sends so the same id is used, preventing duplicates.
    steeringRetryId = useRef<{
      id: string
      text: string
      runId: string
    } | null>(null),
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
      mentions: readonly WireMention[]
    } | null>(null)
  /*
   * Whether the reply the live preview stands in for is in the thread already.
   *
   * The preview outlives its run now, so the stored row lands underneath it —
   * from the re-read at the end of `follow` or from a push that got there
   * first — and the preview is the copy to drop: it has no author line, no
   * timestamp and no attachments.
   *
   * Identity, not text. A run's reply is the agent row that sorts after
   * everything the thread held when the run began, is this reader's own, and
   * carries the provider this run ran as. Comparing bodies instead would
   * retire the preview against an identical answer already above it — and
   * would FAIL to retire it whenever the stored reply differs from the stream
   * by so much as a trailing newline, leaving two copies on screen for good.
   */
  const previewStored =
    agentText !== "" &&
    messages.some(
      (item) =>
        BigInt(item.sequence) > previewFloor.current &&
        item.authorKind === "agent" &&
        item.authorId === auth.session?.user.id &&
        item.agentLabel === previewProvider.current
    )
  // Retired by its own stored row arriving and by nothing else — not by a
  // completed read, which can return before the driver has published.
  useEffect(() => {
    if (previewStored) clearPreview()
  }, [previewStored])
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
  /**
   * A mention is read when its conversation is in front of the reader.
   *
   * Not when the notice was clicked: that is a request to come here, and an
   * unsaved-work question can answer it with "stay". Not when this panel
   * merely exists either — a tab stays MOUNTED while hidden, so a chat behind
   * three others is loaded, connected, and seen by nobody.
   *
   * Three things therefore have to be true together, and all three can become
   * true last: the thread has loaded, this tab is the one on screen, and there
   * is a receipt waiting for it. `staged` is watched for the case with no
   * other signal at all — following a notice for the conversation already in
   * front changes nothing else about this panel.
   */
  const visible = useTabVisible()
  const staged = useSyncExternalStore(
    subscribeStagedMentions,
    stagedVersion,
    () => 0
  )
  useEffect(() => {
    if (!visible || loading || revoked || !auth.session || !auth.scopeKey)
      return
    const claimed = claimMentionReads(auth.scopeKey, resource.id)
    if (claimed.length === 0) return
    void markNativeMentionsRead(claimed, auth.session.csrf, auth.signal)
      .then(() => notifyMentionsRead())
      .catch(() => {
        // The conversation is open either way. A count that stays high for one
        // more poll is the smaller failure, and re-staging would mark a
        // conversation read the next time it is opened for any other reason.
      })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible, staged, loading, revoked, auth.scopeKey, resource.id])
  /**
   * Whether anybody here can be named.
   *
   * A private chat has an audience of one — whoever made it — so there is
   * nobody to notify, and the composer is handed no directory at all: `@` is
   * then an ordinary character. Deliberately not a disabled picker. A list of
   * people who cannot read this conversation is an offer to share it, and
   * naming somebody must never widen who can see anything.
   */
  const canMention = latest.visibility === "organization" && !latest.archived
  /**
   * Fetched when the picker first opens, not on mount: most conversations are
   * read without anybody being named, and the directory is a request nobody
   * asked for until the moment they type `@`.
   */
  const directoryRead = useRef(false)
  async function loadDirectory() {
    if (directoryRead.current || !canMention || !auth.session) return
    directoryRead.current = true
    try {
      const members = new Map<number, NativeMember>()
      for (let offset = 0; ; offset += 100) {
        const page = await loadMembers(offset, scope.signal)
        if (!scope.isCurrent() || scope.signal.aborted) return
        for (const member of page) members.set(member.id, member)
        // Offer the first page immediately while later coworkers load.
        setDirectory([...members.values()])
        if (page.length < 100) break
      }
    } catch {
      // The picker then offers nobody, which is what it already shows while the
      // read is in flight. A directory that could not be listed is not worth
      // interrupting what somebody is writing, and the next `@` asks again.
      directoryRead.current = false
    }
  }
  // A staged file is unsent work in exactly the way typed text is, so leaving
  // with one asks the same question — and so is a message on its way: the box
  // is already empty, but leaving would abort a send that has not landed.
  useEffect(() => {
    onDirtyChange?.(
      !!draftRef.current || !attachments.empty || outgoing !== null
    )
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [attachments.empty, outgoing])
  // Merge server rows into the thread by id, ordered by sequence. Callers never
  // invent a message: everything here came back from the server.
  //
  // The ref is the one this merges from, so a run starting in the same tick as
  // an absorb reads the thread as it actually is — `messages` is a render
  // behind at that point, and the run's baseline is taken from it.
  function absorb(incoming: NativeMessage[]) {
    const merged = [
      ...new Map(
        [...messagesRef.current, ...incoming].map((item) => [item.id, item])
      ).values(),
    ].sort((a, b) =>
      BigInt(a.sequence) < BigInt(b.sequence)
        ? -1
        : BigInt(a.sequence) > BigInt(b.sequence)
          ? 1
          : 0
    )
    messagesRef.current = merged
    setMessages(merged)
  }
  function fail(e: unknown) {
    if (!scope.isCurrent()) return
    if (
      e instanceof NativeContentNotFoundError ||
      (e instanceof NativeClientError &&
        (e.kind === "forbidden" || e.kind === "unauthorized"))
    ) {
      setMessages([])
      messagesRef.current = []
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
    // A message still on its way when the scope moved is ambiguous: it may or
    // may not have landed. Its words go back into the box, and its request id
    // stays in `retry` / `invokeRequest`, so Send again is a replay the server
    // dedupes — never a second message.
    if (outgoing) unsend(outgoing.segments)
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
      // One lookup per OFFERED provider, concurrently: a holder the reader does
      // not own is discarded here as well as server-side. An ACP provider the
      // operator has not gated on is refused like an unknown name, so asking
      // about one would fail the whole replay.
      const sessions = (
        await Promise.all(
          offeredRef.current.map((provider) =>
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
        // The provider is passed rather than read back off `agentProvider`:
        // the set above has not rendered yet.
        await follow(
          live.runId,
          live.createdAt * 1000,
          live.activity,
          live.provider
        )
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
    // `offeredKey`: an ACP provider the server lists once connections load has
    // holders of its own to replay. The read is incremental, so the re-run
    // costs only the new provider's lookups.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope, loading, runtimeEnabled, offeredKey])
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
    if (!connections || providerChosen.current || agentBusy) return
    if (
      connections.connections.some(
        (row) => row.provider === agentProvider && row.state === "connected"
      )
    )
      return
    const connected = connections.connections.find(
      (row) => row.state === "connected"
    )
    if (connected) {
      setAgentProvider(connected.provider)
      setDesired(null)
    } else if (
      // A remembered ACP provider the server no longer offers: point back at
      // one it always does rather than at a provider every request refuses.
      !connections.connections.some((row) => row.provider === agentProvider)
    ) {
      setAgentProvider(NEEV_CORE_AGENT_PROVIDERS[0])
      setDesired(null)
    }
  }, [connections, agentProvider, agentBusy])
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
   *
   * The handover is the whole reason this is written the way it is. Clearing
   * the preview and THEN going to the network for the stored reply meant the
   * answer someone was mid-sentence in vanished for two round trips. So a
   * preview that ANSWERED is retired by `previewStored` below — the arrival of
   * its own stored row — and never by a timer or a completed read. Only a run
   * that published nothing is cleared here.
   */
  async function follow(
    runIdValue: string,
    startedAt: number,
    seed: readonly NeevActivityItem[],
    provider: string,
    // This run's own ask, when the caller has it. The thread high-water is the
    // same thing whenever the ask has been absorbed — but an unread reply from
    // an EARLIER run sorts below it, so reading the ask's sequence off the
    // invocation is what makes the floor right without depending on that.
    askSequence?: string
  ) {
    // Everything after an await, and every callback below, is guarded by this:
    // a handover outlived by the next run must not touch the next run's state.
    const mine = ++generation.current
    const current = () => scope.isCurrent() && generation.current === mine
    busy.current = true
    previewFloor.current = messagesRef.current.reduce(
      (high, item) =>
        BigInt(item.sequence) > high ? BigInt(item.sequence) : high,
      askSequence ? BigInt(askSequence) : 0n
    )
    previewProvider.current = provider
    setAgentBusy(true)
    setAgentRunId(runIdValue)
    setAgentText("")
    setRunStartedAt(startedAt)
    setRunFinishedAt(null)
    setRunCompleted(false)
    setLiveActivity(mergeActivity([], seed))
    setLiveInputs([])
    setSupportsSteering(false)
    followHistory.current = true
    let answered = false
    try {
      await followAgentRun({
        runId: runIdValue,
        signal: scope.signal,
        onPhase: (phase) => {
          if (current() && phase === "lost")
            setError(new NativeClientError("unavailable"))
        },
        onActivity: (item) => {
          if (!current()) return
          // `seq` is the identity: a rejoin replays the stored prefix, so this
          // upserts rather than appends and the trace cannot double.
          setLiveActivity((old) => mergeActivity(old, [item]))
        },
        onInput: (input) => {
          if (!current()) return
          // Shared follow-ups are durable human messages. Fetch that canonical
          // row instead of duplicating it inside the agent transcript.
          void refresh()
          // Upsert by requestId: a reattach replays persisted inputs.
          setLiveInputs((old) => {
            if (old.some((item) => item.requestId === input.requestId))
              return old
            return [...old, input].sort(
              (left, right) => left.responseOffset - right.responseOffset
            )
          })
          // Clear pending steering request when our input is acknowledged.
          setSteeringInFlight((pending) =>
            pending === input.requestId ? null : pending
          )
        },
        onEvent: (event) => {
          if (!current()) return
          if (event.type === "started") {
            setAgentText("")
            setLiveInputs([])
            // Set supportsSteering from the SSE event on both initial and reconnect.
            setSupportsSteering(event.supportsSteering)
          } else if (event.type === "chunk")
            setAgentText((current) => current + event.text)
          else {
            // How it ended, so the preview's trace line reads like the stored
            // one it is about to be swapped for rather than moving the text
            // under the reader by a line.
            answered = event.status === "completed"
            setRunCompleted(answered)
            // Run ended: clear steering state.
            setSteeringInFlight(null)
            setSupportsSteering(false)
            // A failure is NOT reported here as "service unavailable": the
            // turn is re-read below, and the failure card says what the
            // provider actually said, from the stored record.
          }
        },
      })
    } finally {
      // The clock stops and the composer reopens: the run IS over. The text
      // stays — it is still the only copy of the answer on screen. Guarded
      // too: `busy.current` is what tells `loadTraces` a run is in flight, and
      // clearing it on behalf of a run that has been superseded would let the
      // next poll rejoin the one that IS running a second time.
      if (current()) {
        busy.current = false
        setAgentBusy(false)
        setAgentRunId(null)
        setRunFinishedAt(Date.now())
      }
    }
    if (!current()) return
    await refresh()
    await loadTraces()
    // A run that published nothing has no stored row coming, so nothing will
    // ever retire its partial text: clear it here, beside the error saying
    // why. One that ANSWERED is left to `previewStored`.
    if (!answered && current()) clearPreview()
  }
  /**
   * Run the newest failed turn's own ask again. The server re-reads the prompt
   * and files from storage and posts nothing new to the thread, so the ask the
   * team already sees stays the only copy of it.
   */
  async function retryRun(turn: NeevAgentTurn) {
    if (!auth.session || agentBusy || retrying || !scope.isCurrent()) return
    const previous = retryRequest.current
    const request =
      previous?.runId === turn.runId
        ? previous
        : { runId: turn.runId, id: crypto.randomUUID() }
    retryRequest.current = request
    setRetrying(true)
    setError(null)
    // The note describes the last Auto decision; a retry re-runs the stored
    // selection and is not a new decision, so the note would mislead.
    setLastRoute(null)
    try {
      const started = await retryAgentRun(
        turn.runId,
        request.id,
        auth.session.csrf,
        auth.signal
      )
      if (!scope.isCurrent()) return
      retryRequest.current = null
      if (started.turn.status !== "running") {
        // Refused before it ran: the new turn carries the reason.
        keepTurn(started.turn)
        await loadTraces()
        return
      }
      resumed.current.add(started.runId)
      setRetrying(false)
      await follow(
        started.runId,
        Date.now(),
        started.turn.activity,
        started.turn.provider
      )
    } catch (cause) {
      if (scope.isCurrent()) fail(cause)
    } finally {
      if (scope.isCurrent()) setRetrying(false)
    }
  }
  /** Upsert one settled turn into the replayed set, by id. */
  function keepTurn(turn: NeevAgentTurn) {
    const next = [
      ...turnsRef.current.filter((item) => item.id !== turn.id),
      turn,
    ]
    turnsRef.current = next
    setTurns(next)
  }
  function clearPreview() {
    setAgentText("")
    setLiveActivity([])
    setLiveInputs([])
    setRunStartedAt(null)
    setRunFinishedAt(null)
    setRunCompleted(false)
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
    const { prompt, ids, mentions } = composeWirePrompt(
      written,
      new Map(attachments.items.map((item) => [item.id, item.name]))
    )
    // Read before the tray is cleared, because naming happens after the send
    // and by then there are no staged files left to name it from.
    const staged = attachments.items.map((item) => item.name)
    // The ask is posted as this person's own message in this shared thread, so
    // the people it names are named by them — the provider neither sees a
    // different prompt nor gains a way to notify anyone.
    const named = canMention ? mentions : []
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
      viewOnly ||
      attachments.uploading ||
      !auth.session
    )
      return
    if (checkingConnection.current) return
    checkingConnection.current = true
    showSending(written)
    try {
      const wiring = await refreshConnections()
      if (!scope.isCurrent()) return
      // With Auto on, any connected agent can take the ask: the decision may
      // choose one other than the selection, and it only chooses among the
      // caller's connected ones. With it off, the selection must be connected.
      if (
        !wiring.connections.some(
          (row) =>
            row.state === "connected" &&
            (row.provider === agentProvider || autoRouting.active)
        )
      ) {
        providerChosen.current = true
        setError(null)
        setConnectOpen(true)
        unsend(written)
        return
      }
    } catch (cause) {
      if (scope.isCurrent()) {
        unsend(written)
        fail(cause)
      }
      return
    } finally {
      checkingConnection.current = false
    }
    // The run reads the HOLDER, so an unstored choice would be ignored by the
    // very turn it was chosen for. Store it first, and if that fails do not
    // invoke: the prompt stays exactly where it was typed, with the reason
    // beside it, rather than running under a configuration nobody chose.
    if (desired && holder && !(await applySettings(desired))) {
      if (scope.isCurrent()) unsend(written)
      return
    }
    setAgentBusy(true)
    setAgentText("")
    setError(null)
    let durable = false
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
        previous &&
        previous.prompt === prompt &&
        sameKeys(previous.keys, keys) &&
        sameMentions(previous.mentions, named)
          ? previous
          : { id: crypto.randomUUID(), prompt, keys, mentions: named }
      invokeRequest.current = request
      const auto = autoRouting.active
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
          // Hung on the invoker's own message, the same way the files are.
          mentions: named,
          requestId: request.id,
          expectedAuthRevision: pinned.current,
          // The selection above is the fallback when Auto cannot decide.
          auto,
        },
        auth.session.csrf,
        auth.signal
      )
      if (!scope.isCurrent()) return
      // The ask is durable now: clear the composer and show it immediately.
      // The files go with it, and only now — the message the server returned is
      // what carries them from here on.
      invokeRequest.current = null
      durable = true
      settleAttachments(ids)
      retry.current = null
      absorb([invocation.message])
      setOutgoing(null)
      void nameFromFirstMessage(promptText(written), staged)
      // The agent that actually took the ask. With Auto that may not be the
      // selection — and a replay of an earlier Auto ask carries no route, so
      // its turn is what says where it ran.
      const route = invocation.route
      const turnProvider = invocation.turn.provider as NeevAgentProvider
      const ran: NeevAgentProvider = route
        ? route.provider
        : auto && NEEV_AGENT_PROVIDERS.includes(turnProvider)
          ? turnProvider
          : agentProvider
      // A note describes THIS ask's decision; an ask without one clears it
      // rather than leave an earlier decision standing as if it were this one.
      if (route) rememberRoute(latest.id, route)
      setLastRoute(route)
      if (ran !== agentProvider || route?.outcome === "routed") {
        // Show what ran, not the selection it replaced. A held choice belonged
        // to the selection, so it is dropped rather than laid over the routed
        // holder; the holder is read back for the settings it now stores.
        setAgentProvider(ran)
        setDesired(null)
        const owner = auth.session.user.id
        void loadTeamAgentSession(latest.id, ran, scope.signal)
          .then((fresh) => {
            if (!fresh || fresh.ownerId !== owner || !scope.isCurrent()) return
            setHolders((old) => [
              ...old.filter((item) => item.id !== fresh.id),
              fresh,
            ])
          })
          .catch(() => {
            // The read at the end of the run brings the holders back anyway.
          })
      }
      if (invocation.turn.status !== "running") {
        // Refused before it ever ran; the ask stays visible and the reason is
        // named — from the settled turn, by the failure card — rather than left
        // as a fake pending state or a generic "unavailable".
        setAgentBusy(false)
        keepTurn(invocation.turn)
        await refresh()
        await loadTraces()
        return
      }
      resumed.current.add(invocation.runId)
      await follow(
        invocation.runId,
        Date.now(),
        invocation.turn.activity,
        ran,
        invocation.message.sequence
      )
    } catch (cause) {
      // `busy.current` rather than `scope`: a throw from this ask's own
      // handover must not stand a later run down. The reason is still reported.
      if (scope.isCurrent()) {
        if (!busy.current) {
          setAgentBusy(false)
          setAgentRunId(null)
        }
        // Not posted: the ask goes back, and keeps its request id for Send.
        if (!durable) unsend(written)
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
  /**
   * Send a follow-up input into the active run via steering.
   *
   * The request is idempotent on requestId: on failure we keep the same id
   * so a retry cannot duplicate. Text is retained in the composer until
   * the server acknowledges receipt via the SSE input event.
   */
  async function steerAgent() {
    const runId = agentRunId
    if (!runId || !auth.session || !supportsSteering) return
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
      previous?.text === prompt && previous.runId === runId
        ? previous.id
        : crypto.randomUUID()
    steeringRetryId.current = { id: requestId, text: prompt, runId: runId }
    setSteeringInFlight(requestId)
    setError(null)
    try {
      await steerAgentRun(
        runId,
        prompt,
        requestId,
        auth.session.csrf,
        auth.signal
      )
      // Success: clear only the captured draft if the composer still holds it.
      // If the user has edited while waiting, preserve their new text.
      steeringRetryId.current = null
      setSteeringInFlight((pending) => (pending === requestId ? null : pending))
      clearSent(sentDraft)
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
  async function send() {
    // Empty body + files is a message the server accepts and labels; empty
    // body with nothing attached is not a message at all.
    const written = draft
    const {
      prompt: body,
      ids,
      mentions,
    } = composeWirePrompt(
      written,
      new Map(attachments.items.map((item) => [item.id, item.name]))
    )
    // Read before the tray is cleared, because naming happens after the send
    // and by then there are no staged files left to name it from.
    const staged = attachments.items.map((item) => item.name)
    // Belt and braces for the one thing that must never happen: a private
    // conversation cannot notify anybody, so a span that somehow survived a
    // visibility change under an open composer is dropped here rather than
    // sent. The server refuses it too.
    const named = canMention ? mentions : []
    if (
      (promptIsEmpty(written) && attachments.empty) ||
      sending.current ||
      latest.archived ||
      viewOnly ||
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
    showSending(written)
    let keys: string[]
    try {
      // Nothing is posted until every staged file has a key. A failure here
      // keeps the draft AND the files, so the next Send is the same message
      // again rather than one with its attachments quietly missing.
      keys = await attachments.takeKeys(undefined, ids)
    } catch (cause) {
      if (scope.isCurrent()) {
        unsend(written)
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
      mentions: named,
    }
    const previous = retry.current
    const input =
      previous &&
      previous.body === candidate.body &&
      previous.expectedAuthRevision === candidate.expectedAuthRevision &&
      // Same words but a different set of files is a different message, and
      // reusing the request id would make the server return the first one.
      sameKeys(previous.attachmentKeys, keys) &&
      // Same words naming a different coworker is a different message too:
      // two people can share a display name.
      sameMentions(previous.mentions, named)
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
      absorb([message])
      setOutgoing(null)
      // Leave the incremental cursor unchanged: another author's intervening messages may exist.
      settleAttachments(ids)
      retry.current = null
      pinned.current = latest.authRevision
      void nameFromFirstMessage(promptText(written), staged)
    } catch (e) {
      // Kept in `retry`, so Send on the restored words is the same request.
      if (scope.isCurrent()) unsend(written)
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
  /**
   * Show a message the moment it is sent, before the server has it.
   *
   * Called once the synchronous checks pass: the words leave the box and sit
   * at the end of the thread, marked as still going, so sending feels
   * instant. Nothing else changes — the request, its id and its files are the
   * same as before — and `unsend` puts it all back if it does not land.
   */
  function showSending(sent: readonly PromptSegment[]) {
    setOutgoing({
      segments: sent,
      names: new Map(attachments.items.map((item) => [item.id, item.name])),
      files: new Set(attachmentIds(sent)),
      after:
        messagesRef.current[messagesRef.current.length - 1]?.sequence ?? null,
    })
    // The reader just acted, so their message is brought into view; streaming
    // and other people's messages still only follow a reader at the bottom.
    followHistory.current = true
    clearSent(sent)
  }
  /**
   * A message that did not land goes back where it was written, ahead of
   * anything typed since — so neither is lost — with its files still staged.
   */
  function unsend(sent: readonly PromptSegment[]) {
    setOutgoing(null)
    // A file that is no longer staged cannot come back as a chip that would
    // send nothing; the words around it still do.
    const staged = new Set(stagedNow.current.map((item) => item.id))
    const back = sent.filter(
      (segment) => segment.kind !== "attachment" || staged.has(segment.id)
    )
    setDraft((current) => {
      const restored: PromptSegment[] = promptIsEmpty(current)
        ? [...back]
        : [...back, { kind: "text", text: "\n\n" }, ...current]
      draftRef.current = promptText(restored)
      return restored
    })
    onDirtyChange?.(true)
  }
  /** Only the files that went with the message leave the tray. */
  function settleAttachments(ids: readonly string[]) {
    for (const id of ids) attachments.remove(id)
  }
  /** Whichever way this composer is pointed. */
  function submit() {
    // When a run is active and supports steering, Enter sends into
    // the same run. Otherwise it starts a new turn or posts a message.
    if (mode === "agent" && agentBusy && supportsSteering) void steerAgent()
    else if (mode === "agent") void askAgent()
    else void send()
  }
  /**
   * A chat nobody named takes its name from what was said first.
   *
   * Once, and only while it still carries the placeholder: a name someone
   * chose is never overwritten. It runs after the message landed, so the auth
   * revision it uses is the current one, and it takes the send mutex so a
   * second message cannot race the rename into a conflict.
   *
   * `fresh` is the tab that was just created, but it is not the only chat that
   * has never been spoken in: one made yesterday and opened from the sidebar
   * today arrives with `fresh` false and an empty transcript, and used to stay
   * "New chat" forever. So an EMPTY thread arms it too. That is still not a
   * rename of an existing chat — the transcript is read before the composer
   * unlocks (`sendable` waits on `loading`), so an empty one here means empty
   * on the server, and the placeholder check above it means a chat someone
   * named by hand is left alone either way.
   */
  const pendingName = useRef(fresh)
  async function nameFromFirstMessage(body: string, files: readonly string[]) {
    if (!auth.session) return
    if (!pendingName.current && messages.length > 0) return
    // A chat already named by hand is never renamed by us.
    if (latest.title !== t("untitledChat")) {
      pendingName.current = false
      return
    }
    // What was WRITTEN names it — never the wire prompt, whose `[attachment
    // 1: …]` markers are addressing for the provider and read as noise in a
    // sidebar. A message that is only files takes the name of the first of
    // them, which is a real thing the person chose, and matches what a private
    // agent chat does with the same message.
    const derived = firstLineTitle(body) || firstLineTitle(files[0] ?? "")
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
    !(latest.archived || viewOnly)
  // A run that takes follow-ups is the one busy state Enter still submits in:
  // `submit` steers it. Held as busy, Enter fell through to the editor and a
  // follow-up became a new line instead of a message.
  const steerable = mode === "agent" && agentBusy && supportsSteering
  const enterBusy = steerable
    ? steeringInFlight !== null || attachments.uploading
    : composerBusy
  // The composer's one button is Stop for as long as a run has nothing typed
  // it could take. A follow-up waiting to go into the run makes it Send — but
  // only until it is sent: a follow-up in flight shows Stop again, so a steer
  // that never answers cannot leave a disabled Send where Stop should be.
  // Escape stops the run from the composer whichever it shows.
  const stopping =
    agentBusy &&
    (steeringInFlight !== null || !(steerable && !promptIsEmpty(draft)))
  // The newest invocation this reader owns in this chat, across providers. Its
  // failure — live or found on reopen — is shown with the provider's own words
  // and the action that helps; an older failure is history, not a prompt.
  const newestTurn = turns.reduce<NeevAgentTurn | null>(
    (newest, turn) =>
      !newest ||
      turn.createdAt > newest.createdAt ||
      (turn.createdAt === newest.createdAt &&
        turn.sessionId === newest.sessionId &&
        BigInt(turn.sequence) > BigInt(newest.sequence))
        ? turn
        : newest,
    null
  )
  const failure = newestTurn && !agentBusy ? runFailure(newestTurn) : null
  function reconnectedSince(turn: NeevAgentTurn) {
    const row = connections?.connections.find(
      (entry) => entry.provider === turn.provider
    )
    return (
      row?.state === "connected" &&
      row.checkedAt !== null &&
      row.checkedAt >= (turn.completedAt ?? turn.createdAt)
    )
  }
  return (
    <section className="flex min-h-0 flex-1 flex-col" aria-label={latest.title}>
      {/* ONE row, title and standing included. It was a title stacked over a
          metadata line, which cost two rows of height at the top of every
          conversation to say two short things. The conversation is the page. */}
      {/* A hairline under the header: the transcript scrolls and this does not,
          so without an edge a message scrolling up vanished into the title. */}
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-border/60 px-4 sm:gap-3 sm:px-8">
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
            {viewOnly ? ` · ${t("viewOnly")}` : ""}
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
                  actions.visibility !== "organization"
                    ? "shareShort"
                    : "makePrivate"
                )}
                title={t(
                  actions.visibility !== "organization"
                    ? "shareShort"
                    : "makePrivate"
                )}
                aria-describedby={
                  actions.visibility === "organization"
                    ? "neev-make-private-warning"
                    : undefined
                }
                onClick={
                  actions.visibility !== "organization"
                    ? actions.onShare
                    : actions.onMakePrivate
                }
              >
                {actions.visibility !== "organization" ? (
                  <Users aria-hidden />
                ) : (
                  <Lock aria-hidden />
                )}
                <span className="hidden sm:inline">
                  {t(
                    actions.visibility !== "organization"
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
                  {t(actions.archived ? "restoreFromTrash" : "moveToTrash")}
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
        // Asymmetric on purpose: the top stays tight against the header, and
        // the last message gets room to breathe before the composer rather
        // than ending against its top edge.
        className="min-h-0 flex-1 space-y-6 overflow-y-auto overscroll-contain px-4 pb-14 pt-5 text-sm sm:px-8"
      >
        {/* Says what is actually being waited for. The conversation's own
            history, not "your workspace" — the workspace is already here. */}
        {loading && (
          <NativeSkeletonRows
            as="li"
            label={t("loadingConversation")}
            className="mx-auto w-full max-w-3xl"
          />
        )}
        {shownMessages.map((message) => (
          <li
            key={message.id}
            className="mx-auto flex w-full max-w-3xl gap-2.5"
          >
            <AuthorMark
              agent={message.authorKind === "agent"}
              initial={message.authorName.slice(0, 1)}
            />
            {/* A person's words get a quiet tinted surface; an agent's are left
                on the page, since boxing the prose that fills a thread would
                separate nothing. Left-aligned for everyone including the reader:
                right-alignment would mean "me", and a shared transcript may not
                be mirrored per reader. */}
            <div
              className={cn(
                "group/message min-w-0 flex-1 space-y-1",
                message.authorKind === "human" && CHAT_USER_SURFACE
              )}
            >
              {/* An agent reply names the provider that wrote it and the
                  human who invoked it. `authorId` is still the invoker, so the
                  agent never impersonates a teammate. */}
              <MessageMeta
                author={
                  message.authorKind === "agent"
                    ? message.agentLabel
                    : message.authorName
                }
                invoker={
                  message.authorKind === "agent" ? message.authorName : null
                }
                at={message.createdAt * 1000}
              />
              {/* A message written here keeps its files where they were put
                  and its chips where the people were named; anything else — an
                  agent's reply, a message from before either existed — is the
                  markdown it always was.

                  A named message is drawn as the text that was typed, not as
                  markdown, for the same reason one with inline files is: both
                  are one sentence with things sitting inside it, and block
                  rendering would take that apart. */}
              {message.authorKind === "human" &&
              message.attachments.length > 0 ? (
                <SentPrompt
                  body={message.body}
                  attachments={message.attachments}
                  mentions={message.mentions}
                />
              ) : message.mentions?.length ? (
                <p className="whitespace-pre-wrap break-words">
                  <MentionedText text={message.body} spans={message.mentions} />
                </p>
              ) : traces.has(message.id) ? (
                /* The stored trace, replayed in place: the reply cut at the
                   offsets the server recorded, with a row where each tool
                   actually ran. Only ever from this reader's own sessions, and
                   only on a reply they invoked.

                   Reached only on the plain-markdown branch, which is where
                   agent replies retain their formatted prose and tools. Generated
                   attachments are rendered separately below the reply. */
                <TranscriptBody
                  text={message.body}
                  items={traces.get(message.id)!.activity}
                />
              ) : (
                <NativeMarkdown
                  markdown={message.body}
                  copy={message.authorKind === "agent" ? "code" : "none"}
                />
              )}
              {message.authorKind === "agent" && (
                <SentAttachments attachments={message.attachments} />
              )}
              {/* A stored agent reply is complete by construction: Copy (its
                  Markdown, never the trace rows) and Fork to new tab. */}
              {message.authorKind === "agent" && (
                <ReplyActions
                  markdown={() => messageSource(message.body)}
                  forkKey={message.id}
                  onFork={() => forkReply(message.id)}
                  onForkInto={(nodeId) => forkReply(message.id, nodeId)}
                  forking={forkAction.forking}
                  outcome={forkAction.outcome}
                />
              )}
              {/* Below the reply, where the live footer is: a run that put its
                  progress above the answer moved every word of that answer up
                  the screen the moment it finished. The trace itself is NOT
                  repeated here — it is above, interleaved with the prose. */}
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
            </div>
          </li>
        ))}
        {/* The message on its way, drawn like the stored one it becomes. Its
            state is the muted tone and `aria-busy`, not a spinner: a running
            agent already has its own progress line, and one signal is enough.
            It is replaced only by the server's answer to this request — never
            by a message that merely looks the same. */}
        {outgoing && (
          <li
            aria-busy="true"
            className="mx-auto flex w-full max-w-3xl gap-2.5 opacity-70"
          >
            <AuthorMark
              agent={false}
              initial={(auth.session?.user.displayName ?? "").slice(0, 1)}
            />
            <div className={cn("min-w-0 flex-1 space-y-1", CHAT_USER_SURFACE)}>
              <MessageMeta
                author={auth.session?.user.displayName ?? null}
                invoker={null}
                at={null}
              />
              <OutgoingPrompt
                segments={outgoing.segments}
                names={outgoing.names}
              />
            </div>
          </li>
        )}
        {(agentBusy || agentText) && !previewStored && (
          <li className="mx-auto flex w-full max-w-3xl gap-2.5">
            <AuthorMark agent initial="" />
            <div className="min-w-0 flex-1 space-y-1">
              {/* The same component the stored row draws: a header that differs
                  by a line moves the answer down the screen when a run ends. */}
              <MessageMeta
                author={agentProvider}
                invoker={auth.session?.user.displayName ?? ""}
                at={runStartedAt}
              />
              {/* The reply and the work that produced it, interleaved by the
                  offsets the server recorded. "Still working" is said once, by
                  the footer below — a second sr-only "Working…" here was the
                  same fact announced twice for one run.

                  Rendered as soon as there is EITHER text or activity: a run
                  that reaches for a tool before writing a word has something
                  real to show, and used to show nothing until the first
                  token. */}
              <div aria-live="polite">
                {(agentText ||
                  liveActivity.length > 0 ||
                  liveInputs.length > 0) && (
                  <TranscriptBody
                    text={agentText}
                    items={liveActivity}
                    streaming
                  />
                )}
              </div>
              {/* Progress sits AFTER the newest output, near the composer,
                  rather than above a reply that grows and scrolls it off the
                  screen. One spinner for the run, and it stays outside the live
                  region: the clock on this line changes ten times a second, and
                  inside one it would be announced that often. The trace is not
                  repeated here — it is above, in place. */}
              <div className="pt-3">
                <ActivityTimeline
                  items={liveActivity}
                  running={agentBusy}
                  startedAt={runStartedAt}
                  finishedAt={runFinishedAt}
                  completed={runCompleted}
                />
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
        onKeyDown={(event) => {
          // Stop is always one key away, even while the corner shows Send.
          // Not when something inside claimed Escape first (the mention
          // picker), and not from a portalled menu opened from this row,
          // whose Escape only closes that menu.
          if (
            event.key !== "Escape" ||
            !agentBusy ||
            event.defaultPrevented ||
            !event.currentTarget.contains(event.target as Node)
          )
            return
          event.preventDefault()
          void cancelAgent()
        }}
        className="mx-auto w-full max-w-[52rem] shrink-0 px-4 pb-3 pt-1 sm:px-8 sm:pb-5"
      >
        <div
          {...drop.handlers}
          className={cn(
            // One geometry for both chat surfaces, and a focus accent in the
            // same hue as the agent mark: a soft halo, not an outline, because
            // this box is on screen at all times.
            CHAT_COMPOSER,
            drop.over && CHAT_COMPOSER_DROP
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
            retain={outgoing?.files}
            onChange={(next) => {
              // An editor that re-reports what it already holds — remounting
              // after a hidden tab is shown again does this — is not an edit,
              // and must not re-mint the request id of a send in flight.
              if (sameSegments(next, draft)) return
              if (!draftRef.current) pinned.current = latest.authRevision
              draftRef.current = promptText(next)
              setDraft(next)
              retry.current = null
              onDirtyChange?.(
                !promptIsEmpty(next) || !attachments.empty || outgoing !== null
              )
            }}
            tray={attachments}
            label={t("message")}
            placeholder={
              mode === "agent" && agentBusy && supportsSteering
                ? t("agentFollowUp")
                : mode === "agent"
                  ? t("agentPromptPlaceholder")
                  : t("message")
            }
            onSubmit={submit}
            busy={enterBusy}
            readOnly={latest.archived || viewOnly}
            attachDisabled={latest.archived || viewOnly || !auth.session}
            autoFocus={fresh}
            // No source at all in a private chat, so `@` stays a character
            // there. The reader themself is never offered: you do not need to
            // notify yourself, and the server drops a self-mention anyway.
            mentions={
              canMention
                ? {
                    candidates: (query) =>
                      matchCandidates(
                        directory,
                        query,
                        auth.session?.user.id ?? null
                      ),
                    onOpen: () => void loadDirectory(),
                  }
                : undefined
            }
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
          {agentBusy && !attachments.empty && (
            <p role="status" className="text-xs text-muted-foreground">
              {t("agentFollowUpTextOnly")}
            </p>
          )}
          {failure && newestTurn && (
            <NativeRunFailure
              failure={failure}
              provider={providerName(newestTurn.provider)}
              retrying={retrying}
              reconnected={reconnectedSince(newestTurn)}
              onRetry={() => void retryRun(newestTurn)}
              onReconnect={() => {
                if (
                  NEEV_AGENT_PROVIDERS.includes(
                    newestTurn.provider as NeevAgentProvider
                  )
                )
                  setAgentProvider(newestTurn.provider as NeevAgentProvider)
                setConnectOpen(true)
              }}
            />
          )}
          <NativeError error={error} />
          {mode === "agent" && lastRoute && (
            <RouteNote route={lastRoute} className="px-1" />
          )}
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
              disabled={latest.archived || viewOnly || !auth.session}
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
                  disabled={
                    agentBusy || applying || latest.archived || viewOnly
                  }
                  auto={{
                    status: autoRouting.status,
                    on: autoRouting.on,
                    onChange: autoRouting.setOn,
                    disabled: agentBusy || latest.archived || viewOnly,
                  }}
                  onChange={(next, nextProvider) => {
                    providerChosen.current = true
                    const target = connections?.connections.find(
                      (row) => row.provider === nextProvider
                    )
                    if (target && target.state !== "connected") {
                      setError(null)
                      setConnectOpen(true)
                    }
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
                {/* Connect in place, preserving the message and staged files. */}
                {runtimeEnabled &&
                  connection &&
                  connection.state !== "connected" && (
                    <>
                      <p id={connectNote} className="sr-only">
                        {t("providerNotConnected", { name: agentProvider })}
                      </p>
                      <Button
                        size="sm"
                        variant="outline"
                        type="button"
                        className="h-7 shrink-0 gap-1.5 rounded-full px-2.5 text-xs font-normal"
                        aria-describedby={connectNote}
                        onClick={() => {
                          setError(null)
                          setConnectOpen(true)
                        }}
                      >
                        <PlugZap aria-hidden className="size-3.5" />
                        {connection.state === "reconnect_required"
                          ? t("reconnectProvider")
                          : t("connectProvider")}
                      </Button>
                    </>
                  )}
              </>
            )}
            {/* ONE control, always in the same corner. A run shows Stop; typing
                a follow-up the run can take turns it into Send, and clearing
                the text turns it back. */}
            {agentBusy && (
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
              aria-keyshortcuts={agentBusy ? "Escape" : undefined}
              aria-describedby={agentBusy && !stopping ? stopHint : undefined}
              title={
                stopping
                  ? t("stopGeneration")
                  : agentBusy
                    ? `${t("send")} · ${t("stopShortcutHint")}`
                    : t("send")
              }
              disabled={
                stopping
                  ? false
                  : agentBusy && mode === "agent"
                    ? steeringInFlight !== null || !attachments.empty
                    : loading ||
                      !sendable ||
                      (mode === "agent" && !runtimeEnabled)
              }
              onClick={stopping ? () => void cancelAgent() : undefined}
            >
              {stopping ? (
                <Square aria-hidden className="size-3.5 fill-current" />
              ) : (
                <ArrowUp aria-hidden />
              )}
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
      <Dialog
        open={connectOpen}
        onOpenChange={(open) => {
          setConnectOpen(open)
          if (!open) void refreshConnections().catch(() => {})
        }}
      >
        <DialogContent className="max-h-[85dvh] overflow-y-auto">
          <DialogTitle>
            {t("connectProvider")} · {t(`${agentProvider}Label`)}
          </DialogTitle>
          <DialogDescription>
            {t("providerNotConnected", { name: agentProvider })}
          </DialogDescription>
          {connectOpen && (
            <NativeAgentConnectionsPanel
              providerFilter={agentProvider}
              showSettingsLink={false}
              onRefresh={() => {
                void refreshConnections().catch(() => {})
              }}
            />
          )}
        </DialogContent>
      </Dialog>
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
