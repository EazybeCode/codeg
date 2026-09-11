"use client"

import { useEffect } from "react"
import { useRouter } from "next/navigation"
import { isDesktop } from "@/lib/platform"
import { useTeamMode } from "@/components/team/team-mode-boundary"

function OperatorEntry() {
  const router = useRouter()
  useEffect(() => {
    if (isDesktop()) {
      router.replace("/workspace")
      return
    }
    // Web mode: validate token before entering app
    const token = localStorage.getItem("codeg_token")
    if (!token) {
      router.replace("/login")
      return
    }
    // Verify token is still valid
    fetch("/api/health", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${token}`,
      },
      body: "{}",
    })
      .then((res) => {
        if (res.ok) {
          router.replace("/workspace")
          return
        }
        if (res.status === 401) {
          // Token genuinely rejected → clear it and re-authenticate.
          localStorage.removeItem("codeg_token")
          router.replace("/login")
          return
        }
        // Server reachable but unhealthy (5xx / proxy error). Keep the token
        // and enter the app; the in-app reconnect dialog handles recovery
        // instead of bouncing a valid session to /login.
        router.replace("/workspace")
      })
      .catch(() => {
        // Server unreachable (restart, network blip, sleep/wake). The token is
        // almost certainly still valid — don't discard it. Enter the workspace
        // and let WebConnectionGuard surface the offline state and recover.
        router.replace("/workspace")
      })
  }, [router])
  return null
}

function TeamEntry() {
  const router = useRouter()
  useEffect(() => {
    router.replace("/workspace")
  }, [router])
  return null
}

export default function Page() {
  const mode = useTeamMode()
  if (mode === "unknown") return null
  return mode === "team" || mode === "native" ? (
    <TeamEntry />
  ) : (
    <OperatorEntry />
  )
}
