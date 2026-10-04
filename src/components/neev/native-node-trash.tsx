"use client"

import { useCallback, useEffect, useState, useSyncExternalStore } from "react"
import { createTranslator, useTranslations } from "next-intl"
import { Folder, Layers3, Loader2, Trash2 } from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog"
import { DropdownMenuItem } from "@/components/ui/dropdown-menu"
import { NativeClientError, type NativeNode } from "@/lib/neev-client"
import {
  listTrashedNodes,
  moveNodeToTrash,
  previewNodeTrash,
  restoreNode,
  trashBlocker,
  type NodeTrashBlocker,
  type NodeTrashPreview,
  type TrashedNode,
} from "@/lib/neev-node-trash"
import { useNativeAuth } from "./native-auth-provider"

/**
 * English defaults for the `NeevTrash` namespace, used only for a key a locale
 * lacks. All 10 locales carry the namespace (merged from
 * `.context/project-folder-trash/locale-strings.json`), so this is a guard
 * against drift, not the normal path.
 */
export const NODE_TRASH_MESSAGES = {
  moveToTrash: "Move to Trash",
  confirmTitle: "Move “{name}” to Trash?",
  confirmBody:
    "It disappears from the sidebar for everyone in the organization. Anyone can restore it from Trash, back to the same place. Nothing is deleted.",
  contents: "Also moves:",
  folders: "{count, plural, one {# folder} other {# folders}}",
  documents: "{count, plural, one {# document} other {# documents}}",
  chats: "{count, plural, one {# chat} other {# chats}}",
  agentChats:
    "{count, plural, one {# agent chat of yours} other {# agent chats of yours}}",
  empty: "It has nothing in it.",
  checking: "Checking what is inside…",
  agentActive:
    "An agent is still running in here. Stop it first, then move this to Trash.",
  inUse: "Something in here is being edited right now. Try again in a moment.",
  // Yes/no only: how much private work other members keep here is theirs.
  // (Replaces the earlier count-based `privateOthers` text, now unused.)
  privateContent:
    "Contains private content belonging to another member. They need to move or trash it first.",
  changed: "This changed while you were looking. Check again.",
  failed: "Couldn't move this to Trash. Try again.",
  cancel: "Cancel",
  section: "Projects and folders",
  emptySection: "No projects or folders in Trash.",
  loading: "Loading…",
  loadFailed: "Couldn't load trashed projects and folders.",
  restore: "Restore",
  restoreTo: "Restores to {parent}",
  restoreParentFirst: "Restore {parent} first",
  restoreFailed: "Couldn't restore. Try again.",
  trashedBy: "Trashed by {name}",
} as const

export type NodeTrashKey = keyof typeof NODE_TRASH_MESSAGES

const BLOCKER_TEXT: Record<NodeTrashBlocker, NodeTrashKey> = {
  agentActive: "agentActive",
  inUse: "inUse",
  privateOthers: "privateContent",
}
type Values = Record<string, string | number>

const fallback = createTranslator({
  locale: "en",
  messages: { NeevTrash: NODE_TRASH_MESSAGES },
  namespace: "NeevTrash",
}) as unknown as (key: string, values?: Values) => string

/** `NeevTrash.*` from the locale when it has the key, English otherwise. */
export function useNodeTrashText() {
  const t = useTranslations() as unknown as {
    (key: string, values?: Values): string
    has: (key: string) => boolean
  }
  return useCallback(
    (key: NodeTrashKey, values?: Values) =>
      t.has(`NeevTrash.${key}`)
        ? t(`NeevTrash.${key}`, values)
        : fallback(key, values),
    [t]
  )
}

/**
 * One pending request for the whole workspace. The sidebar tree is recursive
 * and the folder view lives in another panel; a module store lets both ask the
 * single mounted host without threading a prop through every level.
 */
let requested: NativeNode | null = null
let hosts = 0
const listeners = new Set<() => void>()
function emit() {
  for (const listener of listeners) listener()
}
function subscribe(listener: () => void) {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}
export function requestNodeTrash(node: NativeNode | null) {
  requested = node
  emit()
}
/** Entry points render only while a host can answer them. */
function useHosted() {
  return useSyncExternalStore(
    subscribe,
    () => hosts > 0,
    () => false
  )
}

/**
 * The one confirmation dialog. `onTrashed` is told the node that left; its
 * subtree went with it.
 */
export function NodeTrashHost({
  onTrashed,
}: {
  onTrashed: (node: NativeNode) => void
}) {
  const target = useSyncExternalStore(
    subscribe,
    () => requested,
    () => null
  )
  useEffect(() => {
    hosts += 1
    emit()
    return () => {
      hosts -= 1
      requested = null
      emit()
    }
  }, [])
  if (!target) return null
  return (
    <NodeTrashDialog
      key={target.id}
      node={target}
      onClose={() => requestNodeTrash(null)}
      onTrashed={() => {
        requestNodeTrash(null)
        onTrashed(target)
      }}
    />
  )
}

/** Sidebar node menu entry. */
export function MoveNodeToTrashMenuItem({ node }: { node: NativeNode }) {
  const hosted = useHosted()
  const text = useNodeTrashText()
  if (!hosted) return null
  return (
    <DropdownMenuItem
      variant="destructive"
      onSelect={() => requestNodeTrash(node)}
    >
      <Trash2 aria-hidden className="size-4" />
      {text("moveToTrash")}
    </DropdownMenuItem>
  )
}

/** Folder view header entry. */
export function MoveNodeToTrashButton({ node }: { node: NativeNode }) {
  const hosted = useHosted()
  const text = useNodeTrashText()
  if (!hosted) return null
  return (
    <Button variant="ghost" size="sm" onClick={() => requestNodeTrash(node)}>
      <Trash2 aria-hidden className="size-4" />
      {text("moveToTrash")}
    </Button>
  )
}

function NodeTrashDialog({
  node,
  onClose,
  onTrashed,
}: {
  node: NativeNode
  onClose: () => void
  onTrashed: () => void
}) {
  const auth = useNativeAuth()
  const text = useNodeTrashText()
  const [preview, setPreview] = useState<NodeTrashPreview | null>(null)
  const [problem, setProblem] = useState<"changed" | "failed" | null>(null)
  const [busy, setBusy] = useState(false)
  const [check, setCheck] = useState(0)

  useEffect(() => {
    const controller = new AbortController()
    setPreview(null)
    previewNodeTrash(node.id, controller.signal)
      .then((value) => {
        if (!controller.signal.aborted) setPreview(value)
      })
      .catch((error) => {
        if (!controller.signal.aborted) {
          setProblem("failed")
          auth.handleError(error)
        }
      })
    return () => controller.abort()
    // auth.handleError is stable for the session; re-reading on it is noise.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [node.id, check])

  const blocker = preview ? trashBlocker(preview) : null
  async function confirm() {
    const csrf = auth.session?.csrf
    if (!csrf || busy) return
    setBusy(true)
    setProblem(null)
    try {
      await moveNodeToTrash(node.id, node.revision, csrf)
      onTrashed()
    } catch (error) {
      if (error instanceof NativeClientError && error.kind === "conflict") {
        // Blocked since the preview, or the folder changed: show the new state.
        setProblem("changed")
        setCheck((n) => n + 1)
      } else {
        setProblem("failed")
        auth.handleError(error)
      }
    } finally {
      setBusy(false)
    }
  }

  const parts = preview
    ? [
        preview.folders && text("folders", { count: preview.folders }),
        preview.documents && text("documents", { count: preview.documents }),
        preview.chats && text("chats", { count: preview.chats }),
        preview.agentChats && text("agentChats", { count: preview.agentChats }),
      ].filter((part): part is string => Boolean(part))
    : []
  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent className="max-h-[90dvh] overflow-y-auto">
        <DialogTitle>{text("confirmTitle", { name: node.name })}</DialogTitle>
        <DialogDescription>{text("confirmBody")}</DialogDescription>
        <div className="text-sm" aria-live="polite">
          {!preview ? (
            problem !== "failed" && (
              <p className="flex items-center gap-1.5 text-muted-foreground">
                <Loader2 aria-hidden className="size-4 animate-spin" />
                {text("checking")}
              </p>
            )
          ) : parts.length ? (
            <>
              <p className="font-medium">{text("contents")}</p>
              <ul className="mt-1 list-disc ps-5" data-testid="trash-contents">
                {parts.map((part) => (
                  <li key={part}>{part}</li>
                ))}
              </ul>
            </>
          ) : (
            <p className="text-muted-foreground">{text("empty")}</p>
          )}
        </div>
        {preview && blocker && (
          <p role="alert" className="text-sm font-medium text-destructive">
            {text(BLOCKER_TEXT[blocker])}
          </p>
        )}
        {problem && (
          <p role="alert" className="text-sm text-destructive">
            {text(problem)}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <Button variant="outline" disabled={busy} onClick={onClose}>
            {text("cancel")}
          </Button>
          <Button
            variant="destructive"
            disabled={!preview || blocker !== null || busy}
            onClick={() => void confirm()}
          >
            {busy && <Loader2 aria-hidden className="size-4 animate-spin" />}
            {text("moveToTrash")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  )
}

/**
 * Trashed projects and folders, inside the existing Trash view. `onRestored`
 * lets the tree re-read itself; restore puts the node back where it was.
 */
export function NodeTrashSection({ onRestored }: { onRestored: () => void }) {
  const auth = useNativeAuth()
  const text = useNodeTrashText()
  const [items, setItems] = useState<TrashedNode[] | null>(null)
  const [failed, setFailed] = useState(false)
  const [busy, setBusy] = useState<number | null>(null)
  const [restoreFailed, setRestoreFailed] = useState(false)
  const [revision, setRevision] = useState(0)

  useEffect(() => {
    const controller = new AbortController()
    setFailed(false)
    listTrashedNodes(controller.signal)
      .then((value) => {
        if (!controller.signal.aborted) setItems(value)
      })
      .catch((error) => {
        if (!controller.signal.aborted) {
          setFailed(true)
          auth.handleError(error)
        }
      })
    return () => controller.abort()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [revision])

  async function restore(item: TrashedNode) {
    const csrf = auth.session?.csrf
    if (!csrf || busy !== null) return
    setBusy(item.nodeId)
    setRestoreFailed(false)
    try {
      await restoreNode(item.nodeId, csrf)
      onRestored()
    } catch (error) {
      setRestoreFailed(true)
      if (!(error instanceof NativeClientError && error.kind === "conflict"))
        auth.handleError(error)
    } finally {
      setBusy(null)
      setRevision((n) => n + 1)
    }
  }

  return (
    <section
      aria-label={text("section")}
      className="mt-3 border-t border-sidebar-border pt-3"
    >
      <h3 className="px-2 pb-2 text-xs font-medium text-muted-foreground">
        {text("section")}
      </h3>
      {failed ? (
        <div role="alert" className="px-2 py-1 text-sm text-destructive">
          {text("loadFailed")}
        </div>
      ) : items === null ? (
        <div className="flex items-center gap-1.5 px-2 py-1 text-sm text-muted-foreground">
          <Loader2 aria-hidden className="size-4 animate-spin" />
          <span>{text("loading")}</span>
        </div>
      ) : !items.length ? (
        <div className="px-2 py-1 text-sm text-muted-foreground">
          {text("emptySection")}
        </div>
      ) : (
        <ul className="space-y-0.5">
          {items.map((item) => {
            const parent = item.parentName ?? ""
            return (
              <li
                key={item.nodeId}
                className="flex min-w-0 items-center gap-2 rounded-lg px-2 py-1 text-sm text-muted-foreground"
              >
                {item.kind === "project" ? (
                  <Layers3 aria-hidden className="size-3.5 shrink-0" />
                ) : (
                  <Folder aria-hidden className="size-3.5 shrink-0" />
                )}
                <span className="min-w-0 flex-1">
                  <span className="block truncate" title={item.name}>
                    {item.name}
                  </span>
                  <span className="block truncate text-xs text-muted-foreground/70">
                    {item.parentTrashed
                      ? text("restoreParentFirst", { parent })
                      : item.parentName
                        ? text("restoreTo", { parent })
                        : text("trashedBy", { name: item.trashedByName })}
                  </span>
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={item.parentTrashed || busy !== null}
                  aria-label={`${text("restore")} ${item.name}`}
                  onClick={() => void restore(item)}
                >
                  {text("restore")}
                </Button>
              </li>
            )
          })}
        </ul>
      )}
      {restoreFailed && (
        <p role="alert" className="px-2 py-1 text-sm text-destructive">
          {text("restoreFailed")}
        </p>
      )}
    </section>
  )
}
