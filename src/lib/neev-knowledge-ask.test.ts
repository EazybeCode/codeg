import { afterEach, describe, expect, it, vi } from "vitest"
import {
  ASK_HISTORY_LIMITS,
  askHistory,
  loadKnowledgeAsk,
  parseKnowledgeAsk,
  prepareAskMarkdown,
  startKnowledgeAsk,
  stopKnowledgeAsk,
  type KnowledgeAsk,
} from "./neev-knowledge-ask"

afterEach(() => vi.unstubAllGlobals())

const json = (value: unknown, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  })

const ID = "5f0c1b9e-1111-4222-8333-444455556666"
export const ask = (over: Record<string, unknown> = {}) => ({
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
  sourceId: "src-1",
  title: "Price list",
  kind: "document",
  nodeId: 12,
  segment: 1,
  pageIds: ["page-1"],
}
const page = { id: "page-1", slug: "pricing", title: "Pricing", revision: 3 }
const answered = (over: Record<string, unknown> = {}) =>
  ask({
    state: "answered",
    finishedAt: 1700000012,
    answer: "Plans start at $10.[^k-0123456789ab] See [[pricing]].",
    citations: [citation],
    pages: [page],
    ...over,
  })

describe("parsing an ask", () => {
  it("reads every terminal state the contract defines", () => {
    expect(parseKnowledgeAsk(answered()).state).toBe("answered")
    expect(
      parseKnowledgeAsk(
        ask({
          state: "insufficient",
          finishedAt: 1,
          insufficient: { reason: "agent", note: "Not covered." },
        })
      ).insufficient
    ).toEqual({ reason: "agent", note: "Not covered." })
    expect(
      parseKnowledgeAsk(
        ask({
          state: "failed",
          finishedAt: 1,
          error: { code: "source_revoked", category: null, message: null },
        })
      ).error?.code
    ).toBe("source_revoked")
    expect(
      parseKnowledgeAsk(ask({ state: "canceled", finishedAt: 1 })).state
    ).toBe("canceled")
  })

  /**
   * An answer is shown only in the one state that may carry it. A failed or
   * revoked ask that still came with text is refused, never rendered.
   */
  it.each([
    [
      "a failed ask with an answer",
      {
        state: "failed",
        finishedAt: 1,
        error: { code: "source_revoked", category: null, message: null },
        answer: "leak",
      },
    ],
    [
      "a failed ask with citations",
      {
        state: "failed",
        finishedAt: 1,
        error: { code: "timeout", category: null, message: null },
        citations: [citation],
      },
    ],
    ["a running ask with an answer", { answer: "early" }],
    [
      "an answered ask without its answer",
      { state: "answered", finishedAt: 1 },
    ],
    [
      "an insufficient ask without its reason",
      { state: "insufficient", finishedAt: 1 },
    ],
    ["an unknown state", { state: "published" }],
    ["an unknown field", { extra: true }],
    [
      "an unknown insufficient reason",
      {
        state: "insufficient",
        finishedAt: 1,
        insufficient: { reason: "maybe", note: null },
      },
    ],
  ])("refuses %s", (_label, over) => {
    expect(() => parseKnowledgeAsk(ask(over))).toThrow()
  })
})

describe("requests", () => {
  it("starts with exactly the contract's fields", async () => {
    const fetcher = vi.fn<
      (path: string, init?: RequestInit) => Promise<Response>
    >(async () => json(ask()))
    vi.stubGlobal("fetch", fetcher)
    await startKnowledgeAsk(
      {
        provider: "claude",
        model: "default",
        question: "  What is our pricing?  ",
        history: [{ question: "Hi", answer: "Hello" }],
      },
      "csrf"
    )
    const [path, init] = fetcher.mock.calls[0]
    expect(path).toBe("/api/neev/knowledge/ask")
    expect(init?.method).toBe("POST")
    expect(JSON.parse(String(init?.body))).toEqual({
      provider: "claude",
      model: "default",
      question: "What is our pricing?",
      history: [{ question: "Hi", answer: "Hello" }],
    })
  })
  it("polls and stops by id, and refuses a path-like id", async () => {
    const fetcher = vi.fn<
      (path: string, init?: RequestInit) => Promise<Response>
    >(async () => json(ask()))
    vi.stubGlobal("fetch", fetcher)
    await loadKnowledgeAsk(ID)
    await stopKnowledgeAsk(ID, "csrf")
    expect(
      fetcher.mock.calls.map(
        ([path, init]) => `${init?.method ?? "GET"} ${path}`
      )
    ).toEqual([
      `GET /api/neev/knowledge/ask/${ID}`,
      `POST /api/neev/knowledge/ask/${ID}/stop`,
    ])
    await expect(loadKnowledgeAsk("../settings")).rejects.toThrow()
    expect(fetcher).toHaveBeenCalledTimes(2)
  })
  it("refuses a question outside 1..2000 characters before sending", async () => {
    const fetcher = vi.fn(async () => json(ask()))
    vi.stubGlobal("fetch", fetcher)
    await expect(
      startKnowledgeAsk(
        { provider: "claude", model: "default", question: "   ", history: [] },
        "c"
      )
    ).rejects.toThrow()
    await expect(
      startKnowledgeAsk(
        {
          provider: "claude",
          model: "default",
          question: "x".repeat(2001),
          history: [],
        },
        "c"
      )
    ).rejects.toThrow()
    expect(fetcher).not.toHaveBeenCalled()
  })
})

describe("follow-up history", () => {
  const turn = (state: string, extra: Partial<KnowledgeAsk> = {}) =>
    ({
      ...parseKnowledgeAsk(
        state === "answered"
          ? answered()
          : state === "insufficient"
            ? ask({
                state,
                finishedAt: 1,
                insufficient: { reason: "agent", note: "Not covered." },
              })
            : ask({
                state,
                finishedAt: 1,
                error:
                  state === "failed"
                    ? { code: "timeout", category: null, message: null }
                    : null,
              })
      ),
      ...extra,
    }) as KnowledgeAsk

  it("sends only answered and insufficient turns, with the server's own text", () => {
    expect(
      askHistory([
        turn("answered"),
        turn("failed"),
        turn("canceled"),
        turn("insufficient"),
      ])
    ).toEqual([
      {
        question: "What is our pricing?",
        answer: "Plans start at $10.[^k-0123456789ab] See [[pricing]].",
      },
      { question: "What is our pricing?", answer: "Not covered." },
    ])
  })
  it("keeps within the contract's bounds: 6 turns, 6000 per answer, 24000 in all", () => {
    const long = Array.from({ length: 9 }, () =>
      turn("answered", { answer: "y".repeat(9000) })
    )
    const history = askHistory(long)
    expect(history.length).toBeLessThanOrEqual(ASK_HISTORY_LIMITS.turns)
    expect(
      history.every((item) => item.answer.length <= ASK_HISTORY_LIMITS.answer)
    ).toBe(true)
    const total = history.reduce(
      (sum, item) => sum + item.question.length + item.answer.length,
      0
    )
    expect(total).toBeLessThanOrEqual(ASK_HISTORY_LIMITS.total)
  })
})

/**
 * The server counts characters as Unicode scalar values; an emoji is one
 * character there and two UTF-16 units here.
 */
describe("character bounds, as the server counts them", () => {
  it("accepts a 300-emoji note, which is 600 UTF-16 units", () => {
    const note = "🚀".repeat(300)
    expect(
      parseKnowledgeAsk(
        ask({
          state: "insufficient",
          finishedAt: 1,
          insufficient: { reason: "agent", note },
        })
      ).insufficient?.note
    ).toBe(note)
    expect(() =>
      parseKnowledgeAsk(
        ask({
          state: "insufficient",
          finishedAt: 1,
          insufficient: { reason: "agent", note: note + "🚀" },
        })
      )
    ).toThrow()
  })
  it("accepts a failure message of 300 emoji", () => {
    const message = "⚠️".repeat(150) // two scalar values each
    expect(
      parseKnowledgeAsk(
        ask({
          state: "failed",
          finishedAt: 1,
          error: { code: "provider", category: "quota", message },
        })
      ).error?.message
    ).toBe(message)
  })
  it("never splits an emoji when cutting history, so it serializes cleanly", () => {
    const answer = "x" + "🚀".repeat(6000)
    const [item] = askHistory([
      parseKnowledgeAsk(answered({ answer: `${answer}[^k-0123456789ab]` })),
    ])
    expect(Array.from(item.answer)).toHaveLength(ASK_HISTORY_LIMITS.answer)
    expect(item.answer.endsWith("🚀")).toBe(true)
    expect(JSON.stringify(item)).not.toMatch(/\\ud[89a-f]/i)
  })
  it("sends a 2000-emoji question, and refuses 2001", async () => {
    const fetcher = vi.fn(async () => json(ask()))
    vi.stubGlobal("fetch", fetcher)
    await startKnowledgeAsk(
      {
        provider: "claude",
        model: "default",
        question: "🚀".repeat(2000),
        history: [],
      },
      "c"
    )
    await expect(
      startKnowledgeAsk(
        {
          provider: "claude",
          model: "default",
          question: "🚀".repeat(2001),
          history: [],
        },
        "c"
      )
    ).rejects.toThrow()
    expect(fetcher).toHaveBeenCalledTimes(1)
  })
})

describe("answer markdown", () => {
  it("links citations to their source and [[slug]] to a page the answer names", () => {
    const parsed = parseKnowledgeAsk(answered())
    expect(prepareAskMarkdown(parsed)).toBe(
      "Plans start at $10.[Price list](#neev-cite-k-0123456789ab) See [pricing](#neev-page-pricing)."
    )
  })
})
