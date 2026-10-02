import {
  act,
  configure,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeKnowledgeAsk } from "./native-knowledge-ask"
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

const ID = "5f0c1b9e-1111-4222-8333-444455556666"
const ID2 = "5f0c1b9e-2222-4222-8333-444455556666"
const ask = (over: Record<string, unknown> = {}) => ({
  id: ID,
  state: "running",
  provider: "claude",
  model: "default",
  question: "What is our pricing?",
  createdAt: 1700000000,
  finishedAt: null,
  pagesConsidered: 2,
  answer: null,
  citations: [],
  pages: [],
  insufficient: null,
  error: null,
  ...over,
})
const citation = {
  key: "k-0123456789ab",
  sourceId: "5f0c1b9e-aaaa-4bbb-8ccc-ddddeeeeffff",
  title: "Price list",
  kind: "document",
  nodeId: 12,
  segment: 1,
  pageIds: ["page-1"],
}
const answered = (over: Record<string, unknown> = {}) =>
  ask({
    state: "answered",
    finishedAt: 1700000012,
    answer: "Plans start at $10.[^k-0123456789ab] See [[pricing]].",
    citations: [citation],
    pages: [{ id: "page-1", slug: "pricing", title: "Pricing", revision: 3 }],
    ...over,
  })
const finished = (state: string, over: Record<string, unknown> = {}) =>
  ask({ state, finishedAt: 1700000012, ...over })

const connected = () =>
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
    ],
  })

type Routes = Parameters<typeof mockNativeSession>[1]
function setup(
  routes: Routes = {},
  props: { onOpenPage?: (id: string) => void } = {}
) {
  const posts: unknown[] = []
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/agent/connections": connected,
    ...routes,
  })
  const view = render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeKnowledgeAsk {...props} />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  return { posts, mock, view }
}
function calls(mock: ReturnType<typeof mockNativeSession>) {
  return mock.fetcher.mock.calls.map(
    ([path, init]) => `${(init as RequestInit)?.method ?? "GET"} ${path}`
  )
}
/** Advance only the poll clock until `check` holds (bounded). */
async function tickUntil(check: () => void, ms = 1_600) {
  for (let attempt = 0; ; attempt += 1) {
    await act(async () => {
      vi.advanceTimersByTime(ms)
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
async function askQuestion(text = "What is our pricing?") {
  const box = await screen.findByRole("textbox", { name: "Question" })
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Ask" })).toBeEnabled()
  )
  fireEvent.change(box, { target: { value: text } })
  fireEvent.keyDown(box, { key: "Enter" })
}

it("says plainly that this is a temporary admin test that nothing else uses", async () => {
  setup()
  expect(
    await screen.findByRole("heading", { name: "Test knowledge" })
  ).toBeVisible()
  expect(
    screen.getByText(/Admin test only\..*Members and agents do not use it\./)
  ).toBeVisible()
  expect(screen.getByText(/temporary and is not saved/)).toBeVisible()
  // Says where the question goes.
  expect(
    screen.getByText(
      /sends your question and the saved pages to your connected AI provider/
    )
  ).toBeVisible()
  expect(document.body.textContent).not.toMatch(
    /published|available to agents/i
  )
})

it("sends on Enter, keeps Shift+Enter as a new line, and polls until answered", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  const bodies: unknown[] = []
  let polls = 0
  const { mock } = setup({
    "POST /api/neev/knowledge/ask": (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json(ask())
    },
    [`GET /api/neev/knowledge/ask/${ID}`]: () => {
      polls += 1
      return json(polls < 2 ? ask() : answered())
    },
  })
  const box = await screen.findByRole("textbox", { name: "Question" })
  fireEvent.change(box, { target: { value: "line one" } })
  fireEvent.keyDown(box, { key: "Enter", shiftKey: true })
  expect(bodies).toEqual([])
  await askQuestion()
  await waitFor(() => expect(bodies).toHaveLength(1))
  expect(bodies[0]).toEqual({
    provider: "claude",
    model: "default",
    question: "What is our pricing?",
    history: [],
  })
  // While it runs: a status, and Stop.
  expect(await screen.findByText("Reading the saved pages…")).toBeVisible()
  expect(screen.getByRole("button", { name: "Stop" })).toBeVisible()
  await tickUntil(() =>
    expect(screen.getByText(/Plans start at \$10/)).toBeVisible()
  )
  expect(screen.queryByRole("button", { name: "Stop" })).toBeNull()
  // The answer is no longer polled at the running cadence.
  const after = calls(mock).length
  await act(async () => {
    vi.advanceTimersByTime(1_600 * 3)
  })
  expect(calls(mock).length).toBe(after)
})

it("stops a running test and asks no more about it", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  const { mock } = setup({
    "POST /api/neev/knowledge/ask": () => json(ask()),
    [`GET /api/neev/knowledge/ask/${ID}`]: () => json(ask()),
    [`POST /api/neev/knowledge/ask/${ID}/stop`]: () =>
      json(finished("canceled")),
  })
  await askQuestion()
  await userEvent
    .setup()
    .click(await screen.findByRole("button", { name: "Stop" }))
  expect(await screen.findByText("Stopped.")).toBeVisible()
  const after = calls(mock).length
  await act(async () => {
    vi.advanceTimersByTime(1_600 * 4)
  })
  expect(calls(mock).length).toBe(after)
})

it("sends the earlier answer as follow-up context, once it is re-confirmed", async () => {
  const bodies: Record<string, unknown>[] = []
  setup({
    [`GET /api/neev/knowledge/ask/${ID}`]: () => json(answered()),
    "POST /api/neev/knowledge/ask": (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json(
        bodies.length === 1
          ? answered()
          : answered({ id: ID2, question: "And yearly?" })
      )
    },
  })
  await askQuestion()
  expect(await screen.findByText(/Plans start at \$10/)).toBeVisible()
  await askQuestion("And yearly?")
  await waitFor(() => expect(bodies).toHaveLength(2))
  expect(bodies[1].history).toEqual([
    {
      question: "What is our pricing?",
      answer: "Plans start at $10.[^k-0123456789ab] See [[pricing]].",
    },
  ])
})

/**
 * Review FB1: an answer revoked since the last re-check must never be sent to
 * the agent as context, where a new answer could restate it.
 */
it("re-reads earlier answers before a follow-up, and sends none that was revoked", async () => {
  const bodies: Record<string, unknown>[] = []
  let revoked = false
  setup({
    [`GET /api/neev/knowledge/ask/${ID}`]: () =>
      json(
        revoked
          ? finished("failed", {
              error: { code: "source_revoked", category: null, message: null },
            })
          : answered()
      ),
    "POST /api/neev/knowledge/ask": (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json(
        bodies.length === 1
          ? answered()
          : answered({ id: ID2, question: "And yearly?" })
      )
    },
  })
  await askQuestion()
  expect(await screen.findByText(/Plans start at \$10/)).toBeVisible()
  revoked = true
  await askQuestion("And yearly?")
  await waitFor(() => expect(bodies).toHaveLength(2))
  expect(bodies[1].history).toEqual([])
  expect(
    await screen.findByText(
      "A source stopped being shared, so this answer was discarded."
    )
  ).toBeVisible()
})

it("leaves out an earlier answer it cannot confirm right now", async () => {
  const bodies: Record<string, unknown>[] = []
  let down = false
  setup({
    [`GET /api/neev/knowledge/ask/${ID}`]: () =>
      down ? json({ code: "unavailable" }, 503) : json(answered()),
    "POST /api/neev/knowledge/ask": (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json(answered({ id: bodies.length === 1 ? ID : ID2 }))
    },
  })
  await askQuestion()
  await screen.findByText(/Plans start at \$10/)
  down = true
  await askQuestion("And yearly?")
  await waitFor(() => expect(bodies).toHaveLength(2))
  expect(bodies[1].history).toEqual([])
})

it("discards a shown answer when re-checking finds the admin role gone", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  let forbidden = false
  setup({
    "POST /api/neev/knowledge/ask": () => json(answered()),
    [`GET /api/neev/knowledge/ask/${ID}`]: () =>
      forbidden ? json({ code: "forbidden" }, 403) : json(answered()),
  })
  await askQuestion()
  expect(await screen.findByText(/Plans start at \$10/)).toBeVisible()
  forbidden = true
  await tickUntil(
    () => expect(screen.queryByText(/Plans start at \$10/)).toBeNull(),
    30_100
  )
  expect(screen.queryByRole("link", { name: "Price list" })).toBeNull()
})

it("re-checks a shown answer as soon as the tab is visible again", async () => {
  let revoked = false
  setup({
    "POST /api/neev/knowledge/ask": () => json(answered()),
    [`GET /api/neev/knowledge/ask/${ID}`]: () =>
      json(
        revoked
          ? finished("failed", {
              error: { code: "source_revoked", category: null, message: null },
            })
          : answered()
      ),
  })
  await askQuestion()
  expect(await screen.findByText(/Plans start at \$10/)).toBeVisible()
  revoked = true
  act(() => {
    document.dispatchEvent(new Event("visibilitychange"))
  })
  expect(
    await screen.findByText(
      "A source stopped being shared, so this answer was discarded."
    )
  ).toBeVisible()
  expect(screen.queryByText(/Plans start at \$10/)).toBeNull()
})

it("cites sources and saved pages accessibly, and opens them", async () => {
  const opened = vi.fn()
  setup(
    { "POST /api/neev/knowledge/ask": () => json(answered()) },
    { onOpenPage: opened }
  )
  await askQuestion()
  const answer = await screen.findByRole("article", { name: "Answer" })
  const user = userEvent.setup()
  // Inline, and listed below the answer.
  const sources = within(answer).getByRole("list", { name: "Sources" })
  expect(
    within(sources).getByRole("button", { name: "Price list" })
  ).toBeVisible()
  await user.click(within(answer).getByRole("link", { name: "Price list" }))
  expect(router.push).toHaveBeenCalledWith("/workspace")
  expect(localStorage.getItem("neev.ws.tabs.1:3")).toContain(citation.sourceId)
  const pages = within(answer).getByRole("list", { name: "Saved pages" })
  await user.click(within(pages).getByRole("button", { name: "Pricing" }))
  expect(opened).toHaveBeenCalledWith("page-1")
  await user.click(within(answer).getByRole("link", { name: "pricing" }))
  expect(opened).toHaveBeenCalledTimes(2)
})

it.each([
  [
    "no saved pages",
    finished("insufficient", {
      insufficient: { reason: "no_readable_pages", note: null },
    }),
    "There are no readable saved pages yet. Compile the wiki first.",
  ],
  [
    "not covered",
    finished("insufficient", {
      insufficient: { reason: "agent", note: "Pricing is not in the pages." },
    }),
    "The saved pages don't cover this.",
  ],
  [
    "no valid citation",
    finished("insufficient", {
      insufficient: { reason: "ungrounded", note: null },
    }),
    "The agent answered without a valid citation, so the answer is not shown.",
  ],
  [
    "a timeout",
    finished("failed", {
      error: { code: "timeout", category: null, message: null },
    }),
    "This took longer than 5 minutes and was stopped.",
  ],
  [
    "an expired sign-in",
    finished("failed", {
      error: { code: "unauthorized", category: "auth_expired", message: null },
    }),
    "Your agent account needs reconnecting under Personal agents.",
  ],
  [
    "a provider failure",
    finished("failed", {
      error: {
        code: "provider",
        category: "quota",
        message: "Quota exceeded.",
      },
    }),
    "The test could not be answered.",
  ],
])("explains %s", async (_label, result, message) => {
  setup({ "POST /api/neev/knowledge/ask": () => json(result) })
  await askQuestion()
  expect(await screen.findByText(message)).toBeVisible()
})

it("shows the agent's own short note when the pages do not cover it", async () => {
  setup({
    "POST /api/neev/knowledge/ask": () =>
      json(
        finished("insufficient", {
          insufficient: {
            reason: "agent",
            note: "Pricing is not in the pages.",
          },
        })
      ),
  })
  await askQuestion()
  expect(await screen.findByText("Pricing is not in the pages.")).toBeVisible()
})

it.each([
  [409, "A test may already be running, or your agent needs reconnecting."],
  [429, "Too many tests are running right now. Try again shortly."],
])("explains a refused start (%s)", async (status, message) => {
  setup({ "POST /api/neev/knowledge/ask": () => json({ code: "x" }, status) })
  await askQuestion()
  expect(await screen.findByText(message)).toBeVisible()
})

it.each([
  ["ask_busy", "A test is already running. Wait for it to finish, or stop it."],
  [
    "credential_missing",
    "Your agent account needs reconnecting under Personal agents.",
  ],
])(
  "tells a busy start from a missing credential (%s)",
  async (code, message) => {
    setup({ "POST /api/neev/knowledge/ask": () => json({ code }, 409) })
    await askQuestion()
    expect(await screen.findByText(message)).toBeVisible()
  }
)

it("says a test answer expired when the server no longer has it", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  setup({
    "POST /api/neev/knowledge/ask": () => json(ask()),
    [`GET /api/neev/knowledge/ask/${ID}`]: () =>
      json({ code: "not_found" }, 404),
  })
  await askQuestion()
  await tickUntil(() =>
    expect(screen.getByText("This test answer expired.")).toBeVisible()
  )
})

/**
 * A source can stop being shared after the answer is shown. The answer is
 * re-checked while it is on screen, and discarded the moment it is revoked.
 */
it("discards a shown answer once a source is revoked", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  let revoked = false
  setup({
    "POST /api/neev/knowledge/ask": () => json(answered()),
    [`GET /api/neev/knowledge/ask/${ID}`]: () =>
      json(
        revoked
          ? finished("failed", {
              error: { code: "source_revoked", category: null, message: null },
            })
          : answered()
      ),
  })
  await askQuestion()
  expect(await screen.findByText(/Plans start at \$10/)).toBeVisible()
  revoked = true
  await tickUntil(
    () =>
      expect(
        screen.getByText(
          "A source stopped being shared, so this answer was discarded."
        )
      ).toBeVisible(),
    30_100
  )
  expect(screen.queryByText(/Plans start at \$10/)).toBeNull()
  expect(screen.queryByRole("link", { name: "Price list" })).toBeNull()
})

it("stops a running test when the panel goes away, and polls no more", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  const { mock, view } = setup({
    "POST /api/neev/knowledge/ask": () => json(ask()),
    [`GET /api/neev/knowledge/ask/${ID}`]: () => json(ask()),
    [`POST /api/neev/knowledge/ask/${ID}/stop`]: () =>
      json(finished("canceled")),
  })
  await askQuestion()
  await screen.findByRole("button", { name: "Stop" })
  view.unmount()
  await waitFor(() =>
    expect(calls(mock)).toContain(`POST /api/neev/knowledge/ask/${ID}/stop`)
  )
  const after = calls(mock).length
  await act(async () => {
    vi.advanceTimersByTime(1_600 * 4)
  })
  expect(calls(mock).length).toBe(after)
})

it("needs the admin's own connected agent", async () => {
  // The default: nothing connected.
  mockNativeSession(orgSession, {})
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeKnowledgeAsk />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  expect(
    await screen.findByText(
      "Connect your own Claude or Codex agent under Personal agents to test."
    )
  ).toBeVisible()
  expect(screen.getByRole("button", { name: "Ask" })).toBeDisabled()
})

it("says the connections could not be loaded, rather than that there are none", async () => {
  setup({
    "GET /api/neev/agent/connections": () => json({ code: "unavailable" }, 503),
  })
  expect(
    await screen.findByText(
      "Your agent connections could not be loaded. Try again later."
    )
  ).toBeVisible()
  expect(screen.getByRole("button", { name: "Ask" })).toBeDisabled()
})

it("starts a new test with nothing carried over", async () => {
  const bodies: Record<string, unknown>[] = []
  setup({
    "POST /api/neev/knowledge/ask": (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json(answered({ id: bodies.length === 1 ? ID : ID2 }))
    },
  })
  await askQuestion()
  await screen.findByText(/Plans start at \$10/)
  await userEvent
    .setup()
    .click(screen.getByRole("button", { name: "New test" }))
  expect(screen.queryByText(/Plans start at \$10/)).toBeNull()
  await askQuestion("Again")
  await waitFor(() => expect(bodies).toHaveLength(2))
  expect(bodies[1].history).toEqual([])
})
