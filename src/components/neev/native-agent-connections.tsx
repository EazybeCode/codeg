"use client"

// The caller's own per-user, per-provider agent connections (spec §5.4).
//
// Everything shown here is the server's own answer from
// `GET /api/neev/agent/connections`: the state, an account label and when it was
// last checked. Credentials never reach the browser. With no runtime bound every
// provider reads `disabled`, so this can never show a "Connected" the user
// cannot actually use.
//
// Refreshing re-reads that state, which is the only connection action the server
// currently exposes. Sign-in, reconnect and disconnect need endpoints that do
// not exist yet; rather than render buttons that would do nothing, the panel
// says so plainly and links to where those flows will live.

import { useCallback, useEffect, useState } from "react"
import Link from "next/link"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import {
  loadAgentConnections,
  type NeevAgentConnections,
} from "@/lib/neev-agent-sessions-client"
import { useNativeAuth } from "./native-auth-provider"
import { NativeError } from "./native-boundary"

export function NativeAgentConnectionsPanel({
  connections,
  onRefresh,
  onRequestSettings,
  showSettingsLink = true,
  heading,
}: {
  /** Supplied when a parent already loaded them; otherwise this loads its own. */
  connections?: NeevAgentConnections | null
  onRefresh?: () => void
  onRequestSettings?: () => void
  showSettingsLink?: boolean
  heading?: string
}) {
  const t = useTranslations("NativeContent"),
    n = useTranslations("Native")
  const auth = useNativeAuth()
  const owned = connections === undefined
  const [loaded, setLoaded] = useState<NeevAgentConnections | null>(null)
  const [error, setError] = useState<unknown>(null)
  const [refreshToken, setRefreshToken] = useState(0)
  const refresh = useCallback(() => setRefreshToken((value) => value + 1), [])

  useEffect(() => {
    if (!owned) return
    const controller = new AbortController()
    const load = async () => {
      try {
        const wiring = await loadAgentConnections(controller.signal)
        if (controller.signal.aborted) return
        setLoaded(wiring)
        setError(null)
      } catch (cause) {
        if (controller.signal.aborted) return
        setError(cause)
        auth.handleAuthorityError(cause)
      }
    }
    void load()
    return () => controller.abort()
  }, [owned, refreshToken, auth])

  const state = owned ? loaded : connections
  return (
    <section className="space-y-3 rounded-xl border p-5">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-sm font-medium">
          {heading ?? t("agentConnections")}
        </h2>
        <Button
          variant="outline"
          size="sm"
          onClick={owned ? refresh : onRefresh}
        >
          {t("refreshConnections")}
        </Button>
      </div>
      <NativeError error={error} />
      {state === null || state === undefined ? (
        <p role="status" className="text-sm text-muted-foreground">
          {n("loading")}
        </p>
      ) : (
        <>
          {!state.runtimeEnabled && (
            <p role="status" className="text-sm text-muted-foreground">
              {t("agentsOff")}
            </p>
          )}
          <ul className="space-y-2">
            {state.connections.map((connection) => (
              <li
                key={connection.provider}
                className="flex flex-wrap items-baseline gap-x-3 gap-y-1 text-sm"
              >
                <span className="font-medium">{connection.provider}</span>
                <span className="text-muted-foreground">
                  {t(`connectionState_${connection.state}`)}
                </span>
                {connection.accountLabel && (
                  <span className="truncate text-xs text-muted-foreground">
                    {connection.accountLabel}
                  </span>
                )}
                {connection.checkedAt !== null && (
                  <span className="text-xs text-muted-foreground">
                    {t("lastChecked")}{" "}
                    <time
                      dateTime={new Date(
                        connection.checkedAt * 1000
                      ).toISOString()}
                    >
                      {new Date(connection.checkedAt * 1000).toLocaleString()}
                    </time>
                  </span>
                )}
              </li>
            ))}
          </ul>
          {/* Honest about the gap: the server has no sign-in/reconnect/
              disconnect endpoint yet, so no button here would do anything. */}
          <p className="text-xs text-muted-foreground">
            {t("agentConnectActionsPending")}
          </p>
        </>
      )}
      {showSettingsLink && (
        <Link
          href="/settings"
          onClick={(event) => {
            if (onRequestSettings) {
              event.preventDefault()
              onRequestSettings()
            }
          }}
          className="inline-block rounded text-xs underline underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring"
        >
          {n("personalAgents")}
        </Link>
      )}
    </section>
  )
}
