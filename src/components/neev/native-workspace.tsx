"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { useRouter } from "next/navigation"
import { useTheme } from "next-themes"
import {
  closeTab as closeWorkspaceTab,
  closeTabByKey,
  openTab as openWorkspaceTab,
  sameTab as sameWorkspaceTab,
  openTabInPlace as openWorkspaceTabInPlace,
  readTabs,
  tabKey,
  writeTabs,
  type WorkspaceTab,
} from "@/lib/neev-workspace-tabs"
import { useTranslations } from "next-intl"
import {
  Bot,
  ChevronRight,
  File,
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
  Trash2,
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
  updateAgentSession,
  type NeevAgentProvider,
  type NeevAgentSession,
} from "@/lib/neev-agent-sessions-client"
import {
  createNativeResource,
  listNativeResources,
  loadNativeResource,
  updateNativeResource,
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

/**
 * What a tab is, drawn from what the thing ACTUALLY is.
 *
 * A resource tab used to be a document icon unconditionally, so every team chat
 * in the strip wore a page — while the same chat one panel away in the sidebar
 * correctly wore a speech bubble. The two now read from the same fact and use
 * the same pair of icons, because a reader scanning the strip is using the icon
 * to tell a conversation from a document.
 *
 * `undefined` means the kind is not known YET: a tab is a reference, and its
 * resource may not be in any loaded list. That gets the neutral sheet rather
 * than a guess, because guessing is what put a chat behind a page icon.
 */
function TabIcon({
  tab,
  resourceKind,
}: {
  tab: WorkspaceTab
  resourceKind?: NativeResourceKind
}) {
  const className = "size-3.5 shrink-0"
  // Nothing in it yet, so it shows the gesture that made it rather than
  // borrowing the icon of a kind it has not been given.
  if (tab.kind === "empty") return <Plus aria-hidden className={className} />
  if (tab.kind === "project")
    return <Layers3 aria-hidden className={className} />
  if (tab.kind === "agent" || tab.kind === "draft")
    return <Bot aria-hidden className={className} />
  if (resourceKind === "chat")
    return <MessagesSquare aria-hidden className={className} />
  if (resourceKind === "document")
    return <FileText aria-hidden className={className} />
  // Not resolved yet. In practice a resource tab only exists once something has
  // answered for it, so this is a floor rather than a state anyone sees — but
  // it is deliberately NOT the document icon, because defaulting to one of the
  // two real answers is exactly what put a chat behind a page.
  return <File aria-hidden className={className} />
}

/**
 * A document or team chat, resolved before it is rendered.
 *
 * The sidebar's lists are lazy and paginated, so a tab's resource is often not
 * in them — being absent from a catalog is not the same as being gone. This
 * asks for the resource itself, and only a refusal from that authorized read
 * closes the tab.
 */
/**
 * The most recently written of several copies of one resource.
 *
 * A panel can hold up to three: what it fetched itself, what the sidebar's list
 * currently says, and what it last had. Taking the first non-empty one meant a
 * restored tab's own fetch outranked a rename that happened in the sidebar
 * afterwards, and the tab kept the old name. `authRevision` is bumped by every
 * metadata write, so it — not the order these happen to be listed in — decides.
 */
function newest(
  ...candidates: (NativeResource | null | undefined)[]
): NativeResource | null {
  let best: NativeResource | null = null
  let bestRevision: bigint | null = null
  for (const candidate of candidates) {
    if (!candidate) continue
    let revision: bigint | null = null
    try {
      revision = BigInt(candidate.authRevision)
    } catch {
      // Unparsable revisions cannot be ordered. Such a copy is still better
      // than nothing, but never displaces one that can be ordered.
      revision = null
    }
    if (!best) {
      best = candidate
      bestRevision = revision
      continue
    }
    if (revision === null) continue
    if (bestRevision === null || revision > bestRevision) {
      best = candidate
      bestRevision = revision
    }
  }
  return best
}

function ResourcePanel({
  orgId,
  resourceId,
  node,
  cached,
  fresh,
  registerGuard,
  onGone,
  onChanged,
  onResolved,
}: {
  orgId: number
  resourceId: string
  /** Newly created, so it may still take its name from what is written. */
  fresh: boolean
  node: NativeNode | undefined
  cached: NativeResource | undefined
  registerGuard: (guard: ContentNavigationGuard) => void
  onGone: () => void
  /** A rename or re-share, on its way to the lists the chrome reads. */
  onChanged: (resource: NativeResource) => void
  /**
   * What this tab turned out to be, whether it was listed or fetched here.
   *
   * Separate from `onChanged`, which speaks for an EDIT and only updates a row
   * the lists already hold. This is the plain fact of the resource's kind, and
   * the tab strip needs it for a resource that is in no loaded list.
   */
  onResolved: (resource: NativeResource) => void
}) {
  const auth = useNativeAuth(),
    t = useTranslations("Native")
  // Seeded from the sidebar's list when it happens to hold this one, and
  // otherwise fetched below. Either way it is read once: the panel then owns
  // what it is showing.
  const [loaded, setLoaded] = useState<NativeResource | null>(null)
  const [error, setError] = useState<unknown>(null)
  const [request, setRequest] = useState(0)
  /**
   * The last resource this panel had, for the renders where it has no other.
   *
   * `cached` is a lookup into a list the sidebar owns and refetches, and a
   * resource can leave that list without going anywhere — moving one to the
   * trash takes it out of a list that excludes trashed things. Falling back to
   * the placeholder at that moment would unmount the panel's contents, and for
   * a document that means tearing down the editor frame and booting it again,
   * seconds of it, because something was renamed in the sidebar.
   *
   * A newer `cached` or `loaded` still wins; this only catches the fall to
   * nothing. State rather than a ref, because it is read while rendering.
   */
  const [retained, setRetained] = useState<NativeResource | null>(null)
  const resource = newest(loaded, cached, retained)
  useEffect(() => {
    if (!resource) return
    // Compared by identity of the VERSION, not of the object: two reads of the
    // same unchanged row are different objects, and storing each one would
    // schedule a render that produces another one just like it.
    if (
      retained?.id !== resource.id ||
      retained.authRevision !== resource.authRevision
    )
      // Deliberate: this remembers what was rendered so a LATER render that
      // loses it has something to fall back on. There is no earlier point to
      // record it from — the value is derived — and the comparison above is
      // what stops it scheduling itself again.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setRetained(resource)
    onResolved(resource)
  }, [resource, retained, onResolved])
  /*
   * Started without waiting for the project tree.
   *
   * This used to also require `node`, which put the whole tree load in front of
   * this one read for any tab whose folder had not been paged in yet — a
   * restored tab, most often. The wait bought nothing: this GET is authorized
   * server-side by organization and resource id, and it 404s for a resource the
   * reader may not see whether or not a tree is loaded. So the two run
   * together, and the render below still waits for both.
   */
  useEffect(() => {
    if (resource) return
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
  }, [resource, orgId, resourceId, auth, onGone])

  if (error) return <NativeError error={error} />
  if (!node || !resource)
    return (
      <p role="status" className="px-6 py-10 text-sm text-muted-foreground">
        {/* What is opening is this one thing, and saying so is both shorter
            and true. "Loading your workspace…" described a whole-application
            boot that had already happened, in a panel beside a sidebar that
            was plainly already loaded. */}
        {t("opening")}
      </p>
    )
  return (
    <NativeContent
      node={node}
      chromeless
      open={{ resource, request, fresh }}
      onRequestNavigationGuard={registerGuard}
      onResourceChanged={(next) => {
        /*
         * Keep what this panel believes it is showing in step with the edit.
         *
         * `loaded` is read once when the tab resolves, and the effect above
         * re-publishes it through `onResolved` whenever it changes. Leaving it
         * stale meant every re-publish restored the name from before the edit,
         * quietly undoing the update the tab strip had just been given.
         */
        setLoaded(next)
        onChanged(next)
      }}
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
  onCreate: (kind: NativeResourceKind | "folder") => void
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
  onCreate: (kind: NativeResourceKind | "folder") => void
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
            ["chat", c("newAgentChat"), Bot],
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
          {t("loadingItems")}
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
  agentSessions,
  selectNode,
  selectedNodeId,
  openAgentSessionId,
  openAgentSession,
  showTrash,
  renameResource,
  trashResource,
  renameSession,
  trashSession,
  actionsBusy,
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
  /** The caller's own saved agent chats, already filed under a node. */
  agentSessions: NeevAgentSession[]
  /** Opening a container shows its contents; the chevron only folds it. */
  selectNode: (node: NativeNode) => void
  selectedNodeId: number | null
  openAgentSessionId: string | null
  openAgentSession: (node: NativeNode, session: NeevAgentSession) => void
  /** Whether trashed rows are listed alongside the rest. */
  showTrash: boolean
  renameResource: (node: NativeNode, resource: NativeResource) => void
  /** Moves to the trash, or brings back out of it. Reversible either way. */
  trashResource: (resource: NativeResource) => void
  renameSession: (session: NeevAgentSession) => void
  trashSession: (session: NeevAgentSession) => void
  /** A sidebar mutation is in flight; a second one must not be started. */
  actionsBusy: boolean
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
                        void createContent(node, "document")
                      }}
                    >
                      <FileText aria-hidden className="size-4" />
                      {c("newDocument")}
                    </DropdownMenuItem>
                    {/* One entry, and it makes an ordinary chat resource.
                        Agent or team, and which provider answers, are decided
                        inside the conversation — where they can be changed
                        without starting a different kind of thing. */}
                    <DropdownMenuItem
                      onSelect={() => {
                        contentOpener.current = node.id
                        void createContent(node, "chat")
                      }}
                    >
                      <Bot aria-hidden className="size-4" />
                      {c("newAgentChat")}
                    </DropdownMenuItem>
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
                    agentSessions={agentSessions}
                    selectNode={selectNode}
                    selectedNodeId={selectedNodeId}
                    openAgentSessionId={openAgentSessionId}
                    openAgentSession={openAgentSession}
                    showTrash={showTrash}
                    renameResource={renameResource}
                    trashResource={trashResource}
                    renameSession={renameSession}
                    trashSession={trashSession}
                    actionsBusy={actionsBusy}
                  />
                  <ul className="ms-3 space-y-0.5 border-s border-sidebar-border/60 ps-1.5">
                    {/*
                      Trashed rows are held back unless the trash is being
                      shown. Filtered here rather than dropped from the list,
                      because a tab open on the thing that was just trashed
                      still reads its row — taking it out of state would make
                      that tab look like it was loading all over again.
                    */}
                    {(list?.items ?? [])
                      .filter((resource) => showTrash || !resource.archived)
                      .map((resource) => (
                        <li key={resource.id}>
                          {/* A row and its menu, in one hoverable group. The
                              menu button is always in the tree for keyboard and
                              screen-reader users; it only *appears* on hover or
                              focus, so a quiet sidebar stays quiet. */}
                          <div
                            className={cn(
                              "group/item flex min-h-8 min-w-0 items-center rounded-lg pe-1",
                              openResourceId === resource.id
                                ? "bg-sidebar-accent font-medium text-sidebar-accent-foreground"
                                : "text-muted-foreground hover:bg-sidebar-accent/60"
                            )}
                          >
                            <button
                              onClick={() => selectResource(node, resource)}
                              aria-current={
                                openResourceId === resource.id
                                  ? "page"
                                  : undefined
                              }
                              className="flex min-h-8 w-full min-w-0 flex-1 items-center gap-2 rounded-lg px-2 text-start text-sm focus-visible:ring-2 focus-visible:ring-ring"
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
                              <span
                                className={cn(
                                  "truncate",
                                  resource.archived && "line-through opacity-70"
                                )}
                                title={resource.title}
                              >
                                {resource.title}
                              </span>
                              {/* Says where it is, so a struck-through row is
                                  not left to be guessed at. */}
                              {resource.archived && (
                                <span className="shrink-0 text-[10px] uppercase tracking-wide text-muted-foreground/70">
                                  {c("inTrash")}
                                </span>
                              )}
                            </button>
                            <DropdownMenu>
                              <DropdownMenuTrigger asChild>
                                <button
                                  aria-label={c("itemActions", {
                                    title: resource.title,
                                  })}
                                  className={cn(
                                    "flex size-7 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring",
                                    // Hidden until hover ONLY where hovering
                                    // exists. On a touch screen there is no
                                    // hover, so an opacity-0 control is simply
                                    // a control nobody can find.
                                    "[@media(hover:hover)]:opacity-0 [@media(hover:hover)]:group-hover/item:opacity-100",
                                    "group-focus-within/item:opacity-100 focus-visible:opacity-100 data-[state=open]:opacity-100"
                                  )}
                                >
                                  <MoreHorizontal
                                    aria-hidden
                                    className="size-3.5"
                                  />
                                </button>
                              </DropdownMenuTrigger>
                              <DropdownMenuContent align="end">
                                <DropdownMenuItem
                                  onSelect={() =>
                                    renameResource(node, resource)
                                  }
                                >
                                  {c("rename")}
                                </DropdownMenuItem>
                                <DropdownMenuItem
                                  disabled={actionsBusy}
                                  onSelect={() => trashResource(resource)}
                                >
                                  {c(
                                    resource.archived
                                      ? "restoreFromTrash"
                                      : "moveToTrash"
                                  )}
                                </DropdownMenuItem>
                              </DropdownMenuContent>
                            </DropdownMenu>
                          </div>
                        </li>
                      ))}
                    {/* A folder's own items, not the workspace. This said
                        "Loading your workspace…" inside an already-loaded
                        sidebar — including every time the trash was toggled or
                        something was created, which is a background refresh
                        and not a boot. */}
                    {list?.loading && (
                      <li
                        role="status"
                        className="px-2 py-1 text-xs text-muted-foreground"
                      >
                        {t("loadingItems")}
                      </li>
                    )}
                    {/* Agent chats are items in the tree, like documents and
                        team chats: the same kind of thing, not a destination. */}
                    {agentSessions
                      .filter((item) => item.nodeId === node.id)
                      .filter((item) => showTrash || !item.archived)
                      .map((item) => (
                        <li key={item.id}>
                          {/* An agent chat gets the same two actions as
                              everything else in the tree. Its API takes one
                              field per request, so rename and trash are
                              separate calls — but to the reader they are the
                              same menu on the same kind of row. */}
                          <div
                            className={cn(
                              "group/item flex min-h-8 min-w-0 items-center rounded-lg pe-1",
                              openAgentSessionId === item.id
                                ? "bg-sidebar-accent font-medium text-sidebar-accent-foreground"
                                : "text-muted-foreground hover:bg-sidebar-accent/60"
                            )}
                          >
                            <button
                              onClick={() => openAgentSession(node, item)}
                              aria-current={
                                openAgentSessionId === item.id
                                  ? "page"
                                  : undefined
                              }
                              className="flex min-h-8 w-full min-w-0 flex-1 items-center gap-2 rounded-lg px-2 text-start text-sm focus-visible:ring-2 focus-visible:ring-ring"
                            >
                              <Bot aria-hidden className="size-3.5 shrink-0" />
                              <span
                                className={cn(
                                  "truncate",
                                  item.archived && "line-through opacity-70"
                                )}
                                title={item.title}
                              >
                                {item.title}
                              </span>
                              <span className="shrink-0 text-[10px] uppercase tracking-wide text-muted-foreground/70">
                                {item.archived ? c("inTrash") : item.provider}
                              </span>
                            </button>
                            <DropdownMenu>
                              <DropdownMenuTrigger asChild>
                                <button
                                  aria-label={c("itemActions", {
                                    title: item.title,
                                  })}
                                  className={cn(
                                    "flex size-7 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring",
                                    "[@media(hover:hover)]:opacity-0 [@media(hover:hover)]:group-hover/item:opacity-100",
                                    "group-focus-within/item:opacity-100 focus-visible:opacity-100 data-[state=open]:opacity-100"
                                  )}
                                >
                                  <MoreHorizontal
                                    aria-hidden
                                    className="size-3.5"
                                  />
                                </button>
                              </DropdownMenuTrigger>
                              <DropdownMenuContent align="end">
                                <DropdownMenuItem
                                  onSelect={() => renameSession(item)}
                                >
                                  {c("rename")}
                                </DropdownMenuItem>
                                <DropdownMenuItem
                                  disabled={actionsBusy}
                                  onSelect={() => trashSession(item)}
                                >
                                  {c(
                                    item.archived
                                      ? "restoreFromTrash"
                                      : "moveToTrash"
                                  )}
                                </DropdownMenuItem>
                              </DropdownMenuContent>
                            </DropdownMenu>
                          </div>
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
/** What a rename is being asked about. Both kinds have their own PATCH. */
type RenameTarget =
  | { kind: "resource"; resource: NativeResource }
  | { kind: "agent"; session: NeevAgentSession }
function WorkspaceContent() {
  const t = useTranslations("Native"),
    c = useTranslations("NativeContent"),
    auth = useNativeAuth(),
    mobile = useIsMobile()
  const router = useRouter()
  // The caller's own saved chats, read once for the whole tree rather than per
  // container: they are private, so this is one small list either way.
  const [agentSessions, setAgentSessions] = useState<NeevAgentSession[]>([])
  const [agentsToken] = useState(0)
  // What the reader is looking for, across the projects and items already
  // loaded plus their own agent chats. Empty means the tree.
  const [query, setQuery] = useState("")
  // One creation at a time, and its failure said out loud.
  const creating = useRef(false)
  /** Request ids held per attempt, so a retry replays rather than duplicates. */
  const pendingCreate = useRef(new Map<string, string>())
  /** The scope as it is right now, for answers that arrive after a change. */
  const scopeRef = useRef(auth.scopeKey)
  scopeRef.current = auth.scopeKey
  const [createError, setCreateError] = useState<unknown>(null)
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
   * Switching between open tabs, asking for a blank one, or opening a dialog
   * leaves every panel mounted and holding its draft, so there is nothing to
   * ask the reader about.
   *
   * NOT for navigation any more. `showTab` replaces the front tab, which
   * unmounts its panel, so it guards that tab itself — see the note there.
   * Closing (`guardTab`) and leaving the workspace (`guardAll`) still destroy
   * things, and still ask.
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
  const org = auth.session?.organization
  const { handleError } = auth
  const [nodes, setNodes] = useState<NativeNode[]>([]),
    [nextAfter, setNextAfter] = useState<number | null>(null),
    [loading, setLoading] = useState(true),
    [error, setError] = useState<unknown>(null)
  const [expanded, setExpanded] = useState(new Set<number>()),
    [drawer, setDrawer] = useState(false),
    [sidebarOpen, setSidebarOpen] = useState(true),
    // The primary workspace section (Agent Chats / Team Chat / Documents) lives in
    // the left rail and drives the center surface. Persisted per user; SSR-safe
    // default until the effect reads localStorage.
    [section, setSectionState] = useState<Section>(DEFAULT_SECTION)
  useEffect(() => setSectionState(getActiveSection()), [])
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

  /**
   * Go to something.
   *
   * Replaces what is in front rather than adding to the strip — browsing a
   * sidebar should not leave a trail of tabs behind it. Something already open
   * is brought forward instead of opened twice, and a second tab is a thing
   * the reader asks for with New tab.
   */
  const showTab = useCallback(
    (tab: WorkspaceTab) => {
      // Captured, not re-read inside `go`. The prompt sits between the two,
      // and a guard that asked about one tab must not act on another.
      const open = tabsRef.current
      const activeIndex = activeTabRef.current
      const front = open[activeIndex]
      const alreadyOpen = open.some((item) => sameWorkspaceTab(item, tab))
      const go = () => {
        const next = openWorkspaceTabInPlace(open, tab, activeIndex)
        setTabLimit(false)
        commitTabs(next.tabs, next.active)
      }

      /*
       * Replacing is destroying.
       *
       * While this ADDED a tab, nothing was at risk: every panel stayed
       * mounted and kept its draft, so navigation needed no permission. Now
       * the front tab is replaced and its panel unmounts, which is exactly as
       * destructive as closing it — so it asks the same question closing does.
       *
       * Only when something actually goes away: going to the tab already in
       * front, or to one already open elsewhere, destroys nothing.
       */
      const replacing =
        front !== undefined &&
        !sameWorkspaceTab(front, tab) &&
        front.kind !== "empty" &&
        !alreadyOpen
      if (!replacing) {
        go()
        return
      }
      guardTab(tabKey(front), () => {
        /*
         * The strip may have moved while the question was on screen.
         *
         * The prompt blocks the READER, not the panels: a draft materialising
         * into a real chat (`replaceTab`) and a panel finding its subject gone
         * (`dropTab`) both commit without anyone clicking. Acting on the list
         * captured before the question would put those back as they were —
         * resurrecting a closed tab, or reverting a chat that now exists to
         * the draft it replaced. So a strip that changed underneath abandons
         * the navigation instead, guard and all: nothing was destroyed, and
         * the reader can ask again against what is actually open.
         *
         * Identity is enough. `commitTabs` assigns a fresh array every time,
         * so anything that moved the strip changed it.
         */
        if (tabsRef.current !== open || activeTabRef.current !== activeIndex)
          return
        // Here and only here does a panel actually go away, so here and only
        // here does its guard. On every other path the tab stays mounted, and
        // dropping a live guard would let the next replacement through unasked.
        guards.current.delete(tabKey(front))
        go()
      })
    },
    [commitTabs, guardTab]
  )

  /**
   * Ask for somewhere else to put the next thing.
   *
   * The only way the strip grows. It stands empty until something is chosen,
   * and that choice fills it rather than opening yet another tab.
   */
  const addEmptyTab = useCallback(() => {
    const next = openWorkspaceTab(tabsRef.current, { kind: "empty" })
    if (next.refused) {
      setTabLimit(true)
      return
    }
    setTabLimit(false)
    commitTabs(next.tabs, next.active)
  }, [commitTabs])

  /** Close a tab. Guarded, like the replacement that `showTab` performs. */
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
        setOpen(null)
        setCreation(undefined)
        setExpanded((old) => new Set([...old, node.id]))
        setDrawer(false)
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
    (_node: NativeNode, session: NeevAgentSession) => {
      keepAll(() => {
        setOpen(null)
        setCreation(undefined)
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
  // The resource kind shown for the active section. Agent Chats has no backend
  // resources yet (§5) — the center renders an honest connect/setup surface.
  const sectionKind: NativeResourceKind =
    section === "docs" ? "document" : "chat"
  // Lazily-loaded, section-scoped resource lists shown directly under each
  // expanded container in the sidebar (Notion/Slack-style direct navigation).
  type ItemList = { loading: boolean; items: NativeResource[]; error: unknown }
  const [itemsByNode, setItemsByNode] = useState<Record<number, ItemList>>({})
  /**
   * Whether trashed things are shown alongside the rest.
   *
   * This is what makes "Move to trash" an honest name. The API has no hard
   * delete — it archives, and archiving is reversible — but a reversal nobody
   * can find is indistinguishable from a deletion, and until now archived items
   * were simply absent from the sidebar with no way to list them. So the trash
   * is a view over the lists that are already here, and the thing that was
   * moved into it can be seen and restored from the same tree it left.
   *
   * Off by default, and off costs nothing: the archived half is only fetched
   * once someone asks to see it.
   */
  const [showTrash, setShowTrash] = useState(false)
  const [open, setOpen] = useState<{
    resource: NativeResource
    request: number
  } | null>(null)
  const openNumber = useRef(0)
  const itemsRequest = useRef(0)
  /** Per-node load counter, so two loads of one node cannot land out of order. */
  const itemGeneration = useRef(new Map<number, number>())
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
      /*
       * Per node, because `itemsRequest` is shared by all of them and does not
       * move between two loads of the SAME node. Toggling the trash on and off
       * quickly issues exactly that, and without this the slower first answer
       * — the one without the trashed rows — could land last and win.
       */
      const generation = (itemGeneration.current.get(nodeId) ?? 0) + 1
      itemGeneration.current.set(nodeId, generation)
      const current = () =>
        stamp === itemsRequest.current &&
        generation === itemGeneration.current.get(nodeId)
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
        //
        // And both states, but only while the trash is being shown. The
        // archived half is a second request per kind and almost nobody is
        // looking at it, so it is asked for when asked for and not before.
        const states = showTrash ? [false, true] : [false]
        const pages = await Promise.all(
          states.flatMap((archived) =>
            CONTENT_KINDS.map((kind) =>
              listNativeResources(
                org.id,
                nodeId,
                kind,
                archived,
                null,
                auth.signal
              )
            )
          )
        )
        if (!current()) return
        const items = pages
          .flatMap((page) => page.items)
          .sort((left, right) => left.title.localeCompare(right.title))
        setItemsByNode((old) => ({
          ...old,
          [nodeId]: { loading: false, items, error: null },
        }))
      } catch (e) {
        if (!current()) return
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
    [org, auth, showTrash]
  )
  /**
   * Showing or hiding the trash re-reads the folders already on screen.
   *
   * In place, deliberately: emptying the lists first would take the open tabs'
   * cached rows with them and make every one of them look like it was opening
   * again. The guard is a ref rather than the state, because `loadItems`
   * writes `itemsByNode` and this effect reads it — without it, the write
   * would trigger the read that caused it.
   */
  const trashShown = useRef(showTrash)
  useEffect(() => {
    if (trashShown.current === showTrash) return
    trashShown.current = showTrash
    for (const id of Object.keys(itemsByNode)) void loadItems(Number(id))
  }, [showTrash, itemsByNode, loadItems])
  // Restored resource tabs need their names even when their folder is closed.
  // Load each required folder once; keep its visible expansion state unchanged.
  useEffect(() => {
    const needed = new Set(expanded)
    for (const tab of tabs) {
      if (tab.kind === "resource") needed.add(tab.nodeId)
    }
    for (const id of needed) {
      if (!itemsByNode[id]) void loadItems(id)
    }
  }, [expanded, tabs, itemsByNode, loadItems])
  // One read for the whole tree. A failure here must not take the workspace
  // down: the sidebar simply shows no agent chats, and the centre surface still
  // reports its own errors.
  useEffect(() => {
    if (!org) return
    const controller = new AbortController()
    const load = async () => {
      try {
        const list = await listAgentSessions(showTrash, controller.signal)
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
            setExpanded((old) => new Set([...old, session.nodeId as number]))
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
  }, [org, agentsToken, auth, user, orgId, showTrash])

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

  /**
   * Kinds learned from a resource's own read.
   *
   * The sidebar's lists are lazy, so a tab restored on a cold start often has
   * no list entry to consult — and the strip would draw the neutral sheet
   * forever. `ResourcePanel` already fetches the resource to show it; this just
   * keeps what it found, so the icon settles as soon as the panel resolves.
   */
  const [openedKinds, setOpenedKinds] = useState<
    Record<string, NativeResourceKind>
  >({})
  /**
   * And its name, for the same reason the kind is kept.
   *
   * A tab restored from a previous session points at a resource that no loaded
   * list holds yet — the panel fetches it by id, and the lists may never be
   * asked for that node at all. Without this the tab strip and breadcrumb fall
   * back to "Loading your workspace…" and stay there for as long as the tab is
   * open, while the document itself is on screen and perfectly fine.
   */
  const [openedTitles, setOpenedTitles] = useState<Record<string, string>>({})
  const noteResource = useCallback((resource: NativeResource) => {
    setOpenedKinds((old) =>
      old[resource.id] === resource.kind
        ? old
        : { ...old, [resource.id]: resource.kind }
    )
    setOpenedTitles((old) =>
      old[resource.id] === resource.title
        ? old
        : { ...old, [resource.id]: resource.title }
    )
  }, [])
  /** What a resource tab points at, when anything yet knows. */
  const tabResourceKind = useCallback(
    (tab: WorkspaceTab): NativeResourceKind | undefined => {
      if (tab.kind !== "resource") return undefined
      const listed = (itemsByNode[tab.nodeId]?.items ?? []).find(
        (item) => item.id === tab.resourceId
      )
      return listed?.kind ?? openedKinds[tab.resourceId]
    },
    [itemsByNode, openedKinds]
  )

  const tabTitle = useCallback(
    (tab: WorkspaceTab): string => {
      if (tab.kind === "project") return nodeById(tab.nodeId)?.name ?? ""
      if (tab.kind === "resource")
        return (
          (itemsByNode[tab.nodeId]?.items ?? []).find(
            (item) => item.id === tab.resourceId
          )?.title ??
          openedTitles[tab.resourceId] ??
          // A tab is a few characters wide. "Loading your workspace…" was
          // truncated to "Loading your wo…" in it, which read as the name of
          // the thing rather than as its absence.
          t("opening")
        )
      if (tab.kind === "agent")
        return (
          agentSessions.find((item) => item.id === tab.sessionId)?.title ??
          t("opening")
        )
      // Asked for and not yet filled. It says what it is, rather than falling
      // through to the draft's name and claiming to be a chat nobody started.
      if (tab.kind === "empty") return t("emptyTab")
      // Provider-neutral: which one answers is chosen inside the chat and can
      // change before the first message, so the tab cannot claim one yet.
      return c("newAgentChat")
    },
    [nodeById, itemsByNode, openedTitles, agentSessions, t, c]
  )

  /** Start a draft of `kind` inside `node`, from the sidebar or the landing. */
  /**
   * Make it now, and let the writing name it.
   *
   * There is no form and no name to think of first: the thing exists a moment
   * after it is asked for, opens with the cursor in the body, and takes its
   * name from the first line written in it. Anyone who would rather name it
   * themselves still can, and doing so stops the name following the text.
   *
   * The request id is held per click, so a retry — or React mounting an effect
   * twice — replays the SAME creation rather than making a second one.
   */
  const createContent = useCallback(
    async (node: NativeNode, kind: NativeResourceKind) => {
      if (!auth.session || creating.current) return
      creating.current = true
      setCreateError(null)
      // One id per (project, kind) attempt, kept until it SUCCEEDS. A create
      // whose response was lost has very likely committed; retrying with a new
      // id would make a second document, so the retry replays the same one and
      // the server returns the row it already made.
      const attempt = `${node.id}:${kind}`
      const requestId =
        pendingCreate.current.get(attempt) ?? crypto.randomUUID()
      pendingCreate.current.set(attempt, requestId)
      // An answer that arrives after the reader has left this organization
      // must not open a tab in the next one. The ref is read at answer time
      // and written on every render, so it reflects the scope NOW — comparing
      // the captured `auth.scopeKey` with itself could only ever agree.
      const scope = scopeRef.current
      try {
        const resource = await createNativeResource(
          node.orgId,
          node.id,
          kind,
          {
            title: c(kind === "document" ? "untitledDocument" : "untitledChat"),
            requestId,
          },
          auth.session.csrf,
          auth.signal
        )
        if (scopeRef.current !== scope) return
        pendingCreate.current.delete(attempt)
        // The server's own row, straight into the list it belongs to: the tab
        // can say what it is called immediately instead of "Loading" until a
        // refetch catches up.
        setItemsByNode((old) => {
          const list = old[node.id] ?? {
            loading: false,
            items: [],
            error: null,
          }
          return {
            ...old,
            [node.id]: {
              ...list,
              items: [
                resource,
                ...list.items.filter((item) => item.id !== resource.id),
              ],
            },
          }
        })
        setExpanded((old) => new Set([...old, node.id]))
        void loadItems(node.id)
        setDrawer(false)
        // The same opening the sidebar performs, so the restore pointer moves
        // to what was just made. Without this a reload after creating
        // something brought the reader back to whatever they had open before
        // it, and the new thing was nowhere.
        setOpen({ resource, request: ++openNumber.current })
        // `fresh` is what lets the panel name it from the first thing typed.
        showTab({
          kind: "resource",
          resourceId: resource.id,
          nodeId: node.id,
          fresh: true,
        })
      } catch (cause) {
        if (scopeRef.current !== scope) return
        setCreateError(cause)
        auth.handleAuthorityError(cause)
      } finally {
        creating.current = false
      }
    },
    [auth, c, loadItems, showTab]
  )
  const openResource = useCallback(
    (node: NativeNode, resource: NativeResource) =>
      keepAll(() => {
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

  /**
   * Renaming and trashing, from the sidebar rather than from inside the thing.
   *
   * Both were reachable only by opening a document or a chat first and finding
   * the menu in its header — which is why they read as missing. The sidebar is
   * where you point at something you are not currently looking at.
   *
   * Neither is new capability. Rename is the title field of the PATCH that
   * already exists, and trash is its `archived` flag: there is no hard delete
   * in the API and nothing here adds one, so everything this does is
   * reversible from the trash view.
   */
  const applyResource = useCallback(
    (next: NativeResource) => {
      noteResource(next)
      setItemsByNode((old) => {
        const list = old[next.nodeId]
        if (!list) return old
        return {
          ...old,
          [next.nodeId]: {
            ...list,
            items: list.items.map((item) =>
              item.id === next.id ? next : item
            ),
          },
        }
      })
    },
    [noteResource]
  )
  const applySession = useCallback((next: NeevAgentSession) => {
    setAgentSessions((old) =>
      old.map((item) => (item.id === next.id ? next : item))
    )
  }, [])

  /**
   * The last sidebar mutation that failed, and whether one is in flight.
   *
   * These used to route the failure through `auth.handleError` alone, which
   * re-proves the session and says nothing: a 403, a revision conflict or a
   * dropped connection left the row exactly as it was with no account of why.
   * A destructive-looking action that silently does nothing is worse than one
   * that fails out loud.
   */
  const [mutationError, setMutationError] = useState<unknown>(null)
  const [mutationBusy, setMutationBusy] = useState(false)
  const [renameTarget, setRenameTarget] = useState<RenameTarget | null>(null)
  const [renameValue, setRenameValue] = useState("")
  const [renameBusy, setRenameBusy] = useState(false)
  const [renameError, setRenameError] = useState<unknown>(null)
  const openRename = useCallback((target: RenameTarget) => {
    // Menu items unmount when the dialog opens; keep a durable return target.
    const active = document.activeElement as HTMLElement | null
    const menu = active?.closest('[role="menu"]')
    opener.current =
      document.getElementById(menu?.getAttribute("aria-labelledby") ?? "") ??
      active
    setRenameValue(
      target.kind === "resource" ? target.resource.title : target.session.title
    )
    setRenameError(null)
    setRenameTarget(target)
  }, [])
  const renameResource = useCallback(
    (_node: NativeNode, resource: NativeResource) =>
      openRename({ kind: "resource", resource }),
    [openRename]
  )
  const renameSession = useCallback(
    (session: NeevAgentSession) => openRename({ kind: "agent", session }),
    [openRename]
  )
  /**
   * Trashing goes through the same unsaved-work guard as navigating away.
   *
   * Archiving what someone is part-way through writing makes the body
   * read-only under them, and anything not yet saved has nowhere to go. The
   * panel's own menu already asks first; reaching the same action from the
   * sidebar must not be the way around that question.
   */
  const trashResource = useCallback(
    (resource: NativeResource) => {
      if (!auth.session || mutationBusy) return
      guardAll(() => {
        void (async () => {
          setMutationBusy(true)
          setMutationError(null)
          try {
            applyResource(
              await updateNativeResource(
                resource.orgId,
                resource.id,
                {
                  nodeId: resource.nodeId,
                  title: resource.title,
                  visibility: resource.visibility,
                  archived: !resource.archived,
                  expectedAuthRevision: resource.authRevision,
                },
                auth.session!.csrf,
                auth.signal
              )
            )
          } catch (cause) {
            setMutationError(cause)
            auth.handleError(cause)
          } finally {
            setMutationBusy(false)
          }
        })()
      })
    },
    [auth, applyResource, guardAll, mutationBusy]
  )
  const trashSession = useCallback(
    (session: NeevAgentSession) => {
      if (!auth.session || mutationBusy) return
      guardAll(() => {
        void (async () => {
          setMutationBusy(true)
          setMutationError(null)
          try {
            applySession(
              await updateAgentSession(
                session.id,
                {
                  archived: !session.archived,
                  expectedRevision: session.revision,
                },
                auth.session!.csrf,
                auth.signal
              )
            )
          } catch (cause) {
            setMutationError(cause)
            auth.handleError(cause)
          } finally {
            setMutationBusy(false)
          }
        })()
      })
    },
    [auth, applySession, guardAll, mutationBusy]
  )
  const saveRename = useCallback(async () => {
    if (!renameTarget || !auth.session || renameBusy) return
    const title = renameValue.trim()
    if (!title) return
    setRenameBusy(true)
    setRenameError(null)
    try {
      if (renameTarget.kind === "resource") {
        const resource = renameTarget.resource
        applyResource(
          await updateNativeResource(
            resource.orgId,
            resource.id,
            {
              nodeId: resource.nodeId,
              title,
              visibility: resource.visibility,
              archived: resource.archived,
              expectedAuthRevision: resource.authRevision,
            },
            auth.session.csrf,
            auth.signal
          )
        )
      } else {
        const session = renameTarget.session
        applySession(
          await updateAgentSession(
            session.id,
            { title, expectedRevision: session.revision },
            auth.session.csrf,
            auth.signal
          )
        )
      }
      setRenameTarget(null)
    } catch (cause) {
      setRenameError(cause)
      auth.handleError(cause)
    } finally {
      setRenameBusy(false)
    }
  }, [renameTarget, renameValue, renameBusy, auth, applyResource, applySession])

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
  /**
   * What the sidebar marks as current.
   *
   * Derived from the active tab ALONE. The legacy `open` and `agentView`
   * pointers outlive the panels that set them — panels stay mounted — so
   * reading them here lit up the last document AND the last agent chat at once,
   * whichever tab was actually in front.
   */
  const front = tabs[activeTab]
  const frontNodeId =
    front === undefined
      ? null
      : front.kind === "project" || front.kind === "resource"
        ? front.nodeId
        : front.kind === "draft"
          ? front.nodeId
          : front.kind === "agent"
            ? (agentSessions.find((item) => item.id === front.sessionId)
                ?.nodeId ?? null)
            : null
  const frontResourceId = front?.kind === "resource" ? front.resourceId : null
  const frontSessionId = front?.kind === "agent" ? front.sessionId : null

  // Where the active tab lives: project, then the thing inside it.
  const breadcrumb: string[] = []
  if (front) {
    for (const name of frontNodeId === null ? [] : trail(nodes, frontNodeId))
      breadcrumb.push(name)
    if (front.kind !== "project") breadcrumb.push(tabTitle(front))
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
            openResourceId={frontResourceId}
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
            agentSessions={agentSessions}
            selectNode={selectNode}
            selectedNodeId={frontNodeId}
            openAgentSessionId={frontSessionId}
            openAgentSession={openAgentSession}
            showTrash={showTrash}
            renameResource={renameResource}
            trashResource={trashResource}
            renameSession={renameSession}
            trashSession={trashSession}
            actionsBusy={mutationBusy}
          />
          {/* Said here, beside the rows it is about. */}
          <NativeError error={mutationError} />
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
          {/* The project list, including its "Load more" pages. Inside an
              already-drawn sidebar, so it is not the workspace arriving. */}
          <p role="status" className="px-2 text-xs text-muted-foreground">
            {loading ? t("loadingItems") : ""}
          </p>
          <NativeError error={error} />
          <NativeError error={createError} />
        </div>
      </nav>
      <div className="border-t border-sidebar-border p-1.5">
        {/*
          The way back out of the trash, and the only reason "Move to trash" is
          an honest name for what archiving does.

          It used to sit at the bottom of the project LIST, which put it after
          however many projects had loaded — so on a full workspace it was below
          the fold, and it moved every time a page of projects arrived. It is
          not a project. It belongs with the things that are always there, one
          row above the account, where the same keyboard walk reaches it no
          matter how long the tree is.
        */}
        <button
          onClick={() => setShowTrash((value) => !value)}
          aria-pressed={showTrash}
          className="flex min-h-8 w-full items-center gap-2 rounded-lg px-2 text-start text-sm text-muted-foreground hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring"
        >
          <Trash2 aria-hidden className="size-3.5 shrink-0" />
          <span className="truncate">
            {t(showTrash ? "hideTrash" : "showTrash")}
          </span>
        </button>
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
            <div className="flex h-9 shrink-0 items-center gap-0.5 border-b px-1.5">
              {/*
                The list holds tabs and nothing else — New tab is a sibling of
                it rather than a child, so the strip does not announce a
                control as one of the things that are open.
              */}
              <div
                role="tablist"
                aria-label={t("openTabs")}
                className="flex min-w-0 items-center gap-0.5 overflow-x-auto"
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
                        <TabIcon
                          tab={tab}
                          resourceKind={tabResourceKind(tab)}
                        />
                        <span className="max-w-40 truncate">
                          {tabTitle(tab)}
                        </span>
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
              {/*
                The only control that grows the strip. Everything else replaces
                what is in front, so opening a second thing is a decision rather
                than a side effect of browsing.
              */}
              <button
                aria-label={t("newTab")}
                title={t("newTab")}
                onClick={addEmptyTab}
                className="flex size-6 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
              >
                <Plus aria-hidden className="size-3.5" />
              </button>
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
                    (tab.kind === "empty" ? (
                      /*
                       * A tab asked for and not yet filled. It says so rather
                       * than showing an empty panel, because a blank region
                       * with no explanation reads as something that failed to
                       * load.
                       */
                      <div
                        role="status"
                        className="flex min-h-0 flex-1 flex-col items-center justify-center gap-1 p-10 text-center"
                      >
                        <p className="text-sm font-medium">{t("emptyTab")}</p>
                        <p className="text-sm text-muted-foreground">
                          {t("emptyTabHint")}
                        </p>
                      </div>
                    ) : tab.kind === "project" ? (
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
                            else void createContent(node, kind)
                          }}
                        />
                      )
                    ) : tab.kind === "resource" ? (
                      <ResourcePanel
                        orgId={org.id}
                        resourceId={tab.resourceId}
                        fresh={tab.fresh === true}
                        onResolved={noteResource}
                        onChanged={(next) => {
                          /*
                           * Record the new name FIRST, and unconditionally.
                           *
                           * The list update below is a no-op when no list for
                           * this node is loaded — which is exactly the restored
                           * tab's situation — so on its own it leaves the tab
                           * strip and breadcrumb showing the name captured when
                           * the panel first resolved. A rename inside the frame
                           * then updates the document and nothing else, which is
                           * the bug this closes.
                           */
                          noteResource(next)
                          setItemsByNode((old) => {
                            const list = old[tab.nodeId]
                            if (!list) return old
                            return {
                              ...old,
                              [tab.nodeId]: {
                                ...list,
                                items: list.items.map((item) =>
                                  item.id === next.id ? next : item
                                ),
                              },
                            }
                          })
                        }}
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
      {/* Renaming, for the sidebar. One field, because a name is the only
          thing being changed and the PATCH behind it carries the rest of the
          row back unaltered. */}
      <Dialog
        open={renameTarget !== null}
        onOpenChange={(value) => {
          if (!value) setRenameTarget(null)
        }}
      >
        <DialogContent
          onCloseAutoFocus={(event) => {
            if (!opener.current) return
            event.preventDefault()
            opener.current.focus()
          }}
        >
          <DialogTitle>{c("rename")}</DialogTitle>
          <form
            className="space-y-4"
            onSubmit={(event) => {
              event.preventDefault()
              void saveRename()
            }}
          >
            <label className="block space-y-1">
              <span className="text-sm">{c("title")}</span>
              <input
                autoFocus
                value={renameValue}
                onChange={(event) => setRenameValue(event.target.value)}
                disabled={renameBusy}
                maxLength={100}
                className="h-10 w-full min-w-0 rounded-lg border bg-background px-3"
              />
            </label>
            <NativeError error={renameError} />
            <p role="status" className="text-sm">
              {renameBusy ? t("working") : ""}
            </p>
            <div className="flex justify-end gap-2">
              <Button
                variant="ghost"
                type="button"
                disabled={renameBusy}
                onClick={() => setRenameTarget(null)}
              >
                {t("cancel")}
              </Button>
              <Button
                type="submit"
                disabled={renameBusy || !renameValue.trim()}
              >
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
