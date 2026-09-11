"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { useRouter } from "next/navigation"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog"
import {
  loadNodes,
  NativeClientError,
  type NativeNode,
} from "@/lib/neev-client"
import {
  createNativeResource,
  listNativeResources,
  updateNativeResource,
  NativeContentNotFoundError,
  type NativeResource,
  type NativeResourceKind,
} from "@/lib/neev-content-client"
import { useNeevContentPoll } from "@/hooks/use-neev-content-poll"
import { useNativeAuth } from "./native-auth-provider"
import { NativeError } from "./native-boundary"
import { NativeChat } from "./native-chat"
import { NativeDocument } from "./native-document"

export type ContentNavigationGuard = (action: () => void) => void
export type ContentCreation = {
  kind: NativeResourceKind
  request: number
  returnFocus?: HTMLElement | null
}
type Props = {
  node: NativeNode
  onRequestNavigationGuard: (guard: ContentNavigationGuard) => void
  creation?: ContentCreation
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
function Content({ node, onRequestNavigationGuard, creation }: Props) {
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
    [draftGeneration, setDraftGeneration] = useState(0)
  const dirtyRef = useRef(false),
    mutationBusy = useRef(false),
    opener = useRef<HTMLElement | null>(creation?.returnFocus ?? null),
    creationSeen = useRef(creation?.request),
    request = useRef<{
      title: string
      kind: NativeResourceKind
      requestId: string
    } | null>(null)
  const scope = useNeevContentPoll({
    signal: auth.signal,
    scopeKey: auth.scopeKey,
    resourceScope: `${node.id}:${kind}:${archived}`,
    enabled: !selected && !unavailable && !loading,
    onPoll: () => load(),
  })
  const onDirtyChange = useCallback((value: boolean) => {
    dirtyRef.current = value
    setDirty(value)
  }, [])
  const guard = useCallback<ContentNavigationGuard>((action) => {
    if (dirtyRef.current) {
      opener.current = document.activeElement as HTMLElement
      setPending(() => action)
    } else action()
  }, [])
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
  useEffect(() => {
    if (!creation || creation.request === creationSeen.current) return
    creationSeen.current = creation.request
    opener.current = creation.returnFocus ?? null
    setKind(creation.kind)
    setSelected(null)
    setTitle("")
    setError(null)
    setDialog("create")
  }, [creation])
  function fail(e: unknown) {
    if (!scope.isCurrent()) return
    if (
      e instanceof NativeContentNotFoundError ||
      (e instanceof NativeClientError &&
        (e.kind === "forbidden" || e.kind === "unauthorized"))
    ) {
      setItems([])
      setSelected(null)
      setTitle("")
      setDialog(null)
      setPending(null)
      onDirtyChange(false)
      setUnavailable(true)
      scope.stop()
    }
    setError(e)
    auth.handleError(e)
  }
  async function load(after: string | null = null) {
    try {
      const page = await listNativeResources(
        node.orgId,
        node.id,
        kind,
        archived,
        after,
        scope.signal
      )
      if (!scope.isCurrent()) return
      setItems((old) =>
        after
          ? [
              ...new Map(
                [...old, ...page.items].map((item) => [item.id, item])
              ).values(),
            ]
          : page.items
      )
      setNextAfter(page.nextAfter)
      setError(null)
    } catch (e) {
      fail(e)
    } finally {
      if (scope.isCurrent()) setLoading(false)
    }
  }
  useEffect(() => {
    setItems([])
    setLoading(true)
    setUnavailable(false)
    void load()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope])
  function back() {
    guard(() => {
      onDirtyChange(false)
      setSelected(null)
      setItems([])
      setError(null)
      setLoading(true)
      void load()
    })
  }
  function open(action: "create" | "share" | "move" | "rename") {
    guard(() => {
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
              if (!scope.isCurrent()) return
              all.push(...page.nodes)
              after = page.nextAfter
            } while (after !== null)
            setDestinations(all)
          } catch (e) {
            fail(e)
          }
        })()
      }
    })
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
      if (!scope.isCurrent()) return
      setSelected(resource)
      setDialog(null)
      request.current = null
      setTitle("")
    } catch (e) {
      fail(e)
    } finally {
      mutationBusy.current = false
      if (scope.isCurrent()) setSaving(false)
    }
  }
  async function update(
    change: Partial<
      Pick<NativeResource, "visibility" | "archived" | "nodeId" | "title">
    >
  ) {
    if (
      !selected ||
      !auth.session ||
      mutationBusy.current ||
      !scope.isCurrent()
    )
      return
    mutationBusy.current = true
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
      if (!scope.isCurrent()) return
      setDialog(null)
      setSelected(resource)
      if (resource.nodeId !== node.id) back()
    } catch (e) {
      fail(e)
    } finally {
      mutationBusy.current = false
      if (scope.isCurrent()) setSaving(false)
    }
  }
  const changed = useCallback(
    (resource: NativeResource) =>
      setSelected((old) => (old?.id === resource.id ? resource : old)),
    []
  )
  return (
    <div className="mx-auto mt-6 max-w-4xl space-y-6 pb-10">
      <h1 className="break-words text-3xl font-semibold tracking-tight">
        {node.name}
      </h1>
      <div
        role="tablist"
        aria-label={node.name}
        className="flex gap-4 border-b"
      >
        {(["chat", "document"] as const).map((value) => (
          <button
            key={value}
            role="tab"
            aria-selected={kind === value}
            tabIndex={kind === value ? 0 : -1}
            onClick={() =>
              guard(() => {
                onDirtyChange(false)
                setSelected(null)
                setKind(value)
              })
            }
            onKeyDown={(event) => {
              if (
                ["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)
              ) {
                event.preventDefault()
                const target =
                  event.key === "Home"
                    ? "chat"
                    : event.key === "End"
                      ? "document"
                      : value === "chat"
                        ? "document"
                        : "chat"
                const tab =
                  event.currentTarget.parentElement?.querySelectorAll<HTMLElement>(
                    '[role="tab"]'
                  )[target === "chat" ? 0 : 1]
                guard(() => {
                  onDirtyChange(false)
                  setSelected(null)
                  setKind(target)
                  opener.current = tab ?? null
                  tab?.focus()
                })
              }
            }}
            className={`rounded-t px-1 py-3 text-sm focus-visible:ring-2 focus-visible:ring-ring ${kind === value ? "border-b-2 border-foreground font-medium" : "text-muted-foreground"}`}
          >
            {t(value === "chat" ? "chats" : "documents")}
          </button>
        ))}
      </div>
      {!selected ? (
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
            <p role="status">{n("loading")}</p>
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
                      setSelected(resource)
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
      ) : (
        <>
          {selected.creatorId === auth.session?.user.id && (
            <div
              aria-label={t("resourceActions")}
              className="flex flex-wrap gap-2"
            >
              <Button
                variant="ghost"
                size="sm"
                disabled={saving}
                onClick={() => open("rename")}
              >
                {t("rename")}
              </Button>
              {!selected.originChatId && (
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={saving}
                  onClick={() =>
                    selected.visibility === "private"
                      ? open("share")
                      : guard(() => void update({ visibility: "private" }))
                  }
                >
                  {t(
                    selected.visibility === "private" ? "share" : "makePrivate"
                  )}
                </Button>
              )}
              <Button
                variant="ghost"
                size="sm"
                disabled={saving}
                onClick={() =>
                  guard(() => void update({ archived: !selected.archived }))
                }
              >
                {t(selected.archived ? "reopen" : "archive")}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                disabled={saving}
                onClick={() => open("move")}
              >
                {t("move")}
              </Button>
            </div>
          )}
          {selected.kind === "chat" ? (
            <NativeChat
              key={`${selected.id}:${draftGeneration}`}
              resource={selected}
              onBack={back}
              onChanged={changed}
              onDirtyChange={onDirtyChange}
              onRequestSettings={() => guard(() => router.push("/settings"))}
            />
          ) : (
            <NativeDocument
              key={`${selected.id}:${draftGeneration}`}
              resource={selected}
              onBack={back}
              onChanged={changed}
              onDirtyChange={onDirtyChange}
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
        open={!!dialog}
        onOpenChange={(value) => {
          if (!value && !saving) {
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
              else if (dialog === "rename") void update({ title: title.trim() })
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
    </div>
  )
}
