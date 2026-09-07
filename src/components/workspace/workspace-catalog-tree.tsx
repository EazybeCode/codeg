"use client"
import {
  ChevronRight,
  Folder,
  FolderGit2,
  Layers,
  MessageSquare,
  MoreHorizontal,
} from "lucide-react"
import { useTranslations } from "next-intl"
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
} from "@/components/ui/dropdown-menu"
import { cn } from "@/lib/utils"
import { useWorkspaceCatalogContext } from "./workspace-catalog-provider"
import { WorkspaceCatalogError } from "./workspace-catalog-error"

export function WorkspaceCatalogTree({
  parentId = null,
  depth = 0,
}: {
  parentId?: number | null
  depth?: number
}) {
  const catalog = useWorkspaceCatalogContext()!
  const t = useTranslations("WorkspaceCatalog")
  return (
    <ul
      className={cn(
        "space-y-0.5",
        depth > 0 && "ml-3 border-l border-sidebar-border/60 pl-1.5"
      )}
    >
      {catalog.nodes
        .filter((node) => node.parent_id === parentId)
        .map((node) => {
          const expanded = catalog.expanded.has(node.id)
          const Icon =
            node.kind === "working_directory"
              ? FolderGit2
              : node.kind === "project"
                ? Layers
                : Folder
          const drafts = catalog.drafts[node.id]
          return (
            <li key={node.id}>
              <div
                className={cn(
                  "group flex h-8 min-w-0 items-center rounded-md pr-1 hover:bg-sidebar-accent",
                  catalog.selectedNodeId === node.id && "bg-sidebar-accent"
                )}
              >
                <button
                  type="button"
                  aria-label={t(expanded ? "collapse" : "expand", {
                    name: node.name,
                  })}
                  aria-expanded={expanded}
                  onClick={() => catalog.toggle(node.id)}
                  className="flex size-6 shrink-0 items-center justify-center rounded focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <ChevronRight
                    aria-hidden
                    className={cn(
                      "size-3 text-muted-foreground transition-transform",
                      expanded && "rotate-90"
                    )}
                  />
                </button>
                <button
                  type="button"
                  onClick={() => catalog.selectNode(node.id)}
                  className="flex h-full min-w-0 flex-1 items-center gap-2 rounded text-left text-sm focus-visible:ring-2 focus-visible:ring-ring"
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
                      className="flex size-6 shrink-0 items-center justify-center rounded text-muted-foreground opacity-60 hover:bg-sidebar-accent hover:opacity-100 focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-ring"
                    >
                      <MoreHorizontal className="size-4" />
                    </button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="start">
                    {(["folder", "working_directory", "chat"] as const).map(
                      (kind) => (
                        <DropdownMenuItem
                          key={kind}
                          onSelect={() => catalog.create(kind, node.id)}
                        >
                          {t(`new_${kind}`)}
                        </DropdownMenuItem>
                      )
                    )}
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>
              {expanded && (
                <>
                  <WorkspaceCatalogTree parentId={node.id} depth={depth + 1} />
                  <div className="ml-6 pl-1.5">
                    {drafts?.loading && (
                      <p
                        role="status"
                        className="px-2 py-1 text-xs text-muted-foreground"
                      >
                        {t("loadingChats")}
                      </p>
                    )}
                    {drafts?.error != null && (
                      <WorkspaceCatalogError
                        error={drafts.error}
                        retry={() => void catalog.refreshDrafts(node.id)}
                      />
                    )}
                    {drafts?.items.map((draft) => (
                      <button
                        key={draft.id}
                        type="button"
                        onClick={() => catalog.openChat(draft.id)}
                        className={cn(
                          "flex min-h-8 w-full items-center gap-2 rounded px-2 text-left text-sm hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring",
                          catalog.chatId === draft.id && "bg-sidebar-accent"
                        )}
                      >
                        <MessageSquare
                          aria-hidden
                          className="size-3.5 shrink-0 text-muted-foreground"
                        />
                        <span className="truncate">
                          {draft.title ?? t("untitledChat")}
                        </span>
                      </button>
                    ))}
                    {drafts &&
                      !drafts.loading &&
                      !drafts.error &&
                      !drafts.items.length && (
                        <p className="px-2 py-1 text-xs text-muted-foreground">
                          {t("noChats")}
                        </p>
                      )}
                  </div>
                </>
              )}
            </li>
          )
        })}
    </ul>
  )
}
