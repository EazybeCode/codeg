"use client"

import { useEffect } from "react"
import { useRouter } from "next/navigation"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { NativeClientError } from "@/lib/neev-client"
import { useNativeAuth } from "./native-auth-provider"

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
  return (
    <div
      key={auth.scopeKey ?? "signed-out"}
      className="min-h-dvh bg-background text-foreground"
    >
      {children}
    </div>
  )
}
