import { render, screen } from "@testing-library/react"
import { useTheme } from "next-themes"
import { useTranslations } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import enMessages from "@/i18n/messages/en.json"
import { LANGUAGE_SETTINGS_STORAGE_KEY } from "@/lib/i18n"

const apiCall = vi.hoisted(() => vi.fn())
vi.mock("@/lib/api", () => ({ getSystemLanguageSettings: apiCall }))

import { TeamLocalProviders } from "./team-local-providers"

function Consumer() {
  const t = useTranslations("TeamLogin")
  const { theme } = useTheme()
  return (
    <p>
      {t("title")}|{theme}
    </p>
  )
}

beforeEach(() => {
  localStorage.clear()
  apiCall.mockReset()
})

describe("TeamLocalProviders", () => {
  it("uses browser-local locale and theme without global settings APIs", async () => {
    localStorage.setItem(
      LANGUAGE_SETTINGS_STORAGE_KEY,
      JSON.stringify({ mode: "manual", language: "de" })
    )
    localStorage.setItem("theme", "light")

    render(
      <TeamLocalProviders initialLocale="en" initialMessages={enMessages}>
        <Consumer />
      </TeamLocalProviders>
    )

    expect(await screen.findByText(/Bei Neev anmelden\|light/)).toBeVisible()
    expect(apiCall).not.toHaveBeenCalled()
  })
})
