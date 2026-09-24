import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import messages from "@/i18n/messages/en.json"
import { afterEach, expect, it, vi } from "vitest"
import { ActivityTimeline, NativeAgentChats } from "./native-agent-chats"
import { NativeBoundary } from "./native-boundary"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  composerText,
  composerEditable,
  typeInComposer,
} from "./test-helpers"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

const SESSION_ID = "11111111-1111-4111-8111-111111111111"
const TURN_ID = "22222222-2222-4222-8222-222222222222"
const RUN_ID = "0123456789abcdef0123456789abcdef"

const session = {
  id: SESSION_ID,
  orgId: 3,
  ownerId: 1,
  title: "Refactor the parser",
  provider: "claude",
  model: "default",
  boundChatId: null,
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
  prompt: "Summarise the parser",
  response: "",
  // Every turn carries its tool trace; an empty one is still a trace.
  activity: [],
  status: "running",
  errorCode: null,
  provider: "claude",
  model: "default",
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
  completedAt: null,
}

const connected = {
  runtimeEnabled: true,
  connections: [
    {
      provider: "claude",
      state: "connected",
      accountLabel: "sagar@example.test",
      checkedAt: 1_700_000_000,
      errorCode: null,
    },
  ],
}

const signedOut = {
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
}

const noRuntime = {
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
}

const TURNS = `GET /api/neev/agent/sessions/${SESSION_ID}/turns`
const EVENTS = `GET /api/neev/agent/runs/${RUN_ID}/events`

function sseBody(...frames: string[]) {
  return new Response(frames.map((frame) => `data: ${frame}\n\n`).join(""), {
    status: 200,
    headers: { "Content-Type": "text/event-stream" },
  })
}

/** A stream the test feeds frame by frame. */
function liveStream() {
  const encoder = new TextEncoder()
  let controller!: ReadableStreamDefaultController<Uint8Array>
  const body = new ReadableStream<Uint8Array>({
    start(c) {
      controller = c
    },
  })
  return {
    response: new Response(body, {
      status: 200,
      headers: { "Content-Type": "text/event-stream" },
    }),
    push: (frame: unknown) =>
      controller.enqueue(encoder.encode(`data: ${JSON.stringify(frame)}\n\n`)),
    close: () => controller.close(),
  }
}

function mount(project?: { nodeId: number; nodeName?: string }) {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentChats
          nodeId={project?.nodeId ?? null}
          nodeName={project?.nodeName}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

const SECOND_ID = "33333333-3333-4333-8333-333333333333"
const second = {
  ...session,
  id: SECOND_ID,
  title: "Audit the parser",
  updatedAt: 1_700_000_100,
}
const NODE_PATCH = `PATCH /api/neev/agent/sessions/${SESSION_ID}/node`

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
  localStorage.clear()
})

it("lists saved sessions and says they are private", async () => {
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
    "GET /api/neev/agent/connections": () => json(connected),
  })
  mount()
  expect(await screen.findByText("Refactor the parser")).toBeVisible()
  expect(screen.getByText(/Private to you/)).toBeVisible()
  // Connection administration lives in Settings, not on a chat surface: this
  // is where you talk to an agent, not where you manage accounts.
  expect(screen.queryByText("Connected")).toBeNull()
  expect(screen.queryByText("sagar@example.test")).toBeNull()
  expect(screen.queryByRole("button", { name: "Disconnect claude" })).toBeNull()
  expect(mock.unknown).toEqual([])
})

it("creates a saved session with a supported provider and opens it", async () => {
  const bodies: Record<string, unknown>[] = []
  let created = false
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () =>
      json({ sessions: created ? [session] : [] }),
    "GET /api/neev/agent/connections": () => json(connected),
    "POST /api/neev/agent/sessions": (init) => {
      bodies.push(JSON.parse(init.body as string))
      created = true
      return json(session, 201)
    },
    [TURNS]: () => json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await screen.findByText(/No saved agent chats yet/)
  await user.click(screen.getByRole("button", { name: "New agent chat" }))
  await user.type(
    screen.getByRole("textbox", { name: "Title" }),
    "Refactor the parser"
  )
  // The model menu IS the provider choice: one compact menu grouped by
  // provider, every entry read from the catalogue rather than written here.
  await user.click(screen.getByRole("button", { name: "Model" }))
  // The menu leads with a shortlist — the catalogue's own first few until
  // someone pins otherwise — and everything else is one submenu away.
  expect(
    within(await screen.findByRole("group", { name: "Claude Code" }))
      .getAllByRole("menuitemradio")
      .map((item) => item.textContent)
  ).toEqual(["Default", "Haiku", "Sonnet"])
  await user.click(screen.getByRole("menuitem", { name: "More models" }))
  const all = await screen.findByRole("menu", { name: "More models" })
  expect(
    within(within(all).getByRole("group", { name: "Claude Code" }))
      .getAllByRole("menuitemradio")
      .map((item) => item.textContent)
  ).toEqual(["Default", "Haiku", "Sonnet", "Opus"])
  expect(
    within(within(all).getByRole("group", { name: "Codex" }))
      .getAllByRole("menuitemradio")
      .map((item) => item.textContent)
  ).toEqual(["Default"])
  // One Escape: Radix closes the whole menu tree, not just the submenu — the
  // dialog behind it must survive, because the chat has not been created yet.
  await user.keyboard("{Escape}")
  await user.click(screen.getByRole("button", { name: "Create" }))
  await waitFor(() => expect(bodies).toHaveLength(1))
  // `effort` and `fast` are absent, not null and false: nobody chose a level
  // and nobody asked to pay for fast mode.
  expect(bodies[0]).toEqual({
    title: "Refactor the parser",
    provider: "claude",
    model: "default",
    requestId: expect.any(String),
  })
  // Creating opens the new session's canvas.
  expect(
    await screen.findByRole("region", { name: "Refactor the parser" })
  ).toBeVisible()
})

it("reattaches to a run left running by a refresh instead of re-sending it", async () => {
  const starts: unknown[] = []
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
    "GET /api/neev/agent/connections": () => json(connected),
    [TURNS]: () => json({ items: [runningTurn], nextAfter: null }),
    [EVENTS]: () =>
      sseBody(
        `{"type":"started","runId":"${RUN_ID}","turnId":"${TURN_ID}"}`,
        '{"type":"chunk","text":"The parser "}',
        '{"type":"chunk","text":"reads tokens."}',
        '{"type":"finished","status":"completed","errorCode":null}'
      ),
    [`POST /api/neev/agent/sessions/${SESSION_ID}/turns`]: (init) => {
      starts.push(init)
      return json({ turn: runningTurn, runId: RUN_ID }, 201)
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByText("Refactor the parser"))
  // The stored prompt and the replayed answer both appear …
  expect(await screen.findByText("Summarise the parser")).toBeVisible()
  expect(await screen.findByText("The parser reads tokens.")).toBeVisible()
  // … and no turn was started: reattaching is a read of the run.
  expect(starts).toEqual([])
})

it("streams a new turn and can cancel it", async () => {
  const stream = liveStream()
  let canceled: unknown = null
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
    "GET /api/neev/agent/connections": () => json(connected),
    [TURNS]: () => json({ items: [], nextAfter: null }),
    [`POST /api/neev/agent/sessions/${SESSION_ID}/turns`]: () =>
      json({ turn: runningTurn, runId: RUN_ID }, 201),
    [EVENTS]: () => stream.response,
    "POST /api/neev/agent/runs/cancel": (init) => {
      canceled = JSON.parse(init.body as string)
      return json({ canceled: true, status: "running" })
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByText("Refactor the parser"))
  const composer = await screen.findByRole("textbox", { name: "Ask the agent" })
  await typeInComposer(composer, "Summarise the parser")
  await user.click(screen.getByRole("button", { name: "Send" }))
  act(() => stream.push({ type: "chunk", text: "Working on it" }))
  expect(await screen.findByText("Working on it")).toBeVisible()
  // One busy indicator, on the activity line — not a second copy in the
  // header, which used to say "Running" for the same fact.
  expect(screen.getByText("Working…")).toBeVisible()
  expect(screen.queryByText("Running")).toBeNull()
  await user.click(screen.getByRole("button", { name: "Cancel" }))
  await waitFor(() => expect(canceled).toEqual({ runId: RUN_ID }))
  act(() =>
    stream.push({
      type: "finished",
      status: "canceled",
      errorCode: "canceled",
    })
  )
  expect(await screen.findByText("Canceled.")).toBeVisible()
  act(() => stream.close())
})

it("shows a refused turn's real reason instead of a pending state", async () => {
  const refused = {
    ...runningTurn,
    status: "failed",
    errorCode: "disabled",
    completedAt: 1_700_000_001,
  }
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
    "GET /api/neev/agent/connections": () => json(connected),
    [TURNS]: () => json({ items: [], nextAfter: null }),
    [`POST /api/neev/agent/sessions/${SESSION_ID}/turns`]: () =>
      json({ turn: refused, runId: RUN_ID }, 201),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByText("Refactor the parser"))
  await typeInComposer(
    await screen.findByRole("textbox", { name: "Ask the agent" }),
    "Summarise the parser"
  )
  await user.click(screen.getByRole("button", { name: "Send" }))
  // The prompt is durable, the refusal is named, and nothing claims to be
  // running or queued.
  expect(
    await screen.findByText("Agents are turned off on this server.")
  ).toBeVisible()
  expect(screen.queryByText("Working…")).toBeNull()
})

it("refuses to accept a prompt while the provider is not signed in", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
    "GET /api/neev/agent/connections": () => json(signedOut),
    [TURNS]: () => json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByText("Refactor the parser"))
  expect(
    composerEditable(
      await screen.findByRole("textbox", { name: "Ask the agent" })
    )
  ).toBe(false)
  // Honest about why, and never a placeholder "Connected".
  expect(screen.getAllByText("Not signed in").length).toBeGreaterThan(0)
  expect(screen.queryByText("Connected")).toBeNull()
})

it("says agents are off rather than offering a connection to a dead runtime", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
    "GET /api/neev/agent/connections": () => json(noRuntime),
    [TURNS]: () => json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByText("Refactor the parser"))
  expect(
    composerEditable(
      await screen.findByRole("textbox", { name: "Ask the agent" })
    )
  ).toBe(false)
  expect(screen.getAllByText("Agents are turned off.").length).toBeGreaterThan(
    0
  )
})

it("renames a session with its expected revision", async () => {
  const patches: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
    "GET /api/neev/agent/connections": () => json(connected),
    [TURNS]: () => json({ items: [], nextAfter: null }),
    [`PATCH /api/neev/agent/sessions/${SESSION_ID}`]: (init) => {
      patches.push(JSON.parse(init.body as string))
      return json({ ...session, title: "Parser notes", revision: "2" })
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByText("Refactor the parser"))
  await user.click(
    await screen.findByRole("button", { name: "Content actions" })
  )
  await user.click(await screen.findByRole("menuitem", { name: "Rename" }))
  const field = await screen.findByRole("textbox", { name: "Title" })
  await user.clear(field)
  await user.type(field, "Parser notes")
  await user.click(screen.getByRole("button", { name: "Save" }))
  await waitFor(() => expect(patches).toHaveLength(1))
  // Exactly one field, with the revision the client last saw.
  expect(patches[0]).toEqual({
    title: "Parser notes",
    expectedRevision: "1",
  })
})

it("archives a session with its expected revision", async () => {
  const patches: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
    "GET /api/neev/agent/connections": () => json(connected),
    [TURNS]: () => json({ items: [], nextAfter: null }),
    [`PATCH /api/neev/agent/sessions/${SESSION_ID}`]: (init) => {
      patches.push(JSON.parse(init.body as string))
      return json({ ...session, archived: true, revision: "2" })
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByText("Refactor the parser"))
  await user.click(
    await screen.findByRole("button", { name: "Content actions" })
  )
  await user.click(await screen.findByRole("menuitem", { name: "Archive" }))
  await waitFor(() => expect(patches).toHaveLength(1))
  expect(patches[0]).toEqual({ archived: true, expectedRevision: "1" })
})

it("reports a run it can no longer follow and offers an explicit reattach", async () => {
  let attempts = 0
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
    "GET /api/neev/agent/connections": () => json(connected),
    [TURNS]: () => json({ items: [runningTurn], nextAfter: null }),
    [EVENTS]: () => {
      attempts += 1
      // Gone for good: the client must stop, not re-run the prompt.
      return json({ code: "not_found" }, 404)
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByText("Refactor the parser"))
  expect(
    await screen.findByText(
      "This run can no longer be followed. Start a new turn."
    )
  ).toBeVisible()
  await waitFor(() => expect(attempts).toBe(1))
  // Recovering is the user's explicit choice, and it is still only a read.
  await user.click(screen.getByRole("button", { name: "Reconnect" }))
  await waitFor(() => expect(attempts).toBe(2))
})

it("shows a project only the agent chats filed under it", async () => {
  // Showing the same global list under every project would tell the user each
  // of them contains all of it.
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () =>
      json({
        sessions: [
          { ...session, nodeId: 7 },
          { ...second, nodeId: 9 },
        ],
      }),
    "GET /api/neev/agent/connections": () =>
      json({ runtimeEnabled: true, connections: [] }),
  })
  mount({ nodeId: 7, nodeName: "Launch" })

  // Await the list, not the static heading: the heading renders while the
  // sessions are still loading.
  expect(await screen.findByText("Refactor the parser")).toBeVisible()
  expect(screen.getByText("Agent chats in Launch")).toBeVisible()
  // Filed under another project, so not this project's.
  expect(screen.queryByText("Audit the parser")).toBeNull()
  // Nothing is unfiled here, so the organizer stays out of the way.
  expect(screen.queryByText(/Not in a project/)).toBeNull()
})

it("files an unfiled session against its revision, and it stays filed", async () => {
  let filed = false
  let moved: Record<string, unknown> | null = null
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () =>
      json({ sessions: [{ ...session, nodeId: filed ? 7 : null }] }),
    "GET /api/neev/agent/connections": () =>
      json({ runtimeEnabled: true, connections: [] }),
    [NODE_PATCH]: (init) => {
      moved = JSON.parse(init.body as string)
      filed = true
      return json({ ...session, nodeId: 7, revision: "2" })
    },
  })
  const user = userEvent.setup()
  const first = mount({ nodeId: 7, nodeName: "Launch" })

  // It starts unfiled — it predates the association — and filing it is an
  // explicit act, not something the project does on its arrival.
  await user.click(
    await screen.findByRole("button", { name: /Not in a project/ })
  )
  await user.click(await screen.findByRole("button", { name: "Move here" }))
  // Against the revision it was read at, so a concurrent rename is not lost.
  await waitFor(() =>
    expect(moved).toEqual({ nodeId: 7, expectedRevision: "1" })
  )
  expect(await screen.findByText("Refactor the parser")).toBeVisible()
  first.unmount()

  // The server holds it now, so it survives more than a reload.
  mount({ nodeId: 7, nodeName: "Launch" })
  expect(await screen.findByText("Refactor the parser")).toBeVisible()
  expect(screen.queryByText(/Not in a project/)).toBeNull()
})

it("does not show one project's agent chats under another", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () =>
      json({ sessions: [{ ...session, nodeId: 7 }] }),
    "GET /api/neev/agent/connections": () =>
      json({ runtimeEnabled: true, connections: [] }),
  })
  mount({ nodeId: 9, nodeName: "Other" })

  expect(await screen.findByText("Agent chats in Other")).toBeVisible()
  expect(screen.queryByText("Refactor the parser")).toBeNull()
})

it("never hides a session from its owner outside a project", async () => {
  // The filing is a view, not an access rule: with no project selected every
  // session the server returned is listed, filed or not.
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () =>
      json({ sessions: [{ ...session, nodeId: 7 }, second] }),
    "GET /api/neev/agent/connections": () =>
      json({ runtimeEnabled: true, connections: [] }),
  })
  mount()

  expect(await screen.findByText("Refactor the parser")).toBeVisible()
  expect(screen.getByText("Audit the parser")).toBeVisible()
  // And no organizer outside a project: there is nowhere to move anything to.
  expect(screen.queryByText(/Not in a project/)).toBeNull()
})

it("treats a server that does not send nodeId as unfiled, not as ours", async () => {
  // A pre-schema-12 server omits the field entirely. Those sessions are
  // offered to the project, never silently adopted into it.
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session, second] }),
    "GET /api/neev/agent/connections": () =>
      json({ runtimeEnabled: true, connections: [] }),
  })
  mount({ nodeId: 7, nodeName: "Launch" })

  // Await the list: the heading renders while the sessions are still loading.
  expect(
    await screen.findByRole("button", { name: /Not in a project \(2\)/ })
  ).toBeVisible()
  expect(screen.getByText("Agent chats in Launch")).toBeVisible()
  // …and none of them is shown as belonging to this project.
  expect(screen.queryByText("Refactor the parser")).toBeNull()
})

it("keeps the session where it was when a move is refused", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
    "GET /api/neev/agent/connections": () =>
      json({ runtimeEnabled: true, connections: [] }),
    // The revision moved underneath: the server refuses rather than clobber.
    [NODE_PATCH]: () => json({ code: "conflict" }, 409),
  })
  const user = userEvent.setup()
  mount({ nodeId: 7, nodeName: "Launch" })

  await user.click(
    await screen.findByRole("button", { name: /Not in a project/ })
  )
  await user.click(await screen.findByRole("button", { name: "Move here" }))
  expect(await screen.findByRole("alert")).toBeVisible()
  // Still offered, still unfiled — the UI does not pretend the move happened.
  expect(
    screen.getByRole("button", { name: /Not in a project \(1\)/ })
  ).toBeVisible()
})

const CONNECTED = {
  runtimeEnabled: true,
  connections: [
    {
      provider: "claude",
      state: "connected",
      accountLabel: "sagar@example.test",
      checkedAt: 1_700_000_000,
      errorCode: null,
    },
    {
      provider: "codex",
      state: "not_signed_in",
      accountLabel: null,
      checkedAt: null,
      errorCode: null,
    },
  ],
}

function composer(project = { nodeId: 7, nodeName: "Launch" }) {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentChats
          nodeId={project.nodeId}
          nodeName={project.nodeName}
          draftProvider="claude"
          onSessionCreated={vi.fn()}
          onDraftCancelled={vi.fn()}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

it("creates the session from the first message, titled by what was typed", async () => {
  // The user came to type, not to name something: no dialog, no title field.
  let created: Record<string, unknown> | null = null
  let started: Record<string, unknown> | null = null
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () => json(CONNECTED),
    "POST /api/neev/agent/sessions": (init) => {
      created = JSON.parse(init.body as string)
      return json({ ...session, nodeId: 7 }, 201)
    },
    [`POST /api/neev/agent/sessions/${SESSION_ID}/turns`]: (init) => {
      started = JSON.parse(init.body as string)
      return json({ turn: runningTurn, runId: RUN_ID }, 201)
    },
  })
  const user = userEvent.setup()
  composer()

  expect(screen.queryByRole("textbox", { name: "Title" })).toBeNull()
  const box = await screen.findByRole("textbox", { name: "Message" })
  await waitFor(() => expect(composerEditable(box)).toBe(true))
  await typeInComposer(box, "Explain the parser to me")
  await user.click(screen.getByRole("button", { name: "Send" }))

  await waitFor(() =>
    expect(started).toEqual({ prompt: "Explain the parser to me" })
  )
  // Filed under the project it was started in, titled from the message, and
  // on the provider's own default model.
  expect(created).toMatchObject({
    title: "Explain the parser to me",
    nodeId: 7,
    provider: "claude",
    model: "default",
  })
})

it("will not let a disconnected provider be typed into", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () => json(CONNECTED),
  })
  const user = userEvent.setup()
  composer()

  // Claude is connected, so switching to Codex must say so rather than
  // accepting a message nothing can answer.
  await waitFor(() =>
    expect(
      composerEditable(screen.getByRole("textbox", { name: "Message" }))
    ).toBe(true)
  )
  // Nothing has been created yet, so picking a model under another provider's
  // heading re-points the whole draft rather than editing a session.
  await user.click(screen.getByRole("button", { name: "Model" }))
  await user.click(screen.getByRole("menuitem", { name: "More models" }))
  const models = await screen.findByRole("menu", { name: "More models" })
  // `fireEvent` for a submenu row: Radix holds a submenu open against the
  // trigger's geometry, and every rect is zero in jsdom.
  fireEvent.click(
    within(within(models).getByRole("group", { name: "Codex" })).getByRole(
      "menuitemradio",
      { name: "Default" }
    )
  )

  expect(await screen.findByText("codex is not connected yet.")).toBeVisible()
  expect(
    composerEditable(screen.getByRole("textbox", { name: "Message" }))
  ).toBe(false)
  // The way out is the real connection flow, never a fake one.
  expect(screen.getByRole("button", { name: "Connect" })).toBeEnabled()
})

async function failingFirstTurn(turns: unknown[]) {
  let creates = 0
  const onCreated = vi.fn()
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () => json(CONNECTED),
    "POST /api/neev/agent/sessions": () => {
      creates += 1
      return json({ ...session, nodeId: 7 }, 201)
    },
    [`POST /api/neev/agent/sessions/${SESSION_ID}/turns`]: () =>
      json({ code: "unavailable" }, 503),
    // What the server says actually happened.
    [`GET /api/neev/agent/sessions/${SESSION_ID}/turns`]: () =>
      json({ items: turns, nextAfter: null }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentChats
          nodeId={7}
          nodeName="Launch"
          draftProvider="claude"
          onSessionCreated={onCreated}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
  const box = await screen.findByRole("textbox", { name: "Message" })
  await waitFor(() => expect(composerEditable(box)).toBe(true))
  await typeInComposer(box, "Explain the parser")
  await user.click(screen.getByRole("button", { name: "Send" }))
  return { onCreated, creates: () => creates, box }
}

it("keeps the message when the first turn left no trace", async () => {
  // The chat exists but nothing was recorded. Discarding the words the user
  // typed and showing an empty transcript would explain nothing.
  const { onCreated, creates, box } = await failingFirstTurn([])

  expect(await screen.findByRole("alert")).toBeVisible()
  expect(
    screen.getByText(/The chat was created but this message was not sent/)
  ).toBeVisible()
  expect(composerText(box)).toBe("Explain the parser")
  // Not resent, and certainly not created again.
  expect(onCreated).not.toHaveBeenCalled()
  expect(creates()).toBe(1)
})

it("hands over when the first turn actually ran", async () => {
  // A lost response is not a failed request: the prompt is in the history, so
  // the work was done and resending it would do it twice.
  const { onCreated, creates } = await failingFirstTurn([
    { ...runningTurn, prompt: "Explain the parser" },
  ])

  await waitFor(() => expect(onCreated).toHaveBeenCalledTimes(1))
  expect(creates()).toBe(1)
})

it("says a missing chat is missing instead of loading forever", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () => json(CONNECTED),
  })
  render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentChats nodeId={7} nodeName="Launch" sessionId={SESSION_ID} />
      </NativeBoundary>
    </NativeTestRoot>
  )
  expect(
    await screen.findByText("That chat is no longer in this project.")
  ).toBeVisible()
  expect(screen.getByRole("button", { name: "Try again" })).toBeEnabled()
})

/**
 * The clock and the duration, tested on the timeline itself.
 *
 * Driven through a stubbed `performance.now` rather than the fake timer's own
 * clock: the count is deliberately NOT read off the wall clock once it starts,
 * so advancing timers alone would leave it frozen — which is the property being
 * asserted, not a limitation of the test.
 */
function renderTimeline(props: Parameters<typeof ActivityTimeline>[0]) {
  // Translations and nothing else: the timeline reads no session, and mounting
  // the auth provider around it would only add a fetch to every assertion.
  return render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <ActivityTimeline {...props} />
    </NextIntlClientProvider>
  )
}

it("counts a running turn in tenths, and stops counting when it ends", async () => {
  vi.useFakeTimers()
  vi.setSystemTime(1_700_000_000_000)
  let elapsed = 0
  vi.spyOn(performance, "now").mockImplementation(() => elapsed)

  // The turn began 1.2s before this was rendered — a reattach mid-run — so the
  // clock starts where the turn is, not at zero.
  const { rerender } = renderTimeline({
    items: [],
    running: true,
    startedAt: 1_700_000_000_000 - 1_200,
  })
  expect(screen.getByText("1.2s")).toBeVisible()

  act(() => {
    elapsed = 900
    vi.advanceTimersByTime(900)
  })
  expect(screen.getByText("2.1s")).toBeVisible()

  // A clock set backwards mid-run must not drag the count back with it: the
  // count advances on `performance.now`, which cannot be set.
  act(() => {
    vi.setSystemTime(1_700_000_000_000 - 60_000)
    elapsed = 1_400
    vi.advanceTimersByTime(500)
  })
  expect(screen.getByText("2.6s")).toBeVisible()

  // Finished: the live clock is gone, and what is left is the recorded
  // duration — whole seconds, because that is all the timestamps hold.
  rerender(
    <NextIntlClientProvider locale="en" messages={messages}>
      <ActivityTimeline
        items={[]}
        running={false}
        completed
        startedAt={1_700_000_000_000}
        finishedAt={1_700_000_003_000}
      />
    </NextIntlClientProvider>
  )
  expect(screen.queryByText("2.6s")).toBeNull()
  expect(screen.getByText("3s")).toBeVisible()

  // And nothing is still ticking behind it.
  act(() => {
    elapsed = 30_000
    vi.advanceTimersByTime(30_000)
  })
  expect(screen.getByText("3s")).toBeVisible()
})

it("keeps the duration of a turn that finished without running a tool", () => {
  renderTimeline({
    items: [],
    running: false,
    completed: true,
    startedAt: 1_700_000_000_000,
    finishedAt: 1_700_000_012_000,
  })
  // The answer came straight back. It still took twelve seconds, and it still
  // finished — but there is no trace, so there is no count to report, and
  // "0 tool calls, 0 results" would be describing an absence as a finding.
  expect(screen.getByText("Completed")).toBeVisible()
  expect(screen.getByText("12s")).toBeVisible()
  expect(screen.queryByText(/tool call/)).toBeNull()
})

it("says nothing at all about a turn with no trace, no outcome and no time", () => {
  const { container } = renderTimeline({
    items: [],
    running: false,
    startedAt: null,
  })
  expect(container.querySelector("button")).toBeNull()
})
