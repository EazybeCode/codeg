"use client"

import { useEffect, useRef } from "react"
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
import { TeamSignedInShell } from "./team-signed-in-shell"
import { useTeamOrganizationSettings } from "./use-team-organization-settings"

export function TeamOrganizationSettingsPage(): React.JSX.Element {
  const settings = useTeamOrganizationSettings()

  return (
    <TeamSignedInShell>
      <OrganizationSettingsForm settings={settings} />
    </TeamSignedInShell>
  )
}

function OrganizationSettingsForm({
  settings,
}: {
  settings: ReturnType<typeof useTeamOrganizationSettings>
}) {
  const t = useTranslations("OrganizationSettings")
  const errorFocusRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (settings.error === "save") errorFocusRef.current?.focus()
  }, [settings.error])

  if (settings.loading || (!settings.organization && !settings.error)) {
    return (
      <div className="flex min-h-40 items-center justify-center text-muted-foreground">
        <Loader2 className="size-5 animate-spin" aria-label={t("loading")} />
      </div>
    )
  }

  if (!settings.organization) {
    return (
      <section
        aria-labelledby="team-organization-settings-title"
        className="space-y-3"
      >
        <h1
          id="team-organization-settings-title"
          className="text-xl font-semibold"
        >
          {t("teamTitle")}
        </h1>
        <SettingsError>{t("loadError")}</SettingsError>
        <Button type="button" size="sm" onClick={settings.retry}>
          {t("retry")}
        </Button>
      </section>
    )
  }

  const organization = settings.organization
  const busy = settings.saving || settings.logoReading
  const fallback = settings.name.trim().slice(0, 1).toUpperCase() || (
    <Building2 className="size-5" aria-hidden="true" />
  )

  return (
    <form
      aria-labelledby="team-organization-settings-title"
      aria-busy={busy}
      className="min-w-0 space-y-4"
      onSubmit={(event) => {
        event.preventDefault()
        void settings.save()
      }}
    >
      <h1
        id="team-organization-settings-title"
        className="text-xl font-semibold"
      >
        {t("teamTitle")}
      </h1>
      <SettingsSection
        icon={Building2}
        title={t("title")}
        description={t("description")}
      >
        {!organization.canEdit ? (
          <p className="rounded-lg border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
            {t("memberReadOnly")}
          </p>
        ) : null}

        {settings.error ? (
          <div ref={errorFocusRef} tabIndex={-1} className="outline-none">
            <SettingsError>
              {t(
                settings.error === "save"
                  ? "saveError"
                  : settings.error === "file"
                    ? "fileError"
                    : "loadError"
              )}
            </SettingsError>
          </div>
        ) : null}
        {settings.saved ? (
          <p
            role="status"
            aria-live="polite"
            className="rounded-lg border border-primary/20 bg-primary/5 px-3 py-2 text-xs text-primary"
          >
            {t("saved")}
          </p>
        ) : null}

        <SettingCard>
          <SettingRow
            title={t("displayName")}
            description={t("displayNameDescription")}
            htmlFor="team-organization-display-name"
          >
            <Input
              id="team-organization-display-name"
              value={settings.name}
              readOnly={!organization.canEdit}
              disabled={settings.saving}
              onChange={(event) => settings.setName(event.target.value)}
            />
          </SettingRow>
          <SettingRow
            title={t("githubLogin")}
            description={t("githubLoginDescription")}
            htmlFor="team-organization-github-login"
          >
            <Input
              id="team-organization-github-login"
              value={organization.login}
              readOnly
            />
          </SettingRow>
          <SettingRow
            icon={ImagePlus}
            title={t("logo")}
            description={t("logoDescription")}
            htmlFor="team-organization-logo-upload"
          >
            <div className="flex min-w-0 flex-wrap items-center gap-3">
              <span className="flex size-16 shrink-0 items-center justify-center overflow-hidden rounded-xl border bg-background text-lg font-semibold">
                {settings.preview ? (
                  // Only validated local data URLs reach this element.
                  // eslint-disable-next-line @next/next/no-img-element
                  <img
                    src={settings.preview}
                    alt={t("previewAlt")}
                    className="size-full object-cover"
                  />
                ) : (
                  fallback
                )}
              </span>
              {organization.canEdit ? (
                <div className="flex min-w-0 flex-wrap gap-2">
                  <label
                    aria-disabled={settings.saving}
                    className="inline-flex h-8 cursor-pointer items-center rounded-full border bg-input/30 px-3 text-sm font-medium hover:bg-input/50 aria-disabled:cursor-not-allowed aria-disabled:opacity-50"
                  >
                    {t("upload")}
                    <input
                      id="team-organization-logo-upload"
                      className="sr-only"
                      type="file"
                      accept="image/png,image/jpeg,image/webp"
                      aria-label={t("uploadLabel")}
                      disabled={settings.saving}
                      onChange={(event) => {
                        const file = event.currentTarget.files?.[0]
                        event.currentTarget.value = ""
                        settings.uploadLogo(file)
                      }}
                    />
                  </label>
                  {settings.hasLogo ? (
                    <Button
                      type="button"
                      size="sm"
                      variant="outline"
                      disabled={settings.saving}
                      onClick={settings.removeLogo}
                    >
                      {t("remove")}
                    </Button>
                  ) : null}
                </div>
              ) : null}
            </div>
          </SettingRow>
        </SettingCard>

        {organization.canEdit ? (
          <SettingsSaveBar
            onSave={() => void settings.save()}
            saving={settings.saving}
            disabled={!settings.canSave}
            label={t("save")}
            savingLabel={t("saving")}
          />
        ) : null}
      </SettingsSection>
    </form>
  )
}
