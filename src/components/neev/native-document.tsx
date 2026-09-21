"use client"

import { Lock, MoreHorizontal, Users } from "lucide-react"

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
import { firstLineTitle } from "@/lib/neev-first-line-title"
import { NativeClientError } from "@/lib/neev-client"
import {
  loadNativeResource,
  updateNativeResource,
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
/**
 * How long the writing has to pause before it is written down.
 *
 * Deliberately not a fast tick. A document is capped at 1000 revisions for its
 * whole life (`content_revision >= 1000` is refused server-side), and every
 * save spends one — so this waits for a real pause in the writing rather than
 * a gap between words. `edit` compares against the saved body, so a rich-editor
 * event that changed nothing never starts the clock at all.
 */
const AUTOSAVE_MS = 2500
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
  /** The autosave pause; shortened by tests, never overridden in the app. */
  autosaveMs?: number
  /**
   * Offer a way to save NOW and say whether it worked.
   *
   * Navigation asks for this before it asks the reader anything: with autosave,
   * "unsaved" usually means "about to be saved", and a dialog whose emphasized
   * button discards work that was a few hundred milliseconds from being written
   * down is a trap.
   */
  onRegisterFlush?: (flush: () => Promise<boolean>) => void
  /**
   * Made a moment ago and still called `Untitled`.
   *
   * Two things follow from it: the cursor belongs in the body rather than in a
   * name field, and the name follows the first line written — until the writer
   * renames it themselves, after which it is theirs and nothing else touches
   * it.
   */
  fresh?: boolean
}
type Comparison = {
  resource: NativeResource
  body: Body
  exported: NativeDocumentContent | null
  revision: string
  history: boolean
}
/**
 * The first line of a body, as a name for it.
 *
 * Delegates the line-picking to the shared helper so a document and a team
 * chat cannot drift apart about what naming-from-content means; all this adds
 * is getting plain text out of a block document.
 */
export function titleFromBody(body: Body): string {
  return firstLineTitle(
    body.format === "markdown"
      ? body.markdown
      : blockText(body.document as unknown)
  )
}

/**
 * Every string a block document holds, in order, without its structure.
 *
 * A `RichDocument` is `{ version, blocks }`, and each block keeps its text
 * under `content`, its nested blocks under `children`, and a table's under
 * `rows`/`cells`. Missing `blocks` was the whole bug: the traversal started
 * one level below the document and every rich document derived an empty name.
 */
function blockText(value: unknown, depth = 0, inline = false): string {
  if (depth > 12 || value === null || typeof value !== "object") return ""
  if (Array.isArray(value))
    return value
      .map((item) => blockText(item, depth + 1, inline))
      .filter(Boolean)
      .join(inline ? "" : "\n")
  const node = value as Record<string, unknown>
  if (typeof node.text === "string") return node.text
  const parts: string[] = []
  // `content` and `cells` are runs WITHIN one line — a heading split across a
  // bold run is still one heading, and joining those with a newline would name
  // the document after its first word. The rest are separate lines.
  for (const key of ["content", "cells"]) {
    const child = node[key]
    if (child !== undefined) parts.push(blockText(child, depth + 1, true))
  }
  const line = parts.filter(Boolean).join("")
  const below: string[] = []
  for (const key of ["blocks", "children", "rows"]) {
    const child = node[key]
    if (child !== undefined) below.push(blockText(child, depth + 1, false))
  }
  return [line, ...below].filter(Boolean).join("\n")
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
  autosaveMs = AUTOSAVE_MS,
  onRegisterFlush,
  fresh = false,
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
    // A save failed: stop trying by ourselves until asked.
    [stalled, setStalled] = useState(false),
    // Bumped when a save finishes and something is STILL unsaved, which is how
    // a change made during the flight gets its own save.
    [pass, setPass] = useState(0),
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
    exported: NativeDocumentContent | null = null,
    keepDraft = false
  ) {
    editGeneration.current += 1
    // `keepDraft` is the save case: the body just PERSISTED becomes the base,
    // but the editor is left alone. Anything typed while the save was in
    // flight is still the newest thing there is, and replacing the editor with
    // the older saved body would silently undo it.
    if (!keepDraft) {
      editorGeneration.current += 1
      draftRef.current = body
      setDraft(body)
      setConversion(null)
    }
    base.current = {
      body,
      content: contentRevision,
      auth: metadata.authRevision,
    }
    dirty.current = !sameBody(draftRef.current, body)
    setSavedExport(exported)
    setConversionFailed(false)
    setLoaded(true)
    onDirtyChange(dirty.current)
    retry.current = null
    setConflict(false)
    // A new basis — a fresh load, an accepted rebase, or a save that landed —
    // means the last failure no longer describes the situation, so automatic
    // saving may resume.
    setStalled(false)
  }
  function edit(body: Body) {
    // Deliberately NOT gated on an in-flight save. Typing while one is running
    // is the normal case once saving is automatic, and dropping those
    // keystrokes would lose words the writer can see on their screen.
    if (
      !scope.isCurrent() ||
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
    setStalled(false)
    setConversionFailed(false)
  }
  /**
   * Was this "conflict" just our own save coming back to us?
   *
   * A save can commit and lose its response. The next edit starts a fresh
   * request id, so the receipt dedup does not catch it, and it arrives with an
   * expected revision the server has already moved past — a 409 for a write
   * nobody else made. Before showing a conflict wall, read what is actually
   * stored: if it is byte-for-byte the body we last sent, that save landed, so
   * rebase onto it and carry on. Anything else is a real conflict and is
   * reported as one.
   */
  async function rebaseAfterLostSave(): Promise<boolean> {
    // The body of the save that never answered — NOT the latest attempt. A
    // later save overwrites `attempted`, and comparing the server against that
    // would ask "did my newest text land?" when the question is whether the
    // OLD one did.
    const mine = unresolved.current
    // Only when a previous save might actually have committed. An ordinary
    // conflict — somebody else wrote — needs no extra reads and gets none.
    if (!mine || !maybeCommitted.current) return false
    try {
      const metadata = await loadNativeResource(
        resource.orgId,
        resource.id,
        scope.signal
      )
      if (!scope.isCurrent()) return false
      const stored = await readBody(metadata)
      const storedBody: Body = stored
        ? stored.document
          ? { format: "blocks", document: stored.document }
          : { format: "markdown", markdown: stored.markdown }
        : emptyBody
      if (!sameBody(storedBody, mine)) return false
      // Ours after all. Advance the base and keep whatever has been typed
      // since; `setBody` recomputes dirty, so the next pause saves the rest.
      setBody(storedBody, metadata, metadata.contentRevision, stored, true)
      setLatest(metadata)
      onChanged(metadata)
      // The refusal has been explained: it was this client's own write. Leaving
      // the message up would report a conflict that never happened.
      setError(null)
      // The rebase left whatever was typed since still unsaved, and it is owed
      // a pass of its own — the same debt a save that finishes dirty incurs.
      if (dirty.current) setPass((value) => value + 1)
      return true
    } catch {
      return false
    }
  }

  function fail(e: unknown) {
    if (!scope.isCurrent()) return
    if (e instanceof NativeClientError && e.kind === "conflict")
      void rebaseAfterLostSave().then((mine) => {
        if (!scope.isCurrent()) return
        maybeCommitted.current = false
        unresolved.current = null
        if (!mine) setConflict(true)
      })
    else if (e instanceof NativeClientError && e.kind === "conflict")
      setConflict(true)
    else {
      // The request did not come back. It may well have been applied — the
      // next conflict has to consider that before calling it somebody else's,
      // and must compare against THIS body rather than a later one.
      maybeCommitted.current = true
      unresolved.current = attempted.current
    }
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
  /**
   * Save shortly after the writing stops.
   *
   * A debounce rather than a keystroke-by-keystroke write: one save per pause,
   * and the existing mutex means a pause during an in-flight save simply
   * queues the next one instead of racing it. `dirty` is re-read when the timer
   * fires, so a change that was already persisted does not save again.
   */
  useEffect(() => {
    // `stalled` is what stops a failed save from being retried forever. A
    // server that is refusing this document will refuse it again in 800ms, and
    // again after that; the reader would get an error every second and the
    // endpoint a request every second. It clears on the next edit or on an
    // explicit Retry — both of which are the reader saying to try again.
    if (
      !loaded ||
      !dirty.current ||
      conflict ||
      stalled ||
      latest.archived ||
      sending.current
    )
      return
    const timer = window.setTimeout(() => void save(), autosaveMs)
    return () => window.clearTimeout(timer)
    // `draft` changes on every edit and `pass` is bumped when a finished save
    // leaves something still unsaved; the rest are the conditions that make
    // saving possible at all.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draft, pass, loaded, conflict, stalled, latest.archived, AUTOSAVE_MS])

  /**
   * Name it once, from what was written, after it is first saved.
   *
   * Once, deliberately. A name that keeps following the text would fight the
   * writer the moment they rename it, and every attempt spends an auth
   * revision. So: only a document that was just created, only while it still
   * carries the placeholder name, and only after a save has succeeded — at
   * which point `metadata.authRevision` is current and the rename cannot
   * conflict with the save that just landed.
   *
   * It takes the save mutex for the same reason: an autosave starting midway
   * would still hold the old auth revision and be refused.
   */
  const pendingName = useRef(fresh)
  async function nameFromWriting(metadata: NativeResource) {
    if (!pendingName.current || !auth.session) return
    // A document already named by hand is never renamed by us.
    if (metadata.title !== t("untitledDocument")) {
      pendingName.current = false
      return
    }
    const derived = titleFromBody(draftRef.current)
    // Nothing written yet — the first save of a brand-new document has no
    // words to take a name from. Keep the chance for the next one rather than
    // spending it here, which left every document called `Untitled`.
    if (!derived || derived === metadata.title) return
    // Now it is a real attempt, and it is the only one: once, whatever
    // happens next, including failure. A name is a nicety.
    pendingName.current = false
    // The caller still holds the save lock; taking or releasing it here would
    // hand it back while this is still running.
    try {
      const renamed = await updateNativeResource(
        resource.orgId,
        resource.id,
        {
          nodeId: metadata.nodeId,
          title: derived,
          visibility: metadata.visibility,
          archived: metadata.archived,
          expectedAuthRevision: metadata.authRevision,
        },
        auth.session.csrf,
        scope.signal
      )
      if (!scope.isCurrent()) return
      // The base moves with it, so the next body save uses the revision the
      // rename produced rather than the one it replaced.
      base.current = { ...base.current, auth: renamed.authRevision }
      setLatest(renamed)
      onChanged(renamed)
    } catch {
      // Naming is a convenience; never interrupt the writing for it.
    }
  }

  /** The save currently running, for anyone who needs to wait for it. */
  const inFlight = useRef<Promise<void>>(Promise.resolve())
  /** `stalled` readable from a callback without making it a dependency. */
  const stalledRef = useRef(false)
  stalledRef.current = stalled

  // The body of the last save we attempted, kept even after further edits.
  // If that save committed and its response was lost, this is what the server
  // now holds — which is how a false conflict is told from a real one.
  const attempted = useRef<Body | null>(null)
  /** A save whose fate is unknown: no response, so possibly committed. */
  const maybeCommitted = useRef(false)
  /** What that save was carrying, held until its fate is settled. */
  const unresolved = useRef<Body | null>(null)

  // Kept in a ref so the registration below never goes stale.
  const flushRef = useRef(async () => true)
  flushRef.current = async () => {
    // Wait for a save already in flight rather than returning past it: `save`
    // declines while one is running, so without this the flush would report
    // "still dirty" and the caller would show a discard dialog for work that
    // was seconds from being written down.
    await inFlight.current
    if (!dirty.current) return true
    await save()
    // Anything typed during that save is a new generation: save it too, once.
    if (dirty.current && !stalledRef.current) await save()
    // `save` leaves `dirty` false only when the server took it.
    return !dirty.current
  }
  useEffect(() => {
    onRegisterFlush?.(() => flushRef.current())
    return () => onRegisterFlush?.(async () => true)
  }, [onRegisterFlush])

  async function save() {
    const running = runSave()
    inFlight.current = running
    await running
  }

  async function runSave() {
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
    attempted.current = draftRef.current
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
        revision.revision,
        null,
        true
      )
      historyRequest.current += 1
      setLatest(metadata)
      onChanged(metadata)
      setRevisions((old) => [
        revision,
        ...old.filter((item) => item.revision !== revision.revision),
      ])
      // `setBody` above recomputed `dirty` against what is in the editor NOW.
      // Still dirty means the writer typed during the flight, so this is not
      // "Saved" yet and another pass is owed.
      setSaved(!dirty.current)
      if (dirty.current) setPass((value) => value + 1)
      // Awaited: the lock must still be held while the rename is in flight,
      // or an autosave will start against the auth revision it is replacing.
      await nameFromWriting(metadata)
    } catch (e) {
      setStalled(true)
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
        // No top padding here: the sticky action header sits flush under the
        // tab strip. The whitespace above the title belongs to the title.
        "min-h-0 min-w-0 flex-1 overflow-y-auto px-6 pb-24 pt-0 sm:px-10 [&>div]:mx-auto " +
        (docWidth === "focus" ? "[&>div]:max-w-[760px]" : "[&>div]:max-w-none")
      }
      aria-label={latest.title}
      onKeyDown={(event) => {
        if (
          (event.ctrlKey || event.metaKey) &&
          event.key.toLowerCase() === "s"
        ) {
          event.preventDefault()
          setStalled(false)
          void save()
        }
      }}
    >
      {/*
        One slim row of actions.
        No Back — the tab strip above is how you leave. No title — the
        breadcrumb says which document this is and the page says it again in
        40px a few lines below. No Save — it saves itself; what is left is the
        truth about whether it did.
      */}
      <header className="sticky top-0 z-10 -mx-6 flex h-10 items-center gap-2 bg-background/95 px-6 backdrop-blur sm:-mx-10 sm:px-10">
        <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground/80">
          {t(latest.visibility)}
          {latest.originChatId ? ` · ${t("sourceAudience")}` : ""}
          {latest.archived ? ` · ${t("archived")}` : ""}
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
        {loaded && stalled && !saving && (
          // Nothing to press while it is working. This appears whenever a save
          // did not land — including an ambiguous one that never answered,
          // which reports no error but has still stopped trying.
          <Button
            size="sm"
            variant="outline"
            disabled={latest.archived}
            onClick={() => {
              setStalled(false)
              void save()
            }}
          >
            {n("retry")}
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
      <div className="pb-1 pt-4">
        <h1 className="break-words text-[2.5rem] font-bold leading-[1.15] tracking-[-0.02em]">
          {latest.title}
        </h1>
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
              autoFocus={fresh}
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
                    // The basis the last save was refused against has been
                    // replaced, so automatic saving may resume.
                    setStalled(false)
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
