import { render, screen, waitFor } from "@testing-library/react"
import { StrictMode } from "react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeLogin, NativeSignInMethods } from "./native-login"
import {
  json,
  mockNativeSession,
  nativeCapabilities,
  NativeTestRoot,
  orgSession,
} from "./test-helpers"
const router = vi.hoisted(() => ({ replace: vi.fn(), push: vi.fn() }))
vi.mock("next/navigation", () => ({ useRouter: () => router }))
afterEach(() => {
  vi.unstubAllGlobals()
  router.replace.mockClear()
  history.replaceState(null, "", "/")
})
it("announces a signed-in callback conflict without redirecting away or retaining URL details", async () => {
  history.replaceState(
    null,
    "",
    "/login?auth_error=account_conflict&code=synthetic-secret#ignored"
  )
  const mock = mockNativeSession(orgSession)
  render(
    <StrictMode>
      <NativeTestRoot>
        <NativeLogin />
      </NativeTestRoot>
    </StrictMode>
  )
  expect(await screen.findByRole("alert")).toHaveTextContent(
    /original sign-in method/i
  )
  expect(screen.getByRole("link", { name: "Settings" })).toHaveAttribute(
    "href",
    "/settings"
  )
  expect(screen.getByRole("button", { name: "Sign out" })).toBeEnabled()
  expect(router.replace).not.toHaveBeenCalled()
  expect(location.pathname + location.search + location.hash).toBe("/login")
  expect(document.body).not.toHaveTextContent("synthetic-secret")
  expect(mock.unknown).toEqual([])
  const user = userEvent.setup()
  await user.tab()
  expect(screen.getByRole("link", { name: "Settings" })).toHaveFocus()
  await user.tab()
  expect(screen.getByRole("button", { name: "Sign out" })).toHaveFocus()
})
it("announces generic callback recovery for an anonymous visitor", async () => {
  history.replaceState(null, "", "/login?auth_error=sign_in_failed")
  mockNativeSession(null)
  render(
    <NativeTestRoot>
      <NativeLogin />
    </NativeTestRoot>
  )
  expect(await screen.findByRole("alert")).toHaveTextContent(
    /could not complete/i
  )
  expect(
    screen.getByRole("button", { name: "Continue with Google" })
  ).toBeEnabled()
  expect(location.search).toBe("")
})
it("removes unknown callback codes without reflecting them or bypassing signed-in navigation", async () => {
  history.replaceState(null, "", "/login?auth_error=untrusted-provider-secret")
  mockNativeSession(orgSession)
  render(
    <NativeTestRoot>
      <NativeLogin />
    </NativeTestRoot>
  )
  await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/workspace"))
  expect(document.body).not.toHaveTextContent("untrusted-provider-secret")
  expect(screen.queryByRole("alert")).not.toBeInTheDocument()
  expect(location.search).toBe("")
})
it("announces a uniform email acceptance without claiming delivery or storing the address", async () => {
  const writes = vi.spyOn(Storage.prototype, "setItem")
  let body: unknown
  const mock = mockNativeSession(null, {
    "GET /api/neev/preauth": () => json({ csrf: "preauth-csrf" }),
    "POST /api/neev/email/login": (init) => {
      body = JSON.parse(init.body as string)
      return json({ status: "accepted", message: "uniform" }, 202)
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeLogin />
    </NativeTestRoot>
  )
  expect(
    await screen.findByRole("button", { name: "Continue with Google" })
  ).toBeEnabled()
  expect(
    screen.getByRole("button", { name: /GitHub.*unavailable/i })
  ).toBeDisabled()
  await user.type(screen.getByLabelText("Email address"), "person@example.test")
  await user.click(screen.getByRole("button", { name: "Request sign-in link" }))
  expect(
    await screen.findByText(
      "If this address can receive a sign-in link, it will arrive shortly. Open it in this browser."
    )
  ).toHaveAttribute("role", "status")
  expect(body).toEqual({ email: "person@example.test" })
  expect(writes).not.toHaveBeenCalled()
  expect(mock.unknown).toEqual([])
  writes.mockRestore()
})
it("refreshes browser binding but sends session CSRF for linking and explains conflicts", async () => {
  let csrf: unknown
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/preauth": () => json({ csrf: "different-preauth-csrf" }),
    "POST /api/neev/auth/google/link": (init) => {
      csrf = (init.headers as Record<string, string>)["x-neev-csrf"]
      return json({ code: "conflict" }, 409)
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSignInMethods linking />
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Link Google" }))
  expect(await screen.findByRole("alert")).toHaveTextContent(/linked/i)
  expect(csrf).toBe("synthetic-csrf")
  expect(mock.unknown).toEqual([])
})

it("offers the sign-in methods the public server advertises, to someone with no session", async () => {
  // The capabilities are PUBLIC and the session is private, and a signed-out
  // visitor has exactly one of them. Reading the pair as all-or-nothing meant a
  // 401 on the session threw away the answer that says how to sign in, and
  // every method on the page rendered unavailable — the one page where being
  // signed out is the normal case.
  const mock = mockNativeSession(undefined, {
    "GET /api/workos/capabilities": () =>
      json({
        ...nativeCapabilities,
        providers: { google: true, email: true, github: false },
      }),
    // Nobody is signed in. That is an answer, not a failure.
    "GET /api/neev/session": () => json({ code: "unauthorized" }, 401),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSignInMethods />
    </NativeTestRoot>
  )
  const google = await screen.findByRole("button", {
    name: /Continue with Google/,
  })
  await waitFor(() => expect(google).toBeEnabled())
  // The label is the symptom: an unavailable method says so in its own name.
  expect(google).toHaveAccessibleName("Continue with Google")
  const email = screen.getByLabelText("Email address")
  expect(email).toBeEnabled()
  // The submit waits for an address, not for permission.
  const send = screen.getByRole("button", { name: /Request sign-in link/ })
  expect(send).toBeDisabled()
  await user.type(email, "sagar@eazybe.com")
  expect(send).toBeEnabled()
  expect(send).toHaveAccessibleName("Request sign-in link")
  // GitHub genuinely is not configured on this server, and still says so.
  const github = screen.getByRole("button", { name: /GitHub/ })
  expect(github).toBeDisabled()
  expect(github).toHaveAccessibleName("GitHub (optional) · unavailable")
  expect(mock.unknown).toEqual([])
})
