import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"

const navigation = vi.hoisted(() => ({ reason: "" }))
vi.mock("next/navigation", () => ({
  useSearchParams: () => new URLSearchParams(navigation.reason),
}))

import { TeamLogin } from "./team-login"

function showLogin() {
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <TeamLogin />
    </NextIntlClientProvider>
  )
}

beforeEach(() => {
  navigation.reason = ""
})

describe("TeamLogin", () => {
  it("offers only the fixed GitHub login destination", () => {
    showLogin()
    expect(
      screen.getByRole("link", { name: /continue with github/i })
    ).toHaveAttribute("href", "/auth/github/login")
    expect(screen.queryByPlaceholderText(/access token/i)).toBeNull()
  })

  it.each([
    ["reason=access_denied", /permission was not granted/i],
    ["reason=invalid_login", /could not be completed/i],
    ["reason=unavailable", /temporarily unavailable/i],
  ])("maps the fixed callback reason %s", (query, expected) => {
    navigation.reason = query
    showLogin()
    expect(screen.getByRole("alert")).toHaveTextContent(expected)
    expect(screen.getByRole("link", { name: /try again/i })).toHaveAttribute(
      "href",
      "/auth/github/login"
    )
  })

  it("ignores unknown callback text instead of reflecting it", () => {
    navigation.reason = "reason=%3Cscript%3Eattacker-text%3C%2Fscript%3E"
    showLogin()
    expect(screen.queryByRole("alert")).toBeNull()
    expect(screen.queryByText(/attacker-text/)).toBeNull()
  })
})
