import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"
import { TeamModeBoundary } from "@/components/team/team-mode-boundary"
import { getTransport } from "@/lib/transport"
import LoginPage from "@/app/login/page"
import { NativeAuthProvider } from "./native-auth-provider"
import { mockNativeSession } from "./test-helpers"
vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))
afterEach(() => vi.unstubAllGlobals())
it("selects the actual native login entry from the server DTO without mounting operator or team effects", async () => {
  const mock = mockNativeSession(null)
  const mounted: string[] = []
  function Denied({ name }: { name: string }) {
    mounted.push(name)
    return <p>{name}</p>
  }
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <TeamModeBoundary
        operator={<Denied name="operator" />}
        team={<Denied name="legacy team" />}
        native={
          <NativeAuthProvider>
            <LoginPage />
          </NativeAuthProvider>
        }
      />
    </NextIntlClientProvider>
  )
  expect(
    await screen.findByRole("button", { name: "Continue with Google" })
  ).toBeVisible()
  // The release hides GitHub on sign-in; its disabled placeholder is
  // reserved for linking methods in Settings.
  expect(screen.queryByRole("button", { name: /GitHub/ })).toBeNull()
  expect(screen.getByLabelText("Email address")).toBeEnabled()
  expect(mounted).toEqual([])
  expect(mock.unknown).toEqual([])
  expect(() => getTransport()).toThrow(/not available/)
})
