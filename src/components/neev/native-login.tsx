"use client"

import { useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import Link from "next/link"
import { Button } from "@/components/ui/button"
import { beginNativeSignIn } from "@/lib/neev-client"
import { useNativeAuth } from "./native-auth-provider"
import { NativeBoundary, NativeError } from "./native-boundary"

export function NativeSignInMethods({
  linking = false,
  linked = [],
}: {
  linking?: boolean
  linked?: string[]
}) {
  const t = useTranslations("Native"),
    auth = useNativeAuth()
  const [email, setEmail] = useState(""),
    [busy, setBusy] = useState(false),
    [accepted, setAccepted] = useState(false),
    [error, setError] = useState<unknown>(null)
  const request = useRef<AbortController | null>(null)
  useEffect(() => () => request.current?.abort(), [])
  async function begin(provider: "google" | "email") {
    if (busy) return
    const controller = new AbortController()
    request.current = controller
    setBusy(true)
    setError(null)
    setAccepted(false)
    try {
      const url = await beginNativeSignIn(
        provider,
        email,
        linking ? auth.session?.csrf : undefined,
        controller.signal
      )
      if (controller.signal.aborted) return
      if (url) window.location.assign(url)
      else {
        setEmail("")
        setAccepted(true)
      }
    } catch (e) {
      if (!controller.signal.aborted) {
        setError(e)
        auth.handleError(e)
      }
    } finally {
      if (!controller.signal.aborted) setBusy(false)
    }
  }
  const googleAvailable =
    auth.capabilities?.providers.google && !linked.includes("google")
  const emailAvailable =
    auth.capabilities?.providers.email && !linked.includes("email")
  return (
    <div className="space-y-4">
      {linking && (
        <p className="text-sm text-muted-foreground">{t("recentAuth")}</p>
      )}
      <Button
        className="w-full"
        disabled={busy || !googleAvailable}
        onClick={() => void begin("google")}
      >
        {t(linking ? "linkGoogle" : "google")}
        {!googleAvailable &&
          ` · ${t(linked.includes("google") ? "linked" : "notConfigured")}`}
      </Button>
      <form
        className="space-y-3"
        onSubmit={(event) => {
          event.preventDefault()
          void begin("email")
        }}
      >
        <label
          className="block text-sm"
          htmlFor={linking ? "link-email" : "login-email"}
        >
          {t("email")}
        </label>
        <input
          id={linking ? "link-email" : "login-email"}
          type="email"
          autoComplete="email"
          required
          maxLength={254}
          value={email}
          disabled={busy || !emailAvailable}
          onChange={(e) => setEmail(e.target.value)}
          className="h-10 w-full min-w-0 rounded-lg border bg-background px-3 text-base focus-visible:outline-ring md:text-sm"
        />
        <Button
          type="submit"
          variant="outline"
          className="w-full"
          disabled={busy || !emailAvailable || !email.trim()}
        >
          {t(linking ? "linkEmail" : "emailSubmit")}
          {!emailAvailable &&
            ` · ${t(linked.includes("email") ? "linked" : "notConfigured")}`}
        </Button>
      </form>
      <Button variant="ghost" className="w-full" disabled>
        {t("github")} ·{" "}
        {t(linked.includes("github") ? "linked" : "notConfigured")}
      </Button>
      <p
        role="status"
        aria-live="polite"
        className="text-sm text-muted-foreground"
      >
        {busy ? t("working") : accepted ? t("emailAccepted") : ""}
      </p>
      <NativeError error={error} />
    </div>
  )
}
export function NativeLogin() {
  const t = useTranslations("Native")
  const auth = useNativeAuth()
  const callbackCaptured = useRef(false)
  const [callbackError, setCallbackError] = useState<
    "pending" | "account_conflict" | "sign_in_failed" | null
  >("pending")
  useEffect(() => {
    if (callbackCaptured.current) return
    callbackCaptured.current = true
    const query = new URLSearchParams(window.location.search)
    const code = query.get("auth_error")
    if (query.has("auth_error")) {
      // Never retain or reflect arbitrary callback/provider data in the URL.
      window.history.replaceState(null, "", window.location.pathname)
    }
    // The server export has no browser URL. Complete this external-state handoff
    // after hydration; keep the boundary paused until the capture is available.
    queueMicrotask(() =>
      setCallbackError(
        code === "account_conflict" || code === "sign_in_failed" ? code : null
      )
    )
  }, [])
  if (callbackError === "pending")
    return (
      <main role="status" aria-busy="true">
        {t("loading")}
      </main>
    )
  return (
    <NativeBoundary publicPage publicRecovery={callbackError !== null}>
      <main className="mx-auto flex min-h-dvh w-full max-w-sm flex-col justify-center gap-7 p-6">
        <header className="space-y-2">
          <p className="text-sm font-medium text-muted-foreground">Neev</p>
          <h1 className="text-3xl font-semibold tracking-tight">
            {t("welcome")}
          </h1>
          <p className="text-sm text-muted-foreground">
            {t("loginDescription")}
          </p>
        </header>
        {callbackError && (
          <p role="alert" className="text-sm text-destructive">
            {t(
              callbackError === "account_conflict"
                ? "callbackConflict"
                : "callbackFailed"
            )}
          </p>
        )}
        {auth.session ? (
          <div className="flex flex-col gap-3">
            <Button asChild>
              <Link href="/settings">{t("settings")}</Link>
            </Button>
            <Button variant="outline" onClick={() => void auth.logout()}>
              {t("signOut")}
            </Button>
          </div>
        ) : (
          <NativeSignInMethods />
        )}
        <p className="text-xs text-muted-foreground">{t("linkGuidance")}</p>
      </main>
    </NativeBoundary>
  )
}
