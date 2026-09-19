"use client"

import { ArrowLeft, ArrowUp, Sparkles } from "lucide-react"
import { NativeMarkdown } from "./native-markdown"

import { useEffect, useLayoutEffect, useRef, useState } from "react"
import Link from "next/link"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { Dialog, DialogTitle, DialogDescription } from "@/components/ui/dialog"
import { NativeClientError } from "@/lib/neev-client"
import {
  appendNativeMessage,
  listNativeMessages,
  loadNativeResource,
  runNativeAgent,
  cancelNativeAgent,
  NativeContentNotFoundError,
  type NativeResource,
  type NativeMessage,
  type AppendNativeMessageInput,
} from "@/lib/neev-content-client"
import { useNeevContentPoll } from "@/hooks/use-neev-content-poll"
import { useNativeAuth } from "./native-auth-provider"
import { NativeDialogContent as DialogContent } from "./native-suspended-surface"
import { NativeError } from "./native-boundary"

type Props = {
  resource: NativeResource
  onBack: () => void
  onChanged: (resource: NativeResource) => void
  onDirtyChange?: (dirty: boolean) => void
  onRequestSettings?: () => void
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
    [agentRunId, setAgentRunId] = useState<string | null>(null)
  const runtimeEnabled = auth.capabilities?.runtimeEnabled ?? false
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
    sending = useRef(false)
  const scope = useNeevContentPoll({
    signal: auth.signal,
    scopeKey: auth.scopeKey,
    resourceScope: resource.id,
    enabled: !revoked && !loading,
    onPoll: refresh,
  })
  useEffect(() => {
    setLatest(resource)
    if (!draftRef.current && !retry.current)
      pinned.current = resource.authRevision
  }, [resource])
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
        setMessages((old) =>
          [
            ...new Map(
              [...old, ...page.items].map((item) => [item.id, item])
            ).values(),
          ].sort((a, b) =>
            BigInt(a.sequence) < BigInt(b.sequence)
              ? -1
              : BigInt(a.sequence) > BigInt(b.sequence)
                ? 1
                : 0
          )
        )
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
  async function askAgent() {
    if (
      !runtimeEnabled ||
      !draft.trim() ||
      agentBusy ||
      latest.archived ||
      !auth.session
    )
      return
    const prompt = draft
    setAgentBusy(true)
    setAgentText("")
    setError(null)
    try {
      await runNativeAgent(
        { chatId: latest.id, prompt, provider: "claude", model: "default" },
        auth.session.csrf,
        (event) => {
          if (!scope.isCurrent()) return
          if (event.type === "started") setAgentRunId(event.runId)
          else if (event.type === "chunk")
            setAgentText((current) => current + event.text)
          else if (event.type === "failed")
            setError(new NativeClientError("unavailable"))
        },
        auth.signal
      )
    } catch (cause) {
      if (scope.isCurrent()) setError(cause)
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
      await cancelNativeAgent(runId, auth.session.csrf, auth.signal)
    } catch {
      // Best-effort: the run stream ends with a failed(canceled) event.
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
      setMessages((old) =>
        [
          ...new Map([...old, message].map((item) => [item.id, item])).values(),
        ].sort((a, b) =>
          BigInt(a.sequence) < BigInt(b.sequence)
            ? -1
            : BigInt(a.sequence) > BigInt(b.sequence)
              ? 1
              : 0
        )
      )
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
          <h2 className="break-words text-lg font-semibold">{latest.title}</h2>
          <p className="text-xs text-muted-foreground">
            {t(latest.visibility)}
            {latest.archived ? ` · ${t("archived")}` : ""}
          </p>
        </div>
      </div>
      {loading ? (
        <p role="status">{n("loading")}</p>
      ) : (
        <ol
          ref={historyElement}
          onScroll={(event) => {
            const history = event.currentTarget
            followHistory.current =
              history.scrollHeight - history.scrollTop - history.clientHeight <
              48
          }}
          aria-label={t("messages")}
          className="min-h-0 flex-1 space-y-8 overflow-y-auto overscroll-contain px-5 py-6 sm:px-8"
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
                {message.authorName.slice(0, 1)}
              </span>
              <div className="min-w-0 flex-1 space-y-2">
                <div className="flex flex-wrap items-baseline gap-2 text-xs text-muted-foreground">
                  <span className="font-medium text-foreground">
                    {message.authorName}
                  </span>
                  <time
                    dateTime={new Date(message.createdAt * 1000).toISOString()}
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
        className="mx-auto w-full max-w-[832px] shrink-0 space-y-3 px-5 pb-5 pt-3 sm:px-8"
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
          {(agentBusy || agentText) && (
            <div
              role="status"
              aria-live="polite"
              aria-label={t("askAgent")}
              className="rounded-md border bg-muted/40 px-3 py-2 text-sm"
            >
              {agentText ? (
                <NativeMarkdown markdown={agentText} />
              ) : (
                <span className="text-muted-foreground">{n("working")}</span>
              )}
            </div>
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
          <div className="flex flex-wrap items-center gap-3">
            <Button
              type="submit"
              size="icon"
              className="order-last ms-auto rounded-md"
              aria-label={saving ? n("working") : t("send")}
              title={saving ? n("working") : t("send")}
              disabled={loading || saving || latest.archived || !draft.trim()}
            >
              <ArrowUp aria-hidden />
            </Button>
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
          </div>
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
