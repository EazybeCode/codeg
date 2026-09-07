"use client"
import { useTranslations } from "next-intl"
import { workspaceErrorKey } from "@/lib/workspace-errors"
import { Button } from "@/components/ui/button"

export function WorkspaceCatalogError({
  error,
  retry,
}: {
  error: unknown
  retry?: () => void
}) {
  const t = useTranslations("WorkspaceCatalog")
  const key = workspaceErrorKey(error)
  return (
    <div
      role="alert"
      className="space-y-2 rounded-md border border-border bg-muted/40 p-3 text-xs"
    >
      <p>{t(key)}</p>
      {key === "authError" ? (
        <a className="underline underline-offset-4" href="/auth/github/login">
          {t("reconnect")}
        </a>
      ) : retry ? (
        <Button size="sm" variant="outline" onClick={retry}>
          {t("retry")}
        </Button>
      ) : null}
    </div>
  )
}
