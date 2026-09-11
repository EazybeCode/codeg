"use client"

import { useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import {
  createOrganization,
  loadMemberships,
  selectOrganization,
  type NativeMembership,
} from "@/lib/neev-client"
import { NativeBoundary, NativeError } from "./native-boundary"
import { useNativeAuth } from "./native-auth-provider"

function MembershipChoices() {
  const t = useTranslations("Native"),
    auth = useNativeAuth()
  const { handleError } = auth
  const [memberships, setMemberships] = useState<NativeMembership[]>([]),
    [nextOffset, setNextOffset] = useState<number | null>(null)
  const [name, setName] = useState(""),
    [busy, setBusy] = useState(false),
    [loading, setLoading] = useState(true),
    [error, setError] = useState<unknown>(null)
  const receipt = useRef<{ name: string; id: string } | null>(null)
  useEffect(() => {
    const controller = new AbortController()
    void loadMemberships(0, controller.signal)
      .then((page) => {
        setMemberships(page.organizations)
        setNextOffset(page.nextOffset)
      })
      .catch((e) => {
        if (!controller.signal.aborted) {
          setError(e)
          handleError(e)
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false)
      })
    return () => controller.abort()
  }, [handleError])
  async function choose(id: number) {
    if (!auth.session || busy) return
    setBusy(true)
    setError(null)
    try {
      await selectOrganization(id, auth.session.csrf, auth.signal)
      await auth.changed()
    } catch (e) {
      if (!auth.signal?.aborted) {
        setError(e)
        auth.handleError(e)
        setBusy(false)
      }
    }
  }
  async function create() {
    if (!auth.session || busy) return
    setBusy(true)
    setError(null)
    const trimmed = name.trim()
    if (receipt.current?.name !== trimmed)
      receipt.current = { name: trimmed, id: crypto.randomUUID() }
    try {
      const org = await createOrganization(
        trimmed,
        receipt.current.id,
        auth.session.csrf,
        auth.signal
      )
      // Creation does not select or rotate the identity-only session.
      await selectOrganization(org.id, auth.session.csrf, auth.signal)
      await auth.changed()
    } catch (e) {
      if (!auth.signal?.aborted) {
        setError(e)
        auth.handleError(e)
        setBusy(false)
      }
    }
  }
  async function more() {
    if (nextOffset === null || loading) return
    setLoading(true)
    setError(null)
    try {
      const page = await loadMemberships(nextOffset, auth.signal)
      setMemberships((value) => [...value, ...page.organizations])
      setNextOffset(page.nextOffset)
    } catch (e) {
      setError(e)
      auth.handleError(e)
    } finally {
      setLoading(false)
    }
  }
  return (
    <main className="mx-auto w-full max-w-xl space-y-8 p-6 py-12 sm:py-20">
      <header className="space-y-2">
        <p className="text-sm text-muted-foreground">
          Neev ·{" "}
          <span className="break-words">{auth.session?.user.displayName}</span>
        </p>
        <h1 className="text-3xl font-semibold tracking-tight">
          {t("chooseWorkspace")}
        </h1>
        <p className="text-sm text-muted-foreground">
          {t("onboardingDescription")}
        </p>
      </header>
      <section className="space-y-3" aria-label={t("memberships")}>
        <h2 className="text-sm font-medium">{t("memberships")}</h2>
        {memberships.map((org) => (
          <Button
            key={org.id}
            variant="outline"
            className="h-auto w-full justify-between gap-3 py-3 text-start"
            disabled={busy}
            onClick={() => void choose(org.id)}
          >
            <span className="min-w-0 truncate" title={org.name}>
              {org.name}
            </span>
            <span className="shrink-0 text-xs text-muted-foreground">
              {t(org.role)}
            </span>
          </Button>
        ))}
        {!loading && memberships.length === 0 && (
          <p className="text-sm text-muted-foreground">{t("noMemberships")}</p>
        )}
        {nextOffset !== null && (
          <Button
            variant="ghost"
            disabled={loading || busy}
            onClick={() => void more()}
          >
            {t("loadMore")}
          </Button>
        )}
      </section>
      <form
        className="space-y-3 border-t pt-6"
        onSubmit={(event) => {
          event.preventDefault()
          void create()
        }}
      >
        <label
          htmlFor="organization-name"
          className="block text-sm font-medium"
        >
          {t("organizationName")}
        </label>
        <input
          autoComplete="organization"
          id="organization-name"
          required
          maxLength={100}
          value={name}
          disabled={busy}
          onChange={(event) => setName(event.target.value)}
          className="h-10 w-full rounded-lg border bg-background px-3 text-base focus-visible:outline-ring md:text-sm"
        />
        <Button disabled={busy || !name.trim()} type="submit">
          {t("createOrganization")}
        </Button>
      </form>
      <p
        role="status"
        aria-live="polite"
        className="text-sm text-muted-foreground"
      >
        {busy || loading ? t("working") : ""}
      </p>
      <NativeError error={error} />
      <Button variant="ghost" onClick={() => void auth.logout()}>
        {t("signOut")}
      </Button>
    </main>
  )
}
export function NativeOnboarding() {
  return (
    <NativeBoundary identity>
      <MembershipChoices />
    </NativeBoundary>
  )
}
