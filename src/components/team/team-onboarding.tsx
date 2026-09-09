"use client"

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
} from "react"
import { Github, Loader2, Search } from "lucide-react"
import { useRouter } from "next/navigation"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { BrowserLink } from "@/components/ui/browser-link"
import { Input } from "@/components/ui/input"
import {
  TeamClientError,
  loadPendingChoices,
  selectOrganization,
  type PendingChoices,
} from "@/lib/team-client"
import { notifyTeamAuthChanged } from "./team-auth-provider"

const INSTALL_APP_URL = "https://github.com/apps/eazyworkos/installations/new"
const SEARCH_THRESHOLD = 6

type LoadState = "loading" | "ready" | "expired" | "unavailable"

export function TeamOnboarding() {
  const t = useTranslations("TeamOnboarding")
  const router = useRouter()
  const [state, setState] = useState<LoadState>("loading")
  const [choices, setChoices] = useState<PendingChoices | null>(null)
  const [selectedId, setSelectedId] = useState("")
  const [query, setQuery] = useState("")
  const [submitting, setSubmitting] = useState(false)
  const [selectionError, setSelectionError] = useState<
    "unavailable" | "invalid" | null
  >(null)
  const errorRef = useRef<HTMLDivElement | null>(null)
  const requestRef = useRef<AbortController | null>(null)

  const load = useCallback(async () => {
    requestRef.current?.abort()
    const controller = new AbortController()
    requestRef.current = controller
    setState("loading")
    setSelectionError(null)
    try {
      const next = await loadPendingChoices(controller.signal)
      if (controller.signal.aborted) return
      setChoices(next)
      setState("ready")
      notifyTeamAuthChanged()
    } catch (error) {
      if (controller.signal.aborted) return
      setChoices(null)
      setState(
        error instanceof TeamClientError && error.kind === "unauthorized"
          ? "expired"
          : "unavailable"
      )
    }
  }, [])

  useEffect(() => {
    void load()
    return () => requestRef.current?.abort()
  }, [load])

  useEffect(() => {
    if (selectionError) errorRef.current?.focus()
  }, [selectionError])

  const visibleOrganizations = useMemo(() => {
    if (!choices) return []
    const normalized = query.trim().toLocaleLowerCase()
    if (!normalized) return choices.organizations
    return choices.organizations.filter((organization) =>
      organization.login.toLocaleLowerCase().includes(normalized)
    )
  }, [choices, query])

  async function submit(event: FormEvent) {
    event.preventDefault()
    if (!choices || !selectedId || submitting) return
    setSubmitting(true)
    setSelectionError(null)
    const controller = new AbortController()
    requestRef.current = controller
    try {
      const result = await selectOrganization(
        selectedId,
        choices.csrf,
        controller.signal
      )
      if (controller.signal.aborted) return
      notifyTeamAuthChanged()
      router.replace(result.redirect)
    } catch (error) {
      if (controller.signal.aborted) return
      if (error instanceof TeamClientError && error.kind === "unauthorized") {
        setChoices(null)
        setState("expired")
      } else {
        setSelectionError(
          error instanceof TeamClientError && error.kind === "unavailable"
            ? "unavailable"
            : "invalid"
        )
      }
    } finally {
      if (!controller.signal.aborted) setSubmitting(false)
    }
  }

  return (
    <main
      aria-busy={state === "loading" || submitting}
      className="flex min-h-screen items-center justify-center bg-background py-8 text-foreground"
    >
      <div className="w-full max-w-lg space-y-6 px-4">
        <div className="space-y-2 text-center">
          <p className="text-sm font-medium text-primary">Neev</p>
          <h1 className="text-2xl font-bold tracking-tight">{t("title")}</h1>
          <p className="text-sm text-muted-foreground">{t("subtitle")}</p>
        </div>

        {state === "loading" && (
          <div className="flex items-center justify-center gap-2 py-8 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" />
            {t("loading")}
          </div>
        )}

        {state === "expired" && (
          <Recovery
            title={t("expiredTitle")}
            description={t("expiredDescription")}
            action={t("restartSignIn")}
          />
        )}

        {state === "unavailable" && (
          <div className="space-y-4 text-center">
            <h2 className="font-semibold">{t("unavailableTitle")}</h2>
            <p className="text-sm text-muted-foreground">
              {t("unavailableDescription")}
            </p>
            <div className="flex flex-wrap justify-center gap-2">
              <Button type="button" onClick={() => void load()}>
                {t("retry")}
              </Button>
              <Button asChild variant="ghost">
                <a href="/auth/github/login">{t("restartSignIn")}</a>
              </Button>
            </div>
          </div>
        )}

        {state === "ready" && choices && (
          <>
            <section className="rounded-2xl border bg-card p-4">
              <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                {t("githubIdentity")}
              </p>
              <div className="mt-3 flex items-center gap-3">
                <IdentityImage src={choices.identity.avatarUrl} />
                <div className="min-w-0">
                  <p className="truncate font-medium">
                    {choices.identity.name ?? choices.identity.login}
                  </p>
                  <p className="truncate text-sm text-muted-foreground">
                    @{choices.identity.login}
                  </p>
                </div>
              </div>
            </section>

            {choices.organizations.length === 0 ? (
              <div className="space-y-4 rounded-2xl border bg-card p-5 text-center">
                <h2 className="font-semibold">{t("zeroTitle")}</h2>
                <p className="text-sm text-muted-foreground">
                  {t("zeroDescription")}
                </p>
                <p className="text-sm text-muted-foreground">
                  {t("pendingInvites")}
                </p>
                <div className="flex flex-col gap-2 sm:flex-row sm:justify-center">
                  <Button asChild variant="outline">
                    <BrowserLink href={INSTALL_APP_URL}>
                      <Github />
                      {t("installApp")}
                    </BrowserLink>
                  </Button>
                  <Button type="button" onClick={() => void load()}>
                    {t("retry")}
                  </Button>
                  <Button asChild variant="ghost">
                    <a href="/auth/github/login">{t("restartSignIn")}</a>
                  </Button>
                </div>
              </div>
            ) : (
              <form onSubmit={submit} className="space-y-4">
                {choices.organizations.length > SEARCH_THRESHOLD && (
                  <div className="relative">
                    <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                    <Input
                      type="search"
                      aria-label={t("searchLabel")}
                      placeholder={t("searchPlaceholder")}
                      value={query}
                      onChange={(event) => setQuery(event.target.value)}
                      className="pl-9"
                    />
                  </div>
                )}
                <fieldset className="space-y-2">
                  <legend className="mb-2 text-sm font-medium">
                    {t("choicesLabel")}
                  </legend>
                  {visibleOrganizations.map((organization) => (
                    <label
                      key={organization.githubOrgId}
                      className="flex cursor-pointer items-center gap-3 rounded-xl border bg-card p-3 has-[:checked]:border-primary has-[:checked]:ring-2 has-[:checked]:ring-primary/20"
                    >
                      <input
                        type="radio"
                        name="organization"
                        value={organization.githubOrgId}
                        checked={selectedId === organization.githubOrgId}
                        onChange={(event) => setSelectedId(event.target.value)}
                        className="size-4 accent-primary"
                      />
                      <IdentityImage src={organization.avatarUrl} />
                      <span className="min-w-0 flex-1 truncate font-medium">
                        {organization.login}
                      </span>
                      <span className="text-xs text-muted-foreground">
                        {t(
                          organization.role === "admin"
                            ? "roleAdmin"
                            : "roleMember"
                        )}
                      </span>
                    </label>
                  ))}
                </fieldset>

                {selectionError && (
                  <div
                    ref={errorRef}
                    role="alert"
                    tabIndex={-1}
                    className="rounded-xl border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive outline-none"
                  >
                    {t(
                      selectionError === "unavailable"
                        ? "selectionUnavailable"
                        : "selectionInvalid"
                    )}
                  </div>
                )}

                <Button
                  type="submit"
                  size="lg"
                  className="w-full"
                  disabled={!selectedId || submitting}
                >
                  {submitting ? t("continuing") : t("continue")}
                </Button>
                <div className="text-center">
                  <a
                    href="/auth/github/login"
                    className="text-sm text-muted-foreground underline-offset-4 hover:underline"
                  >
                    {t("restartSignIn")}
                  </a>
                </div>
              </form>
            )}
          </>
        )}
      </div>
    </main>
  )
}

function IdentityImage({ src }: { src: string | null }) {
  if (!src) {
    return (
      <span className="flex size-10 shrink-0 items-center justify-center rounded-full bg-muted">
        <Github className="size-5" />
      </span>
    )
  }
  return (
    // Provider-controlled identity image; referrer data is never sent.
    // eslint-disable-next-line @next/next/no-img-element
    <img
      src={src}
      alt=""
      referrerPolicy="no-referrer"
      className="size-10 shrink-0 rounded-full object-cover"
    />
  )
}

function Recovery({
  title,
  description,
  action,
}: {
  title: string
  description: string
  action: string
}) {
  return (
    <div className="space-y-4 text-center">
      <h2 className="font-semibold">{title}</h2>
      <p className="text-sm text-muted-foreground">{description}</p>
      <Button asChild>
        <a href="/auth/github/login">{action}</a>
      </Button>
    </div>
  )
}
