import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeJoinCode } from "./native-join-code"
import { json, mockNativeSession, NativeTestRoot, orgSession } from "./test-helpers"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

afterEach(() => {
  vi.unstubAllGlobals()
  vi.clearAllMocks()
})

it("lets an admin generate, rotate, and disable the organization join code", async () => {
  let code: string | null = null
  let disabled = false
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/organization/join-code": () => json({ code, disabled }),
    "POST /api/neev/organization/join-code/rotate": () => {
      code = "ABCD2345EFGH"
      return json({ code, disabled: false })
    },
    "POST /api/neev/organization/join-code/enabled": (init) => {
      disabled = JSON.parse(init.body as string).enabled === false
      return new Response(null, { status: 204 })
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeJoinCode />
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Generate code" }))
  expect(await screen.findByText("ABCD2345EFGH")).toBeVisible()
  // Disable joining without rotating.
  await user.click(screen.getByRole("button", { name: "Disable joining" }))
  expect(await screen.findByText("Joining is disabled")).toBeVisible()
  expect(
    screen.getByRole("button", { name: "Enable joining" })
  ).toBeInTheDocument()
  expect(mock.unknown).toEqual([])
})

it("previews an organization from a code then joins it as a member", async () => {
  const bodies: unknown[] = []
  const mock = mockNativeSession(orgSession, {
    "POST /api/neev/join/preview": (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json({ organization: { id: 7, name: "Acme Research" } })
    },
    "POST /api/neev/join": (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json({ organizationId: 7 })
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeJoinCode />
    </NativeTestRoot>
  )
  await user.type(
    await screen.findByPlaceholderText("Join code"),
    "acme-2345-wxyz"
  )
  await user.click(screen.getByRole("button", { name: "Preview" }))
  expect(await screen.findByText("Join Acme Research?")).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Join" }))
  expect(await screen.findByText("Joined Acme Research.")).toBeVisible()
  // Code sent in the body (never a URL), plus the confirmed org id on accept.
  expect(bodies[0]).toEqual({ code: "acme-2345-wxyz" })
  expect(bodies[1]).toEqual({ code: "acme-2345-wxyz", confirmedOrgId: 7 })
  expect(mock.unknown).toEqual([])
})

it("shows a truthful message for an invalid or disabled code", async () => {
  mockNativeSession(orgSession, {
    "POST /api/neev/join/preview": () => json({ code: "forbidden" }, 403),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeJoinCode />
    </NativeTestRoot>
  )
  await user.type(await screen.findByPlaceholderText("Join code"), "ZZZZZZZZ")
  await user.click(screen.getByRole("button", { name: "Preview" }))
  expect(
    await screen.findByText("That code is invalid, disabled, or unavailable.")
  ).toBeVisible()
})
