import { GithubProjectError } from "@/lib/github-project-api"
import { Button } from "@/components/ui/button"

export function GithubLoadError({
  error,
  retry,
}: {
  error: unknown
  retry: () => void
}) {
  const needsAuth =
    error instanceof GithubProjectError &&
    (error.status === 401 || error.status === 403)
  return (
    <div
      role="alert"
      className="space-y-3 rounded-lg border border-border bg-muted/30 p-5 text-center"
    >
      <p className="text-sm font-medium">
        {needsAuth
          ? "Connect GitHub to browse your repositories"
          : "Couldn’t load repositories"}
      </p>
      <p className="text-xs leading-relaxed text-muted-foreground">
        {needsAuth
          ? "Your GitHub connection is missing or has expired. Reconnect, then try again."
          : "Check your connection and try again. Your existing projects haven’t changed."}
      </p>
      <div className="flex items-center justify-center gap-3">
        {needsAuth && (
          <a
            href="/auth/github/login"
            className="text-sm font-medium underline underline-offset-4"
          >
            Connect GitHub
          </a>
        )}
        <Button type="button" variant="outline" size="sm" onClick={retry}>
          Try again
        </Button>
      </div>
    </div>
  )
}
