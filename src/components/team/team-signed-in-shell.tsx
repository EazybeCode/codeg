"use client"

import { useEffect } from "react"
import { Building2, Github, Loader2 } from "lucide-react"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { useTeamAuth } from "./team-auth-provider"
import dynamic from "next/dynamic"

/**
 * The team workspace is only reached once team auth has resolved, which is
 * already a network round trip, so deferring it costs nothing there — and it
 * keeps the team surface out of the native and operator initial scripts.
 */
const TeamWorkspace = dynamic(
  () => import("./team-workspace").then((module) => module.TeamWorkspace),
  { ssr: false, loading: () => null }
)

export function TeamSignedInShell({
  children,
}: {
  children?: React.ReactNode
}) {
  const t = useTranslations("TeamShell")
  const auth = useTeamAuth()
  const { activate, deactivate } = auth

  useEffect(() => {
    activate()
    return deactivate
  }, [activate, deactivate])

  if (auth.status === "idle" || auth.status === "loading") {
    return (
      <main
        aria-busy="true"
        className="flex min-h-screen items-center justify-center bg-background text-foreground"
      >
        <Loader2 className="size-5 animate-spin text-primary" />
      </main>
    )
  }

  if (auth.status === "unauthenticated") {
    return (
      <StatusCard
        title={t("signInRequired")}
        description={t("signInDescription")}
      >
        <Button asChild>
          <a href="/login">{t("goToSignIn")}</a>
        </Button>
      </StatusCard>
    )
  }

  if (auth.status === "unavailable") {
    return (
      <StatusCard
        title={t("unavailableTitle")}
        description={t("unavailableDescription")}
      >
        <div className="flex flex-wrap justify-center gap-2">
          <Button onClick={() => void auth.refresh()}>{t("retry")}</Button>
          <Button variant="outline" onClick={() => void auth.logout()}>
            {t("signOut")}
          </Button>
        </div>
        {auth.logoutError && (
          <p role="alert" className="text-sm text-destructive">
            {t("logoutFailed")}
          </p>
        )}
      </StatusCard>
    )
  }

  const session = auth.session
  if (!session) return null

  if (children == null) {
    return (
      <TeamWorkspace
        key={auth.scopeKey}
        session={session}
        logout={auth.logout}
        logoutError={auth.logoutError}
      />
    )
  }

  return (
    <main className="min-h-screen bg-background px-4 py-8 text-foreground">
      <div className="mx-auto flex w-full max-w-3xl flex-col gap-6">
        <header className="flex flex-wrap items-center justify-between gap-3">
          <a href="/workspace" className="text-xl font-bold tracking-tight">
            Neev
          </a>
          <div className="flex items-center gap-2">
            <Button asChild variant="ghost">
              <a href="/settings">{t("settings")}</a>
            </Button>
            <Button asChild variant="ghost">
              <a href="/workspace">{t("backToWorkspace")}</a>
            </Button>
            <Button variant="outline" onClick={() => void auth.logout()}>
              {t("signOut")}
            </Button>
          </div>
        </header>

        <section className="grid gap-3 sm:grid-cols-2">
          <IdentityCard
            icon={<Github className="size-5" />}
            label={t("personLabel")}
            name={session.user.name ?? session.user.login}
            login={session.user.login}
            image={session.user.avatarUrl}
          />
          <IdentityCard
            icon={<Building2 className="size-5" />}
            label={t("organizationLabel")}
            name={session.organization.name ?? session.organization.login}
            login={session.organization.login}
            image={session.organization.avatarUrl}
          />
        </section>

        {children}

        {auth.logoutError && (
          <p role="alert" className="text-center text-sm text-destructive">
            {t("logoutFailed")}
          </p>
        )}
      </div>
    </main>
  )
}

function StatusCard({
  title,
  description,
  children,
}: {
  title: string
  description: string
  children: React.ReactNode
}) {
  return (
    <main className="flex min-h-screen items-center justify-center bg-background px-4 text-foreground">
      <div className="w-full max-w-sm space-y-4 text-center">
        <p className="text-sm font-medium text-primary">Neev</p>
        <h1 className="text-xl font-semibold">{title}</h1>
        <p className="text-sm text-muted-foreground">{description}</p>
        {children}
      </div>
    </main>
  )
}

function IdentityCard({
  icon,
  label,
  name,
  login,
  image,
}: {
  icon: React.ReactNode
  label: string
  name: string
  login: string
  image: string | null
}) {
  return (
    <div className="flex items-center gap-3 rounded-2xl border bg-card p-4">
      {image ? (
        // Stored organization branding may be a normalized data:image/png URL.
        // eslint-disable-next-line @next/next/no-img-element
        <img
          src={image}
          alt=""
          referrerPolicy="no-referrer"
          className="size-11 shrink-0 rounded-full object-cover"
        />
      ) : (
        <span className="flex size-11 shrink-0 items-center justify-center rounded-full bg-muted">
          {icon}
        </span>
      )}
      <div className="min-w-0">
        <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          {label}
        </p>
        <p className="truncate font-medium">{name}</p>
        <p className="truncate text-sm text-muted-foreground">@{login}</p>
      </div>
    </div>
  )
}
