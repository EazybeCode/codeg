"use client"
import { useCallback, useEffect, useState } from "react"
import Image from "next/image"
import { Building2, RefreshCw } from "lucide-react"
import { useTranslations } from "next-intl"
import { isDesktop } from "@/lib/platform"

export type Identity = {
  user: { id: number; login: string; name: string | null; role: string }
  organization: {
    id: number
    login: string
    name: string | null
    avatar_url: string | null
  }
}
type IdentityState = {
  value: Identity | null
  status: "loading" | "ready" | "error" | "local"
}
export function useWorkspaceIdentity(enabled = true) {
  const [state, setState] = useState<IdentityState>({
    value: null,
    status: isDesktop() ? "local" : "loading",
  })
  const [attempt, setAttempt] = useState(0)
  const retry = useCallback(() => {
    setState({ value: null, status: "loading" })
    setAttempt((old) => old + 1)
  }, [])
  useEffect(() => {
    if (!enabled || isDesktop()) return
    const revalidate = () => setAttempt((old) => old + 1)
    window.addEventListener("focus", revalidate)
    window.addEventListener("workos:identity-changed", revalidate)
    return () => {
      window.removeEventListener("focus", revalidate)
      window.removeEventListener("workos:identity-changed", revalidate)
    }
  }, [enabled])
  useEffect(() => {
    if (!enabled || isDesktop()) return
    const controller = new AbortController()
    void fetch("/auth/me", {
      credentials: "same-origin",
      signal: controller.signal,
    })
      .then(async (response) => {
        if (response.status === 401) return null
        if (!response.ok) throw new Error("identity unavailable")
        return response.json() as Promise<Identity | null>
      })
      .then((value) => {
        if (!controller.signal.aborted) setState({ value, status: "ready" })
      })
      .catch(() => {
        if (!controller.signal.aborted)
          setState({ value: null, status: "error" })
      })
    return () => controller.abort()
  }, [attempt, enabled])
  return { ...state, retry }
}

export function WorkspaceIdentity({
  state,
}: {
  state?: ReturnType<typeof useWorkspaceIdentity>
}) {
  const own = useWorkspaceIdentity(!state)
  const { value: identity, status, retry } = state ?? own
  const t = useTranslations("WorkspaceCatalog")
  const organization =
    identity?.organization.name?.trim() || identity?.organization.login
  const userName = identity?.user.name?.trim() || identity?.user.login
  return (
    <section
      aria-label={t("account")}
      className="mx-3 mb-3 flex min-h-16 shrink-0 items-center gap-2.5 border-b border-sidebar-border/60 py-3"
    >
      <span className="flex size-8 shrink-0 items-center justify-center rounded-md border border-sidebar-border bg-background text-sm font-semibold">
        {identity?.organization.avatar_url ? (
          <Image
            src={identity.organization.avatar_url}
            alt={organization ?? identity.organization.login}
            width={32}
            height={32}
            unoptimized
            className="size-full rounded-md object-cover"
          />
        ) : organization ? (
          organization.slice(0, 1).toUpperCase()
        ) : (
          <Building2 className="size-4 text-muted-foreground" />
        )}
      </span>
      <div className="min-w-0 flex-1">
        {status === "loading" ? (
          <p className="text-xs text-muted-foreground">{t("accountLoading")}</p>
        ) : status === "error" ? (
          <div className="flex items-center gap-2">
            <p className="text-xs text-muted-foreground">{t("accountError")}</p>
            <button
              type="button"
              aria-label={t("accountRetry")}
              onClick={retry}
              className="rounded p-1 hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring"
            >
              <RefreshCw className="size-3.5" />
            </button>
          </div>
        ) : identity ? (
          <>
            <div
              className="truncate text-sm font-semibold"
              title={organization}
            >
              {organization}
            </div>
            <div className="truncate text-xs text-muted-foreground">
              <span>{userName}</span>
              <span aria-hidden="true"> · </span>
              <span>@{identity.user.login}</span>
            </div>
          </>
        ) : (
          <>
            <div className="text-sm font-semibold">
              {t(status === "local" ? "local" : "signedOut")}
            </div>
            {status !== "local" && (
              <a
                href="/auth/github/login"
                className="text-xs text-muted-foreground underline underline-offset-4"
              >
                {t("signIn")}
              </a>
            )}
          </>
        )}
      </div>
    </section>
  )
}
