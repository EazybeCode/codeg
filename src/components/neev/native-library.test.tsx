import { configure, render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeLibrary } from "./native-library"
import { NativeSuspendedSurface } from "./native-suspended-surface"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  orgSession,
} from "./test-helpers"
vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))
configure({ asyncUtilTimeout: 5000 })
afterEach(() => vi.unstubAllGlobals())
const skill = {
  id: "d230e18b-8102-4560-a358-6d8aac05c712",
  name: "Brand writing",
  description: "Team tone",
  kind: "skill",
  enabled: false,
  revision: "1",
  files: ["SKILL.md"],
  setupNote: null,
}
it("lets members see team skills but reserves management for admins", async () => {
  const mock = mockNativeSession(
    { ...orgSession, role: "member" },
    { "GET /api/neev/library": () => json({ items: [skill] }) }
  )
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  expect(await screen.findByText("Brand writing")).toBeVisible()
  expect(screen.queryByLabelText("Import files")).not.toBeInTheDocument()
  expect(screen.queryByRole("switch")).not.toBeInTheDocument()
  expect(mock.unknown).toEqual([])
})
it("enables a skill with its current revision and confirms before deleting", async () => {
  const changes: unknown[] = []
  let deleted = false
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: deleted ? [] : [skill] }),
    [`PATCH /api/neev/library/${skill.id}`]: (init) => {
      changes.push(JSON.parse(init.body as string))
      return json({ ...skill, enabled: true, revision: "2" })
    },
    [`DELETE /api/neev/library/${skill.id}`]: (init) => {
      changes.push(JSON.parse(init.body as string))
      deleted = true
      return json({ ok: true })
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  await user.click(
    await screen.findByRole("switch", { name: "Enable Brand writing" })
  )
  expect(changes[0]).toEqual({ expectedRevision: "1", enabled: true })
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Delete Brand writing" })
    ).toBeEnabled()
  )
  await user.click(screen.getByRole("button", { name: "Delete Brand writing" }))
  expect(changes).toHaveLength(1)
  await user.click(
    screen.getByRole("button", { name: "Delete from workspace" })
  )
  await waitFor(() =>
    expect(screen.queryByText("Brand writing")).not.toBeInTheDocument()
  )
  expect(changes[1]).toEqual({ expectedRevision: "2" })
  expect(mock.unknown).toEqual([])
})
it("imports files disabled for review and shows safe import errors", async () => {
  let imported = false
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: imported ? [skill] : [] }),
    "POST /api/neev/library/import": (init) => {
      const body = JSON.parse(init.body as string)
      expect(body.fileName).toBe("SKILL.md")
      expect(atob(body.data)).toContain("Brand writing")
      imported = true
      return json({ items: [skill] })
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  await user.upload(
    await screen.findByLabelText("Import files"),
    new File(["# Brand writing"], "SKILL.md", { type: "text/markdown" })
  )
  expect(await screen.findByText("Brand writing")).toBeVisible()
  expect(screen.getByRole("switch")).not.toBeChecked()
  expect(mock.unknown).toEqual([])
})

it("keeps successful bulk imports and identifies rejected files", async () => {
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: [] }),
    "POST /api/neev/library/import": (init) => {
      const body = JSON.parse(init.body as string)
      return body.fileName === "bad.json"
        ? json({ code: "invalid" }, 400)
        : json({ items: [skill] })
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  await user.upload(await screen.findByLabelText("Import files"), [
    new File(["{}"], "bad.json", { type: "application/json" }),
    new File(["# Brand writing"], "SKILL.md", { type: "text/markdown" }),
  ])
  expect(await screen.findByText("Brand writing")).toBeVisible()
  expect(await screen.findByRole("alert")).toHaveTextContent("bad.json")
  expect(screen.getByRole("switch")).not.toBeChecked()
  expect(mock.unknown).toEqual([])
})

it("shows an unavailable library with retry instead of presenting an empty workspace", async () => {
  let calls = 0
  mockNativeSession(orgSession, {
    "GET /api/neev/library": () =>
      ++calls === 1
        ? json({ code: "unavailable" }, 503)
        : json({ items: [skill] }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Retry" }))
  expect(await screen.findByText("Brand writing")).toBeVisible()
})

it("reloads a teammate's changes after a revision conflict and explains the result", async () => {
  let changed = false
  mockNativeSession(orgSession, {
    "GET /api/neev/library": () =>
      json({
        items: [{ ...skill, revision: changed ? "2" : "1", enabled: changed }],
      }),
    [`PATCH /api/neev/library/${skill.id}`]: () => {
      changed = true
      return json({ code: "conflict" }, 409)
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  await user.click(
    await screen.findByRole("switch", { name: "Enable Brand writing" })
  )
  expect(await screen.findByText(/This item changed/)).toBeVisible()
  await waitFor(() => expect(screen.getByRole("switch")).toBeChecked())
})

const mcp = {
  ...skill,
  id: "a230e18b-8102-4560-a358-6d8aac05c713",
  name: "HubSpot",
  kind: "mcp",
  files: [],
  integration: {
    transport: "remote",
    hosts: ["mcp.hubspot.com"],
    // An env-shaped name on a REMOTE integration is still a header.
    secretNames: ["APIKEY", "Authorization"],
    source: "import",
    notes: [],
    status: null,
  },
}
function renderLibrary() {
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
}

it("shows members integration readiness without management controls", async () => {
  mockNativeSession(
    { ...orgSession, role: "member" },
    { "GET /api/neev/library": () => json({ items: [mcp] }) }
  )
  const user = userEvent.setup()
  renderLibrary()
  await user.click(await screen.findByRole("button", { name: "Integrations" }))
  expect(await screen.findByText("HubSpot")).toBeVisible()
  expect(screen.getByText(/Not tested yet/)).toBeVisible()
  expect(screen.getByText(/mcp\.hubspot\.com/)).toBeVisible()
  expect(screen.queryByText("Test")).not.toBeInTheDocument()
  expect(screen.queryByText("Update credentials")).not.toBeInTheDocument()
})

it("lets admins test an integration and shows the fresh status", async () => {
  const bodies: unknown[] = []
  mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: [mcp] }),
    [`POST /api/neev/library/${mcp.id}/test`]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json({
        ...mcp,
        integration: {
          ...mcp.integration,
          status: {
            state: "auth_failed",
            checkedAt: "1700000000",
            toolCount: null,
            detail: null,
            revision: "1",
          },
        },
      })
    },
  })
  const user = userEvent.setup()
  renderLibrary()
  await user.click(await screen.findByRole("button", { name: "Integrations" }))
  await user.click(await screen.findByRole("button", { name: "Test HubSpot" }))
  expect(await screen.findByText(/Credentials were rejected/)).toBeVisible()
  expect(bodies).toEqual([{ expectedRevision: "1" }])
})

it("sends a pinned package's credentials as env", async () => {
  const bodies: unknown[] = []
  const pkg = {
    ...mcp,
    integration: {
      ...mcp.integration,
      transport: "package",
      hosts: ["api.hubspot.com"],
      secretNames: ["PRIVATE_APP_ACCESS_TOKEN"],
    },
  }
  mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: [pkg] }),
    [`PUT /api/neev/library/${mcp.id}/secrets`]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json({ code: "unavailable" }, 503)
    },
  })
  const user = userEvent.setup()
  renderLibrary()
  await user.click(await screen.findByRole("button", { name: "Integrations" }))
  await user.click(
    await screen.findByRole("button", { name: /Update credentials/ })
  )
  await user.type(
    screen.getByLabelText("PRIVATE_APP_ACCESS_TOKEN"),
    "pkg-token"
  )
  await user.click(screen.getByRole("button", { name: "Save credentials" }))
  await waitFor(() => expect(bodies).toHaveLength(1))
  expect(bodies[0]).toEqual({
    expectedRevision: "1",
    env: { PRIVATE_APP_ACCESS_TOKEN: "pkg-token" },
    headers: {},
  })
})

it("submits write-only credentials by transport, then clears them", async () => {
  const bodies: unknown[] = []
  mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: [mcp] }),
    [`PUT /api/neev/library/${mcp.id}/secrets`]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json({ code: "unavailable" }, 503)
    },
  })
  const user = userEvent.setup()
  renderLibrary()
  await user.click(await screen.findByRole("button", { name: "Integrations" }))
  await user.click(
    await screen.findByRole("button", { name: /Update credentials/ })
  )
  const token = screen.getByLabelText("APIKEY")
  expect(token).toHaveAttribute("type", "password")
  expect(token).toHaveValue("")
  await user.type(token, "s3cret-token")
  await user.type(screen.getByLabelText("Authorization"), "Bearer xyz")
  await user.click(screen.getByRole("button", { name: "Save credentials" }))
  await waitFor(() => expect(bodies).toHaveLength(1))
  expect(bodies[0]).toEqual({
    expectedRevision: "1",
    env: {},
    headers: { APIKEY: "s3cret-token", Authorization: "Bearer xyz" },
  })
  expect(screen.queryByLabelText("APIKEY")).toBeNull()
  expect(document.body.innerHTML).not.toContain("s3cret-token")
  await user.click(
    await screen.findByRole("button", { name: /Update credentials/ })
  )
  expect(screen.getByLabelText("APIKEY")).toHaveValue("")
})

it("explains which imported servers were skipped and why", async () => {
  mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: [] }),
    "POST /api/neev/library/import": () =>
      json({
        items: [mcp],
        skipped: [{ name: "local-tool", reason: "unsupported_package" }],
      }),
  })
  const user = userEvent.setup()
  renderLibrary()
  await user.upload(
    await screen.findByLabelText("Import files"),
    new File(["{}"], "mcp.json", { type: "application/json" })
  )
  expect(await screen.findByText(/local-tool/)).toHaveTextContent(
    /not supported/
  )
})

it("shows skipped reasons when an import stores nothing (422)", async () => {
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: [] }),
    "POST /api/neev/library/import": () =>
      json(
        {
          items: [],
          skipped: [{ name: "local-tool", reason: "unsupported_package" }],
        },
        422
      ),
  })
  const user = userEvent.setup()
  renderLibrary()
  await user.upload(
    await screen.findByLabelText("Import files"),
    new File(["{}"], "mcp.json", { type: "application/json" })
  )
  expect(await screen.findByText(/local-tool/)).toHaveTextContent(
    /not supported/
  )
  expect(mock.unknown).toEqual([])
})

const SYNTHETIC_TOKEN = "github_pat_SYNTHETIC0000000000000000"
function renderConnect() {
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
}
function decodeImport(init: RequestInit) {
  const body = JSON.parse(init.body as string) as {
    fileName: string
    data: string
  }
  return { fileName: body.fileName, config: JSON.parse(atob(body.data)) }
}

it("offers Connect GitHub to admins only", async () => {
  mockNativeSession(
    { ...orgSession, role: "member" },
    { "GET /api/neev/library": () => json({ items: [] }) }
  )
  const user = userEvent.setup()
  renderConnect()
  await user.click(await screen.findByRole("button", { name: "Integrations" }))
  expect(
    screen.queryByRole("button", { name: "Connect GitHub" })
  ).not.toBeInTheDocument()
})

it("connects GitHub through the encrypted import, clears the token and does not claim it works", async () => {
  const imports: { fileName: string; config: unknown }[] = []
  const github = {
    ...skill,
    id: "b230e18b-8102-4560-a358-6d8aac05c714",
    name: "github",
    description: "",
    kind: "mcp",
    files: ["mcp.json", "integration.json"],
    setupNote: null,
    integration: {
      transport: "remote",
      hosts: ["api.githubcopilot.com"],
      secretNames: ["Authorization"],
      source: "http:https://api.githubcopilot.com/mcp/",
      notes: [],
      status: null,
    },
  }
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: [] }),
    "POST /api/neev/library/import": (init) => {
      imports.push(decodeImport(init))
      return json({ items: [github], skipped: [] }, 201)
    },
  })
  const user = userEvent.setup()
  renderConnect()
  await user.click(await screen.findByRole("button", { name: "Integrations" }))
  await user.click(screen.getByRole("button", { name: "Connect GitHub" }))
  const token = screen.getByLabelText("GitHub token")
  expect(token).toHaveAttribute("type", "password")
  expect(token).toHaveAttribute("autocomplete", "off")
  await user.type(token, SYNTHETIC_TOKEN)
  await user.click(screen.getByRole("button", { name: "Connect" }))
  await waitFor(() => expect(imports).toHaveLength(1))
  expect(imports[0]).toEqual({
    fileName: "github.json",
    config: {
      mcpServers: {
        github: {
          type: "http",
          url: "https://api.githubcopilot.com/mcp/",
          headers: { Authorization: `Bearer ${SYNTHETIC_TOKEN}` },
        },
      },
    },
  })
  // The token never travels in a URL.
  for (const [path] of mock.fetcher.mock.calls)
    expect(String(path)).not.toContain(SYNTHETIC_TOKEN)
  expect(
    await screen.findByText(/GitHub was added but is not tested yet/)
  ).toBeVisible()
  expect(screen.getByText(/Status: Not tested yet/)).toBeVisible()
  expect(screen.queryByText(/\bready\b/i)).not.toBeInTheDocument()
  expect(screen.queryByLabelText("GitHub token")).not.toBeInTheDocument()
  expect(document.body.innerHTML).not.toContain(SYNTHETIC_TOKEN)
  // Re-opening starts empty.
  await user.click(screen.getByRole("button", { name: "Connect GitHub" }))
  expect(screen.getByLabelText("GitHub token")).toHaveValue("")
})

it("explains an existing GitHub integration and failures without echoing the token", async () => {
  let calls = 0
  mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: [] }),
    "POST /api/neev/library/import": () => {
      calls += 1
      return calls === 1
        ? json(
            {
              items: [],
              skipped: [{ name: "github", reason: "duplicate_name" }],
            },
            422
          )
        : json({ code: "unavailable" }, 503)
    },
  })
  const user = userEvent.setup()
  renderConnect()
  await user.click(await screen.findByRole("button", { name: "Integrations" }))
  const attempt = async (n: number) => {
    await user.click(screen.getByRole("button", { name: "Connect GitHub" }))
    await user.type(screen.getByLabelText("GitHub token"), SYNTHETIC_TOKEN)
    await user.click(screen.getByRole("button", { name: "Connect" }))
    await waitFor(() => expect(calls).toBe(n))
  }
  // Already connected: explained, nothing claimed.
  await attempt(1)
  expect(await screen.findByText(/github/i, { selector: "li" })).toBeVisible()
  expect(document.body.innerHTML).not.toContain(SYNTHETIC_TOKEN)
  // A server failure: an error, still no token anywhere.
  await attempt(2)
  expect(await screen.findByRole("alert")).toBeVisible()
  expect(document.body.innerHTML).not.toContain(SYNTHETIC_TOKEN)
  expect(
    screen.queryByText(/GitHub was added but is not tested yet/)
  ).not.toBeInTheDocument()
}, 15000)

it("refuses a token with spaces before sending anything", async () => {
  let calls = 0
  mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: [] }),
    "POST /api/neev/library/import": () => {
      calls += 1
      return json({ items: [], skipped: [] }, 201)
    },
  })
  const user = userEvent.setup()
  renderConnect()
  await user.click(await screen.findByRole("button", { name: "Integrations" }))
  await user.click(screen.getByRole("button", { name: "Connect GitHub" }))
  await user.type(screen.getByLabelText("GitHub token"), "two words")
  await user.click(screen.getByRole("button", { name: "Connect" }))
  expect(await screen.findByText(/single token/i)).toBeVisible()
  expect(calls).toBe(0)
  expect(document.body.innerHTML).not.toContain("two words")
})
