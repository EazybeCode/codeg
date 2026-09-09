"use client"

import { useEffect } from "react"
import { Github } from "lucide-react"
import { useSearchParams } from "next/navigation"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"

const LOGIN_PATH = "/auth/github/login"

export function TeamLogin() {
  const t = useTranslations("TeamLogin")
  const searchParams = useSearchParams()
  const reason = searchParams.get("reason")
  const reasonKey =
    reason === "access_denied" ||
    reason === "invalid_login" ||
    reason === "unavailable"
      ? reason
      : null

  useEffect(() => {
    document.title = t("documentTitle")
  }, [t])

  return (
    <main className="flex min-h-screen items-center justify-center bg-background px-4 text-foreground">
      <div className="w-full max-w-sm space-y-6 text-center">
        <div className="space-y-2">
          <p className="text-sm font-medium text-primary">Neev</p>
          <h1 className="text-2xl font-bold tracking-tight">{t("title")}</h1>
          <p className="text-sm text-muted-foreground">{t("subtitle")}</p>
        </div>

        {reasonKey && (
          <div
            role="alert"
            className="rounded-xl border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive"
          >
            {t(reasonKey)}
          </div>
        )}

        <Button asChild size="lg" className="w-full">
          <a href={LOGIN_PATH}>
            <Github />
            {reasonKey ? t("tryAgain") : t("continueWithGithub")}
          </a>
        </Button>
        <p className="text-xs text-muted-foreground">{t("privacy")}</p>
      </div>
    </main>
  )
}
