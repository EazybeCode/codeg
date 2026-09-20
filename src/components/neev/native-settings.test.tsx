import { act, render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeSettings } from "./native-settings"
import {
  json,
  mockNativeSession,
  nativeCapabilities,
  NativeTestRoot,
  orgSession,
} from "./test-helpers"
vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))
afterEach(() => vi.unstubAllGlobals())
function fixtures(owner = 1) {
  return {
    "GET /api/neev/organization": () =>
      json({
        id: 3,
        name: "Neev demo",
        avatarUrl: null,
        ownerUserId: owner,
        brandingRevision: "7",
      }),
    "GET /api/neev/organization/members?limit=100&offset=0": () =>
      json({
        members: [
          { id: 8, user: orgSession.user, role: "admin" },
          {
            id: 9,
            user: { id: 2, displayName: "Maya", avatarUrl: null },
            role: "member",
          },
        ],
      }),
    "GET /api/neev/auth/methods": () => json({ methods: ["email"] }),
  }
}
it("protects the owner, exposes only own sign-in methods, and keeps back navigation", async () => {
  const mock = mockNativeSession(orgSession, fixtures())
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  expect(await screen.findByText("Maya")).toBeVisible()
  expect(
    screen.getByRole("link", { name: "Back to workspace" })
  ).toHaveAttribute("href", "/workspace")
  expect(screen.queryByRole("button", { name: "Remove Sagar" })).toBeNull()
  expect(screen.getByRole("button", { name: "Remove Maya" })).toBeEnabled()
  expect(screen.getByText("Owner")).toBeVisible()
  expect(mock.unknown).toEqual([])
})

it("disables invitations when the native email provider is unavailable", async () => {
  const mock = mockNativeSession(orgSession, {
    ...fixtures(),
    "GET /api/workos/capabilities": () =>
      json({
        ...nativeCapabilities,
        providers: { ...nativeCapabilities.providers, email: false },
      }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  await user.type(
    await screen.findByLabelText("Invite by email"),
    "invitee@example.test"
  )
  expect(screen.getByRole("button", { name: "Invite member" })).toBeDisabled()
  expect(
    screen.getByText(
      "Email invitations are unavailable until an email provider is configured."
    )
  ).toBeVisible()
  expect(mock.unknown).toEqual([])
  expect(
    mock.fetcher.mock.calls.some(
      ([path, init]) =>
        path === "/api/neev/organization/invitations" &&
        (init as RequestInit | undefined)?.method === "POST"
    )
  ).toBe(false)
})

it("hides owner-only role grants from a nonowner admin and protects administrator peers", async () => {
  const mock = mockNativeSession(orgSession, {
    ...fixtures(2),
    "GET /api/neev/organization/members?limit=100&offset=0": () =>
      json({
        members: [
          {
            id: 9,
            user: { id: 2, displayName: "Owner Maya", avatarUrl: null },
            role: "admin",
          },
          {
            id: 10,
            user: { id: 5, displayName: "Admin Isha", avatarUrl: null },
            role: "admin",
          },
        ],
      }),
  })
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  await screen.findByText("Owner Maya")
  expect(screen.queryByRole("combobox")).toBeNull()
  expect(screen.queryByRole("button", { name: "Remove Owner Maya" })).toBeNull()
  expect(screen.queryByRole("button", { name: "Remove Admin Isha" })).toBeNull()
  expect(screen.getByRole("button", { name: "Invite member" })).toBeDisabled()
  expect(mock.unknown).toEqual([])
})

it("loads members past the first100 using the actual members-only page DTO", async () => {
  const firstPage = Array.from({ length: 100 }, (_, index) => ({
    id: index + 1,
    user: {
      id: index + 1,
      displayName: `Person ${index + 1}`,
      avatarUrl: null,
    },
    role: "member",
  }))
  const mock = mockNativeSession(
    { ...orgSession, role: "member" },
    {
      ...fixtures(),
      "GET /api/neev/organization/members?limit=100&offset=0": () =>
        json({ members: firstPage }),
      "GET /api/neev/organization/members?limit=100&offset=100": () =>
        json({
          members: [
            {
              id: 101,
              user: { id: 101, displayName: "Last person", avatarUrl: null },
              role: "member",
            },
          ],
        }),
    }
  )
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Load more" }))
  expect(await screen.findByText("Last person")).toBeVisible()
  expect(screen.queryByRole("button", { name: "Load more" })).toBeNull()
  expect(screen.getByText("Person 1")).toBeVisible()
  expect(mock.unknown).toEqual([])
})

it("loads the shifted final member after a first-page removal without discarding a branding draft", async () => {
  let active = Array.from({ length: 101 }, (_, index) => ({
    id: index + 1,
    user: {
      id: index + 1,
      displayName: `Person ${index + 1}`,
      avatarUrl: null,
    },
    role: index === 0 ? "admin" : "member",
  }))
  const offsets: number[] = []
  const page = (offset: number) => () => {
    offsets.push(offset)
    return json({ members: active.slice(offset, offset + 100) })
  }
  const mock = mockNativeSession(orgSession, {
    ...fixtures(),
    "GET /api/neev/organization/members?limit=100&offset=0": page(0),
    "GET /api/neev/organization/members?limit=100&offset=99": page(99),
    "GET /api/neev/organization/members?limit=100&offset=100": page(100),
    "DELETE /api/neev/organization/members/2": () => {
      active = active.filter((member) => member.id !== 2)
      return new Response(null, { status: 204 })
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  await screen.findByText("Person 100")
  expect(screen.queryByText("Person 101")).toBeNull()
  const name = screen.getByLabelText("Organization name")
  await user.clear(name)
  await user.type(name, "Unpublished draft")
  await user.click(screen.getByRole("button", { name: "Remove Person 2" }))
  await user.click(
    within(await screen.findByRole("dialog")).getByRole("button", {
      name: "Confirm removal",
    })
  )
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull())
  expect(screen.queryByText("Person 2")).toBeNull()
  await user.click(screen.getByRole("button", { name: "Load more" }))
  expect(await screen.findByText("Person 101")).toBeVisible()
  expect(offsets).toEqual([0, 99])
  expect(screen.queryByRole("button", { name: "Load more" })).toBeNull()
  expect(name).toHaveValue("Unpublished draft")
  expect(mock.unknown).toEqual([])
})

it("requires explicit removal confirmation and surfaces a server permission denial", async () => {
  let attempts = 0
  const mock = mockNativeSession(orgSession, {
    ...fixtures(),
    "DELETE /api/neev/organization/members/9": () => {
      attempts += 1
      return json({ code: "forbidden" }, 403)
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  const trigger = await screen.findByRole("button", { name: "Remove Maya" })
  await user.click(trigger)
  const dialog = await screen.findByRole("dialog")
  expect(attempts).toBe(0)
  await user.click(
    within(dialog).getByRole("button", { name: "Confirm removal" })
  )
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    /permission/
  )
  expect(screen.getByText("Maya")).toBeVisible()
  expect(attempts).toBe(1)
  await user.keyboard("{Escape}")
  expect(trigger).toHaveFocus()
  expect(mock.unknown).toEqual([])
})
it("hides admin mutations from members and still shows organization and personal identity", async () => {
  const mock = mockNativeSession({ ...orgSession, role: "member" }, fixtures())
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  await screen.findByText("Maya")
  expect(screen.queryByRole("button", { name: "Save organization" })).toBeNull()
  expect(screen.queryByRole("button", { name: "Invite member" })).toBeNull()
  expect(screen.queryByRole("button", { name: /Remove / })).toBeNull()
  expect(mock.unknown).toEqual([])
})
it("uses brandingRevision and surfaces a stale edit without claiming it saved", async () => {
  let body: unknown
  mockNativeSession(orgSession, {
    ...fixtures(),
    "PATCH /api/neev/organization": (init) => {
      body = JSON.parse(init.body as string)
      return json({ code: "conflict" }, 409)
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  const name = await screen.findByLabelText("Organization name")
  await user.clear(name)
  await user.type(name, "Changed company")
  await user.click(screen.getByRole("button", { name: "Save organization" }))
  await waitFor(() =>
    expect(screen.getByRole("alert")).toHaveTextContent(/changed|linked/i)
  )
  expect(body).toEqual({
    expectedBrandingRevision: "7",
    name: "Changed company",
    logo: { action: "keep" },
  })
})

it("keeps the logo draft when a native file picker returns window focus", async () => {
  let body: { logo?: { action: string; data?: string } } | undefined
  const mock = mockNativeSession(orgSession, {
    ...fixtures(),
    "PATCH /api/neev/organization": (init) => {
      body = JSON.parse(init.body as string)
      return json({ code: "unavailable" }, 503)
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  const input = await screen.findByLabelText(/^Organization logo/)
  const bytes = Uint8Array.from(
    atob(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j/YQAAAAASUVORK5CYII="
    ),
    (char) => char.charCodeAt(0)
  )
  await user.upload(input, new File([bytes], "logo.png", { type: "image/png" }))
  expect(
    await screen.findByRole("img", { name: "Organization logo" })
  ).toBeVisible()
  await act(async () => window.dispatchEvent(new Event("focus")))
  expect(screen.getByRole("img", { name: "Organization logo" })).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Save organization" }))
  await screen.findByRole("alert")
  expect(body?.logo?.action).toBe("upload")
  expect(body?.logo?.data).toMatch(/^data:image\/png;base64,/)
  expect(mock.unknown).toEqual([])
})

it("shows the caller's real provider connections instead of a static notice", async () => {
  let checks = 0
  const mock = mockNativeSession(orgSession, {
    ...fixtures(),
    "GET /api/neev/agent/connections": () => {
      checks += 1
      return json({
        runtimeEnabled: true,
        connections: [
          {
            provider: "claude",
            state: checks === 1 ? "not_signed_in" : "connected",
            accountLabel: checks === 1 ? null : "sagar@example.test",
            checkedAt: checks === 1 ? null : 1_700_000_000,
            errorCode: null,
          },
          {
            provider: "codex",
            state: "reconnect_required",
            accountLabel: "sagar@example.test",
            checkedAt: 1_700_000_000,
            errorCode: "unauthorized",
          },
        ],
      })
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  // Per-user, per-provider state straight from the server — not a fixed
  // "agents are unavailable" paragraph.
  expect(await screen.findByText("Not signed in")).toBeVisible()
  expect(screen.getByText("Reconnect needed")).toBeVisible()
  expect(screen.getByText("codex")).toBeVisible()
  // Real per-provider actions, matched to the state the server reports.
  expect(screen.getByRole("button", { name: "Connect claude" })).toBeEnabled()
  expect(screen.getByRole("button", { name: "Reconnect codex" })).toBeEnabled()
  // An existing connection can be checked; one that was never signed in cannot.
  expect(
    screen.getByRole("button", { name: "Test the codex connection" })
  ).toBeEnabled()
  expect(
    screen.queryByRole("button", { name: "Test the claude connection" })
  ).toBeNull()
  // Refresh genuinely re-reads the server's view.
  await user.click(screen.getByRole("button", { name: "Refresh status" }))
  expect(await screen.findByText("Connected")).toBeVisible()
  expect(checks).toBe(2)
  expect(mock.unknown).toEqual([])
})

it("never reports a usable connection over a runtime that is not bound", async () => {
  mockNativeSession(orgSession, {
    ...fixtures(),
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: false,
        connections: [
          {
            provider: "claude",
            state: "disabled",
            accountLabel: null,
            checkedAt: null,
            errorCode: null,
          },
        ],
      }),
  })
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  expect(await screen.findByText("Agents are turned off.")).toBeVisible()
  expect(screen.queryByText("Connected")).toBeNull()
})
