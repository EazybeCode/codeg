"use client"
import { Layers } from "lucide-react"
import { useTranslations } from "next-intl"
import { useWorkspaceCatalogContext } from "./workspace-catalog-provider"
import { Button } from "@/components/ui/button"
import { WorkspaceIdentity } from "@/components/layout/workspace-identity"

export function WorkspaceCatalogWelcome() {
  const t = useTranslations("WorkspaceCatalog")
  const catalog = useWorkspaceCatalogContext()
  if (!catalog?.enabled) return null
  const node = catalog.nodes.find((item) => item.id === catalog.selectedNodeId)
  return (
    <section className="mx-auto flex w-full max-w-xl flex-col items-start gap-5 px-6 py-12 sm:py-24">
      <Layers className="size-8 text-muted-foreground" aria-hidden />
      <div>
        <p className="mb-3 text-xs text-muted-foreground">
          {node
            ? catalog
                .pathFor(node.id)
                .map((item) => item.name)
                .join(" / ")
            : catalog.identity.value?.organization.name}
        </p>
        <h1 className="text-2xl font-semibold tracking-tight">
          {node?.name ?? t("welcome")}
        </h1>
        <p className="mt-3 max-w-md text-sm leading-relaxed text-muted-foreground">
          {node ? t("empty") : t("welcomeHint")}
        </p>
      </div>
      {!catalog.scope ? (
        <WorkspaceIdentity state={catalog.identity} />
      ) : (
        <div className="flex flex-wrap gap-2">
          {node ? (
            (["folder", "working_directory", "chat"] as const).map((kind) => (
              <Button
                key={kind}
                variant={kind === "chat" ? "default" : "outline"}
                onClick={() => catalog.create(kind, node.id)}
              >
                {t(`new_${kind}`)}
              </Button>
            ))
          ) : (
            <Button onClick={() => catalog.create("project")}>
              {t("new_project")}
            </Button>
          )}
        </div>
      )}
      <p className="text-xs text-muted-foreground">{t("trustedPreview")}</p>
    </section>
  )
}
