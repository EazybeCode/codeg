"use client"

import {
  ChevronRight,
  Folder,
  FolderGit2,
  Layers3,
  MessageSquare,
  MoreHorizontal,
} from "lucide-react"
import { useTranslations } from "next-intl"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import type {
  TeamCatalogChat,
  TeamCatalogNode,
  TeamNodeKind,
} from "@/lib/team-catalog-client"

type CreateKind = TeamNodeKind | "chat"

export function TeamWorkspaceTree({
  nodes,
  chats,
  expanded,
  selectedNodeId,
  selectedChatId,
  onToggle,
  onSelectNode,
  onSelectChat,
  onCreate,
  onEdit,
  onLoadMore,
  hasMore,
  loadingMore,
  parentId = null,
  depth = 0,
}: {
  nodes: TeamCatalogNode[]
  chats: TeamCatalogChat[]
  expanded: Set<number>
  selectedNodeId: number | null
  selectedChatId: number | null
  onToggle: (id: number) => void
  onSelectNode: (id: number) => void
  onSelectChat: (id: number) => void
  onCreate: (kind: CreateKind, parentId: number) => void
  onEdit: (node: TeamCatalogNode) => void
  onLoadMore: () => void
  hasMore: boolean
  loadingMore: boolean
  parentId?: number | null
  depth?: number
}) {
  const t = useTranslations("TeamWorkspace")
  const children = nodes.filter((node) => node.parentId === parentId)
  return (
    <ul
      className={cn(
        "space-y-0.5",
        depth > 0 && "ml-3 border-l border-sidebar-border/60 pl-1.5"
      )}
    >
      {children.map((node) => {
        const open = expanded.has(node.id)
        const Icon =
          node.kind === "project"
            ? Layers3
            : node.kind === "working_directory"
              ? FolderGit2
              : Folder
        return (
          <li key={node.id}>
            <div
              className={cn(
                "group flex min-h-8 min-w-0 items-center rounded-lg pr-1 hover:bg-sidebar-accent",
                selectedNodeId === node.id &&
                  selectedChatId == null &&
                  "bg-sidebar-accent"
              )}
            >
              <button
                type="button"
                aria-expanded={open}
                aria-label={t(open ? "collapse" : "expand", {
                  name: node.name,
                })}
                onClick={() => onToggle(node.id)}
                className="flex size-7 shrink-0 items-center justify-center rounded focus-visible:ring-2 focus-visible:ring-ring"
              >
                <ChevronRight
                  aria-hidden
                  className={cn(
                    "size-3.5 text-muted-foreground transition-transform",
                    open && "rotate-90"
                  )}
                />
              </button>
              <button
                type="button"
                onClick={() => onSelectNode(node.id)}
                className="flex min-h-8 min-w-0 flex-1 items-center gap-2 rounded text-left text-sm focus-visible:ring-2 focus-visible:ring-ring"
              >
                <Icon
                  aria-hidden
                  className="size-4 shrink-0 text-muted-foreground"
                />
                <span className="truncate">{node.name}</span>
              </button>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button
                    type="button"
                    aria-label={t("actions", { name: node.name })}
                    className="flex size-7 shrink-0 items-center justify-center rounded text-muted-foreground opacity-60 hover:bg-sidebar-accent focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-ring"
                  >
                    <MoreHorizontal aria-hidden className="size-4" />
                  </button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start">
                  {(["folder", "working_directory", "chat"] as const).map(
                    (kind) => (
                      <DropdownMenuItem
                        key={kind}
                        onSelect={() => onCreate(kind, node.id)}
                      >
                        {t(`new_${kind}`)}
                      </DropdownMenuItem>
                    )
                  )}
                  <DropdownMenuItem onSelect={() => onEdit(node)}>
                    {t("editDetails")}
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
            {open ? (
              <div>
                <TeamWorkspaceTree
                  nodes={nodes}
                  chats={chats}
                  expanded={expanded}
                  selectedNodeId={selectedNodeId}
                  selectedChatId={selectedChatId}
                  onToggle={onToggle}
                  onSelectNode={onSelectNode}
                  onSelectChat={onSelectChat}
                  onCreate={onCreate}
                  onEdit={onEdit}
                  onLoadMore={onLoadMore}
                  hasMore={false}
                  loadingMore={loadingMore}
                  parentId={node.id}
                  depth={depth + 1}
                />
                <div className="ml-6 space-y-0.5 pl-1.5">
                  {chats
                    .filter((chat) => chat.nodeId === node.id)
                    .map((chat) => (
                      <button
                        key={chat.id}
                        type="button"
                        onClick={() => onSelectChat(chat.id)}
                        className={cn(
                          "flex min-h-8 w-full items-center gap-2 rounded-lg px-2 text-left text-sm hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring",
                          selectedChatId === chat.id && "bg-sidebar-accent"
                        )}
                      >
                        <MessageSquare
                          aria-hidden
                          className="size-3.5 shrink-0 text-muted-foreground"
                        />
                        <span className="min-w-0 flex-1 truncate">
                          {chat.title ?? t("untitledChat")}
                        </span>
                        <span className="sr-only">
                          {t(`visibility_${chat.visibility}`)}
                        </span>
                      </button>
                    ))}
                </div>
              </div>
            ) : null}
          </li>
        )
      })}
      {depth === 0 && hasMore ? (
        <li className="pt-2">
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="w-full"
            disabled={loadingMore}
            onClick={onLoadMore}
          >
            {loadingMore ? t("loadingMore") : t("loadMore")}
          </Button>
        </li>
      ) : null}
    </ul>
  )
}
