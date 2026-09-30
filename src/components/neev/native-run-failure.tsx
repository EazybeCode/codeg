"use client"

import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import type { NeevRunFailure } from "@/lib/neev-run-failure"

/** Stored turn codes that have their own sentence (`agentError_*`). */
const STORED_CODES = [
  "disabled",
  "unauthorized",
  "isolation",
  "unavailable",
  "unsupported",
  "io",
  "interrupted",
] as const

function isStoredCode(code: string): code is (typeof STORED_CODES)[number] {
  return (STORED_CODES as readonly string[]).includes(code)
}

/**
 * One failed run, said the way the provider said it, with the one action that
 * helps.
 *
 * The title is our sentence for the class (so it is translated and never
 * vague); the provider's own redacted words sit under it, because "usage limit,
 * resets at 3:05 PM" is information no generic sentence carries. Reconnect is
 * offered only for a sign-in failure and Retry only for one a second attempt
 * can fix — a button that cannot help is not shown.
 */
export function NativeRunFailure({
  failure,
  provider,
  onRetry,
  onReconnect,
  retrying = false,
  reconnected = false,
}: {
  failure: NeevRunFailure
  /** Display name of the provider, when known ("Codex", "Claude"). */
  provider?: string
  onRetry?: () => void
  onReconnect?: () => void
  retrying?: boolean
  /**
   * The provider reads as connected again since this failure. A sign-in
   * failure is then worth retrying, and Retry is offered beside Reconnect.
   */
  reconnected?: boolean
}) {
  const t = useTranslations("NativeContent")
  const canRetry =
    !!onRetry && (failure.retryable || (failure.reconnect && reconnected))
  return (
    <div
      role="alert"
      data-run-failure={failure.category}
      className="space-y-1.5 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm"
    >
      <p className="font-medium text-destructive">
        {failure.errorCode && isStoredCode(failure.errorCode)
          ? t(`agentError_${failure.errorCode}`)
          : t(`runError_${failure.category}`)}
      </p>
      {failure.message && (
        <p className="break-words text-xs text-muted-foreground">
          {provider
            ? t("runErrorProviderSaid", { name: provider })
            : t("runErrorDetail")}{" "}
          <span className="font-mono">{failure.message}</span>
        </p>
      )}
      {((failure.reconnect && onReconnect) || canRetry) && (
        <div className="flex gap-2 pt-0.5">
          {failure.reconnect && onReconnect && (
            <Button type="button" size="sm" onClick={onReconnect}>
              {t("runReconnect")}
            </Button>
          )}
          {canRetry && (
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={retrying}
              onClick={onRetry}
            >
              {retrying ? t("runRetrying") : t("runRetry")}
            </Button>
          )}
        </div>
      )}
    </div>
  )
}
