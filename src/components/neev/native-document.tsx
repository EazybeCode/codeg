"use client"

import { ArrowLeft, Lock, MoreHorizontal, Users } from "lucide-react"

import { useEffect, useRef, useState } from "react"
import dynamic from "next/dynamic"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { Dialog, DialogTitle, DialogDescription } from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { NativeClientError } from "@/lib/neev-client"
import {
  loadNativeResource,
  loadNativeDocumentContent,
  listNativeDocumentRevisions,
  saveNativeDocument,
  restoreNativeDocument,
  uploadNativeImage,
  NativeContentNotFoundError,
  type NativeResource,
  type NativeRevision,
  type NativeDocumentContent,
  type SaveNativeDocumentInput,
  type RestoreNativeDocumentInput,
} from "@/lib/neev-content-client"
import { useNeevContentPoll } from "@/hooks/use-neev-content-poll"
import { useNativeAuth } from "./native-auth-provider"
import {
  NativeDialogContent as DialogContent,
  NativeDropdownMenuContent as DropdownMenuContent,
} from "./native-suspended-surface"
import { NativeError } from "./native-boundary"
import { NativeMarkdown } from "./native-markdown"
import { NativeRichView } from "./native-rich-view"
import { importMarkdown } from "@/lib/neev-rich-adapter"
import type { RichDocument } from "@/lib/neev-rich-document"
import {
  DEFAULT_DOC_WIDTH,
  getDocWidth,
  setDocWidth,
  type DocWidth,
} from "@/lib/neev-workspace-prefs"

const NativeRichEditor = dynamic(
  () =>
    import("./native-rich-editor").then((module) => module.NativeRichEditor),
  { ssr: false }
)
type Body =
  | { format: "markdown"; markdown: string }
  | { format: "blocks"; document: RichDocument }
const emptyBody: Body = { format: "markdown", markdown: "" }
function bodyFromContent(content: NativeDocumentContent | null): Body {
  return content?.document
    ? { format: "blocks", document: content.document }
    : content
      ? { format: "markdown", markdown: content.markdown }
      : { format: "blocks", document: importMarkdown("") }
}
function sameBody(left: Body, right: Body) {
  return JSON.stringify(left) === JSON.stringify(right)
}
function BodyView({ body, source = false }: { body: Body; source?: boolean }) {
  return body.format === "blocks" ? (
    <NativeRichView document={body.document} />
  ) : source ? (
    <pre className="whitespace-pre-wrap break-words rounded border p-3 text-xs">
      {body.markdown}
    </pre>
  ) : (
    <NativeMarkdown markdown={body.markdown} />
  )
}

/** Creator actions bubbled from NativeContent so the document renders ONE header
 *  (no separate action strip). All handlers keep NativeContent's dialogs/guards. */
export type ResourceActions = {
  isCreator: boolean
  archived: boolean
  visibility: "private" | "organization"
  shareable: boolean
  disabled: boolean
  onRename: () => void
  onShare: () => void
  onMakePrivate: () => void
  onArchiveToggle: () => void
  onMove: () => void
}
type Props = {
  resource: NativeResource
  onBack: () => void
  onChanged: (resource: NativeResource) => void
  onDirtyChange: (dirty: boolean) => void
  actions?: ResourceActions
}
type Comparison = {
  resource: NativeResource
  body: Body
  exported: NativeDocumentContent | null
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
function Document({
  resource,
  onBack,
  onChanged,
  onDirtyChange,
  actions,
}: Props) {
  const auth = useNativeAuth(),
    t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  // Per-user viewer width preference (never document state). Start at the SSR
  // default and read the stored value after mount to avoid a hydration mismatch.
  const [docWidth, setDocWidthState] = useState<DocWidth>(DEFAULT_DOC_WIDTH)
  useEffect(() => {
    setDocWidthState(getDocWidth())
  }, [])
  function toggleDocWidth() {
    const next: DocWidth = docWidth === "full" ? "focus" : "full"
    setDocWidthState(next)
    setDocWidth(next)
  }
  const [latest, setLatest] = useState(resource),
    [draft, setDraft] = useState<Body>(emptyBody),
    [conversion, setConversion] = useState<RichDocument | null>(null),
    [conversionFailed, setConversionFailed] = useState(false),
    [savedExport, setSavedExport] = useState<NativeDocumentContent | null>(
      null
    ),
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
      body: emptyBody,
      content: resource.contentRevision,
      auth: resource.authRevision,
    }),
    draftRef = useRef<Body>(emptyBody),
    editorGeneration = useRef(0),
    editGeneration = useRef(0),
    historyHead = useRef<string | null>(null),
    historyRequest = useRef(0),
    dirty = useRef(false),
    sending = useRef(false),
    retry = useRef<SaveNativeDocumentInput | null>(null),
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
  const visibleEditorGeneration = editorGeneration.current
  useEffect(() => {
    setLatest(resource)
  }, [resource])
  function setBody(
    body: Body,
    metadata: NativeResource,
    contentRevision: string,
    exported: NativeDocumentContent | null = null
  ) {
    editGeneration.current += 1
    editorGeneration.current += 1
    base.current = {
      body,
      content: contentRevision,
      auth: metadata.authRevision,
    }
    draftRef.current = body
    dirty.current = false
    setDraft(body)
    setSavedExport(exported)
    setConversion(null)
    setConversionFailed(false)
    setLoaded(true)
    onDirtyChange(false)
    retry.current = null
    setConflict(false)
  }
  function edit(body: Body) {
    if (
      !scope.isCurrent() ||
      sending.current ||
      latest.archived ||
      visibleEditorGeneration !== editorGeneration.current
    )
      return
    editGeneration.current += 1
    draftRef.current = body
    dirty.current = !sameBody(body, base.current.body)
    setDraft(body)
    retry.current = null
    onDirtyChange(dirty.current)
    setSaved(false)
    setConversionFailed(false)
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
      editorGeneration.current += 1
      setDraft(emptyBody)
      draftRef.current = emptyBody
      base.current = { body: emptyBody, content: "0", auth: "1" }
      setConversion(null)
      setConversionFailed(false)
      setSavedExport(null)
      setHistory(false)
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
        setBody(
          bodyFromContent(body),
          metadata,
          body?.revision.revision ?? "0",
          body
        )
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
    const candidate: SaveNativeDocumentInput = {
      requestId: crypto.randomUUID(),
      ...(draftRef.current.format === "blocks"
        ? { document: draftRef.current.document }
        : { markdown: draftRef.current.markdown }),
      expectedContentRevision: base.current.content,
      expectedAuthRevision: base.current.auth,
    }
    const previous = retry.current
    const input =
      previous &&
      JSON.stringify(previous.document ?? previous.markdown) ===
        JSON.stringify(candidate.document ?? candidate.markdown) &&
      previous.expectedContentRevision === candidate.expectedContentRevision &&
      previous.expectedAuthRevision === candidate.expectedAuthRevision
        ? previous
        : candidate
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
      setBody(
        input.document
          ? { format: "blocks", document: input.document }
          : { format: "markdown", markdown: input.markdown },
        metadata,
        revision.revision
      )
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
        body: bodyFromContent(body),
        exported: body,
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
      setBody(comparison.body, metadata, revision.revision)
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
    <section
      data-doc-width={docWidth}
      className={
        "min-h-0 min-w-0 flex-1 overflow-y-auto px-6 pb-24 pt-14 sm:px-10 [&>div]:mx-auto " +
        (docWidth === "focus" ? "[&>div]:max-w-[760px]" : "[&>div]:max-w-none")
      }
      aria-label={latest.title}
      onKeyDown={(event) => {
        if (
          (event.ctrlKey || event.metaKey) &&
          event.key.toLowerCase() === "s"
        ) {
          event.preventDefault()
          void save()
        }
      }}
    >
      {/* One compact header: context + truthful status + Save/Share + a More menu
          (rename/archive/move/history/reload/width/export) — no stacked rows. */}
      <header className="sticky top-0 z-10 -mx-7 flex items-center gap-2 border-b bg-background px-7 py-2 sm:-mx-12 sm:px-12">
        <Button
          variant="ghost"
          size="icon"
          aria-label={t("back")}
          title={t("back")}
          onClick={onBack}
        >
          <ArrowLeft aria-hidden />
        </Button>
        <span
          className="min-w-0 flex-1 truncate text-sm font-medium"
          title={latest.title}
        >
          {latest.title}
        </span>
        <span
          role="status"
          className="hidden text-xs text-muted-foreground sm:inline"
        >
          {saving
            ? n("working")
            : saved
              ? t("saved")
              : dirty.current
                ? t("unsaved")
                : ""}
        </span>
        {loaded && (
          <Button
            size="sm"
            disabled={saving || latest.archived || !dirty.current}
            onClick={() => void save()}
          >
            {saving ? n("working") : n("save")}
          </Button>
        )}
        {actions?.isCreator && actions.shareable && (
          <>
            <Button
              variant="ghost"
              size="sm"
              disabled={actions.disabled}
              // Icon-only on narrow viewports so the title keeps its room; the
              // accessible name is the full sentence at every width.
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
                {t(actions.visibility === "private" ? "share" : "makePrivate")}
              </span>
            </Button>
            {actions.visibility === "organization" && (
              <p id="neev-make-private-warning" className="sr-only">
                {t("makePrivateWarning")}
              </p>
            )}
          </>
        )}
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
            {actions?.isCreator && (
              <DropdownMenuItem
                disabled={actions.disabled}
                onSelect={actions.onRename}
              >
                {t("rename")}
              </DropdownMenuItem>
            )}
            {actions?.isCreator && (
              <DropdownMenuItem
                disabled={actions.disabled}
                onSelect={actions.onArchiveToggle}
              >
                {t(actions.archived ? "reopen" : "archive")}
              </DropdownMenuItem>
            )}
            {actions?.isCreator && (
              <DropdownMenuItem
                disabled={actions.disabled}
                onSelect={actions.onMove}
              >
                {t("move")}
              </DropdownMenuItem>
            )}
            {actions?.isCreator && <DropdownMenuSeparator />}
            <DropdownMenuItem
              disabled={!loaded || busy || saving}
              onSelect={() => setHistory(!history)}
            >
              {t("history")}
            </DropdownMenuItem>
            <DropdownMenuItem
              disabled={!loaded || busy || saving}
              onSelect={() => setReload(true)}
            >
              {t("reloadLatest")}
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={toggleDocWidth}>
              {docWidth === "full" ? t("focusWidth") : t("fullWidth")}
            </DropdownMenuItem>
            {loaded && draft.format === "markdown" && (
              <DropdownMenuItem
                disabled={saving || latest.archived}
                onSelect={() => {
                  try {
                    setConversion(importMarkdown(draft.markdown))
                    setConversionFailed(false)
                  } catch {
                    setConversionFailed(true)
                  }
                }}
              >
                {t("convertRich")}
              </DropdownMenuItem>
            )}
            {loaded && draft.format === "blocks" && savedExport?.document && (
              <DropdownMenuItem
                onSelect={() => {
                  void navigator.clipboard
                    .writeText(savedExport.markdown)
                    .catch(fail)
                }}
              >
                {t("exportSavedMarkdown", {
                  revision: savedExport.revision.revision,
                })}
              </DropdownMenuItem>
            )}
            {loaded && dirty.current && (
              <DropdownMenuItem
                onSelect={() => {
                  void navigator.clipboard
                    .writeText(
                      draftRef.current.format === "blocks"
                        ? JSON.stringify(draftRef.current.document)
                        : draftRef.current.markdown
                    )
                    .catch(fail)
                }}
              >
                {t(draft.format === "blocks" ? "copyRichDraft" : "copyDraft")}
              </DropdownMenuItem>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
      </header>
      <div className="space-y-2 pb-4 pt-6">
        <h1 className="break-words text-[2.5rem] font-bold leading-[1.2] tracking-tight">
          {latest.title}
        </h1>
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
        <div className="flex flex-col gap-4">
          {draft.format === "blocks" ? (
            <NativeRichEditor
              key={`${auth.scopeKey}:${resource.id}:${visibleEditorGeneration}`}
              document={draft.document}
              readOnly={saving || latest.archived}
              onChange={(document) => edit({ format: "blocks", document })}
              uploadImage={
                saving || latest.archived
                  ? undefined
                  : (file) =>
                      uploadNativeImage(
                        resource.id,
                        file,
                        auth.session?.csrf ?? "",
                        auth.signal
                      )
              }
            />
          ) : (
            <label className="block space-y-2">
              <span className="text-sm">{t("documentContent")}</span>
              <textarea
                value={draft.markdown}
                disabled={saving || latest.archived}
                onChange={(event) =>
                  edit({ format: "markdown", markdown: event.target.value })
                }
                className="min-h-64 w-full resize-y rounded-lg border bg-background p-3 font-mono text-base leading-6 sm:text-sm"
              />
            </label>
          )}
          {conversionFailed && (
            <p role="alert" className="text-sm text-destructive">
              {t("conversionRefused")}
            </p>
          )}
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
                    {t(
                      revision.format === "blocks"
                        ? "richFormat"
                        : "markdownFormat"
                    )}{" "}
                    · {revision.authorName} ·{" "}
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
          {draft.format === "markdown" && (
            <section
              className="space-y-3 border-t pt-5"
              aria-label={t("preview")}
            >
              <h3 className="text-xs font-medium text-muted-foreground">
                {t("preview")}
              </h3>
              <BodyView body={draft} />
            </section>
          )}
        </div>
      )}
      <Dialog
        open={!!conversion}
        onOpenChange={(open) => {
          if (!open) setConversion(null)
        }}
      >
        <DialogContent className="max-h-[85dvh] overflow-y-auto sm:max-w-3xl">
          <DialogTitle>{t("conversionPreview")}</DialogTitle>
          <DialogDescription>{t("conversionDescription")}</DialogDescription>
          {conversion && <NativeRichView document={conversion} />}
          <Button variant="ghost" onClick={() => setConversion(null)}>
            {n("cancel")}
          </Button>
          <Button
            disabled={saving || latest.archived}
            onClick={() => {
              if (!conversion) return
              edit({ format: "blocks", document: conversion })
              editorGeneration.current += 1
              setConversion(null)
            }}
          >
            {t("confirmConversion")}
          </Button>
        </DialogContent>
      </Dialog>
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
              <BodyView body={draft} source />
            </div>
            <div>
              <h3 className="mb-2 text-sm font-medium">{t("serverVersion")}</h3>
              {comparison && <BodyView body={comparison.body} source />}
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
                      comparison.body,
                      comparison.resource,
                      comparison.revision,
                      comparison.exported
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
                      body: comparison.body,
                      content: comparison.revision,
                      auth: comparison.resource.authRevision,
                    }
                    dirty.current = !sameBody(draftRef.current, comparison.body)
                    setSavedExport(comparison.exported)
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
