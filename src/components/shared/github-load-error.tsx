import { GithubProjectError } from "@/lib/github-project-api"
import { Button } from "@/components/ui/button"
import { useTranslations } from "next-intl"

export function GithubLoadError({
  error,
  retry,
}: {
  error: unknown
  retry: () => void
}) {
  const t = useTranslations("WorkspaceCatalog")
  const needsAuth =
    error instanceof GithubProjectError &&
    (error.status === 401 || error.status === 403)
  return (
    <div
      role="alert"
      className="space-y-3 rounded-lg border border-border bg-muted/30 p-5 text-center"
    >
      <p className="text-sm font-medium">
        {t(needsAuth ? "authError" : "repositoryLoadError")}
      </p>
      <p className="text-xs leading-relaxed text-muted-foreground">
        {t("repositoryLoadHint")}
      </p>
      <div className="flex items-center justify-center gap-3">
        {needsAuth && (
          <a
            href="/auth/github/login"
            className="text-sm font-medium underline underline-offset-4"
          >
            {t("reconnect")}
          </a>
        )}
        <Button type="button" variant="outline" size="sm" onClick={retry}>
          {t("retry")}
        </Button>
      </div>
    </div>
  )
}
