import {
  act,
  configure,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeKnowledge } from "./native-knowledge"
import { NativeKnowledgePages } from "./native-knowledge-pages"
import { NativeSuspendedSurface } from "./native-suspended-surface"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  orgSession,
} from "./test-helpers"

const router = vi.hoisted(() => ({ replace: vi.fn(), push: vi.fn() }))
vi.mock("next/navigation", () => ({ useRouter: () => router }))
configure({ asyncUtilTimeout: 5000 })
afterEach(() => {
  vi.unstubAllGlobals()
  vi.useRealTimers()
  localStorage.clear()
  router.push.mockReset()
})

const status = (over: Record<string, unknown> = {}) => ({
  captureEnabled: false,
  eligible: 3,
  captured: 2,
  represented: 1,
  stale: 1,
  pages: 2,
  withheldPages: 1,
  sourceProblems: { tooLarge: 0, engineUnavailable: 1, failed: 0 },
  coverage: {
    units: 30,
    cited: 10,
    skippedByAgent: 1,
    uncovered: 2,
    pending: 17,
  },
  captureBlocked: null,
  bytesUsed: 4096,
  job: null,
  ...over,
})
const job = (over: Record<string, unknown> = {}) => ({
  id: "j1",
  mode: "incremental",
  state: "running",
  provider: "claude",
  model: "sonnet",
  pauseReason: null,
  waitReason: null,
  sessionId: null,
  batchesDone: 1,
  unitsRemaining: 13,
  startedBy: 1,
  createdAt: 1,
  updatedAt: 2,
  ...over,
})
/** This admin's own connections: Claude connected, Codex not. */
const connections = () =>
  json({
    runtimeEnabled: true,
    connections: [
      {
        provider: "claude",
        state: "connected",
        accountLabel: "Pro",
        checkedAt: 1,
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
  })
/**
 * Advance the poll clock until `check` holds, one interval at a time.
 *
 * React registers the poller in an effect after the render a test awaited, and
 * under load that can land after a single up-front advance — the poll then
 * never fires and the test waits for nothing. Advancing inside the wait makes
 * the test independent of when the effect ran.
 */
async function tickUntil(check: () => void) {
  // Bounded: at most 20 intervals, each followed by a flush of pending work.
  for (let attempt = 0; ; attempt += 1) {
    await act(async () => {
      vi.advanceTimersByTime(5_100)
    })
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10))
    })
    try {
      check()
      return
    } catch (failure) {
      if (attempt >= 20) throw failure
    }
  }
}
const shell = (ui: React.ReactNode) =>
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>{ui}</NativeSuspendedSurface>
    </NativeTestRoot>
  )

it("toggles capture with the revision it read and states exactly what is captured", async () => {
  const puts: unknown[] = []
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => json(status()),
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: false, revision: 3 }),
    "PUT /api/neev/knowledge/settings": (init) => {
      puts.push(JSON.parse(init.body as string))
      return json({ captureEnabled: true, revision: 4 })
    },
    "GET /api/neev/agent/connections": connections,
  })
  shell(<NativeKnowledge />)
  const toggle = await screen.findByRole("switch", {
    name: "Capture shared knowledge",
  })
  expect(toggle).toHaveAttribute("aria-checked", "false")
  await userEvent.setup().click(toggle)
  expect(puts).toEqual([{ captureEnabled: true, expectedRevision: 3 }])
  await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "true"))
  expect(
    screen.getByText(
      "Captures text from documents and team chats shared with your organization. Private items and private agent chats are never captured."
    )
  ).toBeVisible()
  expect(
    screen.getByText(
      "Files in chats are listed by name; their contents are not captured."
    )
  ).toBeVisible()
  // Saved only: nothing suggests members or agents receive it.
  expect(document.body.textContent).not.toMatch(
    /published|available to agents|shared with members/i
  )
  expect(mock.unknown).toEqual([])
})

it("re-reads the setting instead of overwriting a concurrent change", async () => {
  let reads = 0
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => json(status()),
    "GET /api/neev/knowledge/settings": () => {
      reads += 1
      return json({ captureEnabled: reads > 1, revision: reads > 1 ? 5 : 3 })
    },
    "PUT /api/neev/knowledge/settings": () => json({ code: "conflict" }, 409),
    "GET /api/neev/agent/connections": connections,
  })
  shell(<NativeKnowledge />)
  const toggle = await screen.findByRole("switch", {
    name: "Capture shared knowledge",
  })
  await userEvent.setup().click(toggle)
  expect(
    await screen.findByText(
      "Someone else changed this setting. It now shows the current value."
    )
  ).toBeVisible()
  await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "true"))
  expect(reads).toBe(2)
})

it("shows incomplete coverage, withheld pages, staleness and source problems", async () => {
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () =>
      json(status({ captureBlocked: "quota_exceeded" })),
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
  })
  shell(<NativeKnowledge />)
  expect(
    await screen.findByText(
      "Incomplete: 1 of 3 shared sources are fully represented."
    )
  ).toBeVisible()
  expect(screen.getByText("2 units are uncovered (gaps)")).toBeVisible()
  expect(screen.getByText("1 page withheld")).toBeVisible()
  expect(
    screen.getByText("1 source changed since it was compiled")
  ).toBeVisible()
  expect(
    screen.getByText(/1 La Suite document could not be read right now/)
  ).toBeVisible()
  expect(screen.getByRole("alert")).toHaveTextContent(
    "Capture is paused: this organization's knowledge storage is full."
  )
})

it("compiles only with the admin's own connected agent and a model from its catalogue", async () => {
  const posts: unknown[] = []
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () =>
      json(status({ captureEnabled: true })),
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
    "POST /api/neev/knowledge/jobs": (init) => {
      posts.push(JSON.parse(init.body as string))
      return json(job())
    },
  })
  shell(<NativeKnowledge />)
  const user = userEvent.setup()
  const agent = (await screen.findByRole("combobox", {
    name: "Agent",
  })) as HTMLSelectElement
  // Codex is offered by the server but not connected for this admin.
  expect(Array.from(agent.options).map((o) => o.value)).toEqual(["claude"])
  const model = screen.getByRole("combobox", {
    name: "Model",
  }) as HTMLSelectElement
  await waitFor(() =>
    expect(Array.from(model.options).map((o) => o.value)).toEqual([
      "default",
      "haiku",
      "sonnet",
      "opus",
    ])
  )
  await user.selectOptions(model, "sonnet")
  expect(
    screen.getByText(
      /Copies kept by the provider are outside Neev and cannot be recalled\./
    )
  ).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Start compile" }))
  expect(posts).toEqual([
    { mode: "incremental", provider: "claude", model: "sonnet" },
  ])
  expect(await screen.findByText("About 2 batches remaining")).toBeVisible()
})

it("cannot start a compile without a connected agent of the admin's own", async () => {
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () =>
      json(status({ captureEnabled: true })),
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    // The default: no runtime, nothing connected.
  })
  shell(<NativeKnowledge />)
  expect(
    await screen.findByText(
      /Connect your own agent under Personal agents to compile/
    )
  ).toBeVisible()
  expect(screen.getByRole("button", { name: "Start compile" })).toBeDisabled()
})

it("pauses, resumes and stops the running job", async () => {
  const calls: string[] = []
  let state = "running"
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () =>
      json(status({ job: job({ state }) })),
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
    "POST /api/neev/knowledge/jobs/j1/pause": () => {
      calls.push("pause")
      state = "paused"
      return json(job({ state, pauseReason: "manual" }))
    },
    "POST /api/neev/knowledge/jobs/j1/resume": () => {
      calls.push("resume")
      state = "running"
      return json(job({ state }))
    },
    "POST /api/neev/knowledge/jobs/j1/stop": () => {
      calls.push("stop")
      state = "stopped"
      return json(job({ state }))
    },
  })
  shell(<NativeKnowledge />)
  const user = userEvent.setup()
  await user.click(await screen.findByRole("button", { name: "Pause" }))
  expect(await screen.findByText("Paused by an admin")).toBeVisible()
  await user.click(await screen.findByRole("button", { name: "Resume" }))
  await user.click(await screen.findByRole("button", { name: "Stop" }))
  expect(calls).toEqual(["pause", "resume", "stop"])
})

it("says a job is waiting for a free slot, and offers to retry gaps when it ends with gaps", async () => {
  let current = job({ waitReason: "slot" })
  const resets: string[] = []
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => json(status({ job: current })),
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
    "POST /api/neev/knowledge/uncovered/reset": () => {
      resets.push("reset")
      return json({})
    },
  })
  const view = shell(<NativeKnowledge />)
  expect(await screen.findByText("Waiting for a free agent slot")).toBeVisible()
  view.unmount()

  current = job({ state: "completed_with_gaps" })
  shell(<NativeKnowledge />)
  expect(
    await screen.findByText(
      "Finished with gaps: some source text is not yet represented in any page."
    )
  ).toBeVisible()
  await userEvent
    .setup()
    .click(screen.getByRole("button", { name: "Retry uncovered" }))
  await waitFor(() => expect(resets).toEqual(["reset"]))
})

it("polls only while a job is running, and stops when it leaves", async () => {
  // Only the poller's clock: testing-library's own waits keep real timers.
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  let reads = 0
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => {
      reads += 1
      return json(status({ job: job() }))
    },
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
  })
  const view = shell(<NativeKnowledge />)
  await screen.findByRole("button", { name: "Pause" })
  const first = reads
  await tickUntil(() => expect(reads).toBe(first + 1))
  view.unmount()
  await act(async () => {
    vi.advanceTimersByTime(20_000)
  })
  expect(reads).toBe(first + 1)
})

/**
 * Review M3. The job's session is hidden from session lists (A1), and it is
 * not yet confirmed that the workspace can open an unlisted session by id. No
 * button is offered that could open a tab which then errors.
 */
it("offers no compile-chat link until the workspace can open it", async () => {
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () =>
      json(
        status({
          job: job({ sessionId: "5f0c1b9e-1111-4222-8333-444455556666" }),
        })
      ),
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
  })
  shell(<NativeKnowledge />)
  await screen.findByRole("button", { name: "Pause" })
  expect(screen.queryByRole("button", { name: "Open compile chat" })).toBeNull()
})

// ---------------------------------------------------------------------------
// Pages, preview, log and export.

const pagesRoutes = () => ({
  "GET /api/neev/knowledge/pages": () =>
    json([
      {
        id: "p1",
        status: "withheld",
        revision: 2,
        unreadableSources: 1,
        updatedAt: 1,
      },
      {
        id: "p2",
        status: "readable",
        slug: "beta",
        title: "Beta",
        revision: 1,
        stale: true,
        lint: ["too_long"],
        updatedAt: 1,
      },
      {
        id: "p3",
        status: "readable",
        slug: "gamma",
        title: "Gamma",
        revision: 1,
        stale: false,
        lint: [],
        updatedAt: 1,
      },
    ]),
  "GET /api/neev/knowledge/pages/p1": () =>
    json({ id: "p1", revision: 2, status: "withheld", unreadableSources: 1 }),
  "GET /api/neev/knowledge/pages/p2": () =>
    json({
      id: "p2",
      slug: "beta",
      title: "Beta",
      revision: 1,
      status: "readable",
      markdown:
        "# Beta\n\nShip on Fridays.[^k-0123456789ab] Old claim.[^k-ffffffffffff] See [[gamma]].",
      stale: true,
      lint: ["too_long"],
      citations: [
        {
          key: "k-0123456789ab",
          withheld: false,
          sourceId: "5f0c1b9e-aaaa-4bbb-8ccc-ddddeeeeffff",
          title: "Release notes",
          kind: "document",
          nodeId: 7,
          segment: 1,
        },
        { key: "k-ffffffffffff", withheld: true },
      ],
    }),
  "GET /api/neev/knowledge/pages/p3": () =>
    json({
      id: "p3",
      slug: "gamma",
      title: "Gamma",
      revision: 1,
      status: "readable",
      markdown: "Gamma body",
      stale: false,
      lint: [],
      citations: [],
    }),
  "GET /api/neev/knowledge/log": () =>
    json(
      Array.from({ length: 100 }, (_, n) => ({
        seq: 200 - n,
        at: 1,
        event: "batch_accepted",
        actorId: 1,
        pageId: null,
        pageTitle: null,
        detail: "pages=1",
      }))
    ),
  "GET /api/neev/knowledge/log?before=101": () =>
    json([
      {
        seq: 100,
        at: 1,
        event: "capture_enabled",
        actorId: 1,
        pageId: null,
        pageTitle: null,
        detail: "",
      },
    ]),
})

it("shows a withheld page as opaque: no title, no body, only the count", async () => {
  mockNativeSession(orgSession, pagesRoutes())
  shell(<NativeKnowledgePages />)
  const user = userEvent.setup()
  const list = await screen.findByRole("list", { name: "Pages" })
  const withheld = within(list).getByRole("button", { name: /Withheld page/ })
  await user.click(withheld)
  expect(
    await screen.findByText("Withheld: 1 source is no longer shared")
  ).toBeVisible()
  expect(screen.queryByRole("article")).not.toBeInTheDocument()
})

it("previews a readable page with source links, withheld citations and page links", async () => {
  const mock = mockNativeSession(orgSession, pagesRoutes())
  shell(<NativeKnowledgePages />)
  const user = userEvent.setup()
  const list = await screen.findByRole("list", { name: "Pages" })
  // Chips say what is wrong with a page before it is opened.
  const beta = within(list).getByRole("button", { name: /Beta/ })
  expect(beta).toHaveTextContent("Stale")
  expect(beta).toHaveTextContent("1 lint issue")
  await user.click(beta)
  const article = await screen.findByRole("article")
  const source = within(article).getByRole("link", { name: "Release notes" })
  // A source that is no longer readable is cited without naming it.
  expect(article).toHaveTextContent("[withheld]")
  expect(article).not.toHaveTextContent("k-ffffffffffff")

  await user.click(source)
  expect(router.push).toHaveBeenCalledWith("/workspace")
  expect(localStorage.getItem("neev.ws.tabs.1:3")).toContain(
    "5f0c1b9e-aaaa-4bbb-8ccc-ddddeeeeffff"
  )

  await user.click(within(article).getByRole("link", { name: "gamma" }))
  expect(await screen.findByText("Gamma body")).toBeVisible()
  expect(screen.getByRole("link", { name: "Export" })).toHaveAttribute(
    "href",
    "/api/neev/knowledge/export"
  )
  expect(mock.unknown).toEqual([])
})

it("shows a page a test answer links to, and re-reads it when asked again", async () => {
  const mock = mockNativeSession(orgSession, pagesRoutes())
  const view = shell(<NativeKnowledgePages focus={{ id: "p3", nonce: 1 }} />)
  expect(await screen.findByText("Gamma body")).toBeVisible()
  const reads = () =>
    mock.fetcher.mock.calls.filter(
      ([path]) => path === "/api/neev/knowledge/pages/p3"
    ).length
  const before = reads()
  view.rerender(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeKnowledgePages focus={{ id: "p3", nonce: 2 }} />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  await waitFor(() => expect(reads()).toBe(before + 1))
})

it("shows a linked page as withheld if it has been withheld since the answer", async () => {
  mockNativeSession(orgSession, pagesRoutes())
  shell(<NativeKnowledgePages focus={{ id: "p1", nonce: 1 }} />)
  expect(
    await screen.findByText("Withheld: 1 source is no longer shared")
  ).toBeVisible()
  expect(screen.queryByRole("article")).not.toBeInTheDocument()
})

it("keeps the log collapsed, and loads earlier entries from the last one shown", async () => {
  mockNativeSession(orgSession, pagesRoutes())
  shell(<NativeKnowledgePages />)
  const user = userEvent.setup()
  // Collapsed by default: the section stays compact until asked for.
  const summary = await screen.findByText("Log")
  const details = summary.closest("details") as HTMLDetailsElement
  expect(details.open).toBe(false)
  await user.click(summary)
  expect(details.open).toBe(true)
  await user.click(await screen.findByRole("button", { name: "Load earlier" }))
  expect(await screen.findByText("capture_enabled")).toBeVisible()
  expect(screen.queryByRole("button", { name: "Load earlier" })).toBeNull()
})

// ---------------------------------------------------------------------------
// Review fixes.

/**
 * I1. "Complete" only when nothing at all is outstanding. `represented` alone
 * can be satisfied vacuously by a source with no snapshot, so every other gap
 * has to be clear too.
 */
const whole = {
  eligible: 3,
  captured: 3,
  represented: 3,
  stale: 0,
  sourceProblems: { tooLarge: 0, engineUnavailable: 0, failed: 0 },
  coverage: {
    units: 30,
    cited: 30,
    skippedByAgent: 0,
    uncovered: 0,
    pending: 0,
  },
  captureBlocked: null,
}
it.each([
  ["units still pending", { coverage: { ...whole.coverage, pending: 1 } }],
  ["uncovered gaps", { coverage: { ...whole.coverage, uncovered: 1 } }],
  ["sources not yet captured", { captured: 2 }],
  [
    "a too-large source",
    { sourceProblems: { ...whole.sourceProblems, tooLarge: 1 } },
  ],
  [
    "an unreadable La Suite document",
    { sourceProblems: { ...whole.sourceProblems, engineUnavailable: 1 } },
  ],
  [
    "a failed capture",
    { sourceProblems: { ...whole.sourceProblems, failed: 1 } },
  ],
  ["capture blocked by quota", { captureBlocked: "quota_exceeded" }],
])("never claims completeness with %s", async (_label, over) => {
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => json(status({ ...whole, ...over })),
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
  })
  shell(<NativeKnowledge />)
  expect(await screen.findByText(/^Incomplete:/)).toBeVisible()
  expect(
    screen.queryByText("Every shared source is fully represented.")
  ).toBeNull()
})

it("says complete when nothing is outstanding", async () => {
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => json(status(whole)),
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
  })
  shell(<NativeKnowledge />)
  expect(
    await screen.findByText("Every shared source is fully represented.")
  ).toBeVisible()
})

/**
 * I2. A poll that started before a Pause must not put the job back to
 * "running" when it lands after it.
 */
it("never lets an older poll undo a newer pause", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  let reads = 0
  let releasePoll: (() => void) | undefined
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => {
      reads += 1
      if (reads === 1) return json(status({ job: job() }))
      return new Promise<Response>((resolve) => {
        releasePoll = () => resolve(json(status({ job: job() })))
      }) as unknown as Response
    },
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
    "POST /api/neev/knowledge/jobs/j1/pause": () =>
      json(job({ state: "paused", pauseReason: "manual", updatedAt: 3 })),
  })
  shell(<NativeKnowledge />)
  const user = userEvent.setup()
  const pause = await screen.findByRole("button", { name: "Pause" })
  // The poll goes out and is held.
  await tickUntil(() => expect(releasePoll).toBeDefined())
  await user.click(pause)
  expect(await screen.findByRole("button", { name: "Resume" })).toBeVisible()
  // The older answer arrives last.
  await act(async () => {
    releasePoll?.()
  })
  await act(async () => {
    await Promise.resolve()
  })
  expect(screen.getByRole("button", { name: "Resume" })).toBeVisible()
  expect(screen.queryByRole("button", { name: "Pause" })).toBeNull()
})

it("tells the pages to refresh while a job runs", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  const changed = vi.fn()
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => json(status({ job: job() })),
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
  })
  shell(<NativeKnowledge onPagesChanged={changed} />)
  await screen.findByRole("button", { name: "Pause" })
  const before = changed.mock.calls.length
  await tickUntil(() =>
    expect(changed.mock.calls.length).toBeGreaterThan(before)
  )
})

/** I3. A page whose source was revoked must stop showing its body. */
it("re-reads the open page when it is chosen again, and drops a now-withheld body", async () => {
  let revoked = false
  mockNativeSession(orgSession, {
    ...pagesRoutes(),
    "GET /api/neev/knowledge/pages/p3": () =>
      json(
        revoked
          ? { id: "p3", revision: 2, status: "withheld", unreadableSources: 1 }
          : {
              id: "p3",
              slug: "gamma",
              title: "Gamma",
              revision: 1,
              status: "readable",
              markdown: "Gamma body",
              stale: false,
              lint: [],
              citations: [],
            }
      ),
  })
  shell(<NativeKnowledgePages />)
  const user = userEvent.setup()
  const list = await screen.findByRole("list", { name: "Pages" })
  await user.click(within(list).getByRole("button", { name: /Gamma/ }))
  expect(await screen.findByText("Gamma body")).toBeVisible()
  revoked = true
  await user.click(within(list).getByRole("button", { name: /Gamma/ }))
  expect(
    await screen.findByText("Withheld: 1 source is no longer shared")
  ).toBeVisible()
  expect(screen.queryByText("Gamma body")).toBeNull()
  expect(screen.queryByRole("article")).toBeNull()
})

it("reloads the list and the open page when told the pages changed", async () => {
  let round = 0
  const routes = pagesRoutes()
  mockNativeSession(orgSession, {
    ...routes,
    "GET /api/neev/knowledge/pages": () =>
      round === 0
        ? routes["GET /api/neev/knowledge/pages"]()
        : json([
            {
              id: "p3",
              status: "withheld",
              revision: 2,
              unreadableSources: 1,
              updatedAt: 2,
            },
            {
              id: "p4",
              status: "readable",
              slug: "delta",
              title: "Delta",
              revision: 1,
              stale: false,
              lint: [],
              updatedAt: 2,
            },
          ]),
    "GET /api/neev/knowledge/pages/p3": () =>
      json(
        round === 0
          ? {
              id: "p3",
              slug: "gamma",
              title: "Gamma",
              revision: 1,
              status: "readable",
              markdown: "Gamma body",
              stale: false,
              lint: [],
              citations: [],
            }
          : { id: "p3", revision: 2, status: "withheld", unreadableSources: 1 }
      ),
  })
  const view = render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeKnowledgePages version={0} />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  const user = userEvent.setup()
  await user.click(
    within(await screen.findByRole("list", { name: "Pages" })).getByRole(
      "button",
      { name: /Gamma/ }
    )
  )
  expect(await screen.findByText("Gamma body")).toBeVisible()
  round = 1
  view.rerender(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeKnowledgePages version={1} />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  expect(await screen.findByRole("button", { name: /Delta/ })).toBeVisible()
  expect(
    await screen.findByText("Withheld: 1 source is no longer shared")
  ).toBeVisible()
  expect(screen.queryByText("Gamma body")).toBeNull()
})

it("refreshes the pages on request", async () => {
  let round = 0
  const routes = pagesRoutes()
  mockNativeSession(orgSession, {
    ...routes,
    "GET /api/neev/knowledge/pages": () =>
      round === 0 ? json([]) : routes["GET /api/neev/knowledge/pages"](),
  })
  shell(<NativeKnowledgePages />)
  expect(await screen.findByText("No pages yet.")).toBeVisible()
  round = 1
  await userEvent.setup().click(screen.getByRole("button", { name: "Refresh" }))
  expect(await screen.findByRole("button", { name: /Beta/ })).toBeVisible()
})

/** M1. Gaps can be retried whenever there are some and nothing is running. */
it("offers to retry gaps without a job that ended with gaps", async () => {
  const resets: string[] = []
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () =>
      json(status({ job: job({ state: "stopped" }) })),
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
    "POST /api/neev/knowledge/uncovered/reset": () => {
      resets.push("reset")
      return json({})
    },
  })
  shell(<NativeKnowledge />)
  await userEvent
    .setup()
    .click(await screen.findByRole("button", { name: "Retry uncovered" }))
  await waitFor(() => expect(resets).toEqual(["reset"]))
})

/** M2. Only the providers the knowledge runner supports, until Task 9 says more. */
it("offers only supported providers among the admin's connections", async () => {
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => json(status()),
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [
          {
            provider: "claude",
            state: "connected",
            accountLabel: null,
            checkedAt: 1,
            errorCode: null,
          },
          {
            provider: "codex",
            state: "connected",
            accountLabel: null,
            checkedAt: 1,
            errorCode: null,
          },
          {
            provider: "opencode",
            state: "connected",
            accountLabel: null,
            checkedAt: 1,
            errorCode: null,
          },
        ],
      }),
  })
  shell(<NativeKnowledge />)
  const agent = (await screen.findByRole("combobox", {
    name: "Agent",
  })) as HTMLSelectElement
  await waitFor(() =>
    expect(Array.from(agent.options).map((o) => o.value)).toEqual([
      "claude",
      "codex",
    ])
  )
})

/** M5. A failed read of connections is not "you have none". */
it("says the connections could not be loaded, rather than that there are none", async () => {
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => json(status()),
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": () => json({ code: "unavailable" }, 503),
  })
  shell(<NativeKnowledge />)
  expect(
    await screen.findByText(
      "Your agent connections could not be loaded. Try again later."
    )
  ).toBeVisible()
  expect(screen.queryByText(/Connect your own agent/)).toBeNull()
  expect(screen.getByRole("button", { name: "Start compile" })).toBeDisabled()
})

/** M4. One request per click on "Load earlier", however fast the clicks. */
it("loads earlier entries once, even when clicked twice", async () => {
  let earlier = 0
  let release: (() => void) | undefined
  const routes = pagesRoutes()
  mockNativeSession(orgSession, {
    ...routes,
    "GET /api/neev/knowledge/log?before=101": () => {
      earlier += 1
      return new Promise<Response>((resolve) => {
        release = () =>
          resolve(routes["GET /api/neev/knowledge/log?before=101"]())
      }) as unknown as Response
    },
  })
  shell(<NativeKnowledgePages />)
  const user = userEvent.setup()
  await user.click(await screen.findByText("Log"))
  const more = await screen.findByRole("button", { name: "Load earlier" })
  await user.click(more)
  await user.click(more)
  await act(async () => {
    release?.()
  })
  expect(await screen.findByText("capture_enabled")).toBeVisible()
  expect(earlier).toBe(1)
})

/** M6. A refused export says so; nothing is saved as a file. */
it("reports a refused export instead of saving the error as a file", async () => {
  const created = vi.fn(() => "blob:x")
  vi.stubGlobal(
    "URL",
    Object.assign(URL, { createObjectURL: created, revokeObjectURL: vi.fn() })
  )
  mockNativeSession(orgSession, {
    ...pagesRoutes(),
    "GET /api/neev/knowledge/export": () => json({ code: "forbidden" }, 403),
  })
  shell(<NativeKnowledgePages />)
  await userEvent
    .setup()
    .click(await screen.findByRole("link", { name: "Export" }))
  expect(
    await screen.findByText("The export could not be downloaded.")
  ).toBeVisible()
  expect(created).not.toHaveBeenCalled()
})

it("downloads a successful export as a file", async () => {
  const created = vi.fn<(blob: Blob) => string>(() => "blob:x")
  vi.stubGlobal(
    "URL",
    Object.assign(URL, { createObjectURL: created, revokeObjectURL: vi.fn() })
  )
  mockNativeSession(orgSession, {
    ...pagesRoutes(),
    "GET /api/neev/knowledge/export": () =>
      new Response(new Blob(["zip"]), {
        status: 200,
        headers: { "Content-Type": "application/zip" },
      }),
  })
  shell(<NativeKnowledgePages />)
  await userEvent
    .setup()
    .click(await screen.findByRole("link", { name: "Export" }))
  await waitFor(() => expect(created).toHaveBeenCalledTimes(1))
  expect(screen.queryByText("The export could not be downloaded.")).toBeNull()
})

// ---------------------------------------------------------------------------
// Re-review nits.

/**
 * The download starts from the click; revoking its URL in the same tick can
 * cancel it in some browsers. The URL is revoked later, never before.
 */
it("revokes the export's object URL only after the download has started", async () => {
  const created = vi.fn<(blob: Blob) => string>(() => "blob:x")
  const revoked = vi.fn<(url: string) => void>()
  vi.stubGlobal(
    "URL",
    Object.assign(URL, { createObjectURL: created, revokeObjectURL: revoked })
  )
  const clicked: { download: string; revokedYet: boolean }[] = []
  const click = vi
    .spyOn(HTMLAnchorElement.prototype, "click")
    .mockImplementation(function (this: HTMLAnchorElement) {
      clicked.push({
        download: this.download,
        revokedYet: revoked.mock.calls.length > 0,
      })
    })
  mockNativeSession(orgSession, {
    ...pagesRoutes(),
    "GET /api/neev/knowledge/export": () =>
      new Response(new Blob(["zip"]), {
        status: 200,
        headers: {
          "Content-Type": "application/zip",
          "Content-Disposition":
            'attachment; filename="knowledge-3-1700000000.zip"',
        },
      }),
  })
  shell(<NativeKnowledgePages />)
  await userEvent
    .setup()
    .click(await screen.findByRole("link", { name: "Export" }))
  await waitFor(() => expect(clicked).toHaveLength(1))
  expect(clicked[0]).toEqual({
    download: "knowledge-3-1700000000.zip",
    revokedYet: false,
  })
  // Not in the same tick as the click…
  expect(revoked).not.toHaveBeenCalled()
  // …but it is released.
  await waitFor(() => expect(revoked).toHaveBeenCalledWith("blob:x"), {
    timeout: 4000,
  })
  click.mockRestore()
})

/**
 * A page that stops existing (a rebuild archived it) must not turn every poll
 * into another error banner: the selection is dropped, quietly, once.
 */
it("drops a page that no longer exists instead of re-reporting it on every poll", async () => {
  let gone = false
  let pageReads = 0
  mockNativeSession(orgSession, {
    ...pagesRoutes(),
    "GET /api/neev/knowledge/pages/p3": () => {
      pageReads += 1
      return gone
        ? json({ code: "not_found" }, 404)
        : json({
            id: "p3",
            slug: "gamma",
            title: "Gamma",
            revision: 1,
            status: "readable",
            markdown: "Gamma body",
            stale: false,
            lint: [],
            citations: [],
          })
    },
  })
  const tree = (version: number) => (
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeKnowledgePages version={version} />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  const view = render(tree(0))
  await userEvent
    .setup()
    .click(
      within(await screen.findByRole("list", { name: "Pages" })).getByRole(
        "button",
        { name: /Gamma/ }
      )
    )
  expect(await screen.findByText("Gamma body")).toBeVisible()
  gone = true
  view.rerender(tree(1))
  expect(await screen.findByText("Choose a page to preview it.")).toBeVisible()
  expect(screen.queryByText("Gamma body")).toBeNull()
  expect(screen.queryByRole("alert")).toBeNull()
  const reads = pageReads
  view.rerender(tree(2))
  await screen.findByRole("list", { name: "Pages" })
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 50))
  })
  expect(pageReads).toBe(reads)
  expect(screen.queryByRole("alert")).toBeNull()
})

it("clears a page error once the page reads again", async () => {
  let failing = true
  mockNativeSession(orgSession, {
    ...pagesRoutes(),
    "GET /api/neev/knowledge/pages/p3": () =>
      failing
        ? json({ code: "unavailable" }, 503)
        : json({
            id: "p3",
            slug: "gamma",
            title: "Gamma",
            revision: 1,
            status: "readable",
            markdown: "Gamma body",
            stale: false,
            lint: [],
            citations: [],
          }),
  })
  shell(<NativeKnowledgePages />)
  const user = userEvent.setup()
  const gamma = within(
    await screen.findByRole("list", { name: "Pages" })
  ).getByRole("button", { name: /Gamma/ })
  await user.click(gamma)
  expect(await screen.findByRole("alert")).toBeVisible()
  failing = false
  await user.click(gamma)
  expect(await screen.findByText("Gamma body")).toBeVisible()
  expect(screen.queryByRole("alert")).toBeNull()
})

// ---------------------------------------------------------------------------
// Live QA (2026-10-02): capture enabled, the server captured 3 sources, and
// Status said "Captured 0" until the section was reopened.

/** The number under a status label, e.g. "Captured". */
function stat(label: string) {
  const term = screen.getByText(label, { selector: "dt" })
  return term.nextElementSibling?.textContent
}

/** Advance only the poll clock, `ms` at a time, flushing after each step. */
async function advance(ms: number) {
  await act(async () => {
    vi.advanceTimersByTime(ms)
  })
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 10))
  })
}

it("follows capture as it catches up, then stops asking", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  let reads = 0
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => {
      reads += 1
      return json(
        status({
          captureEnabled: true,
          eligible: 3,
          captured: reads >= 2 ? 3 : 0,
        })
      )
    },
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
  })
  shell(<NativeKnowledge />)
  await screen.findByText("Captured", { selector: "dt" })
  expect(stat("Captured")).toBe("0")
  await tickUntil(() => expect(stat("Captured")).toBe("3"))
  // Caught up: no more reads, however long the panel stays open.
  const settled = reads
  for (let step = 0; step < 4; step += 1) await advance(30_000)
  expect(reads).toBe(settled)
})

it("does not poll when capture is off, or caught up with only a paused job", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  let reads = 0
  let current = status({ captureEnabled: false, eligible: 3, captured: 0 })
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => {
      reads += 1
      return json(current)
    },
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: false, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
  })
  const view = shell(<NativeKnowledge />)
  await screen.findByText("Captured", { selector: "dt" })
  for (let step = 0; step < 4; step += 1) await advance(30_000)
  expect(reads).toBe(1)
  view.unmount()

  reads = 0
  current = status({
    captureEnabled: true,
    eligible: 3,
    captured: 3,
    job: job({ state: "paused", pauseReason: "manual" }),
  })
  shell(<NativeKnowledge />)
  await screen.findByRole("button", { name: "Resume" })
  for (let step = 0; step < 4; step += 1) await advance(30_000)
  expect(reads).toBe(1)
})

it("stops following capture once every missing source is a known failure", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  let reads = 0
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => {
      reads += 1
      // 1 captured + 1 too large + 1 failed + 1 without an engine = 4 of 4.
      return json(
        status({
          captureEnabled: true,
          eligible: 4,
          captured: 1,
          sourceProblems: { tooLarge: 1, engineUnavailable: 1, failed: 1 },
        })
      )
    },
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
  })
  shell(<NativeKnowledge />)
  await screen.findByText("Captured", { selector: "dt" })
  for (let step = 0; step < 4; step += 1) await advance(30_000)
  expect(reads).toBe(1)
})

it("re-reads status right after capture is switched on", async () => {
  let reads = 0
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => {
      reads += 1
      return json(
        status({
          captureEnabled: reads > 1,
          eligible: 3,
          captured: reads > 1 ? 1 : 0,
        })
      )
    },
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: false, revision: 3 }),
    "PUT /api/neev/knowledge/settings": () =>
      json({ captureEnabled: true, revision: 4 }),
    "GET /api/neev/agent/connections": connections,
  })
  shell(<NativeKnowledge />)
  await userEvent
    .setup()
    .click(
      await screen.findByRole("switch", { name: "Capture shared knowledge" })
    )
  await waitFor(() => expect(stat("Captured")).toBe("1"))
  expect(reads).toBe(2)
})

it("re-reads status when asked to refresh", async () => {
  let reads = 0
  mockNativeSession(orgSession, {
    "GET /api/neev/knowledge/status": () => {
      reads += 1
      return json(status({ captured: reads > 1 ? 3 : 0 }))
    },
    "GET /api/neev/knowledge/settings": () =>
      json({ captureEnabled: false, revision: 3 }),
    "GET /api/neev/agent/connections": connections,
  })
  const tree = (signal: number) => (
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeKnowledge refreshSignal={signal} />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  const view = render(tree(0))
  await screen.findByText("Captured", { selector: "dt" })
  expect(reads).toBe(1)
  view.rerender(tree(1))
  await waitFor(() => expect(stat("Captured")).toBe("3"))
  expect(reads).toBe(2)
})

it("asks for a status refresh when the pages are refreshed", async () => {
  const refreshed = vi.fn()
  mockNativeSession(orgSession, pagesRoutes())
  shell(<NativeKnowledgePages onRefresh={refreshed} />)
  await userEvent
    .setup()
    .click(await screen.findByRole("button", { name: "Refresh" }))
  expect(refreshed).toHaveBeenCalledTimes(1)
})
