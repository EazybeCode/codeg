"use client"

import { useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { Dialog, DialogTitle, DialogDescription } from "@/components/ui/dialog"
import { NativeClientError } from "@/lib/neev-client"
import {
  loadNativeResource,
  loadNativeDocumentContent,
  listNativeDocumentRevisions,
  saveNativeDocument,
  restoreNativeDocument,
  NativeContentNotFoundError,
  type NativeResource,
  type NativeRevision,
  type NativeDocumentContent,
  type SaveNativeDocumentInput,
  type RestoreNativeDocumentInput,
} from "@/lib/neev-content-client"
import { useNeevContentPoll } from "@/hooks/use-neev-content-poll"
import { useNativeAuth } from "./native-auth-provider"
import { NativeDialogContent as DialogContent } from "./native-suspended-surface"
import { NativeError } from "./native-boundary"
import { NativeMarkdown } from "./native-markdown"

type Props = {
  resource: NativeResource
  onBack: () => void
  onChanged: (resource: NativeResource) => void
  onDirtyChange: (dirty: boolean) => void
}
type Comparison = {
  resource: NativeResource
  markdown: string
  revision: string
  history: boolean
}
function DocumentError({ error }: { error: unknown }) {
  const t = useTranslations("NativeContent")
  return error instanceof NativeClientError && error.kind === "conflict" ? (
    <p role="alert" className="text-sm text-destructive">
      {t("documentConflict")}
    </p>
  ) : (
    <NativeError error={error} />
  )
}
export function NativeDocument(props: Props) {
  const auth = useNativeAuth()
  if (
    !auth.session?.organization ||
    props.resource.orgId !== auth.session.organization.id
  )
    return null
  return <Document key={`${auth.scopeKey}:${props.resource.id}`} {...props} />
}
function Document({ resource, onBack, onChanged, onDirtyChange }: Props) {
  const auth = useNativeAuth(),
    t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  const [latest, setLatest] = useState(resource),
    [draft, setDraft] = useState(""),
    [loading, setLoading] = useState(true),
    [loaded, setLoaded] = useState(false),
    [saving, setSaving] = useState(false),
    [error, setError] = useState<unknown>(null),
    [conflict, setConflict] = useState(false),
    [revoked, setRevoked] = useState(false),
    [saved, setSaved] = useState(false),
    [history, setHistory] = useState(false),
    [revisions, setRevisions] = useState<NativeRevision[]>([]),
    [nextRevision, setNextRevision] = useState<string | null>(null),
    [comparison, setComparison] = useState<Comparison | null>(null),
    [reload, setReload] = useState(false),
    [busy, setBusy] = useState(false)
  const base = useRef({
      markdown: "",
      content: resource.contentRevision,
      auth: resource.authRevision,
    }),
    draftRef = useRef(""),
    editGeneration = useRef(0),
    historyHead = useRef<string | null>(null),
    historyRequest = useRef(0),
    dirty = useRef(false),
    sending = useRef(false),
    retry = useRef<Extract<
      SaveNativeDocumentInput,
      { markdown: string }
    > | null>(null),
    restoreRetry = useRef<RestoreNativeDocumentInput | null>(null),
    comparisonOpener = useRef<{
      element: HTMLElement
      scopeKey: string | null
    } | null>(null)
  const scope = useNeevContentPoll({
    signal: auth.signal,
    scopeKey: auth.scopeKey,
    resourceScope: resource.id,
    enabled: !loading && !revoked,
    onPoll: refresh,
  })
  const documentError =
    error ?? (conflict ? new NativeClientError("conflict") : null)
  useEffect(() => {
    setLatest(resource)
  }, [resource])
  function setBody(
    markdown: string,
    metadata: NativeResource,
    contentRevision: string
  ) {
    editGeneration.current += 1
    base.current = {
      markdown,
      content: contentRevision,
      auth: metadata.authRevision,
    }
    draftRef.current = markdown
    dirty.current = false
    setDraft(markdown)
    setLoaded(true)
    onDirtyChange(false)
    retry.current = null
    setConflict(false)
  }
  function fail(e: unknown) {
    if (!scope.isCurrent()) return
    if (e instanceof NativeClientError && e.kind === "conflict")
      setConflict(true)
    if (
      e instanceof NativeContentNotFoundError ||
      (e instanceof NativeClientError &&
        (e.kind === "forbidden" || e.kind === "unauthorized"))
    ) {
      setDraft("")
      draftRef.current = ""
      base.current = { markdown: "", content: "0", auth: "1" }
      dirty.current = false
      retry.current = null
      restoreRetry.current = null
      setRevisions([])
      comparisonOpener.current = null
      setComparison(null)
      setReload(false)
      setRevoked(true)
      onDirtyChange(false)
      scope.stop()
      onBack()
    }
    setError(e)
    auth.handleAuthorityError(e)
  }
  async function readBody(
    metadata: NativeResource
  ): Promise<NativeDocumentContent | null> {
    return metadata.contentRevision === "0"
      ? null
      : loadNativeDocumentContent(
          resource.orgId,
          resource.id,
          metadata.contentRevision,
          scope.signal
        )
  }
  async function refresh(validateContent = false) {
    const generation = editGeneration.current
    try {
      const metadata = await loadNativeResource(
        resource.orgId,
        resource.id,
        scope.signal
      )
      if (!scope.isCurrent()) return
      if (metadata.kind !== "document")
        throw new NativeClientError("invalid_response")
      setLatest(metadata)
      onChanged(metadata)
      if (!(await loadHistory(metadata.contentRevision))) return
      // Dirty or ambiguously committed drafts keep BOTH original preconditions.
      if (
        !dirty.current &&
        !retry.current &&
        !sending.current &&
        !conflict &&
        (validateContent ||
          !loaded ||
          metadata.contentRevision !== base.current.content)
      ) {
        const body = await readBody(metadata)
        if (
          !scope.isCurrent() ||
          dirty.current ||
          sending.current ||
          generation !== editGeneration.current
        )
          return
        setBody(body?.markdown ?? "", metadata, body?.revision.revision ?? "0")
      } else if (
        !dirty.current &&
        !retry.current &&
        !sending.current &&
        !conflict
      ) {
        base.current.auth = metadata.authRevision
      }
      setLoading(false)
      setError((previous: unknown) =>
        previous instanceof NativeClientError && previous.kind === "conflict"
          ? previous
          : null
      )
    } catch (e) {
      fail(e)
      if (scope.isCurrent()) setLoading(false)
    }
  }
  useEffect(() => {
    sending.current = false
    setSaving(false)
    setBusy(false)
    void refresh(true)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope])
  async function save() {
    if (
      !dirty.current ||
      sending.current ||
      latest.archived ||
      !auth.session ||
      !scope.isCurrent()
    )
      return
    sending.current = true
    setSaving(true)
    setError(null)
    setSaved(false)
    const candidate = {
      markdown: draftRef.current,
      expectedContentRevision: base.current.content,
      expectedAuthRevision: base.current.auth,
    }
    const previous = retry.current
    const input =
      previous &&
      previous.markdown === candidate.markdown &&
      previous.expectedContentRevision === candidate.expectedContentRevision &&
      previous.expectedAuthRevision === candidate.expectedAuthRevision
        ? previous
        : { ...candidate, requestId: crypto.randomUUID() }
    retry.current = input
    try {
      const revision = await saveNativeDocument(
        resource.id,
        input,
        auth.session.csrf,
        scope.signal
      )
      if (!scope.isCurrent()) return
      const metadata = {
        ...latest,
        contentRevision: revision.revision,
        authRevision: input.expectedAuthRevision,
      }
      setBody(input.markdown, metadata, revision.revision)
      historyRequest.current += 1
      setLatest(metadata)
      onChanged(metadata)
      setRevisions((old) => [
        revision,
        ...old.filter((item) => item.revision !== revision.revision),
      ])
      setSaved(true)
    } catch (e) {
      fail(e)
    } finally {
      if (scope.isCurrent()) {
        sending.current = false
        setSaving(false)
      }
    }
  }
  async function compare(revision?: string) {
    const opener = document.activeElement
    comparisonOpener.current =
      opener instanceof HTMLElement
        ? { element: opener, scopeKey: auth.scopeKey }
        : null
    setBusy(true)
    setError(null)
    try {
      const metadata = await loadNativeResource(
        resource.orgId,
        resource.id,
        scope.signal
      )
      if (!scope.isCurrent()) return
      const body = revision
        ? await loadNativeDocumentContent(
            resource.orgId,
            resource.id,
            revision,
            scope.signal
          )
        : await readBody(metadata)
      if (!scope.isCurrent()) return
      setLatest(metadata)
      onChanged(metadata)
      setComparison({
        resource: metadata,
        markdown: body?.markdown ?? "",
        revision: body?.revision.revision ?? "0",
        history: !!revision,
      })
    } catch (e) {
      fail(e)
    } finally {
      if (scope.isCurrent()) setBusy(false)
    }
  }
  async function restore() {
    if (
      !comparison ||
      !auth.session ||
      sending.current ||
      comparison.resource.archived ||
      !scope.isCurrent()
    )
      return
    sending.current = true
    setSaving(true)
    setError(null)
    const candidate = {
      expectedAuthRevision: comparison.resource.authRevision,
      expectedContentRevision: comparison.resource.contentRevision,
      sourceRevision: comparison.revision,
    }
    const previous = restoreRetry.current
    const input =
      previous &&
      previous.sourceRevision === candidate.sourceRevision &&
      previous.expectedAuthRevision === candidate.expectedAuthRevision &&
      previous.expectedContentRevision === candidate.expectedContentRevision
        ? previous
        : { ...candidate, requestId: crypto.randomUUID() }
    restoreRetry.current = input
    try {
      const revision = await restoreNativeDocument(
        resource.id,
        input,
        auth.session.csrf,
        scope.signal
      )
      if (!scope.isCurrent()) return
      const metadata = {
        ...comparison.resource,
        contentRevision: revision.revision,
      }
      setBody(comparison.markdown, metadata, revision.revision)
      historyRequest.current += 1
      setLatest(metadata)
      onChanged(metadata)
      setRevisions((old) => [revision, ...old])
      setComparison(null)
      restoreRetry.current = null
      setSaved(true)
    } catch (e) {
      fail(e)
    } finally {
      if (scope.isCurrent()) {
        sending.current = false
        setSaving(false)
      }
    }
  }
  async function loadHistory(head: string, more = false) {
    if (!more && historyHead.current === head) return true
    const request = ++historyRequest.current
    const current = () =>
      scope.isCurrent() && request === historyRequest.current
    try {
      const refreshed: NativeRevision[] = []
      const initial = historyHead.current === null
      let cursor: string | null = more
        ? nextRevision
        : initial || historyHead.current === "0"
          ? null
          : historyHead.current
      do {
        const page = await listNativeDocumentRevisions(
          resource.id,
          cursor,
          scope.signal
        )
        if (!current()) return false
        refreshed.push(...page.items)
        cursor = page.nextAfter
        // Initial/explicit pagination loads one page. A poll only catches up
        // newly appended immutable revisions after its previously observed head.
        if (initial || more) break
      } while (cursor)
      setRevisions((previous) => [
        ...new Map(
          [...previous, ...refreshed].map((item) => [item.revision, item])
        ).values(),
      ])
      if (initial || more) setNextRevision(cursor)
      if (!more) historyHead.current = head
      return true
    } catch (e) {
      if (current()) {
        fail(e)
        if (scope.isCurrent()) setLoading(false)
      } else if (scope.isCurrent()) auth.handleError(e)
      return false
    }
  }
  async function moreHistory() {
    if (!nextRevision || busy) return
    setBusy(true)
    await loadHistory(latest.contentRevision, true)
    if (scope.isCurrent()) setBusy(false)
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
          {latest.originChatId ? ` · ${t("sourceAudience")}` : ""}
          {latest.archived ? ` · ${t("archived")}` : ""}
        </p>
      </div>
      {!loaded ? (
        <div className="space-y-3">
          {loading ? (
            <p role="status">{n("loading")}</p>
          ) : (
            <>
              <DocumentError error={documentError} />
              <Button
                onClick={() => {
                  setLoading(true)
                  void refresh(true)
                }}
              >
                {n("retry")}
              </Button>
            </>
          )}
        </div>
      ) : loading ? (
        <p role="status">{n("loading")}</p>
      ) : (
        <>
          <label className="block space-y-2">
            <span className="text-sm">{t("documentContent")}</span>
            <textarea
              value={draft}
              disabled={saving || latest.archived}
              onKeyDown={(event) => {
                if (
                  (event.ctrlKey || event.metaKey) &&
                  event.key.toLowerCase() === "s"
                ) {
                  event.preventDefault()
                  void save()
                }
              }}
              onChange={(event) => {
                editGeneration.current += 1
                draftRef.current = event.target.value
                dirty.current = event.target.value !== base.current.markdown
                setDraft(event.target.value)
                retry.current = null
                onDirtyChange(dirty.current)
                setSaved(false)
              }}
              className="min-h-64 w-full resize-y rounded-lg border bg-background p-3 font-mono text-base leading-6 sm:text-sm"
            />
          </label>
          <div className="flex flex-wrap items-center gap-2">
            <Button
              disabled={saving || latest.archived || !dirty.current}
              onClick={() => void save()}
            >
              {saving ? n("working") : n("save")}
            </Button>
            <Button
              variant="ghost"
              disabled={busy || saving}
              onClick={() => setHistory(!history)}
            >
              {t("history")}
            </Button>
            <Button
              variant="ghost"
              disabled={busy || saving}
              onClick={() => setReload(true)}
            >
              {t("reloadLatest")}
            </Button>
            {dirty.current && (
              <Button
                variant="ghost"
                onClick={() => {
                  void navigator.clipboard
                    .writeText(draftRef.current)
                    .catch(fail)
                }}
              >
                {t("copyDraft")}
              </Button>
            )}
            <span role="status" className="text-xs text-muted-foreground">
              {saved ? t("saved") : dirty.current ? t("unsaved") : ""}
            </span>
          </div>
          <DocumentError error={documentError} />
          {(latest.contentRevision !== base.current.content ||
            latest.authRevision !== base.current.auth ||
            conflict) && (
            <Button
              variant="outline"
              disabled={busy || saving}
              onClick={() => void compare()}
            >
              {t("compareLatest")}
            </Button>
          )}
          {history && (
            <section
              className="space-y-3 rounded-lg border p-4"
              aria-label={t("history")}
            >
              <h3 className="text-sm font-medium">{t("history")}</h3>
              {revisions.map((revision) => (
                <div
                  key={revision.revision}
                  className="flex flex-wrap items-center justify-between gap-2 text-xs"
                >
                  <span>
                    {t("revision", { revision: revision.revision })} ·{" "}
                    {revision.authorName} ·{" "}
                    <time
                      dateTime={new Date(
                        revision.createdAt * 1000
                      ).toISOString()}
                    >
                      {new Date(revision.createdAt * 1000).toLocaleString()}
                    </time>
                    {revision.restoredFrom
                      ? ` · ${t("restoredFrom", { revision: revision.restoredFrom })}`
                      : ""}
                  </span>
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy || saving}
                    onClick={() => void compare(revision.revision)}
                  >
                    {t("compareRevision", { revision: revision.revision })}
                  </Button>
                </div>
              ))}
              {nextRevision && (
                <Button disabled={busy} onClick={() => void moreHistory()}>
                  {n("loadMore")}
                </Button>
              )}
            </section>
          )}
          <section
            className="space-y-3 border-t pt-5"
            aria-label={t("preview")}
          >
            <h3 className="text-xs font-medium text-muted-foreground">
              {t("preview")}
            </h3>
            <NativeMarkdown markdown={draft} />
          </section>
        </>
      )}
      <Dialog open={reload} onOpenChange={setReload}>
        <DialogContent>
          <DialogTitle>{t("reloadLatest")}</DialogTitle>
          <DialogDescription>{t("discardDescription")}</DialogDescription>
          <Button variant="ghost" onClick={() => setReload(false)}>
            {n("cancel")}
          </Button>
          <Button
            onClick={async () => {
              setReload(false)
              await compare()
            }}
          >
            {t("reviewLatest")}
          </Button>
        </DialogContent>
      </Dialog>
      <Dialog
        open={!!comparison}
        onOpenChange={(open) => {
          if (!open && !saving) setComparison(null)
        }}
      >
        <DialogContent
          className="max-h-[85dvh] overflow-y-auto sm:max-w-3xl"
          onCloseAutoFocus={(event) => {
            const opener = comparisonOpener.current
            comparisonOpener.current = null
            if (
              opener &&
              opener.scopeKey === auth.scopeKey &&
              auth.status === "authenticated" &&
              opener.element.isConnected &&
              !opener.element.matches(":disabled") &&
              !opener.element.closest("[inert]")
            ) {
              event.preventDefault()
              opener.element.focus()
            }
          }}
        >
          <DialogTitle>
            {comparison?.history
              ? t("compareRevision", { revision: comparison.revision })
              : t("compareLatest")}
          </DialogTitle>
          <DialogDescription>
            {t("audienceReview", {
              organization: auth.session?.organization?.name ?? "",
              audience: t(comparison?.resource.visibility ?? "private"),
            })}{" "}
            {t("compareDescription")}
          </DialogDescription>
          <div className="grid min-w-0 gap-4 sm:grid-cols-2">
            <div>
              <h3 className="mb-2 text-sm font-medium">{t("yourDraft")}</h3>
              <pre className="whitespace-pre-wrap break-words rounded border p-3 text-xs">
                {draft}
              </pre>
            </div>
            <div>
              <h3 className="mb-2 text-sm font-medium">{t("serverVersion")}</h3>
              <pre className="whitespace-pre-wrap break-words rounded border p-3 text-xs">
                {comparison?.markdown}
              </pre>
            </div>
          </div>
          <DocumentError error={documentError} />
          <div className="flex flex-wrap gap-2">
            <Button
              variant="ghost"
              disabled={saving}
              onClick={() => setComparison(null)}
            >
              {n("cancel")}
            </Button>
            {comparison?.history ? (
              <Button
                disabled={saving || comparison.resource.archived}
                onClick={() => void restore()}
              >
                {t("restore")}
              </Button>
            ) : (
              <>
                <Button
                  variant="outline"
                  disabled={saving}
                  onClick={() => {
                    if (!comparison) return
                    setBody(
                      comparison.markdown,
                      comparison.resource,
                      comparison.revision
                    )
                    setComparison(null)
                    setError(null)
                    setConflict(false)
                    setSaved(false)
                  }}
                >
                  {t("discardAndReload")}
                </Button>
                <Button
                  disabled={saving || comparison?.resource.archived}
                  onClick={() => {
                    if (!comparison) return
                    editGeneration.current += 1
                    base.current = {
                      markdown: comparison.markdown,
                      content: comparison.revision,
                      auth: comparison.resource.authRevision,
                    }
                    dirty.current = draftRef.current !== comparison.markdown
                    onDirtyChange(dirty.current)
                    retry.current = null
                    setComparison(null)
                    setError(null)
                    setConflict(false)
                  }}
                >
                  {t("keepDraftLatest")}
                </Button>
              </>
            )}
          </div>
        </DialogContent>
      </Dialog>
    </section>
  )
}
