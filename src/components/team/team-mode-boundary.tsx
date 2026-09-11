"use client"

import {
  createContext,
  useContext,
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react"
import { Loader2 } from "lucide-react"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { loadServerCapabilities } from "@/lib/neev-client"
import { detectEnvironment } from "@/lib/transport/detect"
import { setBrowserServerMode } from "@/lib/transport"

export type TeamMode = "unknown" | "operator" | "team" | "native"

const TeamModeContext = createContext<TeamMode>("unknown")

function subscribeToNativeEnvironment(): () => void {
  return () => {}
}

function readNativeEnvironment(): boolean {
  return detectEnvironment() === "tauri"
}

function readServerNativeEnvironment(): boolean {
  return false
}

export function useTeamMode(): TeamMode {
  return useContext(TeamModeContext)
}

export function TeamModeBoundary({
  operator,
  team,
  native: nativeIdentity,
}: {
  operator: React.ReactNode
  team: React.ReactNode
  native?: React.ReactNode
}) {
  const t = useTranslations("TeamModeBoundary")
  const native = useSyncExternalStore(
    subscribeToNativeEnvironment,
    readNativeEnvironment,
    readServerNativeEnvironment
  )
  const [mode, setMode] = useState<TeamMode>("unknown")
  const [failed, setFailed] = useState(false)
  const [attempt, setAttempt] = useState(0)
  const requestRef = useRef<AbortController | null>(null)

  useEffect(() => {
    // During hydration useSyncExternalStore deliberately exposes the server
    // snapshot first; re-read here so native never starts a browser request.
    if (readNativeEnvironment()) return

    requestRef.current?.abort()
    const controller = new AbortController()
    requestRef.current = controller
    setBrowserServerMode("unknown")

    void loadServerCapabilities(controller.signal)
      .then((capabilities) => {
        if (controller.signal.aborted) return
        setBrowserServerMode(capabilities.mode)
        if (capabilities.mode === "team" || capabilities.mode === "native") {
          try {
            window.localStorage.removeItem("codeg_token")
          } catch {
            // Storage can be unavailable; transport denial still holds.
          }
        }
        setMode(capabilities.mode)
      })
      .catch(() => {
        if (controller.signal.aborted) return
        setBrowserServerMode("unknown")
        setFailed(true)
      })

    return () => controller.abort()
  }, [attempt, native])

  const visibleMode = native ? "operator" : mode

  if (failed) {
    return (
      <main className="flex min-h-screen items-center justify-center bg-background px-4 text-foreground">
        <div className="w-full max-w-sm space-y-4 text-center">
          <h1 className="text-xl font-semibold">{t("unavailableTitle")}</h1>
          <p className="text-sm text-muted-foreground">
            {t("unavailableDescription")}
          </p>
          <Button
            onClick={() => {
              setMode("unknown")
              setFailed(false)
              setAttempt((value) => value + 1)
            }}
          >
            {t("retry")}
          </Button>
        </div>
      </main>
    )
  }

  if (visibleMode === "unknown") {
    return (
      <main
        aria-label={t("loading")}
        aria-busy="true"
        className="flex min-h-screen items-center justify-center bg-background text-foreground"
      >
        <Loader2 className="size-5 animate-spin text-primary" />
      </main>
    )
  }

  return (
    <TeamModeContext.Provider value={visibleMode}>
      {visibleMode === "native"
        ? nativeIdentity
        : visibleMode === "team"
          ? team
          : operator}
    </TeamModeContext.Provider>
  )
}
