import { render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import messages from "@/i18n/messages/en.json"
import { afterEach, expect, it, vi } from "vitest"
import { NativeAgentChats } from "./native-agent-chats"
import { NativeBoundary } from "./native-boundary"
import { RouteNote, SupervisionSummary } from "./native-agent-auto"
import {
  composerEditable,
  json,
  mockNativeSession,
  NativeTestRoot,
  typeInComposer,
} from "./test-helpers"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

const SESSION_ID = "11111111-1111-4111-8111-111111111111"
const TURN_ID = "22222222-2222-4222-8222-222222222222"
const RUN_ID = "0123456789abcdef0123456789abcdef"

const CONNECTED = {
  runtimeEnabled: true,
  connections: [
    {
      provider: "claude",
      state: "connected",
      accountLabel: "person@example.test",
      checkedAt: 1_700_000_000,
      errorCode: null,
    },
  ],
}

const session = {
  id: SESSION_ID,
  orgId: 3,
  ownerId: 1,
  title: "Redesign the sync engine",
  provider: "claude",
  model: "opus",
  effort: "high",
  fast: false,
  boundChatId: null,
  nodeId: 7,
  archived: false,
  revision: "1",
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
}

const runningTurn = {
  id: TURN_ID,
  sessionId: SESSION_ID,
  sequence: "1",
  runId: RUN_ID,
  prompt: "Redesign the sync engine",
  response: "",
  activity: [],
  status: "running",
  errorCode: null,
  provider: "claude",
  model: "opus",
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
  completedAt: null,
}

const READY = {
  state: "ready",
  router: "jev",
  maxTaskChars: 6000,
  maxPreviousChars: 2000,
  supervision: "off",
  usage: [],
}
const ROUTED = {
  provider: "claude",
  model: "opus",
  label: "Opus",
  effort: "high",
  fast: false,
  outcome: "routed",
  reason: null,
  router: "jev",
  routeId: "44444444-4444-4444-8444-444444444444",
}

/** The one model picker. Its accessible name stays "Model" in every state. */
const picker = () => screen.getByRole("button", { name: "Model" })

async function openPicker(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("button", { name: "Model" }))
  return screen.findByRole("menu")
}

function composer() {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentChats
          nodeId={7}
          nodeName="Launch"
          draftProvider="claude"
          onSessionCreated={vi.fn()}
          onDraftCancelled={vi.fn()}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
})

async function typeAndSend(text: string) {
  const user = userEvent.setup()
  const box = await screen.findByRole("textbox", { name: "Message" })
  await waitFor(() => expect(composerEditable(box)).toBe(true))
  await typeInComposer(box, text)
  await user.click(screen.getByRole("button", { name: "Send" }))
}

it("offers no Auto control when no router is configured", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () => json(CONNECTED),
  })
  const user = userEvent.setup()
  composer()
  await screen.findByRole("textbox", { name: "Message" })
  // No standalone switch, and nothing called Auto inside the picker either.
  expect(screen.queryByRole("button", { name: /^Auto/ })).toBeNull()
  const menu = await openPicker(user)
  expect(within(menu).queryByRole("menuitemradio", { name: "Auto" })).toBeNull()
  expect(
    within(menu).getByRole("menuitem", { name: "Reasoning" })
  ).toBeVisible()
})

it("shows a missing router key as unavailable and sends manually", async () => {
  localStorage.setItem("neev.agent.auto", "1")
  const routed = vi.fn()
  let created: Record<string, unknown> | null = null
  let started: Record<string, unknown> | null = null
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () => json(CONNECTED),
    "GET /api/neev/agent/routing": () =>
      json({
        state: "missing_credential",
        router: "jev",
        maxTaskChars: 6000,
        maxPreviousChars: 2000,
        supervision: "off",
        usage: [],
      }),
    "POST /api/neev/agent/route": () => {
      routed()
      return json(ROUTED)
    },
    "POST /api/neev/agent/sessions": (init) => {
      created = JSON.parse(init.body as string)
      return json({ ...session, model: "default", effort: null }, 201)
    },
    [`POST /api/neev/agent/sessions/${SESSION_ID}/turns`]: (init) => {
      started = JSON.parse(init.body as string)
      return json(
        { turn: { ...runningTurn, model: "default" }, runId: RUN_ID },
        201
      )
    },
  })
  const user = userEvent.setup()
  composer()
  // The saved preference is on, but a missing key never shows as Auto.
  await waitFor(() =>
    expect(picker()).toHaveTextContent("Claude Code · Default")
  )
  const menu = await openPicker(user)
  const auto = within(menu).getByRole("menuitemradio", { name: "Auto" })
  expect(auto).toHaveAttribute("aria-disabled", "true")
  expect(auto.getAttribute("title")).toMatch(/key is missing/)
  expect(auto).toHaveAttribute("aria-checked", "false")
  expect(
    within(menu).getByRole("menuitemradio", { name: "Default" })
  ).toHaveAttribute("aria-checked", "true")
  await user.keyboard("{Escape}")
  await typeAndSend("Redesign the sync engine")
  await waitFor(() => expect(started).not.toBeNull())
  expect(routed).not.toHaveBeenCalled()
  expect(created).toMatchObject({ provider: "claude", model: "default" })
  expect(started).toEqual({ prompt: "Redesign the sync engine" })
})

it("routes a new chat exactly once and creates it on the decision", async () => {
  localStorage.setItem("neev.agent.auto", "1")
  const routes: Record<string, unknown>[] = []
  let created: Record<string, unknown> | null = null
  let started: Record<string, unknown> | null = null
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () => json(CONNECTED),
    "GET /api/neev/agent/routing": () => json(READY),
    "POST /api/neev/agent/route": (init) => {
      routes.push(JSON.parse(init.body as string))
      return json(ROUTED)
    },
    "POST /api/neev/agent/sessions": (init) => {
      created = JSON.parse(init.body as string)
      return json(session, 201)
    },
    [`POST /api/neev/agent/sessions/${SESSION_ID}/turns`]: (init) => {
      started = JSON.parse(init.body as string)
      return json({ turn: runningTurn, runId: RUN_ID }, 201)
    },
  })
  const user = userEvent.setup()
  composer()
  // One picker, and it says Auto: no standalone switch beside it.
  await waitFor(() => expect(picker()).toHaveTextContent(/^Auto$/))
  expect(screen.queryByRole("button", { name: /^Auto/ })).toBeNull()
  // The privacy disclosure is part of the control itself.
  expect(picker().getAttribute("title")).toMatch(
    /6000 characters of your message, plus up to 2000 characters of the previous exchange, are sent to TypeSafe/
  )
  const menu = await openPicker(user)
  const items = within(menu).getAllByRole("menuitemradio")
  // Auto leads, is the choice, and no manual model claims to be selected.
  expect(items[0]).toHaveAccessibleName("Auto")
  expect(items[0]).toHaveAttribute("aria-checked", "true")
  expect(items[0].getAttribute("title")).toMatch(/sent to TypeSafe/)
  for (const item of items.slice(1))
    expect(item).toHaveAttribute("aria-checked", "false")
  // Manual reasoning and speed are not asked for while Auto chooses them.
  expect(within(menu).queryByRole("menuitem", { name: "Reasoning" })).toBeNull()
  expect(
    within(menu).queryByRole("menuitemcheckbox", { name: "Fast" })
  ).toBeNull()
  await user.keyboard("{Escape}")
  await typeAndSend("Redesign the sync engine")
  await waitFor(() => expect(started).not.toBeNull())
  expect(routes).toEqual([
    {
      prompt: "Redesign the sync engine",
      provider: "claude",
      model: "default",
      effort: null,
      fast: false,
      attachedFiles: 0,
    },
  ])
  expect(created).toMatchObject({
    provider: "claude",
    model: "opus",
    effort: "high",
  })
  // Decided once, before creation: the first turn presents that decision
  // and is NOT routed again.
  expect(started).toEqual({
    prompt: "Redesign the sync engine",
    routeId: ROUTED.routeId,
  })
})

it("is on by default once the router is ready, and can be switched off", async () => {
  const routed = vi.fn()
  let started: Record<string, unknown> | null = null
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () => json(CONNECTED),
    "GET /api/neev/agent/routing": () => json(READY),
    "POST /api/neev/agent/route": () => {
      routed()
      return json(ROUTED)
    },
    "POST /api/neev/agent/sessions": () =>
      json({ ...session, model: "default", effort: null }, 201),
    [`POST /api/neev/agent/sessions/${SESSION_ID}/turns`]: (init) => {
      started = JSON.parse(init.body as string)
      return json(
        { turn: { ...runningTurn, model: "default" }, runId: RUN_ID },
        201
      )
    },
  })
  const user = userEvent.setup()
  composer()
  await waitFor(() => expect(picker()).toHaveTextContent(/^Auto$/))
  // The manual override: picking a model — even the very one Auto would fall
  // back to — turns Auto off, and the draft's own selection is sent.
  let menu = await openPicker(user)
  await user.click(within(menu).getByRole("menuitemradio", { name: "Default" }))
  expect(localStorage.getItem("neev.agent.auto:1:3")).toBe("0")
  await waitFor(() =>
    expect(picker()).toHaveTextContent("Claude Code · Default")
  )
  menu = await openPicker(user)
  expect(
    within(menu).getByRole("menuitemradio", { name: "Auto" })
  ).toHaveAttribute("aria-checked", "false")
  expect(
    within(menu).getByRole("menuitemradio", { name: "Default" })
  ).toHaveAttribute("aria-checked", "true")
  // Manual reasoning and speed are back.
  expect(
    within(menu).getByRole("menuitem", { name: "Reasoning" })
  ).toBeVisible()
  expect(
    within(menu).getByRole("menuitemcheckbox", { name: "Fast" })
  ).toBeInTheDocument()
  await user.keyboard("{Escape}")
  await typeAndSend("Redesign the sync engine")
  await waitFor(() => expect(started).not.toBeNull())
  expect(routed).not.toHaveBeenCalled()
})

it("turns Auto on from inside the picker, without asking for a model", async () => {
  localStorage.setItem("neev.agent.auto", "0")
  const routes: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () => json(CONNECTED),
    "GET /api/neev/agent/routing": () => json(READY),
    "POST /api/neev/agent/route": (init) => {
      routes.push(JSON.parse(init.body as string))
      return json(ROUTED)
    },
    "POST /api/neev/agent/sessions": () => json(session, 201),
    [`POST /api/neev/agent/sessions/${SESSION_ID}/turns`]: () =>
      json({ turn: runningTurn, runId: RUN_ID }, 201),
  })
  const user = userEvent.setup()
  composer()
  await waitFor(() =>
    expect(picker()).toHaveTextContent("Claude Code · Default")
  )
  const menu = await openPicker(user)
  await user.click(within(menu).getByRole("menuitemradio", { name: "Auto" }))
  expect(localStorage.getItem("neev.agent.auto:1:3")).toBe("1")
  await waitFor(() => expect(picker()).toHaveTextContent(/^Auto$/))
  await typeAndSend("Redesign the sync engine")
  await waitFor(() => expect(routes).toHaveLength(1))
})

function savedChat(turns: Record<string, unknown>[]) {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
    [`GET /api/neev/agent/sessions/${SESSION_ID}`]: () => json(session),
    "GET /api/neev/agent/connections": () => json(CONNECTED),
    "GET /api/neev/agent/routing": () => json(READY),
    [`GET /api/neev/agent/sessions/${SESSION_ID}/turns`]: () =>
      json({ items: [], nextAfter: null }),
    [`PATCH /api/neev/agent/sessions/${SESSION_ID}/settings`]: () =>
      json({ ...session, revision: "2" }),
    [`POST /api/neev/agent/sessions/${SESSION_ID}/turns`]: (init) => {
      turns.push(JSON.parse(init.body as string))
      return json(
        {
          turn: { ...runningTurn, status: "failed", errorCode: "x" },
          runId: RUN_ID,
        },
        201
      )
    },
  })
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentChats sessionId={SESSION_ID} />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

async function sendSaved(user: ReturnType<typeof userEvent.setup>) {
  const box = await screen.findByRole("textbox", { name: "Ask the agent" })
  await waitFor(() => expect(composerEditable(box)).toBe(true))
  await typeInComposer(box, "Tighten the error handling")
  await user.click(screen.getByRole("button", { name: "Send" }))
}

it("a saved chat shows Auto in its one picker and asks for it on the turn", async () => {
  const turns: Record<string, unknown>[] = []
  const user = userEvent.setup()
  savedChat(turns)
  await waitFor(() => expect(picker()).toHaveTextContent(/^Auto$/))
  expect(screen.queryByRole("button", { name: /^Auto/ })).toBeNull()
  const menu = await openPicker(user)
  expect(
    within(menu).getByRole("menuitemradio", { name: "Auto" })
  ).toHaveAttribute("aria-checked", "true")
  expect(
    within(menu).getByRole("menuitemradio", { name: "Opus" })
  ).toHaveAttribute("aria-checked", "false")
  expect(within(menu).queryByRole("menuitem", { name: "Reasoning" })).toBeNull()
  await user.keyboard("{Escape}")
  await sendSaved(user)
  await waitFor(() => expect(turns).toHaveLength(1))
  expect(turns[0]).toMatchObject({ auto: true })
})

it("a saved chat's manual pick — even its stored model — turns Auto off", async () => {
  const turns: Record<string, unknown>[] = []
  const user = userEvent.setup()
  savedChat(turns)
  await waitFor(() => expect(picker()).toHaveTextContent(/^Auto$/))
  const menu = await openPicker(user)
  await user.click(within(menu).getByRole("menuitemradio", { name: "Opus" }))
  expect(localStorage.getItem("neev.agent.auto:1:3")).toBe("0")
  await waitFor(() => expect(picker()).toHaveTextContent(/^Opus/))
  await sendSaved(user)
  await waitFor(() => expect(turns).toHaveLength(1))
  expect(turns[0]).not.toHaveProperty("auto")
})

it("keeps a manual choice and its model across a reload", async () => {
  const turns: Record<string, unknown>[] = []
  const user = userEvent.setup()
  const first = savedChat(turns)
  await waitFor(() => expect(picker()).toHaveTextContent(/^Auto$/))
  const menu = await openPicker(user)
  await user.click(within(menu).getByRole("menuitemradio", { name: "Opus" }))
  await waitFor(() => expect(picker()).toHaveTextContent(/^Opus/))
  first.unmount()
  // A reload: the stored manual choice is not overwritten by the default.
  savedChat(turns)
  await waitFor(() => expect(picker()).toHaveTextContent(/^Opus/))
  const again = await openPicker(user)
  expect(
    within(again).getByRole("menuitemradio", { name: "Auto" })
  ).toHaveAttribute("aria-checked", "false")
  expect(
    within(again).getByRole("menuitemradio", { name: "Opus" })
  ).toHaveAttribute("aria-checked", "true")
})

it("creates nothing when the draft goes away while Auto is deciding", async () => {
  localStorage.setItem("neev.agent.auto", "1")
  let answer: (() => void) | null = null
  const created = vi.fn()
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () => json(CONNECTED),
    "GET /api/neev/agent/routing": () => json(READY),
    "POST /api/neev/agent/route": () =>
      new Promise<Response>((resolve) => {
        answer = () => resolve(json(ROUTED))
      }),
    "POST /api/neev/agent/sessions": () => {
      created()
      return json(session, 201)
    },
  })
  const view = composer()
  await typeAndSend("Redesign the sync engine")
  expect(await screen.findByText("Choosing a model…")).toBeVisible()
  await waitFor(() => expect(answer).not.toBeNull())
  view.unmount()
  answer!()
  await new Promise((resolve) => setTimeout(resolve, 50))
  expect(created).not.toHaveBeenCalled()
})

function note(route: Parameters<typeof RouteNote>[0]["route"]) {
  return render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <RouteNote
        route={
          route &&
          Object.assign(
            { withheld: [], supervision: null, routeId: null },
            route
          )
        }
      />
    </NextIntlClientProvider>
  )
}

it("says what ran, what was kept, and why a fallback happened", () => {
  const routed = note(ROUTED as never)
  expect(screen.getByText("Auto: Opus · high")).toBeVisible()
  routed.unmount()
  const kept = note({ ...ROUTED, outcome: "kept", label: null } as never)
  expect(
    screen.getByText("Auto kept opus · high for this follow-up")
  ).toBeVisible()
  kept.unmount()
  note({
    ...ROUTED,
    model: "sonnet",
    label: null,
    effort: null,
    outcome: "fallback",
    reason: "timeout",
  } as never)
  expect(
    screen.getByText("Auto used your selection, sonnet (no answer in time)")
  ).toBeVisible()
})

it("names the supervisor, or says plainly why there is none, and what was skipped", () => {
  const active = note({
    ...ROUTED,
    supervision: {
      state: "active",
      phases: "plan_review",
      model: "opus",
      reason: null,
    },
    withheld: [{ provider: "codex", availability: "exhausted", until: null }],
  } as never)
  expect(
    screen.getByText(
      "Auto: Opus · high · planned and reviewed by Claude opus on your own Claude plan · codex skipped: usage limit"
    )
  ).toBeVisible()
  active.unmount()
  note({
    ...ROUTED,
    supervision: {
      state: "unavailable",
      phases: null,
      model: null,
      reason: "no_claude_connection",
    },
    withheld: [],
  } as never)
  expect(
    screen.getByText(
      "Auto: Opus · high · no review: connect Claude to enable it"
    )
  ).toBeVisible()
})

it("shows a review as a model's reading with its scope, never as verification", () => {
  const detail =
    "supervisor: claude/opus\nverdict: concerns\nScope: the reply text only. Nothing was run, tested or verified."
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <SupervisionSummary
        items={[
          {
            seq: 1,
            offset: 0,
            id: "supervision:plan",
            kind: "status",
            state: "plan",
            detail: "supervisor: claude/opus\n1. Read the module",
            verdict: "ready",
          },
          {
            seq: 2,
            offset: 10,
            id: "supervision:review",
            kind: "status",
            state: "review",
            detail,
            verdict: "concerns",
          },
        ]}
      />
    </NextIntlClientProvider>
  )
  expect(screen.getByText("Plan ready")).toBeVisible()
  expect(screen.getByText("Review: concerns")).toBeVisible()
  expect(
    screen.getByText(/Nothing was run, tested or verified/)
  ).toBeInTheDocument()
})
