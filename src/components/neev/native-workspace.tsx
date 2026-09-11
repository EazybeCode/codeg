"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { useTranslations } from "next-intl"
import {
  ChevronRight,
  Folder,
  Layers3,
  Menu,
  MoreHorizontal,
  Plus,
  Settings2,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { useIsMobile } from "@/hooks/use-mobile"
import { cn } from "@/lib/utils"
import {
  loadNodes,
  nativeMutation,
  parseNode,
  NativeClientError,
  type NativeNode,
} from "@/lib/neev-client"
import { useNativeAuth } from "./native-auth-provider"
import { NativeBoundary, NativeError } from "./native-boundary"
import {
  NativeContent,
  type ContentCreation,
  type ContentNavigationGuard,
} from "./native-content"
import type { NativeResourceKind } from "@/lib/neev-content-client"

// Native adapter of TeamWorkspaceTree's compact nested rows and focusable menus.
// Content creation always carries the native node selected in this menu.
function NativeWorkspaceTree({
  nodes,
  parentId = null,
  expanded,
  selected,
  toggle,
  select,
  create,
  edit,
  createContent,
}: {
  nodes: NativeNode[]
  parentId?: number | null
  expanded: Set<number>
  selected: number | null
  toggle: (id: number) => void
  select: (id: number) => void
  create: (node: NativeNode) => void
  edit: (node: NativeNode) => void
  createContent: (node: NativeNode, kind: NativeResourceKind) => void
}) {
  const t = useTranslations("Native")
  const c = useTranslations("NativeContent")
  return (
    <ul
      className={cn(
        "space-y-0.5",
        parentId !== null && "ms-3 border-s border-sidebar-border/60 ps-1.5"
      )}
    >
      {nodes
        .filter((node) => node.parentId === parentId)
        .map((node) => (
          <li key={node.id}>
            <div
              className={cn(
                "group flex min-h-9 min-w-0 items-center rounded-lg pe-1 hover:bg-sidebar-accent",
                selected === node.id && "bg-sidebar-accent"
              )}
            >
              <button
                aria-label={t(expanded.has(node.id) ? "collapse" : "expand", {
                  name: node.name,
                })}
                aria-expanded={expanded.has(node.id)}
                onClick={() => toggle(node.id)}
                className="flex size-8 shrink-0 items-center justify-center rounded focus-visible:ring-2 focus-visible:ring-ring"
              >
                <ChevronRight
                  aria-hidden
                  className={cn(
                    "size-3.5 text-muted-foreground rtl:rotate-180",
                    expanded.has(node.id) && "rotate-90 rtl:rotate-90"
                  )}
                />
              </button>
              <button
                onClick={() => select(node.id)}
                aria-current={selected === node.id ? "page" : undefined}
                className="flex min-h-9 min-w-0 flex-1 items-center gap-2 rounded text-start text-sm focus-visible:ring-2 focus-visible:ring-ring"
              >
                {node.kind === "project" ? (
                  <Layers3
                    aria-hidden
                    className="size-4 shrink-0 text-muted-foreground"
                  />
                ) : (
                  <Folder
                    aria-hidden
                    className="size-4 shrink-0 text-muted-foreground"
                  />
                )}
                <span className="truncate" title={node.name}>
                  {node.name}
                </span>
              </button>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button
                    aria-label={t("actions", { name: node.name })}
                    className="flex size-8 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring"
                  >
                    <MoreHorizontal aria-hidden className="size-4" />
                  </button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start">
                  <DropdownMenuItem onSelect={() => create(node)}>
                    {t("newSubfolder")}
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onSelect={() => createContent(node, "chat")}
                  >
                    {c("newChat")}
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onSelect={() => createContent(node, "document")}
                  >
                    {c("newDocument")}
                  </DropdownMenuItem>
                  <DropdownMenuItem onSelect={() => edit(node)}>
                    {t("editDetails")}
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
            {expanded.has(node.id) && (
              <NativeWorkspaceTree
                nodes={nodes}
                parentId={node.id}
                expanded={expanded}
                selected={selected}
                toggle={toggle}
                select={select}
                create={create}
                edit={edit}
                createContent={createContent}
              />
            )}
          </li>
        ))}
    </ul>
  )
}
type Editor = {
  kind: "project" | "folder"
  parentId: number | null
  node?: NativeNode
}
function WorkspaceContent() {
  const t = useTranslations("Native"),
    auth = useNativeAuth(),
    mobile = useIsMobile()
  const router = useRouter(),
    c = useTranslations("NativeContent")
  const navigationGuard = useRef<ContentNavigationGuard>((action) => action())
  const registerGuard = useCallback((guard: ContentNavigationGuard) => {
    navigationGuard.current = guard
  }, [])
  const [creation, setCreation] = useState<ContentCreation | undefined>()
  const creationNumber = useRef(0)
  const [nodes, setNodes] = useState<NativeNode[]>([]),
    [nextAfter, setNextAfter] = useState<number | null>(null),
    [loading, setLoading] = useState(true),
    [error, setError] = useState<unknown>(null)
  const [expanded, setExpanded] = useState(new Set<number>()),
    [selected, setSelected] = useState<number | null>(null),
    [drawer, setDrawer] = useState(false)
  const [editor, setEditor] = useState<Editor | null>(null),
    [name, setName] = useState(""),
    [parent, setParent] = useState<number | null>(null),
    [saving, setSaving] = useState(false),
    [editError, setEditError] = useState<unknown>(null)
  const opener = useRef<HTMLElement | null>(null)
  const org = auth.session?.organization
  const { handleError } = auth
  useEffect(() => {
    if (!org) return
    const controller = new AbortController()
    void loadNodes(org.id, null, controller.signal)
      .then((page) => {
        if (!controller.signal.aborted) {
          setNodes(page.nodes)
          setNextAfter(page.nextAfter)
        }
      })
      .catch((e) => {
        if (!controller.signal.aborted) {
          setError(e)
          handleError(e)
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false)
      })
    return () => controller.abort()
  }, [org, handleError])
  if (!org || !auth.session) return null
  const selectedNode = nodes.find((node) => node.id === selected)
  const breadcrumbs: NativeNode[] = []
  let ancestor = selectedNode
  while (ancestor && !breadcrumbs.some((node) => node.id === ancestor?.id)) {
    breadcrumbs.unshift(ancestor)
    ancestor = nodes.find((node) => node.id === ancestor?.parentId)
  }
  function openEditor(value: Editor) {
    const active = document.activeElement as HTMLElement | null
    const menu = active?.closest('[role="menu"]')
    // Menu items unmount when the editor opens. Restore its durable trigger.
    opener.current =
      document.getElementById(menu?.getAttribute("aria-labelledby") ?? "") ??
      active
    setName(value.node?.name ?? "")
    setParent(value.parentId)
    setEditError(null)
    setEditor(value)
  }
  function isDescendant(candidate: NativeNode): boolean {
    const visited = new Set<number>()
    let node: NativeNode | undefined = candidate
    while (node && !visited.has(node.id)) {
      if (node.id === editor?.node?.id) return true
      visited.add(node.id)
      node = nodes.find((n) => n.id === node?.parentId)
    }
    return false
  }
  async function save() {
    if (!editor || !auth.session || !org || saving) return
    setSaving(true)
    setEditError(null)
    try {
      const body = {
        kind: editor.kind,
        name: name.trim(),
        parentId: parent,
        ...(editor.node ? { expectedRevision: editor.node.revision } : {}),
      }
      const node = parseNode(
        await nativeMutation(
          `/api/neev/nodes${editor.node ? `/${editor.node.id}` : ""}`,
          body,
          auth.session.csrf,
          auth.signal,
          editor.node ? "PATCH" : "POST"
        )
      )
      if (node.orgId !== org.id) throw new NativeClientError("invalid_response")
      if (auth.signal?.aborted) return
      setNodes((old) =>
        [...old.filter((n) => n.id !== node.id), node].sort(
          (a, b) => a.id - b.id
        )
      )
      setSelected(node.id)
      if (node.parentId !== null)
        setExpanded((old) => new Set([...old, node.parentId!]))
      setEditor(null)
    } catch (e) {
      if (!auth.signal?.aborted) {
        setEditError(e)
        auth.handleError(e)
      }
    } finally {
      if (!auth.signal?.aborted) setSaving(false)
    }
  }
  async function more() {
    if (!org || nextAfter === null || loading) return
    setLoading(true)
    setError(null)
    try {
      const page = await loadNodes(org.id, nextAfter, auth.signal)
      setNodes((old) =>
        [
          ...new Map(
            [...old, ...page.nodes].map((node) => [node.id, node])
          ).values(),
        ].sort((a, b) => a.id - b.id)
      )
      setNextAfter(page.nextAfter)
    } catch (e) {
      setError(e)
      auth.handleError(e)
    } finally {
      setLoading(false)
    }
  }
  const sidebar = (
    <nav
      aria-label={t("projects")}
      className="flex h-full min-w-0 flex-col gap-4 p-3"
    >
      <div className="flex items-center justify-between gap-2 px-2 pt-2">
        <h2 className="text-xs font-medium text-muted-foreground">
          {t("projects")}
        </h2>
      </div>
      <Button
        variant="ghost"
        className="justify-start gap-2"
        onClick={() =>
          navigationGuard.current(() =>
            openEditor({ kind: "project", parentId: null })
          )
        }
      >
        <Plus aria-hidden className="size-4" />
        {t("newProject")}
      </Button>
      <div className="min-h-0 flex-1 overflow-y-auto">
        <NativeWorkspaceTree
          nodes={nodes}
          expanded={expanded}
          selected={selected}
          toggle={(id) =>
            setExpanded((old) => {
              const next = new Set(old)
              if (next.has(id)) next.delete(id)
              else next.add(id)
              return next
            })
          }
          select={(id) =>
            navigationGuard.current(() => {
              setSelected(id)
              setCreation(undefined)
              setDrawer(false)
            })
          }
          create={(node) =>
            navigationGuard.current(() =>
              openEditor({ kind: "folder", parentId: node.id })
            )
          }
          edit={(node) =>
            navigationGuard.current(() =>
              openEditor({ kind: node.kind, parentId: node.parentId, node })
            )
          }
          createContent={(node, kind) => {
            const active = document.activeElement as HTMLElement | null
            const menu = active?.closest('[role="menu"]')
            const returnFocus =
              document.getElementById(
                menu?.getAttribute("aria-labelledby") ?? ""
              ) ?? active
            navigationGuard.current(() => {
              setSelected(node.id)
              setCreation({
                kind,
                request: ++creationNumber.current,
                returnFocus,
              })
              setDrawer(false)
            })
          }}
        />
        {!loading && nodes.length === 0 && (
          <p className="px-2 text-sm text-muted-foreground">
            {t("emptyProjects")}
          </p>
        )}
        {nextAfter !== null && (
          <Button
            variant="ghost"
            disabled={loading}
            onClick={() => void more()}
          >
            {t("loadMore")}
          </Button>
        )}
        <p role="status" className="px-2 text-xs text-muted-foreground">
          {loading ? t("loading") : ""}
        </p>
        <NativeError error={error} />
      </div>
      <div className="space-y-1 border-t border-sidebar-border pt-3">
        <Button asChild variant="ghost" className="w-full justify-start gap-2">
          <Link
            href="/settings"
            onClick={(event) => {
              event.preventDefault()
              navigationGuard.current(() => router.push("/settings"))
            }}
          >
            <Settings2 aria-hidden className="size-4" />
            {t("settings")}
          </Link>
        </Button>
        <Button
          variant="ghost"
          className="w-full justify-start text-muted-foreground"
          onClick={() => void auth.logout()}
        >
          {t("signOut")}
        </Button>
      </div>
    </nav>
  )
  return (
    <div className="flex h-dvh min-w-0 flex-col overflow-hidden">
      <header className="flex min-h-16 shrink-0 items-center gap-3 border-b px-4 sm:px-6">
        {mobile && (
          <Dialog open={drawer} onOpenChange={setDrawer}>
            <DialogTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                aria-label={t("openNavigation")}
              >
                <Menu aria-hidden />
              </Button>
            </DialogTrigger>
            <DialogContent
              showCloseButton={false}
              className="h-[calc(100dvh-2rem)] max-w-sm gap-2 p-2"
            >
              <DialogTitle className="sr-only">{t("projects")}</DialogTitle>
              <DialogDescription className="sr-only">
                {t("navigationDescription")}
              </DialogDescription>
              <DialogClose asChild>
                <Button variant="ghost" className="justify-self-end">
                  {t("close")}
                </Button>
              </DialogClose>
              {sidebar}
            </DialogContent>
          </Dialog>
        )}
        <div className="flex min-w-0 flex-1 items-center gap-2">
          {org.avatarUrl?.startsWith("data:image/") ? (
            <span
              role="img"
              aria-label={t("organizationLogo")}
              className="size-8 shrink-0 rounded-md bg-contain bg-center bg-no-repeat"
              style={{ backgroundImage: `url(${org.avatarUrl})` }}
            />
          ) : (
            <span
              aria-hidden
              className="flex size-8 shrink-0 items-center justify-center rounded-md bg-muted text-sm font-semibold"
            >
              {org.name.slice(0, 1)}
            </span>
          )}
          <span className="truncate text-sm font-semibold" title={org.name}>
            {org.name}
          </span>
        </div>
        <span
          className="max-w-[35%] truncate text-xs text-muted-foreground"
          title={auth.session.user.displayName}
        >
          {auth.session.user.displayName}
        </span>
      </header>
      <div className="flex min-h-0 min-w-0 flex-1">
        {!mobile && (
          <aside className="w-64 shrink-0 border-e bg-sidebar text-sidebar-foreground">
            {sidebar}
          </aside>
        )}
        <main className="min-w-0 flex-1 overflow-y-auto p-5 sm:p-10">
          <nav
            aria-label={t("breadcrumbs")}
            className="flex flex-wrap items-center gap-1 text-xs text-muted-foreground"
          >
            {breadcrumbs.map((node, index) => (
              <span
                className="flex min-w-0 max-w-full items-center gap-1"
                key={node.id}
              >
                {index > 0 && (
                  <ChevronRight
                    aria-hidden
                    className="size-3 shrink-0 rtl:rotate-180"
                  />
                )}
                <button
                  className="truncate rounded px-1 py-1 hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring"
                  title={node.name}
                  onClick={() =>
                    navigationGuard.current(() => {
                      setSelected(node.id)
                      setCreation(undefined)
                    })
                  }
                >
                  {node.name}
                </button>
              </span>
            ))}
          </nav>
          {selectedNode ? (
            <NativeContent
              node={selectedNode}
              creation={creation}
              onRequestNavigationGuard={registerGuard}
            />
          ) : (
            <div className="mx-auto mt-12 max-w-xl space-y-5 sm:mt-20">
              <div className="flex size-11 items-center justify-center rounded-xl bg-muted">
                <Layers3 aria-hidden className="size-5 text-muted-foreground" />
              </div>
              <h1 className="break-words text-3xl font-semibold tracking-tight">
                {t("workspaceTitle")}
              </h1>
              <p className="text-sm leading-6 text-muted-foreground">
                {c("noSelection")}
              </p>
              <section className="space-y-2 rounded-xl border p-5">
                <h2 className="text-sm font-medium">{t("agentUnavailable")}</h2>
                <p className="text-sm leading-6 text-muted-foreground">
                  {t("agentDescription")}
                </p>
                <Link
                  href="/settings"
                  className="inline-block rounded text-sm underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
                >
                  {t("personalAgents")}
                </Link>
              </section>
            </div>
          )}
        </main>
      </div>
      <Dialog
        open={editor !== null}
        onOpenChange={(open) => {
          if (!open && !saving) setEditor(null)
        }}
      >
        <DialogContent
          showCloseButton={false}
          onCloseAutoFocus={(event) => {
            event.preventDefault()
            opener.current?.focus()
          }}
        >
          <DialogTitle>
            {t(
              editor?.node
                ? "editDetails"
                : editor?.kind === "folder"
                  ? "newSubfolder"
                  : "newProject"
            )}
          </DialogTitle>
          <DialogDescription>
            {editor?.parentId
              ? t("insideFolder", {
                  name:
                    nodes.find((node) => node.id === editor.parentId)?.name ??
                    "",
                })
              : t("projectDescription")}
          </DialogDescription>
          <form
            onSubmit={(event) => {
              event.preventDefault()
              void save()
            }}
            className="space-y-4"
          >
            <label className="block space-y-2">
              <span className="text-sm">{t("name")}</span>
              <input
                autoFocus
                required
                maxLength={100}
                value={name}
                disabled={saving}
                onChange={(event) => setName(event.target.value)}
                className="h-10 w-full min-w-0 rounded-lg border bg-background px-3 text-base md:text-sm"
              />
            </label>
            {editor?.node?.kind === "folder" && (
              <label className="block space-y-2">
                <span className="text-sm">{t("parentFolder")}</span>
                <select
                  value={parent ?? ""}
                  onChange={(event) => setParent(Number(event.target.value))}
                  disabled={saving}
                  className="h-10 w-full min-w-0 rounded-lg border bg-background px-3"
                >
                  {nodes
                    .filter((node) => !isDescendant(node))
                    .map((node) => (
                      <option key={node.id} value={node.id}>
                        {node.name} · #{node.id}
                      </option>
                    ))}
                </select>
              </label>
            )}
            <NativeError error={editError} />
            <p role="status" className="text-sm">
              {saving ? t("working") : ""}
            </p>
            <div className="flex justify-end gap-2">
              <Button
                variant="ghost"
                type="button"
                disabled={saving}
                onClick={() => setEditor(null)}
              >
                {t("cancel")}
              </Button>
              <Button type="submit" disabled={saving || !name.trim()}>
                {t("save")}
              </Button>
            </div>
          </form>
        </DialogContent>
      </Dialog>
    </div>
  )
}
export function NativeWorkspace() {
  return (
    <NativeBoundary>
      <WorkspaceContent />
    </NativeBoundary>
  )
}
