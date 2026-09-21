"use client"

import { useEffect } from "react"
import { useRouter } from "next/navigation"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { NativeClientError } from "@/lib/neev-client"
import { useNativeAuth } from "./native-auth-provider"
import { NativeSuspendedSurface } from "./native-suspended-surface"

export function NativeError({ error }: { error: unknown }) {
  const t = useTranslations("Native")
  if (
    !error ||
    (error instanceof NativeClientError && error.kind === "aborted")
  )
    return null
  const kind = error instanceof NativeClientError ? error.kind : "unavailable"
  return (
    <p role="alert" className="text-sm text-destructive">
      {t(`error_${kind}`)}
    </p>
  )
}
export function NativeBoundary({
  children,
  identity = false,
  publicPage = false,
  publicRecovery = false,
}: {
  children: React.ReactNode
  identity?: boolean
  publicPage?: boolean
  publicRecovery?: boolean
}) {
  const auth = useNativeAuth(),
    router = useRouter(),
    t = useTranslations("Native")
  const destination =
    auth.status === "unauthenticated" && !publicPage
      ? "/login"
      : auth.session?.scope === "identity" && !identity && !publicPage
        ? "/onboarding"
        : auth.session?.scope === "organization" &&
            (identity || (publicPage && !publicRecovery))
          ? "/workspace"
          : auth.session?.scope === "identity" && publicPage && !publicRecovery
            ? "/onboarding"
            : null
  useEffect(() => {
    if (destination) router.replace(destination)
  }, [destination, router])
  if (auth.status === "unavailable")
    return (
      <main className="mx-auto flex min-h-dvh max-w-md flex-col justify-center gap-4 p-6 text-foreground">
        <h1 className="text-xl font-semibold">{t("unavailable")}</h1>
        <p role="alert">
          {t(auth.logoutError ? "logoutFailed" : "error_unavailable")}
        </p>
        <Button
          onClick={() =>
            void (auth.logoutError ? auth.logout() : auth.refresh())
          }
        >
          {t(auth.logoutError ? "retryLogout" : "retry")}
        </Button>
      </main>
    )
  if (auth.status === "loading" || destination)
    return (
      <main
        className="flex min-h-dvh items-center justify-center bg-background text-foreground"
        role="status"
        aria-busy="true"
      >
        {t("loading")}
      </main>
    )
  // Both of these hold the workspace still without taking it away. While a
  // check is in flight that is only a pause; when one could not be made at all,
  // it is a pause with a way out of it — and in neither case is anything under
  // here reachable, so nothing can be read or written until authority is proved
  // again. A definitive refusal never reaches this point: it clears the session
  // above, and the subtree goes with it.
  const held = auth.status === "revalidating" || auth.status === "unverified"
  return (
    <>
      {auth.status === "revalidating" && (
        <main role="status" aria-busy="true">
          {t("loading")}
        </main>
      )}
      {auth.status === "unverified" && (
        <main className="mx-auto flex max-w-md flex-col justify-center gap-4 p-6 text-foreground">
          <p role="alert">{t("error_unavailable")}</p>
          {/* Re-proves the session that is still held rather than starting
              from nothing, so what is behind this survives the attempt —
              including an attempt that fails again. */}
          <Button onClick={() => void auth.retry()}>{t("retry")}</Button>
        </main>
      )}
      <NativeSuspendedSurface
        key={auth.scopeKey ?? "signed-out"}
        hidden={held}
        className="min-h-dvh bg-background text-foreground"
      >
        {children}
      </NativeSuspendedSurface>
    </>
  )
}
