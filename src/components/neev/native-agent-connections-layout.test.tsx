// The AI connections list as a reader and a keyboard meet it: one titled
// region with a privacy line and an icon refresh; one compact row per offered
// provider with its mark, name, status and its own actions; and a sign-in
// panel that opens under the row being signed in to — and only that row.
import { render, screen, waitFor, within } from "@testing-library/react"
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

function row(
  provider: string,
  state: string,
  accountLabel: string | null = null
) {
  return { provider, state, accountLabel, checkedAt: null, errorCode: null }
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

/** The row for one provider, by its visible name. */
async function rowOf(name: string) {
  const list = await screen.findByRole("list")
  return within(list)
    .getAllByRole("listitem")
    .find((item) => within(item).queryByText(name, { exact: true }))!
}

afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
})

it("is one titled region with a privacy line and an icon refresh that reloads", async () => {
  let reads = 0
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () => {
      reads += 1
      return json({
        runtimeEnabled: true,
        connections: [row("claude", "not_signed_in")],
      })
    },
  })
  const user = userEvent.setup()
  mount()
  const region = await screen.findByRole("region", { name: "AI connections" })
  expect(
    within(region).getByRole("heading", { level: 2, name: "AI connections" })
  ).toBeVisible()
  expect(
    within(region).getByText(/Neev never sees your password/)
  ).toBeVisible()
  // An icon, so its name is its label — and it does what it says.
  const refresh = within(region).getByRole("button", { name: "Refresh status" })
  expect(refresh).toHaveTextContent("")
  await waitFor(() => expect(reads).toBe(1))
  await user.click(refresh)
  await waitFor(() => expect(reads).toBe(2))
})

it("gives each provider a compact row: mark, name, status, and only its own actions", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [
          row("claude", "connected", "Max plan"),
          row("codex", "not_signed_in"),
          row("gemini", "reconnect_required"),
        ],
      }),
  })
  mount()
  const claude = await rowOf("Claude")
  // The mark is decoration: hidden from assistive tech, and it does not repeat
  // the provider's name (the SVG <title> is dropped).
  const mark = claude
    .querySelector("[aria-hidden] svg")!
    .closest("[aria-hidden]")!
  expect(mark).toHaveClass("size-8")
  expect(claude.querySelector("title")).toBeNull()
  // Connected reads in the OK tone (green); the plan/account is secondary text.
  const connected = within(claude).getByText("Connected")
  expect(connected.className).toMatch(/neev-ok/)
  expect(within(claude).getByText("Max plan")).toBeVisible()
  expect(
    within(claude)
      .getAllByRole("button")
      .map((button) => button.getAttribute("aria-label"))
  ).toEqual(["Test the claude connection", "Disconnect claude"])

  const codex = await rowOf("Codex")
  expect(within(codex).getByText("Not signed in").className).not.toMatch(
    /neev-ok/
  )
  expect(
    within(codex).getByRole("button", { name: "Connect codex" })
  ).toBeEnabled()
  expect(
    within(codex).queryByRole("button", { name: /Disconnect|Test/ })
  ).toBeNull()

  const gemini = await rowOf("Gemini CLI")
  expect(within(gemini).getByText("Beta")).toBeVisible()
  expect(within(gemini).getByText("Reconnect needed").className).toMatch(
    /neev-warn/
  )
  expect(
    within(gemini).getByRole("button", { name: "Reconnect gemini" })
  ).toBeEnabled()
})

it("keeps the Codex prerequisite to one line until it is asked for", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [row("codex", "not_signed_in")],
      }),
  })
  const user = userEvent.setup()
  mount()
  const hint = await screen.findByText(/needs device code authorization/)
  expect(hint).toBeVisible()
  const settings = screen.getByRole("link", { name: "Open ChatGPT settings" })
  expect(settings).not.toBeVisible()
  await user.click(hint)
  expect(settings).toBeVisible()
})

it("opens the sign-in under the row being signed in to, with one action and a small Cancel", async () => {
  let rowState = "not_signed_in"
  const cancelled: unknown[] = []
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [row("claude", "connected"), row("codex", rowState)],
      }),
    "POST /api/neev/agent/connections/codex/connect": () => {
      rowState = "connecting"
      return json(
        {
          outcome: "pending",
          attemptId: ATTEMPT_ID,
          expiresAt: FAR_FUTURE,
          connection: null,
        },
        202
      )
    },
    [`GET /api/neev/agent/connections/codex/attempts/${ATTEMPT_ID}`]: () =>
      json({
        state: "pending",
        expiresAt: FAR_FUTURE,
        approval: {
          kind: "device",
          verificationUrl: "https://auth.openai.com/codex/device",
          userCode: "ABCD-EFGH",
          needsCode: false,
        },
      }),
    "POST /api/neev/agent/connections/codex/cancel": (init) => {
      cancelled.push(JSON.parse(init.body as string))
      rowState = "not_signed_in"
      return json({
        operationId: "fedcba9876543210fedcba9876543210",
        generation: "2",
        connection: row("codex", "not_signed_in"),
      })
    },
  })
  const user = userEvent.setup()
  mount()
  const codex = await rowOf("Codex")
  await user.click(within(codex).getByRole("button", { name: "Connect codex" }))

  const panel = await within(codex).findByRole("group", {
    name: "Sign in to Codex",
  })
  // What the next step needs, and nothing competing with it.
  expect(within(panel).getByText("ABCD-EFGH")).toBeVisible()
  expect(within(panel).getByRole("button", { name: "Copy code" })).toBeVisible()
  expect(
    within(panel).getByRole("link", { name: "Open the sign-in page" })
  ).toHaveAttribute("href", "https://auth.openai.com/codex/device")
  expect(within(panel).getByText("Expires")).toBeVisible()
  // The row's own action area steps aside; Cancel lives in the panel.
  expect(
    within(codex).queryByRole("button", { name: "Connect codex" })
  ).toBeNull()
  expect(within(codex).getByText("Waiting for sign-in…")).toBeVisible()
  // Only this row opened.
  const claude = await rowOf("Claude")
  expect(within(claude).queryByRole("group", { name: /Sign in/ })).toBeNull()

  await user.click(
    within(panel).getByRole("button", { name: "Cancel codex sign-in" })
  )
  await waitFor(() =>
    expect(cancelled).toEqual([
      { attemptId: ATTEMPT_ID, requestKey: expect.any(String) },
    ])
  )
  await waitFor(() =>
    expect(
      within(codex).queryByRole("group", { name: "Sign in to Codex" })
    ).toBeNull()
  )
  expect(
    await within(codex).findByRole("button", { name: "Connect codex" })
  ).toBeEnabled()
})

it("says the sign-in is being prepared before the provider has answered", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [row("claude", "not_signed_in")],
      }),
    "POST /api/neev/agent/connections/claude/connect": () =>
      json(
        {
          outcome: "pending",
          attemptId: ATTEMPT_ID,
          expiresAt: FAR_FUTURE,
          connection: null,
        },
        202
      ),
    [`GET /api/neev/agent/connections/claude/attempts/${ATTEMPT_ID}`]: () =>
      json({ state: "pending", expiresAt: FAR_FUTURE, approval: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(
    await screen.findByRole("button", { name: "Connect claude" })
  )
  const panel = await screen.findByRole("group", { name: "Sign in to Claude" })
  expect(within(panel).getByText("Preparing sign-in…")).toBeVisible()
  expect(
    within(panel).getByRole("button", { name: "Cancel claude sign-in" })
  ).toBeEnabled()
})

it("reaches every action by keyboard, refresh first", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [
          row("claude", "connected"),
          row("codex", "not_signed_in"),
        ],
      }),
  })
  const user = userEvent.setup()
  mount()
  await screen.findByRole("button", { name: "Connect codex" })
  const order: (string | null)[] = []
  for (let step = 0; step < 4; step += 1) {
    await user.tab()
    order.push(document.activeElement?.getAttribute("aria-label") ?? null)
  }
  expect(order).toEqual([
    "Refresh status",
    "Test the claude connection",
    "Disconnect claude",
    "Connect codex",
  ])
})
