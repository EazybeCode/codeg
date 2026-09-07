"use client"
import { Plus } from "lucide-react"
import { useTranslations } from "next-intl"
import { useWorkspaceCatalogContext } from "./workspace-catalog-provider"
import { WorkspaceCatalogTree } from "./workspace-catalog-tree"
import { WorkspaceCatalogError } from "./workspace-catalog-error"
export function WorkspaceCatalogSidebar() {
  const t = useTranslations("WorkspaceCatalog")
  const catalog = useWorkspaceCatalogContext()
  if (!catalog?.enabled || !catalog.scope) return null
  return (
    <section aria-label={t("projects")} className="space-y-2 px-2 pb-3">
      <button
        type="button"
        onClick={() => catalog.create("project")}
        className="flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-sm hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring"
      >
        <Plus className="size-4 text-muted-foreground" />
        {t("new_project")}
      </button>
      <div className="px-2 pt-3 text-[11px] font-medium tracking-wide text-muted-foreground">
        {t("projects")}
      </div>
      {catalog.loading && (
        <p role="status" className="px-2 text-xs text-muted-foreground">
          {t("loading")}
        </p>
      )}
      {catalog.error != null && (
        <WorkspaceCatalogError
          error={catalog.error}
          retry={() => void catalog.refresh()}
        />
      )}
      {!catalog.loading && !catalog.error && !catalog.nodes.length && (
        <p className="px-2 py-2 text-xs leading-relaxed text-muted-foreground">
          {t("empty")}
        </p>
      )}
      <WorkspaceCatalogTree />
      <p className="px-2 pt-4 text-[11px] leading-relaxed text-muted-foreground">
        {t("trustedPreview")}
      </p>
    </section>
  )
}
