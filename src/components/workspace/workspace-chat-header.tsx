"use client"
import { Files, GitBranch } from "lucide-react"
import { useTranslations } from "next-intl"
import type { WorkspaceChatRuntime } from "@/lib/workspace-execution"
import { useAuxPanelContext } from "@/contexts/aux-panel-context"
import { useWorkspaceCatalogContext } from "./workspace-catalog-provider"

export function WorkspaceChatHeader({
  runtime,
}: {
  runtime: WorkspaceChatRuntime
}) {
  const t = useTranslations("WorkspaceCatalog")
  const catalog = useWorkspaceCatalogContext()
  const { revealInFileTree } = useAuxPanelContext()
  return (
    <header className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-2 border-b border-border/60 px-4 py-2 text-xs">
      <nav
        aria-label={t("projects")}
        className="min-w-0 flex-1 truncate text-muted-foreground"
      >
        {catalog
          ?.pathFor(runtime.draft.node_id)
          .map((node) => node.name)
          .join(" / ")}
      </nav>
      <span className="max-w-full truncate">{runtime.draft.full_name}</span>
      <span
        className="flex min-w-0 items-center gap-1 text-muted-foreground"
        title={t("branch")}
      >
        <GitBranch className="size-3 shrink-0" />
        <span className="truncate">{runtime.branch_name}</span>
      </span>
      <span className="rounded border border-border px-1.5 py-0.5 text-[10px] text-muted-foreground">
        {t("unpublished")}
      </span>
      <button
        type="button"
        onClick={() => {
          if (runtime.folder) revealInFileTree(runtime.folder.path)
        }}
        className="flex items-center gap-1 rounded px-2 py-1 hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
      >
        <Files className="size-3.5" />
        {t("files")}
      </button>
    </header>
  )
}
