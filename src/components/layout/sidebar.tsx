"use client"

import { useCallback, useEffect, useRef, useState, type ReactNode } from "react"
import {
  Crosshair,
  ChevronDown,
  Eye,
  ListChevronsDownUp,
  ListChevronsUpDown,
  LayoutTemplate,
  ListTodo,
  Map as MapIcon,
  Menu,
  MessagesSquare,
  Zap,
  type LucideIcon,
} from "lucide-react"
import { useTranslations } from "next-intl"
import { useSidebarContext } from "@/contexts/sidebar-context"
import { useAutomationsView } from "@/contexts/automations-view-context"
import { useTasksView } from "@/contexts/tasks-view-context"
import { useWorkbenchRoute } from "@/contexts/workbench-route-context"
import {
  SidebarConversationList,
  type SidebarConversationListHandle,
} from "@/components/conversations/sidebar-conversation-list"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { useIsMobile } from "@/hooks/use-mobile"
import { usePlatform } from "@/hooks/use-platform"
import { useZoomLevel } from "@/hooks/use-appearance"
import { isDesktop } from "@/lib/platform"
import { leftChromeReserve } from "@/lib/window-chrome"
import {
  isNavItemVisible,
  loadNavItemVisibility,
  loadShowCompleted,
  loadShowRecent,
  loadShowWorktrees,
  loadSortMode,
  loadSectionOrder,
  moveSectionInOrder,
  saveNavItemVisibility,
  saveShowCompleted,
  saveShowRecent,
  saveShowWorktrees,
  saveSortMode,
  saveSectionOrder,
  DEFAULT_SECTION_ORDER,
  SIDEBAR_NAV_ITEM_IDS,
  type SidebarNavItemId,
  type SidebarNavItemVisibility,
  type SidebarSectionId,
  type SidebarSortMode,
  type SidebarSectionOrder,
} from "@/lib/sidebar-view-mode-storage"
import { SidebarSectionOrderControl } from "./sidebar-section-order-control"
import { WorkspaceIdentity } from "./workspace-identity"
import { useWorkspaceCatalogContext } from "@/components/workspace/workspace-catalog-provider"
import { WorkspaceCatalogSidebar } from "@/components/workspace/workspace-catalog-sidebar"
import { cn } from "@/lib/utils"

// Which sections the order editor should render as switched-off. Module
// constants rather than a per-render `new Set`, so the reference is stable and
// the two states are spelled out once. "Recent" is the only section with a
// visibility toggle today.
const NO_HIDDEN_SECTIONS: ReadonlySet<SidebarSectionId> = new Set()
const RECENT_HIDDEN: ReadonlySet<SidebarSectionId> = new Set(["recent"])

// Icon per optional nav row. The visibility checkboxes in the view-options menu
// carry the same glyph as the row they switch, so that group reads as a mirror
// of the nav block rather than three bare labels.
const NAV_ITEM_ICONS: Record<SidebarNavItemId, LucideIcon> = {
  automations: Zap,
  tasks: ListTodo,
  forge: LayoutTemplate,
  canvas: MapIcon,
}

/**
 * A fixed top-of-sidebar action / route row. `active` marks the row as the
 * current workbench route (selected styling); `trailing` carries a shortcut hint
 * or a count badge. Extracting this keeps every fixed nav item — and any future
 * route — on one geometry instead of copy-pasting the className. Each row is a
 * `group` so a `group-hover`-revealed trailing element works.
 */
function SidebarNavButton({
  icon: Icon,
  label,
  onClick,
  active,
  trailing,
}: {
  icon: LucideIcon
  label: string
  onClick: () => void
  active?: boolean
  trailing?: ReactNode
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={label}
      aria-current={active ? "page" : undefined}
      className={cn(
        "group flex h-8 w-full items-center gap-[0.4375rem] rounded-md pl-[0.4375rem] pr-1.5",
        "text-[0.875rem] text-sidebar-foreground outline-none",
        "transition-colors duration-150 hover:bg-sidebar-accent",
        "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
        active && "bg-sidebar-primary/8"
      )}
    >
      <Icon className="h-[0.875rem] w-[0.875rem] shrink-0 text-muted-foreground" />
      <span className="truncate">{label}</span>
      {trailing}
    </button>
  )
}

export function Sidebar() {
  const t = useTranslations("Folder.sidebar")
  const tWorkspace = useTranslations("WorkspaceNavigation")
  const tFileTree = useTranslations("Folder.fileTreeTab")
  const { isOpen, toggle } = useSidebarContext()
  const { unseenFailures } = useAutomationsView()
  const { attentionCount } = useTasksView()
  const { routeId, setRoute } = useWorkbenchRoute()
  const [toolsExpanded, setToolsExpanded] = useState(false)
  const { isMac: platformIsMac } = usePlatform()
  const { zoomLevel } = useZoomLevel()
  const isMobile = useIsMobile()
  const listRef = useRef<SidebarConversationListHandle>(null)
  // On desktop the header's top-left is owned by the fixed window-chrome overlay
  // (sidebar toggle + remote); reserve exactly its width so the view controls
  // and drag region clear it. The reserve scales with the app zoom to track the
  // rem-sized overlay buttons. Mobile has no overlay (the sidebar is a Drawer).
  const leftReserve = leftChromeReserve(platformIsMac && isDesktop(), zoomLevel)

  // `showCompleted` defaults OFF; `showWorktrees` and `showRecent` default ON
  // (the mount effect below reconciles a persisted override). Each initial
  // value matches its own default so the pre-hydration render doesn't flash as
  // the stored preference is applied.
  const [showCompleted, setShowCompleted] = useState(false)
  const [showWorktrees, setShowWorktrees] = useState(true)
  const [showRecent, setShowRecent] = useState(true)
  // Empty = every nav row shown, which is also the hydrated default — so the
  // pre-hydration render matches for a user who never hid one.
  const [navItems, setNavItems] = useState<SidebarNavItemVisibility>({})
  const [sortMode, setSortMode] = useState<SidebarSortMode>("created")
  const [sectionOrder, setSectionOrder] = useState<SidebarSectionOrder>(
    DEFAULT_SECTION_ORDER
  )
  const [allExpanded, setAllExpanded] = useState(true)
  // General umbrella name for the eye menu (list toggles + nav rows + sort +
  // section order). Kept generic so the accessible name / tooltip stays
  // accurate as the menu gains options.
  const viewOptionsLabel = t("viewOptions")
  const toggleExpandLabel = allExpanded
    ? t("collapseAllGroups")
    : t("expandAllGroups")
  const hiddenSections = showRecent ? NO_HIDDEN_SECTIONS : RECENT_HIDDEN

  useEffect(() => {
    // Hydrate from localStorage after mount to keep SSR/CSR markup consistent.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setShowCompleted(loadShowCompleted())
    setShowWorktrees(loadShowWorktrees())
    setShowRecent(loadShowRecent())
    setNavItems(loadNavItemVisibility())
    setSortMode(loadSortMode())
    setSectionOrder(loadSectionOrder())
  }, [])

  const handleSetShowCompleted = useCallback((value: boolean) => {
    setShowCompleted(value)
    saveShowCompleted(value)
  }, [])

  const handleSetShowWorktrees = useCallback((value: boolean) => {
    setShowWorktrees(value)
    saveShowWorktrees(value)
  }, [])

  const handleSetShowRecent = useCallback((value: boolean) => {
    setShowRecent(value)
    saveShowRecent(value)
  }, [])

  const handleSetNavItem = useCallback(
    (id: SidebarNavItemId, visible: boolean) => {
      setNavItems((prev) => {
        const next = { ...prev, [id]: visible }
        saveNavItemVisibility(next)
        return next
      })
    },
    []
  )

  const handleSetSortMode = useCallback((value: string) => {
    const mode: SidebarSortMode = value === "updated" ? "updated" : "created"
    setSortMode(mode)
    saveSortMode(mode)
  }, [])

  // Nudge one section up/down a slot. `moveSectionInOrder` returns the SAME
  // array when the move would fall off an end, so a clamped nudge neither
  // re-renders the list nor rewrites localStorage.
  const handleMoveSection = useCallback(
    (id: SidebarSectionId, delta: number) => {
      setSectionOrder((prev) => {
        const next = moveSectionInOrder(prev, id, delta)
        if (next !== prev) saveSectionOrder(next)
        return next
      })
    },
    []
  )

  const handleToggleExpandAll = useCallback(() => {
    if (allExpanded) {
      listRef.current?.collapseAll()
      setAllExpanded(false)
    } else {
      listRef.current?.expandAll()
      setAllExpanded(true)
    }
  }, [allExpanded])

  const catalog = useWorkspaceCatalogContext()
  if (!isOpen) return null

  return (
    <aside className="@container/sidebar flex h-full min-h-0 flex-col overflow-hidden text-sidebar-foreground select-none">
      <div
        className={cn(
          "flex h-10 shrink-0 items-center gap-2 pr-2",
          // Desktop: the fixed left window-chrome overlay (reserved below) owns
          // the top-left, so drop the header's own left padding. Off-image the
          // divider is border-border/50, matching the conversation / file detail
          // headers. But the sidebar sits on a FROSTED surface (ws-surface-sidebar)
          // while those headers sit on the transparent canvas: with a workspace
          // background image on, a border-border/50 hairline washes out against the
          // frosted shade, so it takes the boosted `ws-chrome-border` (like the
          // frosted status bar) to stay legible. Mobile (Drawer): keep the original
          // title padding + a full-strength divider — mobile is unchanged.
          isMobile
            ? "border-b border-border pl-4"
            : "border-b border-border/50 ws-chrome-border pl-0"
        )}
      >
        {isMobile ? (
          <div className="flex min-w-0 items-center gap-4">
            <h2 className="truncate text-[0.875rem] font-bold tracking-[-0.00625rem] text-sidebar-foreground">
              {catalog?.enabled ? tFileTree("workspace") : t("title")}
            </h2>
          </div>
        ) : (
          // Reserve exactly the fixed left overlay's width so the view controls
          // clear it; the empty reserved space is a window-drag region.
          <div
            data-tauri-drag-region
            className="h-full shrink-0"
            style={{ width: leftReserve }}
          />
        )}
        {/* Draggable filler between the two clusters — the header is the
            window's top edge, so its empty space must move the window. */}
        <div data-tauri-drag-region className="h-full min-w-0 flex-1" />
        {!catalog?.enabled && (
          <div className="flex items-center gap-0.5">
            {/* Locate the active conversation in the list below (moved here from
              the conversation detail header). Always shown, leading the header
              cluster. The sidebar is unmounted while collapsed, so `listRef` is
              live whenever this button is visible. */}
            <Button
              variant="ghost"
              size="icon"
              className="h-6 w-6 shrink-0 text-muted-foreground"
              onClick={() => listRef.current?.scrollToActive()}
              title={t("locateActiveConversation")}
              aria-label={t("locateActiveConversation")}
            >
              <Crosshair aria-hidden="true" className="h-3.5 w-3.5" />
            </Button>
            {/* Expand/collapse-all is an icon-only header button on every
              viewport (it used to be a labelled row inside the view-options
              menu on desktop): it acts on the list right now instead of storing
              a display preference, so it does not belong among that menu's
              settings rows — and it took two clicks there. The icon itself
              states the direction, hence label-free with a tooltip. */}
            <Button
              variant="ghost"
              size="icon"
              className="h-6 w-6 shrink-0 text-muted-foreground"
              onClick={handleToggleExpandAll}
              title={toggleExpandLabel}
              aria-label={toggleExpandLabel}
            >
              {allExpanded ? (
                <ListChevronsDownUp
                  aria-hidden="true"
                  className="h-3.5 w-3.5"
                />
              ) : (
                <ListChevronsUpDown
                  aria-hidden="true"
                  className="h-3.5 w-3.5"
                />
              )}
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6 shrink-0 text-muted-foreground"
                  title={viewOptionsLabel}
                  aria-label={viewOptionsLabel}
                >
                  {/* An eye, not a funnel: nothing in this menu filters the list
                    down to matches — every option decides what is shown and in
                    what order. */}
                  <Eye aria-hidden="true" className="h-3.5 w-3.5" />
                </Button>
              </DropdownMenuTrigger>
              {/* Wider than the shared `min-w-48`: the section-order rows carry a
                position chip and two move buttons beside the name, and the menu
                clips overflow-x — 48 would start truncating longer localized
                section names. */}
              <DropdownMenuContent align="end" className="min-w-56">
                {/* Four groups: what the list shows → which nav rows exist → how
                  the list is sorted → how its sections are stacked. The first
                  two are hover-opened submenus rather than inline blocks: they
                  are set-and-forget on/off inventories, six checkboxes between
                  them, and inlining all six left a fifteen-row menu with Sort
                  by / Section order — the two settings people actually come
                  back for — stranded at the bottom of it. Those two stay
                  inline: a pair of radios and a ranked list read wrong behind
                  another hop, and the order rows need this menu's width.
                  Every option keeps the menu open on select (the default is to
                  close): this menu is a settings panel, not a command list, and
                  flipping two of them used to cost two round trips through the
                  trigger. The one action it used to carry — expand/collapse
                  all — is now a header button of its own. */}
                <DropdownMenuSub>
                  <DropdownMenuSubTrigger>
                    <MessagesSquare className="text-muted-foreground" />
                    {t("listOptions")}
                  </DropdownMenuSubTrigger>
                  {/* No width override: unlike the root content — held at exactly
                    `min-w-56` and clipping overflow-x — the sub-content grows to
                    fit its rows, which is the headroom these three (the longest
                    labels in the menu) want. */}
                  <DropdownMenuSubContent>
                    <DropdownMenuCheckboxItem
                      checked={showCompleted}
                      onCheckedChange={handleSetShowCompleted}
                      onSelect={(event) => event.preventDefault()}
                    >
                      {t("showCompleted")}
                    </DropdownMenuCheckboxItem>
                    <DropdownMenuCheckboxItem
                      checked={showWorktrees}
                      onCheckedChange={handleSetShowWorktrees}
                      onSelect={(event) => event.preventDefault()}
                    >
                      {t("showWorktrees")}
                    </DropdownMenuCheckboxItem>
                    <DropdownMenuCheckboxItem
                      checked={showRecent}
                      onCheckedChange={handleSetShowRecent}
                      onSelect={(event) => event.preventDefault()}
                    >
                      {t("showRecent")}
                    </DropdownMenuCheckboxItem>
                  </DropdownMenuSubContent>
                </DropdownMenuSub>
                <DropdownMenuSub>
                  <DropdownMenuSubTrigger>
                    <Menu className="text-muted-foreground" />
                    {t("navigationItems")}
                  </DropdownMenuSubTrigger>
                  {/* Driven by the id list itself, so a route added there can
                    never ship a row without its toggle. Each id doubles as its
                    message key. Hiding one only drops the sidebar shortcut: the
                    status bar's quick-actions menu still reaches every route, so
                    no choice here can strand the user on a page. */}
                  <DropdownMenuSubContent>
                    {SIDEBAR_NAV_ITEM_IDS.map((id) => {
                      const Icon = NAV_ITEM_ICONS[id]
                      return (
                        <DropdownMenuCheckboxItem
                          key={id}
                          checked={isNavItemVisible(navItems, id)}
                          onCheckedChange={(value) =>
                            handleSetNavItem(id, value)
                          }
                          onSelect={(event) => event.preventDefault()}
                        >
                          <Icon className="text-muted-foreground" />
                          {t(id)}
                        </DropdownMenuCheckboxItem>
                      )
                    })}
                  </DropdownMenuSubContent>
                </DropdownMenuSub>
                <DropdownMenuSeparator />
                <DropdownMenuLabel>{t("sortBy")}</DropdownMenuLabel>
                <DropdownMenuRadioGroup
                  value={sortMode}
                  onValueChange={handleSetSortMode}
                >
                  <DropdownMenuRadioItem
                    value="created"
                    onSelect={(event) => event.preventDefault()}
                  >
                    {t("sortByCreatedAt")}
                  </DropdownMenuRadioItem>
                  <DropdownMenuRadioItem
                    value="updated"
                    onSelect={(event) => event.preventDefault()}
                  >
                    {t("sortByUpdatedAt")}
                  </DropdownMenuRadioItem>
                </DropdownMenuRadioGroup>
                <DropdownMenuSeparator />
                <DropdownMenuLabel>{t("sectionOrder")}</DropdownMenuLabel>
                <SidebarSectionOrderControl
                  order={sectionOrder}
                  onMove={handleMoveSection}
                  hiddenSections={hiddenSections}
                />
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        )}
      </div>

      <WorkspaceIdentity state={catalog?.identity} />

      {/* On mobile, clicking a conversation card auto-closes the Drawer */}
      <div
        className="flex flex-col flex-1 min-h-0 overflow-hidden pt-1.5"
        onClick={
          isMobile
            ? (e) => {
                const target = e.target as HTMLElement
                if (target.closest("[data-conversation-id]")) {
                  toggle()
                }
              }
            : undefined
        }
      >
        {catalog?.enabled ? (
          <div className="flex-1 min-h-0 overflow-y-auto">
            <WorkspaceCatalogSidebar />
          </div>
        ) : (
          <SidebarConversationList
            ref={listRef}
            showCompleted={showCompleted}
            showWorktrees={showWorktrees}
            showRecent={showRecent}
            sortMode={sortMode}
            sectionOrder={sectionOrder}
          />
        )}
      </div>
      {!catalog?.enabled && (
        <div className="shrink-0 border-t border-sidebar-border/60 p-2">
          <button
            type="button"
            onClick={() => setToolsExpanded((value) => !value)}
            aria-expanded={toolsExpanded}
            aria-controls="workspace-tools"
            className="flex h-8 w-full items-center gap-2 rounded-md px-2 text-sm text-sidebar-foreground/80 hover:bg-sidebar-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <Menu className="size-4 text-muted-foreground" />
            {tWorkspace("tools")}
            <ChevronDown
              aria-hidden="true"
              className={cn(
                "ml-auto size-3.5 transition-transform",
                toolsExpanded && "rotate-180"
              )}
            />
          </button>
          <nav
            id="workspace-tools"
            aria-label={tWorkspace("tools")}
            hidden={!toolsExpanded}
            className="mt-1 max-h-[30dvh] overflow-y-auto space-y-0.5 border-l border-sidebar-border/60 pl-3 ml-3"
          >
            {isNavItemVisible(navItems, "automations") && (
              <SidebarNavButton
                icon={Zap}
                label={t("automations")}
                active={routeId === "automations"}
                onClick={() => {
                  if (isMobile) toggle()
                  setRoute("automations")
                }}
                trailing={
                  unseenFailures > 0 ? (
                    <span className="ml-auto inline-flex h-[0.9375rem] min-w-[0.9375rem] shrink-0 items-center justify-center rounded-full bg-destructive/15 px-1 font-mono text-[0.625rem] font-medium leading-none text-destructive">
                      {unseenFailures}
                    </span>
                  ) : null
                }
              />
            )}
            {isNavItemVisible(navItems, "tasks") && (
              <SidebarNavButton
                icon={ListTodo}
                label={t("tasks")}
                active={routeId === "tasks"}
                onClick={() => {
                  if (isMobile) toggle()
                  setRoute("tasks")
                }}
                trailing={
                  attentionCount > 0 ? (
                    // Attention (not failure): tasks waiting on the user — primary
                    // tint like the shortcut chips, not destructive.
                    <span className="ml-auto inline-flex h-[0.9375rem] min-w-[0.9375rem] shrink-0 items-center justify-center rounded-full bg-primary/10 px-1 font-mono text-[0.625rem] font-medium leading-none text-primary">
                      {attentionCount}
                    </span>
                  ) : null
                }
              />
            )}
            {isNavItemVisible(navItems, "forge") && (
              <SidebarNavButton
                icon={LayoutTemplate}
                label={t("forge")}
                active={routeId === "forge"}
                onClick={() => {
                  if (isMobile) toggle()
                  setRoute("forge")
                }}
              />
            )}
            {isNavItemVisible(navItems, "canvas") && (
              <SidebarNavButton
                icon={MapIcon}
                label={t("canvas")}
                active={routeId === "canvas"}
                onClick={() => {
                  if (isMobile) toggle()
                  setRoute("canvas")
                }}
              />
            )}
          </nav>
        </div>
      )}
    </aside>
  )
}
