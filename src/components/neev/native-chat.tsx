"use client"

import {
  ArrowLeft,
  ArrowUp,
  Lock,
  MoreHorizontal,
  PanelRight,
  Sparkles,
  Users,
  X,
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
import { NativeClientError } from "@/lib/neev-client"
import type { ResourceActions } from "./native-document"
import {
  appendNativeMessage,
  listNativeMessages,
  loadNativeResource,
  NativeContentNotFoundError,
  type NativeResource,
  type NativeMessage,
  type AppendNativeMessageInput,
} from "@/lib/neev-content-client"
import {
  cancelAgentRun,
  followAgentRun,
  invokeTeamAgent,
  NEEV_AGENT_PROVIDERS,
  NEEV_DEFAULT_MODEL,
  type NeevAgentProvider,
} from "@/lib/neev-agent-sessions-client"
import { useNeevContentPoll } from "@/hooks/use-neev-content-poll"
import { useNeevMessageStream } from "@/hooks/use-neev-message-stream"
import { useNativeAuth } from "./native-auth-provider"
import {
  NativeDialogContent as DialogContent,
  NativeDropdownMenuContent as DropdownMenuContent,
} from "./native-suspended-surface"
import { NativeError } from "./native-boundary"
import {
  getRightPanelOpen,
  setRightPanelOpen,
} from "@/lib/neev-workspace-prefs"

type Props = {
  resource: NativeResource
  onBack: () => void
  onChanged: (resource: NativeResource) => void
  onDirtyChange?: (dirty: boolean) => void
  onRequestSettings?: () => void
  actions?: ResourceActions
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
    [agentRunId, setAgentRunId] = useState<string | null>(null)
  // The agent lives in a collapsible right context panel so the human team
  // thread is the primary surface (view-only pref; SSR default then read on mount).
  const [agentPanel, setAgentPanel] = useState(false)
  useEffect(() => {
    setAgentPanel(getRightPanelOpen())
  }, [])
  function toggleAgentPanel() {
    setAgentPanel((open) => {
      const next = !open
      setRightPanelOpen(next)
      return next
    })
  }
  const runtimeEnabled = auth.capabilities?.runtimeEnabled ?? false
  // Absent capability reads as "no push": the client then polls and says so.
  const pushAdvertised = auth.capabilities?.realtime ?? false
  const historyElement = useRef<HTMLOListElement>(null)
  const followHistory = useRef(true)
  useLayoutEffect(() => {
    const history = historyElement.current
    if (history && followHistory.current)
      history.scrollTop = history.scrollHeight
  }, [messages, loading])
  const pinned = useRef(resource.authRevision),
    draftRef = useRef(""),
    sequence = useRef<string | null>(null),
    retry = useRef<AppendNativeMessageInput | null>(null),
    sending = useRef(false),
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
    setSaving(false)
    void refresh()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope])
  // Explicit team invocation (spec §5.3). The prompt is posted as an ordinary
  // human message by the server, the reply is written by the run driver as an
  // agent-authored message, and this view re-reads the thread once the run
  // settles — so what everyone sees is what was actually persisted. No private
  // Agent Chat history travels with the prompt; only this prompt does.
  async function askAgent() {
    if (
      !runtimeEnabled ||
      !draft.trim() ||
      agentBusy ||
      latest.archived ||
      !auth.session ||
      !scope.isCurrent()
    )
      return
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
      setDraft("")
      draftRef.current = ""
      retry.current = null
      onDirtyChange?.(false)
      followHistory.current = true
      absorb([invocation.message])
      if (invocation.turn.status !== "running") {
        // Refused before it ever ran; the ask stays visible and the reason is
        // named rather than left as a fake pending state.
        setAgentBusy(false)
        setError(new NativeClientError("unavailable"))
        await refresh()
        return
      }
      setAgentRunId(invocation.runId)
      await followAgentRun({
        runId: invocation.runId,
        signal: scope.signal,
        onPhase: () => {},
        onEvent: (event) => {
          if (!scope.isCurrent()) return
          if (event.type === "started") setAgentText("")
          else if (event.type === "chunk")
            setAgentText((current) => current + event.text)
          else if (event.status !== "completed")
            setError(new NativeClientError("unavailable"))
        },
      })
      if (!scope.isCurrent()) return
      // The reply the team keeps is the persisted one, not this preview.
      setAgentText("")
      await refresh()
    } catch (cause) {
      if (scope.isCurrent()) fail(cause)
    } finally {
      if (scope.isCurrent()) {
        setAgentBusy(false)
        setAgentRunId(null)
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
      setDraft("")
      draftRef.current = ""
      retry.current = null
      pinned.current = latest.authRevision
      onDirtyChange?.(false)
    } catch (e) {
      fail(e)
    } finally {
      if (scope.isCurrent()) {
        sending.current = false
        setSaving(false)
      }
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
  return (
    <section className="flex min-h-0 flex-1 flex-col" aria-label={latest.title}>
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
            title={latest.title}
          >
            {latest.title}
          </h2>
          {/* One meta line, not three stacked ones: on a short viewport the
              composer has to stay above the fold. */}
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
          <Button
            variant="ghost"
            size="icon"
            aria-label={t("agentPanel")}
            title={t("agentPanel")}
            aria-pressed={agentPanel}
            onClick={toggleAgentPanel}
          >
            <PanelRight aria-hidden />
          </Button>
        </div>
      </div>
      <div className="flex min-h-0 flex-1">
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          {loading ? (
            <p role="status">{n("loading")}</p>
          ) : (
            <ol
              ref={historyElement}
              onScroll={(event) => {
                const history = event.currentTarget
                followHistory.current =
                  history.scrollHeight -
                    history.scrollTop -
                    history.clientHeight <
                  48
              }}
              aria-label={t("messages")}
              className="min-h-0 flex-1 space-y-8 overflow-y-auto overscroll-contain px-4 py-4 sm:px-8 sm:py-6"
            >
              {messages.map((message) => (
                <li
                  key={message.id}
                  className="mx-auto flex w-full max-w-3xl gap-3"
                >
                  <span
                    aria-hidden
                    className="flex size-8 shrink-0 items-center justify-center rounded-md bg-muted text-xs font-medium"
                  >
                    {message.authorKind === "agent" ? (
                      <Sparkles className="size-4" />
                    ) : (
                      message.authorName.slice(0, 1)
                    )}
                  </span>
                  <div className="min-w-0 flex-1 space-y-2">
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
                        dateTime={new Date(
                          message.createdAt * 1000
                        ).toISOString()}
                      >
                        {new Date(message.createdAt * 1000).toLocaleString()}
                      </time>
                    </div>
                    <NativeMarkdown markdown={message.body} />
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
                <span className="sr-only">{t("message")}</span>
                <textarea
                  placeholder={t("message")}
                  value={draft}
                  disabled={loading || saving || latest.archived}
                  onChange={(event) => {
                    if (!draftRef.current) pinned.current = latest.authRevision
                    draftRef.current = event.target.value
                    setDraft(event.target.value)
                    retry.current = null
                    onDirtyChange?.(!!event.target.value)
                  }}
                  className="h-20 max-h-40 min-h-12 w-full resize-y border-0 bg-transparent px-1 py-2 text-base outline-none sm:text-sm"
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
              <div className="flex items-center">
                <Button
                  type="submit"
                  size="icon"
                  className="ms-auto rounded-md"
                  aria-label={saving ? n("working") : t("send")}
                  title={saving ? n("working") : t("send")}
                  disabled={
                    loading || saving || latest.archived || !draft.trim()
                  }
                >
                  <ArrowUp aria-hidden />
                </Button>
              </div>
            </div>
          </form>
        </div>
        {agentPanel && (
          <aside
            aria-label={t("askAgent")}
            className="flex w-full max-w-sm shrink-0 flex-col gap-3 overflow-y-auto border-s bg-sidebar/30 p-4 sm:w-80"
          >
            <div className="flex items-center justify-between">
              <h3 className="text-sm font-semibold">{t("askAgent")}</h3>
              <Button
                variant="ghost"
                size="icon"
                aria-label={n("close")}
                title={n("close")}
                onClick={toggleAgentPanel}
              >
                <X aria-hidden />
              </Button>
            </div>
            {!runtimeEnabled && (
              <p role="status" className="text-sm text-muted-foreground">
                {t("agentsOff")}
              </p>
            )}
            {/* Both the ask and the reply become team-visible messages, so the
                panel must say so unmistakably before send (spec §5.3). */}
            <p className="text-xs text-muted-foreground">
              {t("agentPostsToTeam")}
            </p>
            <label className="block space-y-1">
              <span className="text-xs font-medium">{t("agentProvider")}</span>
              <select
                value={agentProvider}
                disabled={agentBusy}
                onChange={(event) =>
                  setAgentProvider(event.target.value as NeevAgentProvider)
                }
                className="w-full rounded-md border bg-background px-2 py-1.5 text-sm"
              >
                {NEEV_AGENT_PROVIDERS.map((name) => (
                  <option key={name} value={name}>
                    {name}
                  </option>
                ))}
              </select>
            </label>
            {(agentBusy || agentText) && (
              <div
                role="status"
                aria-live="polite"
                className="rounded-md border bg-muted/40 px-3 py-2 text-sm"
              >
                {agentText ? (
                  <NativeMarkdown markdown={agentText} />
                ) : (
                  <span className="text-muted-foreground">{n("working")}</span>
                )}
              </div>
            )}
            <div className="mt-auto flex flex-wrap items-center gap-2">
              <Button
                type="button"
                variant="outline"
                onClick={() => void askAgent()}
                disabled={
                  !runtimeEnabled ||
                  loading ||
                  saving ||
                  latest.archived ||
                  !draft.trim() ||
                  agentBusy
                }
              >
                <Sparkles aria-hidden />
                {t("askAgent")}
              </Button>
              {agentBusy && (
                <Button
                  type="button"
                  variant="ghost"
                  onClick={() => void cancelAgent()}
                >
                  {n("cancel")}
                </Button>
              )}
            </div>
            <Link
              href="/settings"
              onClick={(event) => {
                if (onRequestSettings) {
                  event.preventDefault()
                  onRequestSettings()
                }
              }}
              className="rounded text-xs underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
            >
              {n("personalAgents")}
            </Link>
          </aside>
        )}
      </div>
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
