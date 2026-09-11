import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeOnboarding } from "./native-onboarding"
import {
  identitySession,
  json,
  mockNativeSession,
  NativeTestRoot,
  orgSession,
} from "./test-helpers"
const router = vi.hoisted(() => ({ replace: vi.fn(), push: vi.fn() }))
vi.mock("next/navigation", () => ({ useRouter: () => router }))
afterEach(() => {
  vi.unstubAllGlobals()
  vi.clearAllMocks()
})
it("creates and explicitly selects a native company without GitHub", async () => {
  let selected = false
  const bodies: unknown[] = []
  const mock = mockNativeSession(identitySession, {
    "GET /api/neev/session": () =>
      json(selected ? orgSession : identitySession),
    "GET /api/neev/organizations?limit=100&offset=0": () =>
      json({ organizations: [], nextOffset: null }),
    "POST /api/neev/organizations": (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json(
        {
          organization: orgSession.organization,
          role: "admin",
          brandingRevision: "1",
        },
        201
      )
    },
    "POST /api/neev/organizations/select": (init) => {
      bodies.push(JSON.parse(init.body as string))
      selected = true
      return json({ organization: orgSession.organization, role: "admin" })
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeOnboarding />
    </NativeTestRoot>
  )
  await user.type(
    await screen.findByLabelText("Organization name"),
    "Neev demo"
  )
  await user.click(screen.getByRole("button", { name: "Create organization" }))
  await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/workspace"))
  expect(bodies).toEqual([
    { name: "Neev demo", requestId: expect.stringMatching(/^[a-f0-9-]{36}$/) },
    { organizationId: 3 },
  ])
  expect(mock.unknown).toEqual([])
})
it("selects exact same-name membership by ID and exposes another page", async () => {
  let chosen: unknown
  const mock = mockNativeSession(identitySession, {
    "GET /api/neev/organizations?limit=100&offset=0": () =>
      json({
        organizations: [{ id: 3, name: "Same company", role: "admin" }],
        nextOffset: 100,
      }),
    "GET /api/neev/organizations?limit=100&offset=100": () =>
      json({
        organizations: [{ id: 4, name: "Same company", role: "member" }],
        nextOffset: null,
      }),
    "POST /api/neev/organizations/select": (init) => {
      chosen = JSON.parse(init.body as string)
      return json({ code: "forbidden" }, 403)
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeOnboarding />
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Load more" }))
  await user.click(
    await screen.findByRole("button", { name: "Same company Member" })
  )
  expect(await screen.findByRole("alert")).toHaveTextContent(/permission/i)
  expect(chosen).toEqual({ organizationId: 4 })
  expect(mock.unknown).toEqual([])
})
