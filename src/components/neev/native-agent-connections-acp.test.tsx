// The ACP providers (Cursor, Gemini CLI, OpenCode) through the SAME connect
// panel as Claude and Codex. What matters here: a provider appears only when
// the server lists it, each sign-in shape is rendered as the one thing the user
// can actually do, and an approval URL is held to its own provider's host.
import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeAgentConnectionsPanel } from "./native-agent-connections"
import { NativeBoundary } from "./native-boundary"
import { json, mockNativeSession, NativeTestRoot } from "./test-helpers"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

const ATTEMPT_ID = "0123456789abcdef0123456789abcdef"
const FAR_FUTURE = Math.floor(Date.now() / 1000) + 600

const route = (provider: string, tail: string) =>
  `/api/neev/agent/connections/${provider}${tail}`
const connectOf = (provider: string) => `POST ${route(provider, "/connect")}`
const attemptOf = (provider: string) =>
  `GET ${route(provider, `/attempts/${ATTEMPT_ID}`)}`

function row(provider: string, state: string) {
  return {
    provider,
    state,
    accountLabel: null,
    checkedAt: null,
    errorCode: null,
  }
}

function listing(rows: ReturnType<typeof row>[]) {
  return json({ runtimeEnabled: true, connections: rows })
}

function pendingConnect() {
  return json(
    {
      outcome: "pending",
      attemptId: ATTEMPT_ID,
      expiresAt: FAR_FUTURE,
      connection: null,
    },
    202
  )
}

function pending(approval: unknown) {
  return json({ state: "pending", expiresAt: FAR_FUTURE, approval })
}

function mount() {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentConnectionsPanel pollMs={5} showSettingsLink={false} />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
})

it("offers no ACP provider the server does not list", async () => {
  // The default deployment: `NEEV_AGENT_ACP_PROVIDERS` is empty, so the server
  // lists Claude and Codex only. Knowing the ids must not conjure rows.
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      listing([row("claude", "not_signed_in"), row("codex", "connected")]),
  })
  mount()
  expect(
    await screen.findByRole("button", { name: "Connect claude" })
  ).toBeVisible()
  for (const name of ["Cursor", "Gemini CLI", "OpenCode"])
    expect(screen.queryByText(name)).toBeNull()
  for (const id of ["cursor", "gemini", "opencode"])
    expect(screen.queryByRole("button", { name: `Connect ${id}` })).toBeNull()
})

it("walks a Cursor browser approval: open, approve, nothing to paste", async () => {
  let state = "not_signed_in"
  let polls = 0
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      listing([row("claude", "connected"), row("cursor", state)]),
    [connectOf("cursor")]: () => {
      state = "connecting"
      return pendingConnect()
    },
    [attemptOf("cursor")]: () => {
      polls += 1
      if (polls < 4)
        return pending({
          kind: "browser",
          verificationUrl: "https://cursor.com/loginDeepControl?challenge=x",
          userCode: null,
          needsCode: false,
        })
      state = "connected"
      return json({ state: "completed", expiresAt: FAR_FUTURE })
    },
  })
  const user = userEvent.setup()
  mount()
  // The row is named for a reader, not by its wire id.
  expect(await screen.findByText("Cursor")).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Connect cursor" }))
  const link = await screen.findByRole("link", {
    name: "Open the sign-in page",
  })
  expect(link).toHaveAttribute(
    "href",
    "https://cursor.com/loginDeepControl?challenge=x"
  )
  expect(screen.getByText(/there is nothing to paste/)).toBeVisible()
  // No code shown, none asked for.
  expect(screen.queryByRole("textbox")).toBeNull()
  expect(screen.queryByText("Your code:")).toBeNull()
  // The CLI notices the approval; only the server saying so reads Connected.
  expect(await screen.findByText("Connected")).toBeVisible()
  await waitFor(() =>
    expect(
      screen.queryByRole("link", { name: "Open the sign-in page" })
    ).toBeNull()
  )
})

it("takes a Gemini CLI paste code through the same prompt as Claude", async () => {
  const submitted: unknown[] = []
  let state = "not_signed_in"
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => listing([row("gemini", state)]),
    [connectOf("gemini")]: () => pendingConnect(),
    [attemptOf("gemini")]: () =>
      pending({
        kind: "authorizationCode",
        verificationUrl:
          "https://accounts.google.com/o/oauth2/v2/auth?client_id=x",
        userCode: null,
        needsCode: true,
      }),
    [`POST ${route("gemini", `/attempts/${ATTEMPT_ID}/code`)}`]: (init) => {
      submitted.push(JSON.parse(init.body as string))
      state = "connected"
      return json(
        { state: "completed", expiresAt: FAR_FUTURE, approval: null },
        202
      )
    },
  })
  const user = userEvent.setup()
  mount()
  expect(await screen.findByText("Gemini CLI")).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Connect gemini" }))
  expect(
    await screen.findByRole("link", { name: "Open the sign-in page" })
  ).toHaveAttribute(
    "href",
    "https://accounts.google.com/o/oauth2/v2/auth?client_id=x"
  )
  await user.type(
    screen.getByRole("textbox", { name: "Paste the authorization code" }),
    " 4/0Aabc "
  )
  await user.click(screen.getByRole("button", { name: "Submit code" }))
  await waitFor(() => expect(submitted).toEqual([{ code: "4/0Aabc" }]))
  expect(await screen.findByText("Connected")).toBeVisible()
})

it("shows an OpenCode device code on OpenAI's own device page", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      listing([row("opencode", "not_signed_in")]),
    [connectOf("opencode")]: () => pendingConnect(),
    [attemptOf("opencode")]: () =>
      pending({
        kind: "device",
        verificationUrl: "https://auth.openai.com/codex/device",
        userCode: "WXYZ-12345",
        needsCode: false,
      }),
  })
  const user = userEvent.setup()
  mount()
  expect(await screen.findByText("OpenCode")).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Connect opencode" }))
  expect(await screen.findByText("WXYZ-12345")).toBeVisible()
  expect(
    screen.getByRole("link", { name: "Open the sign-in page" })
  ).toHaveAttribute("href", "https://auth.openai.com/codex/device")
  expect(screen.queryByRole("textbox")).toBeNull()
})

it("refuses a Cursor attempt pointing anywhere but cursor.com", async () => {
  // Google's authorize URL is allowed server-side — for Gemini. On a Cursor
  // attempt it is a supervisor telling the user to approve the wrong thing.
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      listing([row("cursor", "not_signed_in")]),
    [connectOf("cursor")]: () => pendingConnect(),
    [attemptOf("cursor")]: () =>
      pending({
        kind: "browser",
        verificationUrl: "https://accounts.google.com/o/oauth2/v2/auth?x=1",
        userCode: null,
        needsCode: false,
      }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Connect cursor" })
  )
  expect(await screen.findByRole("alert")).toBeVisible()
  expect(
    screen.queryByRole("link", { name: "Open the sign-in page" })
  ).toBeNull()
  // The attempt is kept: still cancellable, never declared failed.
  expect(
    screen.getByRole("button", { name: "Cancel cursor sign-in" })
  ).toBeEnabled()
})
