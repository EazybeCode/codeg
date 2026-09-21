"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { useRouter } from "next/navigation"
import { useTheme } from "next-themes"
import {
  closeTab as closeWorkspaceTab,
  closeTabByKey,
  openTab as openWorkspaceTab,
  readTabs,
  tabKey,
  writeTabs,
  type WorkspaceTab,
} from "@/lib/neev-workspace-tabs"
import { useTranslations } from "next-intl"
import {
  Bot,
  ChevronRight,
  FileText,
  Check,
  Folder,
  Layers3,
  Menu,
  MessagesSquare,
  Monitor,
  Moon,
  MoreHorizontal,
  PanelLeft,
  Plus,
  Search,
  Settings2,
  X,
  Sun,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogDescription,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { useIsMobile } from "@/hooks/use-mobile"
import { cn } from "@/lib/utils"
import {
  DEFAULT_SECTION,
  getActiveSection,
  getOpenAgentSession,
  setOpenAgentSession,
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
import { NativeTabPanel } from "./native-suspended-surface"
import {
  listAgentSessions,
  NEEV_AGENT_PROVIDERS,
  type NeevAgentProvider,
  type NeevAgentSession,
} from "@/lib/neev-agent-sessions-client"
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
const CONTENT_KINDS: NativeResourceKind[] = ["document", "chat"]

/**
 * What a project actually holds, as its own page.
 *
 * The centre used to be a generic invitation to create a project even when the
 * reader had several, which told them nothing about the one they had selected.
 * This lists its documents, its team chats and its agent chats together, in one
 * measure, with the same five ways to add that the sidebar menu offers.
 */

/** A container and every ancestor above it, outermost first. */
function trail(nodes: NativeNode[], nodeId: number): string[] {
  const byId = new Map(nodes.map((node) => [node.id, node]))
  const names: string[] = []
  let current = byId.get(nodeId)
  // Bounded by the node count: a corrupt `parentId` chain must not spin.
  for (let step = 0; current && step <= nodes.length; step += 1) {
    names.unshift(current.name)
    current = current.parentId === null ? undefined : byId.get(current.parentId)
  }
  return names
}

function TabIcon({ tab }: { tab: WorkspaceTab }) {
  const className = "size-3.5 shrink-0"
  if (tab.kind === "project")
    return <Layers3 aria-hidden className={className} />
  if (tab.kind === "agent" || tab.kind === "draft")
    return <Bot aria-hidden className={className} />
  return <FileText aria-hidden className={className} />
}

/**
 * A document or team chat, resolved before it is rendered.
 *
 * The sidebar's lists are lazy and paginated, so a tab's resource is often not
 * in them — being absent from a catalog is not the same as being gone. This
 * asks for the resource itself, and only a refusal from that authorized read
 * closes the tab.
 */
function ResourcePanel({
  orgId,
  resourceId,
  node,
  cached,
  registerGuard,
  onGone,
}: {
  orgId: number
  resourceId: string
  node: NativeNode | undefined
  cached: NativeResource | undefined
  registerGuard: (guard: ContentNavigationGuard) => void
  onGone: () => void
}) {
  const auth = useNativeAuth(),
    t = useTranslations("Native")
  // Seeded from the sidebar's list when it happens to hold this one, and
  // otherwise fetched below. Either way it is read once: the panel then owns
  // what it is showing.
  const [loaded, setLoaded] = useState<NativeResource | null>(null)
  const [error, setError] = useState<unknown>(null)
  const [request, setRequest] = useState(0)
  const resource = loaded ?? cached ?? null
  useEffect(() => {
    if (resource || !node) return
    const controller = new AbortController()
    const load = async () => {
      try {
        const found = await loadNativeResource(
          orgId,
          resourceId,
          controller.signal
        )
        if (controller.signal.aborted) return
        setRequest((value) => value + 1)
        setLoaded(found)
      } catch (cause) {
        if (controller.signal.aborted) return
        auth.handleAuthorityError(cause)
        // Gone or no longer permitted, from the resource's OWN read — not
        // from its absence in a lazily-loaded catalog.
        if (
          cause instanceof NativeClientError &&
          (cause.status === 404 || cause.status === 403)
        )
          onGone()
        else setError(cause)
      }
    }
    void load()
    return () => controller.abort()
  }, [resource, node, orgId, resourceId, auth, onGone])

  if (error) return <NativeError error={error} />
  if (!node || !resource)
    return (
      <p role="status" className="px-6 py-10 text-sm text-muted-foreground">
        {t("loading")}
      </p>
    )
  return (
    <NativeContent
      node={node}
      chromeless
      open={{ resource, request }}
      onRequestNavigationGuard={registerGuard}
    />
  )
}

/**
 * A project's own page: what it holds, and the ways to add to it.
 */
function ProjectPanel({
  node,
  items,
  loading,
  sessions,
  onOpenResource,
  onOpenSession,
  onCreate,
}: {
  node: NativeNode | undefined
  items: NativeResource[]
  loading: boolean
  sessions: NeevAgentSession[]
  onOpenResource: (resource: NativeResource) => void
  onOpenSession: (session: NeevAgentSession) => void
  onCreate: (kind: NativeResourceKind | "folder" | NeevAgentProvider) => void
}) {
  if (!node) return null
  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto w-full max-w-[760px] px-6 pb-24 pt-14">
        <h1 className="break-words text-[2.5rem] font-bold leading-[1.2] tracking-tight">
          {node.name}
        </h1>
        <ProjectContents
          node={node}
          items={items}
          loading={loading}
          sessions={sessions}
          onOpenResource={onOpenResource}
          onOpenSession={onOpenSession}
          onCreate={onCreate}
        />
      </div>
    </div>
  )
}

/** Nothing open yet: the projects there are, and the way to make one. */
function WorkspaceStart({
  nodes,
  onOpen,
  onCreate,
}: {
  nodes: NativeNode[]
  onOpen: (node: NativeNode) => void
  onCreate: () => void
}) {
  const t = useTranslations("Native")
  const roots = nodes.filter((node) => node.parentId === null)
  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto w-full max-w-[760px] px-6 pb-24 pt-14">
        <h1 className="break-words text-[2.5rem] font-bold leading-[1.2] tracking-tight">
          {t("projects")}
        </h1>
        <p className="mt-3 text-sm leading-6 text-muted-foreground">
          {roots.length === 0 ? t("emptyProjects") : t("projectsLanding")}
        </p>
        <ul className="mt-6 space-y-0.5">
          {roots.map((node) => (
            <li key={node.id}>
              <button
                onClick={() => onOpen(node)}
                className="flex w-full items-center gap-2.5 rounded-lg px-2 py-2 text-start text-sm hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
              >
                <Layers3
                  aria-hidden
                  className="size-4 shrink-0 text-muted-foreground"
                />
                <span className="truncate">{node.name}</span>
              </button>
            </li>
          ))}
        </ul>
        <Button variant="outline" size="sm" className="mt-6" onClick={onCreate}>
          <Plus aria-hidden className="size-4" />
          {t("newProject")}
        </Button>
      </div>
    </div>
  )
}

function ProjectContents({
  node,
  items,
  loading,
  sessions,
  onOpenResource,
  onOpenSession,
  onCreate,
}: {
  node: NativeNode
  items: NativeResource[]
  loading: boolean
  sessions: NeevAgentSession[]
  onOpenResource: (resource: NativeResource) => void
  onOpenSession: (session: NeevAgentSession) => void
  onCreate: (kind: NativeResourceKind | "folder" | NeevAgentProvider) => void
}) {
  const t = useTranslations("Native"),
    c = useTranslations("NativeContent")
  const empty = !loading && items.length === 0 && sessions.length === 0
  const row =
    "flex w-full min-w-0 items-center gap-2.5 rounded-lg px-2 py-2 text-start text-sm hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
  return (
    <div className="mt-8 space-y-6">
      <div className="flex flex-wrap gap-2">
        {(
          [
            ["document", c("newDocument"), FileText],
            ["chat", c("newChat"), MessagesSquare],
            ["claude", c("newClaudeChat"), Bot],
            ["codex", c("newCodexChat"), Bot],
            ["folder", t("newSubfolder"), Folder],
          ] as const
        ).map(([kind, label, Icon]) => (
          <Button
            key={kind}
            variant="outline"
            size="sm"
            onClick={() => onCreate(kind)}
          >
            <Icon aria-hidden className="size-4" />
            {label}
          </Button>
        ))}
      </div>
      {loading && (
        <p role="status" className="text-sm text-muted-foreground">
          {t("loading")}
        </p>
      )}
      {empty && (
        <p className="text-sm leading-6 text-muted-foreground">
          {t("sectionEmpty")}
        </p>
      )}
      {(items.length > 0 || sessions.length > 0) && (
        <ul aria-label={node.name} className="space-y-0.5">
          {items.map((resource) => (
            <li key={resource.id}>
              <button className={row} onClick={() => onOpenResource(resource)}>
                {resource.kind === "document" ? (
                  <FileText
                    aria-hidden
                    className="size-4 shrink-0 text-muted-foreground"
                  />
                ) : (
                  <MessagesSquare
                    aria-hidden
                    className="size-4 shrink-0 text-muted-foreground"
                  />
                )}
                <span className="truncate">{resource.title}</span>
              </button>
            </li>
          ))}
          {sessions.map((session) => (
            <li key={session.id}>
              <button className={row} onClick={() => onOpenSession(session)}>
                <Bot
                  aria-hidden
                  className="size-4 shrink-0 text-muted-foreground"
                />
                <span className="truncate">{session.title}</span>
                <span className="shrink-0 text-[10px] uppercase tracking-wide text-muted-foreground/70">
                  {session.provider}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

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
  openAgents,
  agentsOpen,
  agentSessions,
  selectNode,
  selectedNodeId,
  openAgentSessionId,
  openAgentSession,
  startAgentChat,
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
  /** Agent chats are private to their owner and filed under a node by the
   *  server (schema 12). Opening them from here moves nothing. */
  openAgents: (node: NativeNode) => void
  agentsOpen: boolean
  /** The caller's own saved agent chats, already filed under a node. */
  agentSessions: NeevAgentSession[]
  /** Opening a container shows its contents; the chevron only folds it. */
  selectNode: (node: NativeNode) => void
  selectedNodeId: number | null
  openAgentSessionId: string | null
  openAgentSession: (node: NativeNode, session: NeevAgentSession) => void
  startAgentChat: (node: NativeNode, provider: NeevAgentProvider) => void
}) {
  const t = useTranslations("Native")
  const c = useTranslations("NativeContent")
  const contentOpener = useRef<number | null>(null)
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
                  onClick={() => selectNode(node)}
                  aria-current={selectedNodeId === node.id ? "page" : undefined}
                  className={cn(
                    "flex min-h-9 min-w-0 flex-1 items-center gap-2 rounded text-start text-sm focus-visible:ring-2 focus-visible:ring-ring",
                    selectedNodeId === node.id &&
                      "font-medium text-sidebar-accent-foreground"
                  )}
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
                {/* One menu for everything a project can hold, instead of a
                    permanent row per kind repeated under every container. */}
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
                      <FileText aria-hidden className="size-4" />
                      {c("newDocument")}
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      onSelect={() => {
                        contentOpener.current = node.id
                        createContent(node, "chat")
                      }}
                    >
                      <MessagesSquare aria-hidden className="size-4" />
                      {c("newChat")}
                    </DropdownMenuItem>
                    {NEEV_AGENT_PROVIDERS.map((agentProvider) => (
                      <DropdownMenuItem
                        key={agentProvider}
                        onSelect={() => startAgentChat(node, agentProvider)}
                      >
                        <Bot aria-hidden className="size-4" />
                        {c(
                          agentProvider === "claude"
                            ? "newClaudeChat"
                            : "newCodexChat"
                        )}
                      </DropdownMenuItem>
                    ))}
                    <DropdownMenuItem onSelect={() => create(node)}>
                      <Folder aria-hidden className="size-4" />
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
                    openAgents={openAgents}
                    agentsOpen={agentsOpen}
                    agentSessions={agentSessions}
                    selectNode={selectNode}
                    selectedNodeId={selectedNodeId}
                    openAgentSessionId={openAgentSessionId}
                    openAgentSession={openAgentSession}
                    startAgentChat={startAgentChat}
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
                          {resource.kind === "document" ? (
                            <FileText
                              aria-hidden
                              className="size-3.5 shrink-0"
                            />
                          ) : (
                            <MessagesSquare
                              aria-hidden
                              className="size-3.5 shrink-0"
                            />
                          )}
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
                    {/* Agent chats are items in the tree, like documents and
                        team chats: the same kind of thing, not a destination. */}
                    {agentSessions
                      .filter((item) => item.nodeId === node.id)
                      .map((item) => (
                        <li key={item.id}>
                          <button
                            onClick={() => openAgentSession(node, item)}
                            aria-current={
                              openAgentSessionId === item.id
                                ? "page"
                                : undefined
                            }
                            className={cn(
                              "flex min-h-8 w-full min-w-0 items-center gap-2 rounded-lg px-2 text-start text-sm focus-visible:ring-2 focus-visible:ring-ring",
                              openAgentSessionId === item.id
                                ? "bg-sidebar-accent font-medium text-sidebar-accent-foreground"
                                : "text-muted-foreground hover:bg-sidebar-accent/60"
                            )}
                          >
                            <Bot aria-hidden className="size-3.5 shrink-0" />
                            <span className="truncate" title={item.title}>
                              {item.title}
                            </span>
                            <span className="shrink-0 text-[10px] uppercase tracking-wide text-muted-foreground/70">
                              {item.provider}
                            </span>
                          </button>
                        </li>
                      ))}
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
    c = useTranslations("NativeContent"),
    auth = useNativeAuth(),
    mobile = useIsMobile()
  const router = useRouter()
  // What the centre is showing when it is showing an agent chat: a saved one,
  // or a draft for a provider that has no session yet.
  const [agentView, setAgentView] = useState<{
    sessionId: string | null
    provider: NeevAgentProvider | null
  } | null>(null)
  // The caller's own saved chats, read once for the whole tree rather than per
  // container: they are private, so this is one small list either way.
  const [agentSessions, setAgentSessions] = useState<NeevAgentSession[]>([])
  const [agentsToken] = useState(0)
  // What the reader is looking for, across the projects and items already
  // loaded plus their own agent chats. Empty means the tree.
  const [query, setQuery] = useState("")
  // What is open, and which one is in front. A tab is a reference; the views
  // below still fetch and authorize their own subject.
  const [tabs, setTabs] = useState<WorkspaceTab[]>([])
  const [activeTab, setActiveTab] = useState(-1)
  const [tabLimit, setTabLimit] = useState(false)
  const tabsRestored = useRef(false)
  // The tabs as they stand RIGHT NOW, for handlers that open or close several
  // in one gesture. Deliberately not read from `tabs` inside a state updater:
  // an updater must be pure, and the extra `setActiveTab` in one was being
  // dropped on re-invocation.
  const tabsRef = useRef<WorkspaceTab[]>([])
  /**
   * Tabs that have actually been looked at.
   *
   * A restored tab is a reference, not a running editor: mounting all of them
   * at startup would open a dozen editors and pollers before the first paint,
   * for panels the reader may never return to. A panel mounts when its tab is
   * first brought to the front, and from then on it STAYS mounted — which is
   * what keeps a draft safe when switching away and back.
   */
  const [visited, setVisited] = useState<ReadonlySet<string>>(new Set())
  const activeTabRef = useRef(-1)
  const commitTabs = useCallback((next: WorkspaceTab[], active: number) => {
    tabsRef.current = next
    activeTabRef.current = active
    setTabs(next)
    setActiveTab(active)
    const front = next[active]
    if (front) setVisited((old) => new Set([...old, tabKey(front)]))
  }, [])
  const { theme, setTheme } = useTheme()
  // Restoration is attempted once per mount, like the resource pointer.
  const agentRestored = useRef(false)
  const user = auth.session?.user.id
  const orgId = auth.session?.organization?.id
  /**
   * One guard per open tab, not one for the workspace.
   *
   * Every mounted panel registers its own, and they are all mounted at once —
   * a single shared guard would mean closing one tab consulted whichever panel
   * happened to register last, and a hidden panel's unsaved work would be
   * discarded without anyone being asked about it.
   */
  /**
   * Do it. Nothing is being destroyed.
   *
   * Opening a tab, switching to one, or opening a dialog leaves every other
   * panel mounted and holding its draft, so there is nothing to ask the reader
   * about. Only closing a tab (`guardTab`) or leaving the workspace
   * (`guardAll`) destroys anything.
   */
  const keepAll = (action: () => void) => action()
  const guards = useRef(new Map<string, ContentNavigationGuard>())
  const registerGuardFor = useCallback(
    (key: string) => (guard: ContentNavigationGuard) => {
      guards.current.set(key, guard)
    },
    []
  )
  /**
   * Bring a tab to the front, so whatever it is about to say is visible.
   *
   * A panel's confirmation renders INSIDE that panel — which is what stops a
   * hidden tab's menus appearing over the one in front. The same containment
   * would hide a confirmation we asked for, leaving a dialog nobody can see and
   * a Close button that appears to do nothing.
   */
  const facePanel = useCallback((key: string) => {
    const index = tabsRef.current.findIndex((tab) => tabKey(tab) === key)
    if (index < 0 || index === activeTabRef.current) return
    activeTabRef.current = index
    setActiveTab(index)
    setVisited((old) => new Set([...old, key]))
  }, [])

  /** Ask ONE tab. Used when closing it, which is the only thing that destroys it. */
  const guardTab = useCallback(
    (key: string, action: () => void) => {
      const guard = guards.current.get(key)
      if (!guard) return action()
      // Show the tab being asked about before it asks.
      facePanel(key)
      guard(action)
    },
    [facePanel]
  )
  /**
   * Ask EVERY open tab, then act.
   *
   * For leaving the workspace altogether — settings, sign-out — where every
   * mounted panel is about to go, not just the one in front.
   */
  const guardAll = useCallback(
    (action: () => void) => {
      const pending = [...guards.current.entries()]
      const run = (index: number): void => {
        if (index >= pending.length) return action()
        const [key, guard] = pending[index]
        // Whichever panel stops to ask something becomes the visible one, so
        // the reader is looking at the draft they are being asked about.
        facePanel(key)
        guard(() => run(index + 1))
      }
      run(0)
    },
    [facePanel]
  )
  // The draft being written, and the project it belongs to — a workspace with
  // several open tabs needs to know which one is showing it.
  const [creation, setCreation] = useState<
    (ContentCreation & { nodeId: number }) | undefined
  >()
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
    keepAll(() => {
      setSectionState(next)
      setActiveSection(next)
      setDrawer(false)
    })
  }, [])
  /**
   * Open this container's private agent chats.
   *
   * Opening changes nothing: the sessions stay private to their owner with the
   * same sharing, and the server holds which project each one belongs to. A
   * session is only filed by an explicit move, or by being created here.
   */
  const openAgents = useCallback(
    (node: NativeNode) => {
      setSelected(node.id)
      changeSection("agents")
    },
    [changeSection]
  )
  /**
   * Show a container's contents in the centre.
   *
   * Clicking the name used to only fold the row, which left the reader looking
   * at whatever was open before — the project they just clicked was the one
   * thing the screen did not show. Expanding comes along for the ride, because
   * opening something you cannot see the inside of is half a gesture.
   */
  const nodeById = useCallback(
    (id: number) => nodes.find((node) => node.id === id),
    [nodes]
  )
  /** Close a tab by identity, for a panel reporting its own subject gone. */
  const dropTab = useCallback(
    (key: string) => {
      const next = closeTabByKey(tabsRef.current, key, activeTabRef.current)
      commitTabs(next.tabs, next.active)
      guards.current.delete(key)
    },
    [commitTabs]
  )
  /** A draft that became a real chat keeps its place in the strip. */
  const replaceTab = useCallback(
    (key: string, tab: WorkspaceTab) => {
      const index = tabsRef.current.findIndex((item) => tabKey(item) === key)
      if (index < 0) return
      const next = [...tabsRef.current]
      next[index] = tab
      // The guard belonged to the draft; the chat registers its own.
      guards.current.delete(key)
      commitTabs(next, activeTabRef.current)
    },
    [commitTabs]
  )

  /** Bring a tab to the front, adding it if it is not open yet. */
  const showTab = useCallback(
    (tab: WorkspaceTab) => {
      const next = openWorkspaceTab(tabsRef.current, tab)
      // Refused at the limit rather than evicting something that may hold
      // unsaved work; the reader closes a tab themselves.
      if (next.refused) {
        setTabLimit(true)
        return
      }
      setTabLimit(false)
      commitTabs(next.tabs, next.active)
    },
    [commitTabs]
  )

  /** Close a tab. The only destructive one of these, so the only guarded one. */
  const dismissTab = useCallback(
    (index: number) => {
      const tab = tabsRef.current[index]
      if (!tab) return
      // Only THIS tab is being destroyed, so only this tab is asked.
      guardTab(tabKey(tab), () => {
        const next = closeWorkspaceTab(
          tabsRef.current,
          tabsRef.current.findIndex((item) => tabKey(item) === tabKey(tab)),
          activeTabRef.current
        )
        commitTabs(next.tabs, next.active)
        guards.current.delete(tabKey(tab))
        setTabLimit(false)
      })
    },
    [guardTab, commitTabs]
  )

  const selectNode = useCallback(
    (node: NativeNode) => {
      keepAll(() => {
        setSelected(node.id)
        setOpen(null)
        setCreation(undefined)
        setAgentView(null)
        setExpanded((old) => new Set([...old, node.id]))
        setDrawer(false)
        showTab({ kind: "project", nodeId: node.id })
      })
    },
    [showTab]
  )

  /** Start a draft of `kind` inside `node`, from the sidebar or the landing. */
  const createContent = useCallback(
    (node: NativeNode, kind: NativeResourceKind) => {
      const active = document.activeElement as HTMLElement | null
      const menu = active?.closest('[role="menu"]')
      const returnFocus =
        document.getElementById(menu?.getAttribute("aria-labelledby") ?? "") ??
        active
      keepAll(() => {
        // The created kind decides the section, so the new item lands and
        // appears under this container in the right list.
        const nextSection = kind === "document" ? "docs" : "team"
        setSectionState(nextSection)
        setActiveSection(nextSection)
        setSelected(node.id)
        setOpen(null)
        setAgentView(null)
        setExpanded((old) => new Set([...old, node.id]))
        setCreation({
          kind,
          nodeId: node.id,
          request: ++creationNumber.current,
          returnFocus,
        })
        setDrawer(false)
        // The draft is written on the project's own page, so that is the tab
        // that has to be in front.
        showTab({ kind: "project", nodeId: node.id })
      })
    },
    [showTab]
  )
  /**
   * Open a saved agent chat straight into its transcript.
   *
   * Same gesture as opening a document: the sidebar owns selection and the
   * centre shows the thing selected, with no intervening list.
   */
  const openAgentSession = useCallback(
    (node: NativeNode, session: NeevAgentSession) => {
      keepAll(() => {
        setSelected(node.id)
        setOpen(null)
        setCreation(undefined)
        setAgentView({ sessionId: session.id, provider: null })
        setSectionState("agents")
        setActiveSection("agents")
        if (user !== undefined && orgId !== undefined)
          setOpenAgentSession(user, orgId, session.id)
        setDrawer(false)
        showTab({ kind: "agent", sessionId: session.id })
      })
    },
    [user, orgId, showTab]
  )
  /**
   * Start a new agent chat: straight to a composer for that provider.
   *
   * Nothing is created here. The session comes into existence with the first
   * message, so an abandoned draft leaves nothing behind to clean up.
   */
  const startAgentChat = useCallback(
    (node: NativeNode, provider: NeevAgentProvider) => {
      keepAll(() => {
        setSelected(node.id)
        setOpen(null)
        setCreation(undefined)
        setAgentView({ sessionId: null, provider })
        setSectionState("agents")
        setActiveSection("agents")
        // A draft is not a place to come back to.
        if (user !== undefined && orgId !== undefined)
          setOpenAgentSession(user, orgId, null)
        setDrawer(false)
        showTab({ kind: "draft", provider, nodeId: node.id })
      })
    },
    [user, orgId, showTab]
  )
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
  // Deliberately NOT invalidated by the section: one tree lists documents and
  // team chats together, so moving between an agent chat and a document does
  // not change what a container holds. Dropping the cache here made every
  // document and team chat vanish the moment an agent chat was opened.
  // Organization scope still invalidates it, below, where it belongs.
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
        // Both kinds, together: a project holds its documents and its team
        // chats, and the sidebar no longer has a mode that hides one of them.
        const pages = await Promise.all(
          CONTENT_KINDS.map((kind) =>
            listNativeResources(org.id, nodeId, kind, false, null, auth.signal)
          )
        )
        if (stamp !== itemsRequest.current) return
        const items = pages
          .flatMap((page) => page.items)
          .sort((left, right) => left.title.localeCompare(right.title))
        setItemsByNode((old) => ({
          ...old,
          [nodeId]: { loading: false, items, error: null },
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
    [org, auth]
  )
  // Fetch any expanded container that has no list yet — in every section,
  // because the sidebar shows the same tree whatever the centre is showing.
  useEffect(() => {
    for (const id of expanded) {
      if (!itemsByNode[id]) void loadItems(id)
    }
  }, [expanded, itemsByNode, loadItems])
  // One read for the whole tree. A failure here must not take the workspace
  // down: the sidebar simply shows no agent chats, and the centre surface still
  // reports its own errors.
  useEffect(() => {
    if (!org) return
    const controller = new AbortController()
    const load = async () => {
      try {
        const list = await listAgentSessions(false, controller.signal)
        if (controller.signal.aborted) return
        setAgentSessions(list)
        // Restore what the reader had open, once and only into a session the
        // server still returns — a stale pointer selects nothing.
        if (!agentRestored.current) {
          agentRestored.current = true
          const remembered =
            user !== undefined && orgId !== undefined
              ? getOpenAgentSession(user, orgId)
              : null
          const session = list.find((item) => item.id === remembered)
          if (session && session.nodeId !== null) {
            setSelected(session.nodeId)
            setExpanded((old) => new Set([...old, session.nodeId as number]))
            setAgentView({ sessionId: session.id, provider: null })
          } else if (remembered && user !== undefined && orgId !== undefined)
            setOpenAgentSession(user, orgId, null)
        }
      } catch (cause) {
        if (controller.signal.aborted) return
        auth.handleAuthorityError(cause)
      }
    }
    void load()
    return () => controller.abort()
  }, [org, agentsToken, auth, user, orgId])

  // Saved references only, and only once the first restore has run — writing
  // an empty list before reading would erase what we are about to read.
  useEffect(() => {
    if (!tabsRestored.current || user === undefined || orgId === undefined)
      return
    writeTabs(user, orgId, tabs, activeTab)
  }, [tabs, activeTab, user, orgId])
  useEffect(() => {
    if (tabsRestored.current || user === undefined || orgId === undefined)
      return
    tabsRestored.current = true
    const saved = readTabs(user, orgId)
    if (saved.tabs.length === 0) return
    commitTabs(saved.tabs, saved.active)
  }, [user, orgId, commitTabs])

  /** What a tab is called in the strip: the thing itself, never its kind. */

  const tabTitle = useCallback(
    (tab: WorkspaceTab): string => {
      if (tab.kind === "project") return nodeById(tab.nodeId)?.name ?? ""
      if (tab.kind === "resource")
        return (
          (itemsByNode[tab.nodeId]?.items ?? []).find(
            (item) => item.id === tab.resourceId
          )?.title ?? t("loading")
        )
      if (tab.kind === "agent")
        return (
          agentSessions.find((item) => item.id === tab.sessionId)?.title ??
          t("loading")
        )
      return c(tab.provider === "claude" ? "newClaudeChat" : "newCodexChat")
    },
    [nodeById, itemsByNode, agentSessions, t, c]
  )

  const openResource = useCallback(
    (node: NativeNode, resource: NativeResource) =>
      keepAll(() => {
        setSelected(node.id)
        setCreation(undefined)
        setOpen({ resource, request: ++openNumber.current })
        setDrawer(false)
        showTab({
          kind: "resource",
          resourceId: resource.id,
          nodeId: node.id,
        })
      }),
    [showTab]
  )
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
          // Restored into a tab, like every other way of opening something.
          showTab({
            kind: "resource",
            resourceId: resource.id,
            nodeId: resource.nodeId,
          })
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
  }, [loading, user, orgId, section, nodes, loadItems, showTab])
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
  // Where the active tab lives: project, then the thing inside it.
  const activeTabRef2 = tabs[activeTab]
  const breadcrumb: string[] = []
  if (activeTabRef2) {
    const nodeId =
      activeTabRef2.kind === "project"
        ? activeTabRef2.nodeId
        : activeTabRef2.kind === "resource"
          ? activeTabRef2.nodeId
          : activeTabRef2.kind === "draft"
            ? activeTabRef2.nodeId
            : (agentSessions.find((item) => item.id === activeTabRef2.sessionId)
                ?.nodeId ?? null)
    for (const name of nodeId === null ? [] : trail(nodes, nodeId))
      breadcrumb.push(name)
    if (activeTabRef2.kind !== "project")
      breadcrumb.push(tabTitle(activeTabRef2))
  }

  const sidebar = (
    <div className="flex h-full min-w-0 flex-col">
      {/* The organization is identity, not a banner across the window. */}
      <div className="flex min-w-0 items-center gap-2 px-2.5 py-2">
        {org.avatarUrl?.startsWith("data:image/") ? (
          <span
            role="img"
            aria-label={t("organizationLogo")}
            className="size-5 shrink-0 rounded bg-contain bg-center bg-no-repeat"
            style={{ backgroundImage: `url(${org.avatarUrl})` }}
          />
        ) : (
          <span
            aria-hidden
            className="flex size-5 shrink-0 items-center justify-center rounded bg-sidebar-accent text-[11px] font-semibold"
          >
            {org.name.slice(0, 1)}
          </span>
        )}
        <span className="truncate text-sm font-medium" title={org.name}>
          {org.name}
        </span>
      </div>
      <div className="px-2.5 pb-2">
        <label className="sr-only" htmlFor="neev-workspace-search">
          {t("search")}
        </label>
        <div className="relative">
          <Search
            aria-hidden
            className="pointer-events-none absolute start-2 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground"
          />
          <input
            id="neev-workspace-search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t("search")}
            className="h-7 w-full rounded-md bg-sidebar-accent/60 ps-7 pe-2 text-[13px] outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring"
          />
        </div>
      </div>
      <nav aria-label={t("projects")} className="flex min-h-0 flex-1 flex-col">
        <div className="flex items-center justify-between gap-2 px-2.5 pb-1">
          <h2 className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
            {t("projects")}
          </h2>
          <button
            aria-label={t("newProject")}
            title={t("newProject")}
            onClick={() =>
              keepAll(() => openEditor({ kind: "project", parentId: null }))
            }
            className="flex size-5 items-center justify-center rounded text-muted-foreground hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring"
          >
            <Plus aria-hidden className="size-3.5" />
          </button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-1.5">
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
              keepAll(() => openEditor({ kind: "folder", parentId: node.id }))
            }
            edit={(node) =>
              keepAll(() =>
                openEditor({ kind: node.kind, parentId: node.parentId, node })
              )
            }
            createContent={createContent}
            selectResource={openResource}
            openAgents={openAgents}
            agentsOpen={section === "agents"}
            agentSessions={agentSessions}
            selectNode={selectNode}
            selectedNodeId={selected}
            openAgentSessionId={agentView?.sessionId ?? null}
            openAgentSession={openAgentSession}
            startAgentChat={startAgentChat}
          />
          {!loading && nodes.length === 0 && !query.trim() && (
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
      <div className="border-t border-sidebar-border p-1.5">
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button className="flex min-h-8 w-full min-w-0 items-center gap-2 rounded-lg px-2 text-start text-sm hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring">
              <span
                aria-hidden
                className="flex size-5 shrink-0 items-center justify-center rounded-full bg-sidebar-accent text-[11px] font-semibold"
              >
                {auth.session.user.displayName.slice(0, 1)}
              </span>
              <span className="truncate">{auth.session.user.displayName}</span>
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-56">
            <DropdownMenuItem
              onSelect={() => guardAll(() => router.push("/settings"))}
            >
              <Settings2 aria-hidden className="size-4" />
              {t("settings")}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            {/* A real control, not a label: the app defaults to dark and there
                was nowhere to say otherwise. An explicit choice is kept. */}
            <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
              {t("appearance")}
            </DropdownMenuLabel>
            {(["light", "dark", "system"] as const).map((option) => (
              <DropdownMenuItem key={option} onSelect={() => setTheme(option)}>
                {option === "light" ? (
                  <Sun aria-hidden className="size-4" />
                ) : option === "dark" ? (
                  <Moon aria-hidden className="size-4" />
                ) : (
                  <Monitor aria-hidden className="size-4" />
                )}
                {t(
                  option === "light"
                    ? "themeLight"
                    : option === "dark"
                      ? "themeDark"
                      : "themeSystem"
                )}
                {theme === option && (
                  <Check aria-hidden className="ms-auto size-4" />
                )}
              </DropdownMenuItem>
            ))}
            <DropdownMenuSeparator />
            <DropdownMenuItem
              onSelect={() => guardAll(() => void auth.logout())}
            >
              {t("signOut")}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </div>
  )
  return (
    <div className="flex h-dvh min-w-0 flex-col overflow-hidden">
      <div className="flex min-h-0 min-w-0 flex-1">
        {!mobile && sidebarOpen && (
          <aside className="w-[248px] shrink-0 border-e bg-sidebar text-sidebar-foreground">
            {sidebar}
          </aside>
        )}
        <main className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
          {/* Where you are, and what else is open. Two thin bars instead of a
              window-wide banner repeating the organization and the email. */}
          <div className="flex h-10 shrink-0 items-center gap-1 px-2">
            {mobile ? (
              <Dialog open={drawer} onOpenChange={setDrawer}>
                <DialogTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="size-7"
                    aria-label={t("openNavigation")}
                  >
                    <Menu aria-hidden className="size-4" />
                  </Button>
                </DialogTrigger>
                <DialogContent
                  showCloseButton={false}
                  className="h-[calc(100dvh-2rem)] max-w-sm gap-0 p-0"
                >
                  <DialogTitle className="sr-only">{t("projects")}</DialogTitle>
                  <DialogDescription className="sr-only">
                    {t("navigationDescription")}
                  </DialogDescription>
                  {sidebar}
                </DialogContent>
              </Dialog>
            ) : (
              <Button
                variant="ghost"
                size="icon"
                className="size-7"
                aria-label={t("openNavigation")}
                title={t("openNavigation")}
                aria-expanded={sidebarOpen}
                onClick={() => setSidebarOpen(!sidebarOpen)}
              >
                <PanelLeft aria-hidden className="size-4" />
              </Button>
            )}
            <nav
              aria-label={t("breadcrumb")}
              className="flex min-w-0 flex-1 items-center gap-1 text-[13px] text-muted-foreground"
            >
              {breadcrumb.map((crumb, index) => (
                <span
                  key={`${crumb}:${index}`}
                  className="flex min-w-0 items-center gap-1"
                >
                  {index > 0 && (
                    <ChevronRight
                      aria-hidden
                      className="size-3 shrink-0 opacity-60 rtl:rotate-180"
                    />
                  )}
                  <span
                    className={cn(
                      "truncate",
                      index === breadcrumb.length - 1 && "text-foreground"
                    )}
                    title={crumb}
                  >
                    {crumb}
                  </span>
                </span>
              ))}
            </nav>
          </div>
          {tabLimit && (
            <p
              role="status"
              className="shrink-0 border-b px-3 py-1.5 text-xs text-muted-foreground"
            >
              {t("tabLimit")}
            </p>
          )}
          {tabs.length > 0 && (
            <div
              role="tablist"
              aria-label={t("openTabs")}
              className="flex h-9 shrink-0 items-center gap-0.5 overflow-x-auto border-b px-1.5"
            >
              {tabs.map((tab, index) => {
                const current = index === activeTab
                return (
                  <div
                    key={tabKey(tab)}
                    className={cn(
                      "group/tab flex h-7 min-w-0 shrink-0 items-center gap-1.5 rounded-md ps-2 pe-1 text-[13px]",
                      current
                        ? "bg-accent text-foreground"
                        : "text-muted-foreground hover:bg-accent/50"
                    )}
                  >
                    <button
                      role="tab"
                      aria-selected={current}
                      onClick={() => {
                        activeTabRef.current = index
                        setActiveTab(index)
                        setVisited(
                          (old) => new Set([...old, tabKey(tabs[index])])
                        )
                      }}
                      className="flex min-w-0 items-center gap-1.5 rounded focus-visible:ring-2 focus-visible:ring-ring"
                    >
                      <TabIcon tab={tab} />
                      <span className="max-w-40 truncate">{tabTitle(tab)}</span>
                    </button>
                    <button
                      aria-label={t("closeTab", { name: tabTitle(tab) })}
                      onClick={() => dismissTab(index)}
                      className="flex size-4 shrink-0 items-center justify-center rounded opacity-0 hover:bg-background/70 focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-ring group-hover/tab:opacity-100"
                    >
                      <X aria-hidden className="size-3" />
                    </button>
                  </div>
                )
              })}
            </div>
          )}
          <div className="relative flex min-h-0 min-w-0 flex-1 flex-col">
            {tabs.length === 0 ? (
              <WorkspaceStart
                nodes={nodes}
                onOpen={(node) => selectNode(node)}
                onCreate={() =>
                  keepAll(() => openEditor({ kind: "project", parentId: null }))
                }
              />
            ) : (
              tabs.map((tab, index) => (
                // Every open tab stays MOUNTED. Switching hides a panel rather
                // than destroying it, so a half-written document or an unsent
                // message is still there when the reader comes back — without
                // being asked to discard anything on the way.
                <NativeTabPanel
                  key={tabKey(tab)}
                  hidden={index !== activeTab}
                  className={cn(
                    "min-h-0 min-w-0 flex-1 flex-col",
                    index === activeTab ? "flex" : "hidden"
                  )}
                >
                  {visited.has(tabKey(tab)) &&
                    (tab.kind === "project" ? (
                      creation && creation.nodeId === tab.nodeId ? (
                        <NativeContent
                          node={nodeById(tab.nodeId) ?? nodes[0]}
                          chromeless
                          creation={creation}
                          onCreationCancelled={() => setCreation(undefined)}
                          onRequestNavigationGuard={registerGuardFor(
                            tabKey(tab)
                          )}
                          onOpened={(resource) => {
                            setCreation(undefined)
                            setExpanded(
                              (old) => new Set([...old, resource.nodeId])
                            )
                            void loadItems(resource.nodeId)
                            openResource(
                              nodeById(resource.nodeId) ?? nodes[0],
                              resource
                            )
                          }}
                        />
                      ) : (
                        <ProjectPanel
                          node={nodeById(tab.nodeId)}
                          items={itemsByNode[tab.nodeId]?.items ?? []}
                          loading={itemsByNode[tab.nodeId]?.loading ?? false}
                          sessions={agentSessions.filter(
                            (item) => item.nodeId === tab.nodeId
                          )}
                          onOpenResource={(resource) => {
                            const node = nodeById(tab.nodeId)
                            if (node) openResource(node, resource)
                          }}
                          onOpenSession={(session) => {
                            const node = nodeById(tab.nodeId)
                            if (node) openAgentSession(node, session)
                          }}
                          onCreate={(kind) => {
                            const node = nodeById(tab.nodeId)
                            if (!node) return
                            if (kind === "folder")
                              keepAll(() =>
                                openEditor({
                                  kind: "folder",
                                  parentId: node.id,
                                })
                              )
                            else if (kind === "claude" || kind === "codex")
                              startAgentChat(node, kind)
                            else createContent(node, kind)
                          }}
                        />
                      )
                    ) : tab.kind === "resource" ? (
                      <ResourcePanel
                        orgId={org.id}
                        resourceId={tab.resourceId}
                        node={nodeById(tab.nodeId)}
                        cached={(itemsByNode[tab.nodeId]?.items ?? []).find(
                          (item) => item.id === tab.resourceId
                        )}
                        registerGuard={registerGuardFor(tabKey(tab))}
                        onGone={() => dropTab(tabKey(tab))}
                      />
                    ) : (
                      <NativeAgentChats
                        nodeId={
                          tab.kind === "agent"
                            ? (agentSessions.find(
                                (item) => item.id === tab.sessionId
                              )?.nodeId ?? null)
                            : tab.nodeId
                        }
                        nodeName={
                          nodeById(
                            tab.kind === "agent"
                              ? (agentSessions.find(
                                  (item) => item.id === tab.sessionId
                                )?.nodeId ?? -1)
                              : (tab.nodeId ?? -1)
                          )?.name
                        }
                        sessionId={tab.kind === "agent" ? tab.sessionId : null}
                        draftProvider={
                          tab.kind === "draft"
                            ? (tab.provider as NeevAgentProvider)
                            : null
                        }
                        onSessionCreated={(session) => {
                          setAgentSessions((old) => [
                            session,
                            ...old.filter((item) => item.id !== session.id),
                          ])
                          // The draft became a real chat: its tab becomes that
                          // chat rather than leaving both open.
                          replaceTab(tabKey(tab), {
                            kind: "agent",
                            sessionId: session.id,
                          })
                          if (user !== undefined && orgId !== undefined)
                            setOpenAgentSession(user, orgId, session.id)
                        }}
                        onDraftCancelled={() => dropTab(tabKey(tab))}
                        onClosed={() => dropTab(tabKey(tab))}
                        onRequestNavigationGuard={registerGuardFor(tabKey(tab))}
                        onRequestSettings={() =>
                          // Straight to the AI connections, not to Settings'
                          // front page: a Connect button that leaves the user
                          // hunting is not one.
                          guardAll(() =>
                            router.push("/settings?section=agents")
                          )
                        }
                      />
                    ))}
                </NativeTabPanel>
              ))
            )}
          </div>
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
