"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { useTranslations } from "next-intl"
import {
  Bot,
  ChevronRight,
  FileText,
  Folder,
  Layers3,
  Menu,
  MessagesSquare,
  MoreHorizontal,
  PanelLeft,
  Plus,
  Settings2,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogClose,
  DialogDescription,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { useIsMobile } from "@/hooks/use-mobile"
import { cn } from "@/lib/utils"
import {
  DEFAULT_SECTION,
  getActiveSection,
  getOpenResource,
  setActiveSection,
  setOpenResource,
  type Section,
} from "@/lib/neev-workspace-prefs"
import {
  loadNodes,
  nativeMutation,
  parseNode,
  NativeClientError,
  type NativeNode,
} from "@/lib/neev-client"
import { useNativeAuth } from "./native-auth-provider"
import {
  NativeDialogContent as DialogContent,
  NativeDropdownMenuContent as DropdownMenuContent,
} from "./native-suspended-surface"
import { NativeBoundary, NativeError } from "./native-boundary"
import {
  NativeContent,
  type ContentCreation,
  type ContentNavigationGuard,
} from "./native-content"
import { NativeAgentChats } from "./native-agent-chats"
import {
  listNativeResources,
  loadNativeResource,
  type NativeResource,
  type NativeResourceKind,
} from "@/lib/neev-content-client"

/**
 * The container and every ancestor above it, so revealing a nested resource
 * actually reveals it: expanding only its own folder leaves it inside a
 * collapsed project and therefore invisible.
 *
 * Bounded by the node count and cycle-guarded — a corrupt `parentId` chain must
 * not spin here.
 */
function ancestry(nodes: NativeNode[], nodeId: number): number[] {
  const byId = new Map(nodes.map((node) => [node.id, node]))
  const chain: number[] = []
  const seen = new Set<number>()
  let current: number | null = nodeId
  while (
    current !== null &&
    !seen.has(current) &&
    chain.length < nodes.length
  ) {
    seen.add(current)
    chain.push(current)
    current = byId.get(current)?.parentId ?? null
  }
  return chain
}

// Compact nested containers that reveal their real, directly-selectable resources
// (documents or team conversations) inline — navigation happens here, not behind a
// separate index page. Creation always carries the container selected in this menu.
function NativeWorkspaceTree({
  nodes,
  parentId = null,
  expanded,
  kind,
  itemsByNode,
  openResourceId,
  toggle,
  create,
  edit,
  createContent,
  selectResource,
}: {
  nodes: NativeNode[]
  parentId?: number | null
  expanded: Set<number>
  kind: NativeResourceKind
  itemsByNode: Record<
    number,
    { loading: boolean; items: NativeResource[]; error: unknown }
  >
  openResourceId: string | null
  toggle: (id: number) => void
  create: (node: NativeNode) => void
  edit: (node: NativeNode) => void
  createContent: (node: NativeNode, kind: NativeResourceKind) => void
  selectResource: (node: NativeNode, resource: NativeResource) => void
}) {
  const t = useTranslations("Native")
  const c = useTranslations("NativeContent")
  const contentOpener = useRef<number | null>(null)
  const ItemIcon = kind === "document" ? FileText : MessagesSquare
  return (
    <ul
      className={cn(
        "space-y-0.5",
        parentId !== null && "ms-3 border-s border-sidebar-border/60 ps-1.5"
      )}
    >
      {nodes
        .filter((node) => node.parentId === parentId)
        .map((node) => {
          const list = itemsByNode[node.id]
          const isOpen = expanded.has(node.id)
          return (
            <li key={node.id}>
              <div className="group flex min-h-9 min-w-0 items-center rounded-lg pe-1 hover:bg-sidebar-accent">
                <button
                  aria-label={t(isOpen ? "collapse" : "expand", {
                    name: node.name,
                  })}
                  aria-expanded={isOpen}
                  onClick={() => toggle(node.id)}
                  className="flex size-8 shrink-0 items-center justify-center rounded focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <ChevronRight
                    aria-hidden
                    className={cn(
                      "size-3.5 text-muted-foreground rtl:rotate-180",
                      isOpen && "rotate-90 rtl:rotate-90"
                    )}
                  />
                </button>
                <button
                  onClick={() => toggle(node.id)}
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
                  <DropdownMenuContent
                    align="start"
                    onCloseAutoFocus={(event) => {
                      if (contentOpener.current === node.id) {
                        event.preventDefault()
                        contentOpener.current = null
                      }
                    }}
                  >
                    <DropdownMenuItem
                      onSelect={() => {
                        contentOpener.current = node.id
                        createContent(node, "document")
                      }}
                    >
                      {c("newDocument")}
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      onSelect={() => {
                        contentOpener.current = node.id
                        createContent(node, "chat")
                      }}
                    >
                      {c("newChat")}
                    </DropdownMenuItem>
                    <DropdownMenuItem onSelect={() => create(node)}>
                      {t("newSubfolder")}
                    </DropdownMenuItem>
                    <DropdownMenuItem onSelect={() => edit(node)}>
                      {t("editDetails")}
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>
              {isOpen && (
                <>
                  <NativeWorkspaceTree
                    nodes={nodes}
                    parentId={node.id}
                    expanded={expanded}
                    kind={kind}
                    itemsByNode={itemsByNode}
                    openResourceId={openResourceId}
                    toggle={toggle}
                    create={create}
                    edit={edit}
                    createContent={createContent}
                    selectResource={selectResource}
                  />
                  <ul className="ms-3 space-y-0.5 border-s border-sidebar-border/60 ps-1.5">
                    {(list?.items ?? []).map((resource) => (
                      <li key={resource.id}>
                        <button
                          onClick={() => selectResource(node, resource)}
                          aria-current={
                            openResourceId === resource.id ? "page" : undefined
                          }
                          className={cn(
                            "flex min-h-8 w-full min-w-0 items-center gap-2 rounded-lg px-2 text-start text-sm focus-visible:ring-2 focus-visible:ring-ring",
                            openResourceId === resource.id
                              ? "bg-sidebar-accent font-medium text-sidebar-accent-foreground"
                              : "text-muted-foreground hover:bg-sidebar-accent/60"
                          )}
                        >
                          <ItemIcon aria-hidden className="size-3.5 shrink-0" />
                          <span className="truncate" title={resource.title}>
                            {resource.title}
                          </span>
                        </button>
                      </li>
                    ))}
                    {list?.loading && (
                      <li
                        role="status"
                        className="px-2 py-1 text-xs text-muted-foreground"
                      >
                        {t("loading")}
                      </li>
                    )}
                    {list && !list.loading && list.items.length === 0 && (
                      <li className="px-2 py-1 text-xs text-muted-foreground">
                        {c(
                          kind === "document" ? "emptyDocuments" : "emptyChats"
                        )}
                      </li>
                    )}
                    <li>
                      <button
                        onClick={() => createContent(node, kind)}
                        className="flex min-h-8 w-full items-center gap-2 rounded-lg px-2 text-start text-xs text-muted-foreground hover:bg-sidebar-accent/60 focus-visible:ring-2 focus-visible:ring-ring"
                      >
                        <Plus aria-hidden className="size-3.5 shrink-0" />
                        {c(kind === "document" ? "newDocument" : "newChat")}
                      </button>
                    </li>
                  </ul>
                </>
              )}
            </li>
          )
        })}
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
  const org = auth.session?.organization
  const { handleError } = auth
  const [nodes, setNodes] = useState<NativeNode[]>([]),
    [nextAfter, setNextAfter] = useState<number | null>(null),
    [loading, setLoading] = useState(true),
    [error, setError] = useState<unknown>(null)
  const [expanded, setExpanded] = useState(new Set<number>()),
    [selected, setSelected] = useState<number | null>(null),
    [drawer, setDrawer] = useState(false),
    [sidebarOpen, setSidebarOpen] = useState(true),
    // The primary workspace section (Agent Chats / Team Chat / Documents) lives in
    // the left rail and drives the center surface. Persisted per user; SSR-safe
    // default until the effect reads localStorage.
    [section, setSectionState] = useState<Section>(DEFAULT_SECTION)
  useEffect(() => setSectionState(getActiveSection()), [])
  const changeSection = useCallback((next: Section) => {
    navigationGuard.current(() => {
      setSectionState(next)
      setActiveSection(next)
      setDrawer(false)
    })
  }, [])
  // The resource kind shown for the active section. Agent Chats has no backend
  // resources yet (§5) — the center renders an honest connect/setup surface.
  const sectionKind: NativeResourceKind =
    section === "docs" ? "document" : "chat"
  // Lazily-loaded, section-scoped resource lists shown directly under each
  // expanded container in the sidebar (Notion/Slack-style direct navigation).
  type ItemList = { loading: boolean; items: NativeResource[]; error: unknown }
  const [itemsByNode, setItemsByNode] = useState<Record<number, ItemList>>({})
  const [open, setOpen] = useState<{
    resource: NativeResource
    request: number
  } | null>(null)
  const openNumber = useRef(0)
  const itemsRequest = useRef(0)
  // Restoration is attempted once per mount. Until it has run, the persist
  // effect below stays quiet — the section effect nulls `open` on mount, and
  // writing that through would erase the very pointer we are about to read.
  const restored = useRef(false)
  // Section switches change the resource kind: drop cached lists + any open item.
  useEffect(() => {
    itemsRequest.current += 1
    setItemsByNode({})
    setOpen(null)
  }, [section])
  const loadItems = useCallback(
    async (nodeId: number) => {
      if (!org) return
      const stamp = itemsRequest.current
      setItemsByNode((old) => ({
        ...old,
        [nodeId]: {
          loading: true,
          items: old[nodeId]?.items ?? [],
          error: null,
        },
      }))
      try {
        const page = await listNativeResources(
          org.id,
          nodeId,
          sectionKind,
          false,
          null,
          auth.signal
        )
        if (stamp !== itemsRequest.current) return
        setItemsByNode((old) => ({
          ...old,
          [nodeId]: { loading: false, items: page.items, error: null },
        }))
      } catch (e) {
        if (stamp !== itemsRequest.current) return
        setItemsByNode((old) => ({
          ...old,
          [nodeId]: {
            loading: false,
            items: old[nodeId]?.items ?? [],
            error: e,
          },
        }))
        // A 403/401 on listing a container's items reveals revoked access —
        // route through the authority handler so membership loss re-bootstraps.
        auth.handleAuthorityError(e)
      }
    },
    [org, sectionKind, auth]
  )
  // A section switch keeps containers expanded but invalidates their lists.
  // Fetch missing lists after invalidation as well as on first expansion.
  useEffect(() => {
    if (section === "agents") return
    for (const id of expanded) {
      if (!itemsByNode[id]) void loadItems(id)
    }
  }, [expanded, itemsByNode, loadItems, section])
  const openResource = useCallback(
    (node: NativeNode, resource: NativeResource) =>
      navigationGuard.current(() => {
        setSelected(node.id)
        setCreation(undefined)
        setOpen({ resource, request: ++openNumber.current })
        setDrawer(false)
      }),
    []
  )
  const user = auth.session?.user.id
  const orgId = org?.id
  // Remember only WHICH resource is open, scoped to this user and org. The
  // pointer confers nothing: it is re-fetched and re-authorized on restore.
  useEffect(() => {
    if (!restored.current || user === undefined || orgId === undefined) return
    setOpenResource(
      user,
      orgId,
      open
        ? { resourceId: open.resource.id, nodeId: open.resource.nodeId }
        : null
    )
  }, [open, user, orgId])
  // Bring the reader back to what they were looking at after a reload or a
  // dropped connection. Runs once, after the tree has loaded so the node the
  // resource hangs under is known.
  useEffect(() => {
    if (
      restored.current ||
      loading ||
      user === undefined ||
      orgId === undefined
    )
      return
    const stored = getOpenResource(user, orgId)
    if (!stored) {
      restored.current = true
      return
    }
    const controller = new AbortController()
    const restore = async () => {
      try {
        // The server is the authority: this 404s for a deleted resource and for
        // one this member may no longer see, and `loadNativeResource` refuses a
        // row belonging to another organization.
        const resource = await loadNativeResource(
          orgId,
          stored.resourceId,
          controller.signal
        )
        if (controller.signal.aborted) return
        const kind: NativeResourceKind =
          section === "docs" ? "document" : "chat"
        // Only restore into the section that actually shows this kind, and only
        // when its container is on screen; anything else would land the reader
        // somewhere they did not leave.
        if (
          resource.kind === kind &&
          resource.nodeId === stored.nodeId &&
          nodes.some((node) => node.id === resource.nodeId)
        ) {
          setSelected(resource.nodeId)
          setExpanded(
            (old) => new Set([...old, ...ancestry(nodes, resource.nodeId)])
          )
          setOpen({ resource, request: ++openNumber.current })
          void loadItems(resource.nodeId)
        }
      } catch {
        // Missing, deleted or no longer permitted: restore nothing and leave
        // the reader in the section they chose. Never surfaced as an error —
        // they did not ask for this fetch.
      } finally {
        if (!controller.signal.aborted) restored.current = true
      }
    }
    void restore()
    return () => controller.abort()
  }, [loading, user, orgId, section, nodes, loadItems])
  const [editor, setEditor] = useState<Editor | null>(null),
    [name, setName] = useState(""),
    [parent, setParent] = useState<number | null>(null),
    [saving, setSaving] = useState(false),
    [editError, setEditError] = useState<unknown>(null)
  const opener = useRef<HTMLElement | null>(null)
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
  const sectionItems: { id: Section; label: string; icon: typeof Bot }[] = [
    { id: "agents", label: c("agentChats"), icon: Bot },
    { id: "team", label: c("teamChat"), icon: MessagesSquare },
    { id: "docs", label: c("documents"), icon: FileText },
  ]
  const sidebar = (
    <div className="flex h-full min-w-0 flex-col gap-4 p-3">
      <nav aria-label={t("sections")} className="space-y-1">
        {sectionItems.map((item) => (
          <button
            key={item.id}
            onClick={() => changeSection(item.id)}
            aria-current={section === item.id ? "page" : undefined}
            className={cn(
              "flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-sm font-medium focus-visible:ring-2 focus-visible:ring-ring",
              section === item.id
                ? "bg-sidebar-accent text-sidebar-accent-foreground"
                : "text-muted-foreground hover:bg-sidebar-accent/60"
            )}
          >
            <item.icon aria-hidden className="size-4 shrink-0" />
            {item.label}
          </button>
        ))}
      </nav>
      <nav
        aria-label={t("projects")}
        className="flex min-h-0 flex-1 flex-col gap-2"
      >
        <div className="flex items-center justify-between gap-2 px-2 pt-1">
          <h2 className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
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
            kind={sectionKind}
            itemsByNode={itemsByNode}
            openResourceId={open?.resource.id ?? null}
            toggle={(id) =>
              setExpanded((old) => {
                const next = new Set(old)
                if (next.has(id)) next.delete(id)
                else {
                  next.add(id)
                }
                return next
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
                // Cross-section creation: switch to the created kind's section so the
                // new item lands and appears under this container in the right list.
                const nextSection = kind === "document" ? "docs" : "team"
                setSectionState(nextSection)
                setActiveSection(nextSection)
                setSelected(node.id)
                setOpen(null)
                setExpanded((old) => new Set([...old, node.id]))
                setCreation({
                  kind,
                  request: ++creationNumber.current,
                  returnFocus,
                })
                setDrawer(false)
              })
            }}
            selectResource={openResource}
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
      </nav>
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
    </div>
  )
  return (
    <div className="flex h-dvh min-w-0 flex-col overflow-hidden">
      <header className="flex min-h-16 shrink-0 items-center gap-3 border-b px-4 sm:px-6">
        {!mobile && (
          <Button
            variant="ghost"
            size="icon"
            aria-label={t("openNavigation")}
            title={t("openNavigation")}
            aria-expanded={sidebarOpen}
            onClick={() => setSidebarOpen(!sidebarOpen)}
          >
            <PanelLeft aria-hidden />
          </Button>
        )}
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
        {!mobile && sidebarOpen && (
          <aside className="w-64 shrink-0 border-e bg-sidebar text-sidebar-foreground">
            {sidebar}
          </aside>
        )}
        <main className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
          {section === "agents" ? (
            // Saved, server-owned agent sessions (spec §5). They are private to
            // the owner and are not filed under a project/folder, so this
            // surface is available whatever the rail has selected.
            <NativeAgentChats
              onRequestNavigationGuard={registerGuard}
              onRequestSettings={() =>
                navigationGuard.current(() => router.push("/settings"))
              }
            />
          ) : selectedNode && (open || creation) ? (
            <NativeContent
              node={selectedNode}
              chromeless
              open={open ?? undefined}
              creation={creation}
              onCreationCancelled={() => setCreation(undefined)}
              onRequestNavigationGuard={registerGuard}
              onOpened={(resource) => {
                setOpen({ resource, request: ++openNumber.current })
                setCreation(undefined)
                setExpanded((old) => new Set([...old, resource.nodeId]))
                void loadItems(resource.nodeId)
              }}
            />
          ) : (
            <div className="mx-auto flex w-full max-w-md flex-col items-center gap-4 px-6 py-24 text-center">
              <div className="flex size-11 items-center justify-center rounded-xl bg-muted">
                {section === "docs" ? (
                  <FileText
                    aria-hidden
                    className="size-5 text-muted-foreground"
                  />
                ) : (
                  <MessagesSquare
                    aria-hidden
                    className="size-5 text-muted-foreground"
                  />
                )}
              </div>
              <h1 className="text-xl font-semibold tracking-tight">
                {section === "docs" ? c("documents") : c("teamChat")}
              </h1>
              <p className="max-w-sm text-sm leading-6 text-muted-foreground">
                {t("sectionEmpty")}
              </p>
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
