"use client"

import { useEffect, useRef, useState } from "react"
import Link from "next/link"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog"
import { NativeClientError } from "@/lib/neev-client"
import {
  appendNativeMessage,
  listNativeMessages,
  loadNativeResource,
  NativeContentNotFoundError,
  type NativeResource,
  type NativeMessage,
  type AppendNativeMessageInput,
} from "@/lib/neev-content-client"
import { useNeevContentPoll } from "@/hooks/use-neev-content-poll"
import { useNativeAuth } from "./native-auth-provider"
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
    [review, setReview] = useState(false)
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
    auth.handleError(e)
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
        setMessages((old) => [
          ...new Map(
            [...old, ...page.items].map((item) => [item.id, item])
          ).values(),
        ])
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
    void refresh() /* The request lifetime owns the initial read. */
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope])
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
      setMessages((old) => [
        ...new Map([...old, message].map((item) => [item.id, item])).values(),
      ])
      // Leave the incremental cursor unchanged: another author's intervening messages may exist.
      setDraft("")
      draftRef.current = ""
      retry.current = null
      pinned.current = latest.authRevision
      onDirtyChange?.(false)
    } catch (e) {
      fail(e)
    } finally {
      sending.current = false
      if (scope.isCurrent()) setSaving(false)
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
    <section className="space-y-5" aria-label={latest.title}>
      <Button variant="ghost" onClick={onBack}>
        {t("back")}
      </Button>
      <div className="space-y-1">
        <h2 className="break-words text-2xl font-semibold tracking-tight">
          {latest.title}
        </h2>
        <p className="text-xs text-muted-foreground">
          {t(latest.visibility)}
          {latest.archived ? ` · ${t("archived")}` : ""}
        </p>
      </div>
      {loading ? (
        <p role="status">{n("loading")}</p>
      ) : (
        <ol aria-label={t("messages")} className="space-y-5">
          {messages.map((message) => (
            <li key={message.id} className="space-y-1 border-b pb-4">
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
              <p className="whitespace-pre-wrap break-words text-sm leading-6">
                {message.body}
              </p>
            </li>
          ))}
        </ol>
      )}
      <form
        onSubmit={(event) => {
          event.preventDefault()
          void send()
        }}
        className="space-y-3"
      >
        <label className="block space-y-2">
          <span className="text-sm">{t("message")}</span>
          <textarea
            value={draft}
            disabled={loading || saving || latest.archived}
            onChange={(event) => {
              if (!draftRef.current) pinned.current = latest.authRevision
              draftRef.current = event.target.value
              setDraft(event.target.value)
              retry.current = null
              onDirtyChange?.(!!event.target.value)
            }}
            className="min-h-24 w-full resize-y rounded-lg border bg-background p-3 text-base sm:text-sm"
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
        <div className="flex flex-wrap items-center gap-3">
          <Button
            type="submit"
            disabled={loading || saving || latest.archived || !draft.trim()}
          >
            {saving ? n("working") : t("send")}
          </Button>
          <Button type="button" variant="outline" disabled>
            {t("askAgent")}
          </Button>
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
        <p className="text-xs text-muted-foreground">{n("agentDescription")}</p>
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
