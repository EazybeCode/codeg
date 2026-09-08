"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import Image from "next/image"
import { Building2, ImagePlus, Loader2 } from "lucide-react"
import { useTranslations } from "next-intl"

import { SettingCard, SettingRow } from "@/components/shared/setting-card"
import {
  SettingsError,
  SettingsSaveBar,
  SettingsSection,
} from "@/components/shared/settings-section"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { ScrollArea } from "@/components/ui/scroll-area"

type Organization = {
  id: number
  login: string
  name: string
  avatar_url: string | null
  can_edit: boolean
}

type LogoChange =
  | { action: "keep" }
  | { action: "remove" }
  | { action: "upload"; data: string }

async function readResponse(response: Response): Promise<Organization> {
  if (!response.ok)
    throw new Error(`organization request failed: ${response.status}`)
  return response.json() as Promise<Organization>
}

export function OrganizationSettings() {
  const t = useTranslations("OrganizationSettings")
  const [organization, setOrganization] = useState<Organization | null>(null)
  const [name, setName] = useState("")
  const [logo, setLogo] = useState<LogoChange>({ action: "keep" })
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [logoReading, setLogoReading] = useState(false)
  const [error, setError] = useState<"load" | "save" | "file" | null>(null)
  const [attempt, setAttempt] = useState(0)
  const logoReadVersion = useRef(0)

  useEffect(() => {
    const controller = new AbortController()
    setLoading(true)
    setError(null)
    void fetch("/auth/organization", {
      credentials: "same-origin",
      signal: controller.signal,
    })
      .then(readResponse)
      .then((value) => {
        if (controller.signal.aborted) return
        setOrganization(value)
        setName(value.name)
        setLogo({ action: "keep" })
      })
      .catch(() => {
        if (!controller.signal.aborted) setError("load")
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false)
      })
    return () => controller.abort()
  }, [attempt])

  const preview = useMemo(() => {
    if (logo.action === "remove") return null
    if (logo.action === "upload") return logo.data
    return organization?.avatar_url ?? null
  }, [logo, organization])
  const normalizedName = name.trim()
  const validName =
    normalizedName.length > 0 &&
    Array.from(normalizedName).length <= 80 &&
    !Array.from(normalizedName).some((character) => /\p{Cc}/u.test(character))
  const dirty =
    !!organization &&
    (normalizedName !== organization.name || logo.action !== "keep")

  const uploadLogo = useCallback((file: File | undefined) => {
    if (!file) return
    const version = ++logoReadVersion.current
    if (
      file.size > 1024 * 1024 ||
      !["image/png", "image/jpeg", "image/webp"].includes(file.type)
    ) {
      setLogoReading(false)
      setError("file")
      return
    }
    setLogoReading(true)
    const reader = new FileReader()
    reader.onload = () => {
      if (
        version === logoReadVersion.current &&
        typeof reader.result === "string"
      ) {
        setLogo({ action: "upload", data: reader.result })
        setLogoReading(false)
        setError(null)
      }
    }
    reader.onerror = () => {
      if (version === logoReadVersion.current) {
        setLogoReading(false)
        setError("file")
      }
    }
    reader.readAsDataURL(file)
  }, [])

  const save = useCallback(async () => {
    if (!organization || !validName || !dirty || saving || logoReading) return
    logoReadVersion.current += 1
    setSaving(true)
    setError(null)
    try {
      const updated = await fetch("/auth/organization", {
        method: "PATCH",
        credentials: "same-origin",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ name: normalizedName, logo }),
      }).then(readResponse)
      setOrganization(updated)
      setName(updated.name)
      setLogo({ action: "keep" })
      window.dispatchEvent(new Event("workos:identity-changed"))
    } catch {
      setError("save")
    } finally {
      setSaving(false)
    }
  }, [
    dirty,
    logo,
    logoReading,
    normalizedName,
    organization,
    saving,
    validName,
  ])

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center text-muted-foreground">
        <Loader2 className="size-5 animate-spin" aria-label={t("loading")} />
      </div>
    )
  }

  if (!organization) {
    return (
      <div className="p-4 sm:p-6">
        <SettingsError>{t("loadError")}</SettingsError>
        <Button
          className="mt-3"
          size="sm"
          onClick={() => setAttempt((value) => value + 1)}
        >
          {t("retry")}
        </Button>
      </div>
    )
  }

  return (
    <ScrollArea className="h-full">
      <div className="mx-auto max-w-3xl space-y-4 p-4 sm:p-6">
        <SettingsSection
          icon={Building2}
          title={t("title")}
          description={t("description")}
        >
          {!organization.can_edit ? (
            <p className="rounded-lg border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
              {t("memberReadOnly")}
            </p>
          ) : null}
          {error ? (
            <SettingsError>
              {t(
                error === "save"
                  ? "saveError"
                  : error === "file"
                    ? "fileError"
                    : "loadError"
              )}
            </SettingsError>
          ) : null}
          <SettingCard>
            <SettingRow
              title={t("displayName")}
              description={t("displayNameDescription")}
              htmlFor="organization-display-name"
            >
              <Input
                id="organization-display-name"
                value={name}
                readOnly={!organization.can_edit}
                disabled={saving}
                onChange={(event) => setName(event.target.value)}
              />
            </SettingRow>
            <SettingRow
              title={t("githubLogin")}
              description={t("githubLoginDescription")}
              htmlFor="organization-github-login"
            >
              <Input
                id="organization-github-login"
                value={organization.login}
                readOnly
              />
            </SettingRow>
            <SettingRow
              icon={ImagePlus}
              title={t("logo")}
              description={t("logoDescription")}
              htmlFor="organization-logo-upload"
            >
              <div className="flex flex-wrap items-center gap-3">
                <span className="flex size-16 shrink-0 items-center justify-center overflow-hidden rounded-xl border bg-background text-lg font-semibold">
                  {preview ? (
                    <Image
                      src={preview}
                      alt={t("previewAlt")}
                      width={64}
                      height={64}
                      unoptimized
                      className="size-full object-cover"
                    />
                  ) : (
                    normalizedName.slice(0, 1).toUpperCase() || (
                      <Building2 className="size-5" />
                    )
                  )}
                </span>
                {organization.can_edit ? (
                  <div className="flex flex-wrap gap-2">
                    <label
                      aria-disabled={saving}
                      className="inline-flex h-8 cursor-pointer items-center rounded-full border bg-input/30 px-3 text-sm font-medium hover:bg-input/50 aria-disabled:cursor-not-allowed aria-disabled:opacity-50"
                    >
                      {t("upload")}
                      <input
                        id="organization-logo-upload"
                        className="sr-only"
                        type="file"
                        accept="image/png,image/jpeg,image/webp"
                        aria-label={t("uploadLabel")}
                        disabled={saving}
                        onChange={(event) => {
                          const file = event.currentTarget.files?.[0]
                          event.currentTarget.value = ""
                          uploadLogo(file)
                        }}
                      />
                    </label>
                    {preview ? (
                      <Button
                        type="button"
                        size="sm"
                        variant="outline"
                        disabled={saving}
                        onClick={() => {
                          logoReadVersion.current += 1
                          setLogoReading(false)
                          setLogo({ action: "remove" })
                        }}
                      >
                        {t("remove")}
                      </Button>
                    ) : null}
                  </div>
                ) : null}
              </div>
            </SettingRow>
          </SettingCard>
          {organization.can_edit ? (
            <SettingsSaveBar
              onSave={() => void save()}
              saving={saving}
              disabled={!dirty || !validName || logoReading}
              label={t("save")}
              savingLabel={t("saving")}
            />
          ) : null}
        </SettingsSection>
      </div>
    </ScrollArea>
  )
}
