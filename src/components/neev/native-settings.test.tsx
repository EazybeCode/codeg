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
/**
 * Settings is sectioned, so a test has to say which section it is looking at.
 *
 * Inactive panels are hidden rather than unmounted, which is deliberate — see
 * `native-settings.tsx` — so role queries and `toBeVisible` are the honest
 * assertions here. A bare `getByText` would also match a section the reader
 * cannot currently see.
 */
async function openSection(
  user: ReturnType<typeof userEvent.setup>,
  name: string
) {
  await user.click(await screen.findByRole("tab", { name }))
}
const MEMBERS = "Members",
  AGENTS = "Personal AI connections",
  PROFILE = "Your profile",
  ORGANIZATION = "Organization"
const PANEL_ID: Record<string, string> = {
  [ORGANIZATION]: "neev-settings-organization",
  [MEMBERS]: "neev-settings-members",
  [AGENTS]: "neev-settings-agents",
  [PROFILE]: "neev-settings-profile",
}
/**
 * The panel for a section, open or closed.
 *
 * Deliberately by id rather than by role and name: a hidden element has no
 * computed accessible name, so `getByRole("tabpanel", { name })` can only ever
 * reach the section that happens to be open — which is precisely the section a
 * test asserting "this one is *not* showing" is not looking at. The id is on the
 * panel whatever its state, so the query works from either side and the
 * visibility assertion stays the test's own to make.
 */
function panelOf(section: string) {
  const panel = document.getElementById(PANEL_ID[section])
  if (!panel) throw new Error(`no settings panel for section "${section}"`)
  return panel
}
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
    "GET /api/neev/library": () => json({ items: [] }),
  }
}
it("protects the owner, exposes only own sign-in methods, and keeps back navigation", async () => {
  const mock = mockNativeSession(orgSession, fixtures())
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  // Back navigation is page furniture, not part of a section.
  expect(
    await screen.findByRole("link", { name: "Back to workspace" })
  ).toHaveAttribute("href", "/workspace")
  await openSection(user, MEMBERS)
  expect(await screen.findByText("Maya")).toBeVisible()
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
  await openSection(user, MEMBERS)
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
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  await openSection(user, MEMBERS)
  await screen.findByText("Owner Maya")
  // Asserted with Members open, so the absence is a real absence rather than a
  // section that simply is not on screen yet.
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
  await openSection(user, MEMBERS)
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
  // The draft is typed on Organization, the removal happens on Members, and the
  // draft has to survive the round trip — an unsaved edit is not something a
  // reader expects a section switch to throw away.
  const name = await screen.findByLabelText("Organization name")
  await user.clear(name)
  await user.type(name, "Unpublished draft")
  await openSection(user, MEMBERS)
  await screen.findByText("Person 100")
  expect(screen.queryByText("Person 101")).toBeNull()
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
  await openSection(user, ORGANIZATION)
  expect(screen.getByLabelText("Organization name")).toHaveValue(
    "Unpublished draft"
  )
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
  await openSection(user, MEMBERS)
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
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  // Organization opens first, so its admin-only control is checked in place.
  expect(
    await screen.findByText(
      "Only administrators can edit organization branding."
    )
  ).toBeVisible()
  expect(screen.queryByRole("button", { name: "Save organization" })).toBeNull()
  await openSection(user, MEMBERS)
  await screen.findByText("Maya")
  expect(screen.queryByRole("button", { name: "Invite member" })).toBeNull()
  expect(screen.queryByRole("button", { name: /Remove / })).toBeNull()
  // Personal identity is still the reader's own, on its own section. Scoped to
  // the panel: the same name also appears in the (now hidden) member list, and
  // an unscoped text query would match both.
  await openSection(user, PROFILE)
  expect(within(panelOf(PROFILE)).getByText("Sagar")).toBeVisible()
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
  await openSection(user, AGENTS)
  // Per-user, per-provider state straight from the server — not a fixed
  // "agents are unavailable" paragraph.
  expect(await screen.findByText("Not signed in")).toBeVisible()
  expect(screen.getByText("Reconnect needed")).toBeVisible()
  // The row names the provider the way people do, not the way it is keyed.
  expect(screen.getByText("Codex")).toBeVisible()
  // Real per-provider actions, matched to the state the server reports.
  expect(
    screen.getByRole("button", { name: "Sign in with Claude" })
  ).toBeEnabled()
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
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  await openSection(user, AGENTS)
  expect(await screen.findByText("Agents are turned off.")).toBeVisible()
  expect(screen.queryByText("Connected")).toBeNull()
})

it("shows one section at a time and moves the selection with it", async () => {
  const mock = mockNativeSession(orgSession, fixtures())
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  // Organization is where the page opens; the other three are present but not
  // on screen, which is the whole point of sectioning a page this long.
  expect(
    await screen.findByRole("tab", { name: ORGANIZATION })
  ).toHaveAttribute("aria-selected", "true")
  expect(panelOf(ORGANIZATION)).toBeVisible()
  for (const section of [MEMBERS, AGENTS, PROFILE]) {
    expect(panelOf(section)).not.toBeVisible()
  }

  await openSection(user, PROFILE)
  expect(panelOf(PROFILE)).toBeVisible()
  expect(panelOf(ORGANIZATION)).not.toBeVisible()
  expect(screen.getByRole("tab", { name: PROFILE })).toHaveAttribute(
    "aria-selected",
    "true"
  )
  expect(screen.getByRole("tab", { name: ORGANIZATION })).toHaveAttribute(
    "aria-selected",
    "false"
  )
  // Signing out lives with the identity it ends, not loose on the page.
  expect(
    within(panelOf(PROFILE)).getByRole("button", { name: "Sign out" })
  ).toBeVisible()
  expect(mock.unknown).toEqual([])
})

it("walks the sections with the arrow, home and end keys", async () => {
  mockNativeSession(orgSession, fixtures())
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  const first = await screen.findByRole("tab", { name: ORGANIZATION })
  first.focus()
  // Either axis works: the nav is a column on a desktop and a row on a phone,
  // and the component cannot see which one the reader has.
  await user.keyboard("{ArrowDown}")
  expect(screen.getByRole("tab", { name: MEMBERS })).toHaveFocus()
  expect(panelOf(MEMBERS)).toBeVisible()
  // Every member's directory, after Members.
  await user.keyboard("{ArrowRight}")
  expect(screen.getByRole("tab", { name: "People & teams" })).toHaveFocus()
  await user.keyboard("{ArrowRight}")
  expect(screen.getByRole("tab", { name: "Shared library" })).toHaveFocus()
  // An admin has the Knowledge section next.
  await user.keyboard("{ArrowRight}")
  expect(screen.getByRole("tab", { name: "Knowledge" })).toHaveFocus()
  await user.keyboard("{ArrowRight}")
  expect(screen.getByRole("tab", { name: AGENTS })).toHaveFocus()
  await user.keyboard("{End}")
  expect(screen.getByRole("tab", { name: PROFILE })).toHaveFocus()
  expect(panelOf(PROFILE)).toBeVisible()
  await user.keyboard("{Home}")
  expect(first).toHaveFocus()
  expect(panelOf(ORGANIZATION)).toBeVisible()
  // Only the selected tab is a tab stop, so Tab leaves the nav rather than
  // walking every section on the way to the content.
  expect(first).toHaveAttribute("tabindex", "0")
  expect(screen.getByRole("tab", { name: MEMBERS })).toHaveAttribute(
    "tabindex",
    "-1"
  )
})

it("hides the connections panel across a section switch instead of discarding it", async () => {
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
            state: "not_signed_in",
            accountLabel: null,
            checkedAt: null,
            errorCode: null,
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
  await openSection(user, AGENTS)
  // A node from *inside* the connections panel. React cannot preserve this exact
  // element across an unmount, so its identity is the assertion.
  const refresh = await screen.findByRole("button", { name: "Refresh status" })

  await openSection(user, MEMBERS)
  expect(panelOf(AGENTS)).not.toBeVisible()
  await openSection(user, AGENTS)

  // The panel that comes back is the same one, still holding whatever sign-in it
  // had under way. If this ever regresses to conditional rendering, the remount
  // produces a fresh node here and re-reads the server, and a user part-way
  // through a provider login silently loses it.
  expect(screen.getByRole("button", { name: "Refresh status" })).toBe(refresh)
  expect(checks).toBe(1)
  expect(mock.unknown).toEqual([])
})

it("opens the section a deep link names, and ignores one it does not", async () => {
  // The composer's Connect sends the user here for one reason; landing them on
  // the front page leaves them hunting for the thing they clicked towards.
  const original = window.location
  const at = (search: string) => {
    Object.defineProperty(window, "location", {
      configurable: true,
      value: { ...original, search },
    })
  }
  const restore = () =>
    Object.defineProperty(window, "location", {
      configurable: true,
      value: original,
    })

  at("?section=agents")
  mockNativeSession(orgSession, fixtures())
  const shown = render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  expect(
    await screen.findByRole("tab", { name: AGENTS, selected: true })
  ).toBeVisible()
  shown.unmount()

  // A URL can say anything; an unrecognised section is simply no preference.
  at("?section=../evil")
  mockNativeSession(orgSession, fixtures())
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  expect(
    await screen.findByRole("tab", { name: ORGANIZATION, selected: true })
  ).toBeVisible()
  restore()
})

const knowledgeRoutes = () => ({
  "GET /api/neev/knowledge/status": () =>
    json({
      captureEnabled: false,
      eligible: 0,
      captured: 0,
      represented: 0,
      stale: 0,
      pages: 0,
      withheldPages: 0,
      sourceProblems: { tooLarge: 0, engineUnavailable: 0, failed: 0 },
      coverage: {
        units: 0,
        cited: 0,
        skippedByAgent: 0,
        uncovered: 0,
        pending: 0,
      },
      captureBlocked: null,
      bytesUsed: 0,
      job: null,
    }),
  "GET /api/neev/knowledge/settings": () =>
    json({ captureEnabled: false, revision: 1 }),
  "GET /api/neev/knowledge/pages": () => json([]),
  "GET /api/neev/knowledge/log": () => json([]),
})

it("gives admins a Knowledge section", async () => {
  const mock = mockNativeSession(orgSession, {
    ...fixtures(),
    ...knowledgeRoutes(),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  await openSection(user, "Knowledge")
  expect(
    await screen.findByRole("switch", { name: "Capture shared knowledge" })
  ).toBeVisible()
  expect(screen.getByRole("link", { name: "Export" })).toBeVisible()
  // Settings is a Neev surface: the same warm palette as the workspace, which
  // menus and dialogs opened from it pick up through this marker.
  expect(screen.getByRole("main")).toHaveAttribute("data-neev-theme", "brand")
  // The admin-only test panel sits in the section, and asks nothing by itself.
  expect(
    await screen.findByRole("heading", { name: "Test knowledge" })
  ).toBeVisible()
  expect(
    mock.fetcher.mock.calls.some(([path]) =>
      String(path).startsWith("/api/neev/knowledge/ask")
    )
  ).toBe(false)
  expect(mock.unknown).toEqual([])
})

/**
 * Knowledge is admin-only. A member has no tab for it, and a deep link to it
 * lands on Organization without asking the knowledge API for anything.
 */
it("shows members no Knowledge section, even through a deep link", async () => {
  const original = window.location
  Object.defineProperty(window, "location", {
    configurable: true,
    value: { ...original, search: "?section=knowledge" },
  })
  const mock = mockNativeSession({ ...orgSession, role: "member" }, fixtures())
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  expect(
    await screen.findByRole("tab", { name: ORGANIZATION, selected: true })
  ).toBeVisible()
  expect(screen.queryByRole("tab", { name: "Knowledge" })).toBeNull()
  expect(document.getElementById("neev-settings-knowledge")).toBeNull()
  expect(
    mock.fetcher.mock.calls.some(([path]) =>
      String(path).startsWith("/api/neev/knowledge")
    )
  ).toBe(false)
  Object.defineProperty(window, "location", {
    configurable: true,
    value: original,
  })
})

/** Live QA: Refresh in Knowledge must update the status counters too. */
it("refreshes the knowledge status from the section's Refresh", async () => {
  let reads = 0
  const routes = knowledgeRoutes()
  mockNativeSession(orgSession, {
    ...fixtures(),
    ...routes,
    "GET /api/neev/knowledge/status": () => {
      reads += 1
      return routes["GET /api/neev/knowledge/status"]()
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSettings />
    </NativeTestRoot>
  )
  await openSection(user, "Knowledge")
  await screen.findByRole("switch", { name: "Capture shared knowledge" })
  const before = reads
  await user.click(screen.getByRole("button", { name: "Refresh" }))
  await waitFor(() => expect(reads).toBe(before + 1))
})
