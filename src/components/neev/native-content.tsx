"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { useRouter } from "next/navigation"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { Dialog, DialogTitle, DialogDescription } from "@/components/ui/dialog"
import {
  loadNodes,
  NativeClientError,
  type NativeNode,
} from "@/lib/neev-client"
import {
  createNativeResource,
  listNativeResources,
  loadNativeResource,
  updateNativeResource,
  NativeContentNotFoundError,
  type NativeResource,
  type NativeResourceKind,
} from "@/lib/neev-content-client"
import { useNeevContentPoll } from "@/hooks/use-neev-content-poll"
import { useNativeAuth } from "./native-auth-provider"
import {
  NativeDialogContent as DialogContent,
  NativeSuspendedSurface,
} from "./native-suspended-surface"
import { NativeError } from "./native-boundary"
import { NativeChat } from "./native-chat"
import { NativeDocument } from "./native-document"
import { EngineDocument } from "./engine-document"

export type ContentNavigationGuard = (action: () => void) => void
export type ContentCreation = {
  kind: NativeResourceKind
  request: number
  returnFocus?: HTMLElement | null
}
export type ContentOpen = {
  resource: NativeResource
  request: number
  /** Just created: its name may still follow the first line written in it. */
  fresh?: boolean
}
type Props = {
  node: NativeNode
  onRequestNavigationGuard: (guard: ContentNavigationGuard) => void
  /** The document's autosave pause; shortened by tests, never in the app. */
  autosaveMs?: number
  /**
   * A resource changed under this surface — renamed, archived, re-shared.
   *
   * The workspace keeps the lists the sidebar, the tabs and the breadcrumb all
   * read from. Without this, a document that names itself from its first line
   * is called by its new name on the page and `Untitled` everywhere else.
   */
  onResourceChanged?: (resource: NativeResource) => void
  creation?: ContentCreation
  /** Shell mode: the left rail owns navigation, so hide the built-in folder index
   *  (tablist + resource list) and render only the open resource / creation. */
  chromeless?: boolean
  /** The resource selected in the sidebar (shell mode). */
  open?: ContentOpen
  /** Bubbled when this surface selects/creates a resource so the shell can keep the
   *  sidebar list + selection in sync. */
  onOpened?: (resource: NativeResource) => void
  onCreationCancelled?: () => void
}
export function NativeContent(props: Props) {
  const auth = useNativeAuth()
  if (
    !auth.session?.organization ||
    props.node.orgId !== auth.session.organization.id
  )
    return null
  return <Content key={`${auth.scopeKey}:${props.node.id}`} {...props} />
}
function Content({
  node,
  onRequestNavigationGuard,
  creation,
  chromeless,
  open: openTarget,
  onOpened,
  onCreationCancelled,
  autosaveMs,
  onResourceChanged,
}: Props) {
  const auth = useNativeAuth(),
    router = useRouter(),
    t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  const [kind, setKind] = useState<NativeResourceKind>(
      creation?.kind ?? "chat"
    ),
    [archived, setArchived] = useState(false),
    [items, setItems] = useState<NativeResource[]>([]),
    [nextAfter, setNextAfter] = useState<string | null>(null),
    [selected, setSelected] = useState<NativeResource | null>(null),
    [loading, setLoading] = useState(true),
    [error, setError] = useState<unknown>(null),
    [unavailable, setUnavailable] = useState(false),
    [dialog, setDialog] = useState<
      "create" | "share" | "move" | "rename" | null
    >(creation ? "create" : null),
    [title, setTitle] = useState(""),
    [saving, setSaving] = useState(false),
    [destinations, setDestinations] = useState<NativeNode[]>([]),
    [destination, setDestination] = useState(node.id),
    [pending, setPending] = useState<(() => void) | null>(null),
    [dirty, setDirty] = useState(false),
    [draftGeneration, setDraftGeneration] = useState(0),
    // Which of the three workspace sections is shown in the folder view. Team
    // chat and Documents map to the backend resource `kind`; Agent chats is a
    // truthful placeholder with no backend until agent sessions ship (Phase 1).
    [section, setSection] = useState<"team" | "agents" | "docs">(
      creation?.kind === "document" ? "docs" : "team"
    )
  const [authorizedSignal, setAuthorizedSignal] = useState(auth.signal)
  const dirtyRef = useRef(false),
    mutationBusy = useRef(false),
    selectionGeneration = useRef(0),
    mutationRequest = useRef(0),
    listRequest = useRef(0),
    destinationRequest = useRef(0),
    loadedPages = useRef(1),
    windowScope = useRef(`${node.id}:${kind}:${archived}`),
    opener = useRef<HTMLElement | null>(creation?.returnFocus ?? null),
    creationSeen = useRef(creation?.request),
    createTitleRef = useRef<HTMLInputElement>(null),
    request = useRef<{
      title: string
      kind: NativeResourceKind
      requestId: string
    } | null>(null)
  const scope = useNeevContentPoll({
    signal: auth.signal,
    scopeKey: auth.scopeKey,
    resourceScope: `${node.id}:${kind}:${archived}`,
    // In shell (chromeless) mode the left rail owns the resource list, so this
    // surface never polls a folder index — it only renders the open resource.
    enabled: !chromeless && !selected && !unavailable && !loading,
    onPoll: async () => {
      await load()
    },
  })
  const actions = useRef({ load, open: openNow, update })
  useEffect(() => {
    // A confirmation may outlive a visibility refresh. Resolve its intent
    // against the current request lifetime, never an aborted closure.
    actions.current = { load, open: openNow, update }
  })
  const onDirtyChange = useCallback((value: boolean) => {
    dirtyRef.current = value
    setDirty(value)
  }, [])
  const selectResource = useCallback((resource: NativeResource | null) => {
    selectionGeneration.current += 1
    listRequest.current += 1
    mutationRequest.current += 1
    mutationBusy.current = false
    setSaving(false)
    setSelected(resource)
  }, [])
  /**
   * Try to save, and only ask if that failed.
   *
   * Autosave means "unsaved" is usually "unsaved for another 800ms". Showing
   * the discard dialog then — with Discard as its emphasized button — offers to
   * throw away work that was about to be written down anyway. So: flush first,
   * and put the question to the reader only when the flush did not land, which
   * is the one case where there is a real choice to make.
   */
  // Defaults to "could not save this", so anything that does not offer a
  // flush — a team chat's unsent message, for instance — still gets asked
  // about rather than silently passed over as clean.
  const flush = useRef<() => Promise<boolean>>(async () => false)
  const registerFlush = useCallback((next: () => Promise<boolean>) => {
    flush.current = next
  }, [])
  const guard = useCallback<ContentNavigationGuard>((action) => {
    if (!dirtyRef.current) return action()
    const ask = () => {
      opener.current = document.activeElement as HTMLElement
      setPending(() => action)
    }
    void flush
      .current()
      .then((saved) => (saved ? action() : ask()))
      .catch(ask)
  }, [])
  // Shell mode: when the left rail opens a resource, select it directly (guarded
  // against an unsaved draft) and mirror its kind for follow-on actions.
  const openSeen = useRef<number | undefined>(undefined)
  useEffect(() => {
    if (!openTarget || openTarget.request === openSeen.current) return
    openSeen.current = openTarget.request
    guard(() => {
      onDirtyChange(false)
      setKind(openTarget.resource.kind)
      setError(null)
      setDialog(null)
      selectResource(openTarget.resource)
    })
  }, [openTarget, guard, onDirtyChange, selectResource])
  /**
   * The same resource, changed from outside this component.
   *
   * The effect above only fires when `request` moves, which means "open this",
   * and it goes through `guard` and resets the draft to do it. A rename or a
   * trashing from the sidebar is not an open: the reader is already looking at
   * this document and must keep looking at it, keystrokes intact.
   *
   * But the metadata is now wrong here. `selected` is this component's own
   * copy, and the header reads from it — so trashing from the sidebar left the
   * open document saying "Private" with no "Archived", and an editor that was
   * still writable, while the sidebar said it was in the trash.
   *
   * Only ever forwards, and only for the resource already open: a strictly
   * greater `authRevision` on the same id. That makes it safe against a stale
   * list answer arriving late, and it cannot silently swap one document for
   * another — which is what the guarded path above is for.
   */
  useEffect(() => {
    const incoming = openTarget?.resource
    if (!incoming || !selected || incoming.id !== selected.id) return
    let newer = false
    try {
      newer = BigInt(incoming.authRevision) > BigInt(selected.authRevision)
    } catch {
      return
    }
    if (!newer) return
    setSelected(incoming)
  }, [openTarget, selected])
  const changeSection = (
    next: "team" | "agents" | "docs",
    focusTab?: HTMLElement | null
  ) =>
    guard(() => {
      onDirtyChange(false)
      selectResource(null)
      setSection(next)
      // Team chat and Documents drive the backend list; Agent chats does not.
      if (next !== "agents") setKind(next === "docs" ? "document" : "chat")
      if (focusTab) {
        opener.current = focusTab
        focusTab.focus()
      }
    })
  useEffect(() => {
    onRequestNavigationGuard(guard)
    return () => onRequestNavigationGuard((action) => action())
  }, [guard, onRequestNavigationGuard])
  useEffect(() => {
    if (!dirty) return
    const unload = (event: BeforeUnloadEvent) => {
      event.preventDefault()
      event.returnValue = ""
    }
    window.addEventListener("beforeunload", unload)
    return () => window.removeEventListener("beforeunload", unload)
  }, [dirty])
  function cancelCreation() {
    if (saving) return
    setDialog(null)
    setTitle("")
    setError(null)
    onCreationCancelled?.()
    opener.current?.focus()
  }

  // Focus the full-page draft title after the menu that opened it closes (Radix
  // returns focus to the trigger on close, so defer past that with rAF).
  useEffect(() => {
    if (!chromeless || dialog !== "create") return
    const frame = requestAnimationFrame(() => createTitleRef.current?.focus())
    return () => cancelAnimationFrame(frame)
  }, [chromeless, dialog])
  useEffect(() => {
    if (!creation || creation.request === creationSeen.current) return
    creationSeen.current = creation.request
    opener.current = creation.returnFocus ?? null
    setKind(creation.kind)
    selectResource(null)
    setTitle("")
    setError(null)
    setDialog("create")
  }, [creation, selectResource])
  function fail(e: unknown) {
    if (!scope.isCurrent()) return
    if (
      e instanceof NativeContentNotFoundError ||
      (e instanceof NativeClientError &&
        (e.kind === "forbidden" || e.kind === "unauthorized"))
    ) {
      setItems([])
      selectResource(null)
      setTitle("")
      setDialog(null)
      setPending(null)
      onDirtyChange(false)
      setUnavailable(true)
      scope.stop()
    }
    setError(e)
    auth.handleAuthorityError(e)
  }
  async function load(after: string | null = null) {
    if (after) loadedPages.current += 1
    const pages = loadedPages.current
    const requestId = ++listRequest.current
    const selection = selectionGeneration.current
    const current = () =>
      scope.isCurrent() &&
      requestId === listRequest.current &&
      selection === selectionGeneration.current
    try {
      const refreshed: NativeResource[] = []
      let cursor: string | null = null
      for (let index = 0; index < pages; index++) {
        const page = await listNativeResources(
          node.orgId,
          node.id,
          kind,
          archived,
          cursor,
          scope.signal
        )
        if (!current()) return false
        refreshed.push(...page.items)
        cursor = page.nextAfter
        if (!cursor) break
      }
      setItems([...new Map(refreshed.map((item) => [item.id, item])).values()])
      setNextAfter(cursor)
      setError(null)
      return true
    } catch (e) {
      if (current()) {
        fail(e)
        return null
      } else if (scope.isCurrent()) auth.handleError(e)
      return false
    } finally {
      if (current()) setLoading(false)
    }
  }
  useEffect(() => {
    const nextWindow = `${node.id}:${kind}:${archived}`
    if (windowScope.current !== nextWindow) {
      loadedPages.current = 1
      windowScope.current = nextWindow
    }
    setItems([])
    setLoading(true)
    setUnavailable(false)
    mutationBusy.current = false
    setSaving(false)
    void (async () => {
      const selection = selectionGeneration.current
      const current = () =>
        scope.isCurrent() && selection === selectionGeneration.current
      const revalidating = authorizedSignal !== auth.signal
      try {
        if (revalidating && selected) {
          const metadata = await loadNativeResource(
            node.orgId,
            selected.id,
            scope.signal
          )
          if (!current()) return
          if (metadata.kind !== selected.kind)
            throw new NativeClientError("invalid_response")
          setSelected(metadata)
        }
        // Shell mode: the left rail owns the resource list. Do not fetch a folder
        // index here — only revalidate the open resource (above) and settle.
        if (chromeless) {
          if (!current()) return
          setAuthorizedSignal(auth.signal)
          setLoading(false)
          return
        }
        const loaded = await load()
        if (loaded === null && revalidating && !auth.signal?.aborted) {
          // Failed authority never reveals the retained draft, even for a
          // transient network error. Restart with a clean auth scope.
          void auth.refresh()
          return
        }
        if (!current()) return
        if (loaded) setAuthorizedSignal(auth.signal)
      } catch (e) {
        if (!current()) return
        fail(e)
        if (revalidating) void auth.refresh()
      }
    })()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope])
  function back() {
    guard(() => {
      onDirtyChange(false)
      selectResource(null)
      setItems([])
      setError(null)
      setLoading(true)
      void actions.current.load()
    })
  }
  function open(action: "create" | "share" | "move" | "rename") {
    guard(() => actions.current.open(action))
  }
  function openNow(action: "create" | "share" | "move" | "rename") {
    const requestId = ++destinationRequest.current
    const selection = selectionGeneration.current
    const current = () =>
      scope.isCurrent() &&
      requestId === destinationRequest.current &&
      selection === selectionGeneration.current
    opener.current = document.activeElement as HTMLElement
    setError(null)
    setTitle(action === "create" ? "" : (selected?.title ?? ""))
    setDialog(action)
    if (action === "move") {
      setDestination(selected?.nodeId ?? node.id)
      setDestinations([])
      void (async () => {
        try {
          let after: number | null = null
          const all: NativeNode[] = []
          do {
            const page = await loadNodes(node.orgId, after, scope.signal)
            if (!current()) return
            all.push(...page.nodes)
            after = page.nextAfter
          } while (after !== null)
          setDestinations(all)
        } catch (e) {
          if (current()) fail(e)
          else if (scope.isCurrent()) auth.handleError(e)
        }
      })()
    }
  }
  async function create() {
    if (
      !auth.session ||
      mutationBusy.current ||
      !title.trim() ||
      !scope.isCurrent()
    )
      return
    mutationBusy.current = true
    const requestId = ++mutationRequest.current
    const selection = selectionGeneration.current
    const current = () =>
      scope.isCurrent() &&
      requestId === mutationRequest.current &&
      selection === selectionGeneration.current
    setSaving(true)
    setError(null)
    const previous = request.current
    const input =
      previous && previous.title === title.trim() && previous.kind === kind
        ? previous
        : { title: title.trim(), kind, requestId: crypto.randomUUID() }
    request.current = input
    try {
      const resource = await createNativeResource(
        node.orgId,
        node.id,
        kind,
        { title: input.title, requestId: input.requestId },
        auth.session.csrf,
        scope.signal
      )
      if (!current()) return
      selectResource(resource)
      onOpened?.(resource)
      setDialog(null)
      request.current = null
      setTitle("")
    } catch (e) {
      if (current()) fail(e)
      else if (scope.isCurrent()) auth.handleError(e)
    } finally {
      if (current()) {
        mutationBusy.current = false
        setSaving(false)
      }
    }
  }
  function guardUpdate(
    change: { visibility: "private" } | { archived: boolean }
  ) {
    const targetId = selected?.id
    const selection = selectionGeneration.current
    if (!targetId) return
    guard(() => {
      // Preserve the requested change, not a toggle against refreshed state.
      // An old confirmation must never transfer to a different selection.
      if (selection === selectionGeneration.current)
        void actions.current.update(change, targetId)
    })
  }
  async function update(
    change: Partial<
      Pick<NativeResource, "visibility" | "archived" | "nodeId" | "title">
    >,
    targetId = selected?.id
  ) {
    if (
      !selected ||
      selected.id !== targetId ||
      !auth.session ||
      mutationBusy.current ||
      !scope.isCurrent()
    )
      return
    mutationBusy.current = true
    const requestId = ++mutationRequest.current
    const selection = selectionGeneration.current
    const current = () =>
      scope.isCurrent() &&
      requestId === mutationRequest.current &&
      selection === selectionGeneration.current
    setSaving(true)
    setError(null)
    try {
      const resource = await updateNativeResource(
        node.orgId,
        selected.id,
        {
          expectedAuthRevision: selected.authRevision,
          title: selected.title,
          nodeId: selected.nodeId,
          visibility: selected.visibility,
          archived: selected.archived,
          ...change,
        },
        auth.session.csrf,
        scope.signal
      )
      if (!current()) return
      setDialog(null)
      // `changed`, not `setSelected`: this is the one place a rename, a move or
      // an archive made from THIS menu becomes true, and the tab strip, the
      // breadcrumb and the sidebar all read from lists that live above here.
      // Keeping it local meant the document renamed itself and nothing else
      // heard about it — the name went stale everywhere but the page you were
      // already looking at.
      changed(resource)
      onOpened?.(resource)
      if (resource.nodeId !== node.id) back()
    } catch (e) {
      if (current()) fail(e)
      else if (scope.isCurrent()) auth.handleError(e)
    } finally {
      if (current()) {
        mutationBusy.current = false
        setSaving(false)
      }
    }
  }
  const changed = useCallback(
    (resource: NativeResource) => {
      setSelected((old) => (old?.id === resource.id ? resource : old))
      setItems((old) =>
        old.map((item) => (item.id === resource.id ? resource : item))
      )
      // And out to whoever owns the lists the sidebar, the tabs and the
      // breadcrumb read from — otherwise a document that names itself is
      // called by its new name here and `Untitled` everywhere else.
      onResourceChanged?.(resource)
    },
    [onResourceChanged]
  )
  return (
    <>
      {/* The pane is hidden while this mount's authority is re-proved — the
          gate is unchanged and deliberate. Only the words: the sidebar is
          still on screen, so nothing about the workspace is loading, and
          saying so sent readers looking for a problem that was not there. */}
      {authorizedSignal !== auth.signal && (
        <p role="status" aria-busy="true">
          {n("checkingAccess")}
        </p>
      )}
      <NativeSuspendedSurface
        hidden={authorizedSignal !== auth.signal}
        className={
          selected || chromeless
            ? "flex min-h-0 flex-1 flex-col"
            : "mx-auto w-full max-w-[760px] space-y-6 overflow-y-auto px-6 py-10 sm:px-10"
        }
      >
        {!selected && !chromeless && (
          <>
            <h1 className="break-words text-3xl font-semibold tracking-tight">
              {node.name}
            </h1>
            <div
              role="tablist"
              aria-label={node.name}
              className="flex gap-4 border-b"
            >
              {(
                [
                  { id: "team", label: t("teamChat") },
                  { id: "agents", label: t("agentChats") },
                  { id: "docs", label: t("documents") },
                ] as const
              ).map((tab, index, all) => (
                <button
                  key={tab.id}
                  role="tab"
                  aria-selected={section === tab.id}
                  tabIndex={section === tab.id ? 0 : -1}
                  onClick={() => changeSection(tab.id)}
                  onKeyDown={(event) => {
                    if (
                      ["ArrowLeft", "ArrowRight", "Home", "End"].includes(
                        event.key
                      )
                    ) {
                      event.preventDefault()
                      const next =
                        event.key === "Home"
                          ? 0
                          : event.key === "End"
                            ? all.length - 1
                            : event.key === "ArrowRight"
                              ? (index + 1) % all.length
                              : (index - 1 + all.length) % all.length
                      const element =
                        event.currentTarget.parentElement?.querySelectorAll<HTMLElement>(
                          '[role="tab"]'
                        )[next]
                      changeSection(all[next].id, element)
                    }
                  }}
                  className={`rounded-t px-1 py-3 text-sm focus-visible:ring-2 focus-visible:ring-ring ${section === tab.id ? "border-b-2 border-foreground font-medium" : "text-muted-foreground"}`}
                >
                  {tab.label}
                </button>
              ))}
            </div>
          </>
        )}
        {!selected ? (
          chromeless ? (
            dialog === "create" ? (
              <section
                aria-label={t(kind === "chat" ? "newChat" : "newDocument")}
                onKeyDown={(event) => {
                  if (event.key === "Escape") {
                    event.preventDefault()
                    cancelCreation()
                  }
                }}
                className="mx-auto w-full max-w-[760px] px-6 pb-24 pt-6 sm:px-10"
              >
                <p className="text-xs text-muted-foreground">
                  {n("insideFolder", { name: node.name })}
                </p>
                <form
                  onSubmit={(event) => {
                    event.preventDefault()
                    void create()
                  }}
                >
                  <input
                    ref={createTitleRef}
                    aria-label={t("title")}
                    placeholder={t(
                      kind === "chat" ? "newChatTitle" : "newDocumentTitle"
                    )}
                    value={title}
                    disabled={saving}
                    maxLength={100}
                    onChange={(event) => setTitle(event.target.value)}
                    className="mt-2 w-full border-0 bg-transparent p-0 text-[2rem] font-bold leading-[1.2] tracking-tight outline-none placeholder:text-muted-foreground/40 rounded-md focus-visible:bg-muted/50 focus-visible:ring-4 focus-visible:ring-muted/50 sm:text-[2.5rem]"
                  />
                  <NativeError error={error} />
                  {/*
                    A fallback path only. Documents and chats are made the
                    moment they are asked for, named `Untitled`, and named
                    properly by whatever gets written in them — see
                    `createContent` in native-workspace.tsx. Nothing here
                    submits on its own.
                  */}
                  <div className="mt-6 flex gap-2">
                    <Button
                      type="submit"
                      disabled={
                        saving ||
                        !title.trim() ||
                        [...title.trim()].length > 100 ||
                        /\p{Cc}/u.test(title)
                      }
                    >
                      {saving ? n("working") : t("create")}
                    </Button>
                    <Button
                      type="button"
                      variant="ghost"
                      disabled={saving}
                      onClick={cancelCreation}
                    >
                      {n("cancel")}
                    </Button>
                  </div>
                </form>
              </section>
            ) : null
          ) : section === "agents" ? (
            <section
              role="tabpanel"
              aria-label={t("agentChats")}
              className="space-y-4"
            >
              <p className="py-8 text-sm text-muted-foreground">
                {t("agentChatsUnavailable")}
              </p>
            </section>
          ) : (
            <section
              role="tabpanel"
              aria-label={t(kind === "chat" ? "chats" : "documents")}
              className="space-y-4"
            >
              <div className="flex flex-wrap items-center justify-between gap-3">
                <Button disabled={unavailable} onClick={() => open("create")}>
                  {t(kind === "chat" ? "newChat" : "newDocument")}
                </Button>
                <label className="flex items-center gap-2 text-xs">
                  <input
                    type="checkbox"
                    checked={archived}
                    onChange={(event) => setArchived(event.target.checked)}
                  />
                  {t("showArchived")}
                </label>
              </div>
              {loading ? (
                // This folder's items. The re-authorization notice above is
                // the one place here that is genuinely about the workspace.
                <p role="status">{n("loadingItems")}</p>
              ) : items.length === 0 && !unavailable ? (
                <p className="py-8 text-sm text-muted-foreground">
                  {t(kind === "chat" ? "emptyChats" : "emptyDocuments")}
                </p>
              ) : (
                <ul className="divide-y">
                  {items.map((resource) => (
                    <li key={resource.id}>
                      <button
                        onClick={() => {
                          selectResource(resource)
                          setError(null)
                        }}
                        className="flex w-full min-w-0 flex-wrap items-center justify-between gap-2 rounded py-4 text-start hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring"
                      >
                        <span className="min-w-0 break-words text-sm font-medium">
                          {resource.title}
                        </span>
                        <span className="text-xs text-muted-foreground">
                          {t(resource.visibility)}
                          {resource.archived ? ` · ${t("archived")}` : ""}
                        </span>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
              {nextAfter && (
                <Button disabled={loading} onClick={() => void load(nextAfter)}>
                  {n("loadMore")}
                </Button>
              )}
            </section>
          )
        ) : (
          <>
            {selected.kind === "chat" ? (
              <NativeChat
                key={`${selected.id}:${draftGeneration}`}
                resource={selected}
                fresh={openTarget?.fresh === true}
                onBack={back}
                showBack={!chromeless}
                onChanged={changed}
                onDirtyChange={onDirtyChange}
                onRequestSettings={() => guard(() => router.push("/settings"))}
                actions={{
                  isCreator: selected.creatorId === auth.session?.user.id,
                  archived: selected.archived,
                  visibility: selected.visibility,
                  shareable: !selected.originChatId,
                  disabled: saving,
                  onRename: () => open("rename"),
                  onShare: () => open("share"),
                  onMakePrivate: () => guardUpdate({ visibility: "private" }),
                  onArchiveToggle: () =>
                    guardUpdate({ archived: !selected.archived }),
                  onMove: () => open("move"),
                }}
              />
            ) : selected.engine === "lasuite" ? (
              /*
               * The embedded engine owns the document's content, title and
               * saving, so none of the native document's saving machinery is
               * passed to it — no `onRegisterFlush`, no `autosaveMs`, no
               * `onDirtyChange`. Two writers against one document is the
               * failure this avoids, and the absence of those props is what
               * makes it structural rather than a promise.
               */
              <EngineDocument
                key={selected.id}
                resource={selected}
                onBack={back}
                showBack={!chromeless}
                onChanged={changed}
                actions={{
                  isCreator: selected.creatorId === auth.session?.user.id,
                  archived: selected.archived,
                  visibility: selected.visibility,
                  shareable: !selected.originChatId,
                  disabled: saving,
                  onRename: () => open("rename"),
                  onShare: () => open("share"),
                  onMakePrivate: () => guardUpdate({ visibility: "private" }),
                  onArchiveToggle: () =>
                    guardUpdate({ archived: !selected.archived }),
                  onMove: () => open("move"),
                }}
              />
            ) : (
              <NativeDocument
                key={`${selected.id}:${draftGeneration}`}
                resource={selected}
                fresh={openTarget?.fresh === true}
                onBack={back}
                onChanged={changed}
                onDirtyChange={onDirtyChange}
                onRegisterFlush={registerFlush}
                autosaveMs={autosaveMs}
                actions={{
                  isCreator: selected.creatorId === auth.session?.user.id,
                  archived: selected.archived,
                  visibility: selected.visibility,
                  shareable: !selected.originChatId,
                  disabled: saving,
                  onRename: () => open("rename"),
                  onShare: () => open("share"),
                  onMakePrivate: () => guardUpdate({ visibility: "private" }),
                  onArchiveToggle: () =>
                    guardUpdate({ archived: !selected.archived }),
                  onMove: () => open("move"),
                }}
              />
            )}
          </>
        )}
        {unavailable ? (
          <p role="alert">{t("unavailable")}</p>
        ) : (
          <NativeError error={error} />
        )}
        <Dialog
          open={!!dialog && !(chromeless && dialog === "create")}
          onOpenChange={(value) => {
            if (!value && !saving) {
              destinationRequest.current += 1
              setDialog(null)
              setTitle("")
            }
          }}
        >
          <DialogContent
            onCloseAutoFocus={(event) => {
              event.preventDefault()
              opener.current?.focus()
            }}
          >
            <DialogTitle>
              {t(
                dialog === "share"
                  ? "shareTitle"
                  : dialog === "move"
                    ? "moveTitle"
                    : dialog === "rename"
                      ? "rename"
                      : kind === "chat"
                        ? "newChat"
                        : "newDocument"
              )}
            </DialogTitle>
            <DialogDescription>
              {dialog === "share"
                ? t("shareDescription", {
                    organization: auth.session?.organization?.name ?? "",
                  })
                : n("insideFolder", { name: node.name })}
            </DialogDescription>
            <form
              className="space-y-4"
              onSubmit={(event) => {
                event.preventDefault()
                if (dialog === "create") void create()
                else if (dialog === "share")
                  void update({ visibility: "organization" })
                else if (dialog === "move") void update({ nodeId: destination })
                else if (dialog === "rename")
                  void update({ title: title.trim() })
              }}
            >
              {(dialog === "create" || dialog === "rename") && (
                <label className="block space-y-2">
                  <span className="text-sm">{t("title")}</span>
                  <input
                    autoFocus
                    required
                    value={title}
                    disabled={saving}
                    onChange={(event) => setTitle(event.target.value)}
                    className="h-10 w-full rounded-lg border bg-background px-3"
                  />
                </label>
              )}
              {dialog === "move" && (
                <label className="block space-y-2">
                  <span className="text-sm">{t("destination")}</span>
                  <select
                    value={destination}
                    disabled={saving}
                    onChange={(event) =>
                      setDestination(Number(event.target.value))
                    }
                    className="h-10 w-full rounded-lg border bg-background px-3"
                  >
                    {destinations.map((value) => (
                      <option key={value.id} value={value.id}>
                        {value.name} · #{value.id}
                      </option>
                    ))}
                  </select>
                </label>
              )}
              <NativeError error={error} />
              <div className="flex justify-end gap-2">
                <Button
                  type="button"
                  variant="ghost"
                  disabled={saving}
                  onClick={() => {
                    destinationRequest.current += 1
                    setDialog(null)
                    setTitle("")
                  }}
                >
                  {n("cancel")}
                </Button>
                <Button
                  type="submit"
                  disabled={
                    saving ||
                    ((dialog === "create" || dialog === "rename") &&
                      (!title.trim() ||
                        [...title.trim()].length > 100 ||
                        /\p{Cc}/u.test(title))) ||
                    (dialog === "move" && !destinations.length)
                  }
                >
                  {saving
                    ? n("working")
                    : dialog === "share"
                      ? t("confirmShare")
                      : dialog === "create"
                        ? t("create")
                        : n("save")}
                </Button>
              </div>
            </form>
          </DialogContent>
        </Dialog>
        <Dialog
          open={!!pending}
          onOpenChange={(value) => {
            if (!value) setPending(null)
          }}
        >
          <DialogContent
            onCloseAutoFocus={(event) => {
              event.preventDefault()
              opener.current?.focus()
            }}
          >
            <DialogTitle>{t("discardTitle")}</DialogTitle>
            <DialogDescription>{t("discardDescription")}</DialogDescription>
            <div className="flex flex-wrap justify-end gap-2">
              <Button variant="ghost" onClick={() => setPending(null)}>
                {t("keepEditing")}
              </Button>
              <Button
                onClick={() => {
                  const action = pending
                  setPending(null)
                  onDirtyChange(false)
                  setDraftGeneration((value) => value + 1)
                  action?.()
                }}
              >
                {t("discard")}
              </Button>
            </div>
          </DialogContent>
        </Dialog>
      </NativeSuspendedSurface>
    </>
  )
}
