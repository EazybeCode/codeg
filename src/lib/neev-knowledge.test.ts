import { afterEach, describe, expect, it, vi } from "vitest"
import {
  batchesFor,
  knowledgeExportUrl,
  loadKnowledgePage,
  parseKnowledgeLog,
  parseKnowledgePage,
  parseKnowledgePages,
  parseKnowledgeStatus,
  prepareKnowledgeMarkdown,
  saveKnowledgeSettings,
  startKnowledgeJob,
} from "./neev-knowledge"

afterEach(() => vi.unstubAllGlobals())

const json = (value: unknown, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  })

export const status = (over: Record<string, unknown> = {}) => ({
  captureEnabled: true,
  eligible: 3,
  captured: 2,
  represented: 1,
  stale: 0,
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
export const job = (over: Record<string, unknown> = {}) => ({
  id: "j1",
  mode: "incremental",
  state: "running",
  provider: "claude",
  model: "default",
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

describe("status", () => {
  it("reads the amended contract, coverage and quota included", () => {
    const parsed = parseKnowledgeStatus(status({ job: job() }))
    expect(parsed.coverage.uncovered).toBe(2)
    expect(parsed.job?.state).toBe("running")
  })
  it.each([
    ["an unknown job state", status({ job: job({ state: "published" }) })],
    ["an unknown capture block", status({ captureBlocked: "full" })],
    ["a negative count", status({ eligible: -1 })],
    [
      "missing coverage",
      Object.fromEntries(
        Object.entries(status()).filter(([k]) => k !== "coverage")
      ),
    ],
    ["an unknown wait reason", status({ job: job({ waitReason: "lunch" }) })],
  ])("refuses %s", (_label, value) => {
    expect(() => parseKnowledgeStatus(value)).toThrow()
  })
  it("accepts completed_with_gaps and the slot wait", () => {
    expect(
      parseKnowledgeStatus(
        status({
          job: job({ state: "completed_with_gaps", waitReason: "slot" }),
        })
      ).job
    ).toMatchObject({ state: "completed_with_gaps", waitReason: "slot" })
  })
})

describe("pages", () => {
  const readable = {
    id: "p1",
    slug: "beta",
    title: "Beta",
    revision: 1,
    status: "readable",
    markdown: "# Beta",
    citations: [],
    stale: false,
    lint: [],
  }
  it("accepts a withheld page that carries nothing about its content", () => {
    expect(
      parseKnowledgePage({
        id: "p2",
        revision: 2,
        status: "withheld",
        unreadableSources: 1,
      })
    ).toEqual({
      id: "p2",
      revision: 2,
      status: "withheld",
      unreadableSources: 1,
    })
  })
  /**
   * A withheld page is opaque (spec A3). A response that names it anyway is a
   * server bug this client refuses to display rather than quietly render.
   */
  it.each([
    ["a title", { title: "Secret plan" }],
    ["a slug", { slug: "secret-plan" }],
    ["a body", { markdown: "# Secret" }],
  ])("refuses a withheld page carrying %s", (_label, extra) => {
    expect(() =>
      parseKnowledgePage({
        id: "p2",
        revision: 2,
        status: "withheld",
        unreadableSources: 1,
        ...extra,
      })
    ).toThrow()
    expect(() =>
      parseKnowledgePages([
        {
          id: "p2",
          revision: 2,
          status: "withheld",
          unreadableSources: 1,
          updatedAt: 1,
          ...extra,
        },
      ])
    ).toThrow()
  })
  it("refuses a readable page without its body", () => {
    expect(() =>
      parseKnowledgePage(
        Object.fromEntries(
          Object.entries(readable).filter(([k]) => k !== "markdown")
        )
      )
    ).toThrow()
  })
  it("refuses a withheld citation that still names its source", () => {
    expect(() =>
      parseKnowledgePage({
        ...readable,
        citations: [{ key: "k-0123456789ab", withheld: true, title: "Old" }],
      })
    ).toThrow()
  })
  it("loads a page by its opaque id, and refuses anything path-like", async () => {
    const fetcher = vi.fn<(path: string) => Promise<Response>>(async () =>
      json({ id: "p2", revision: 2, status: "withheld", unreadableSources: 3 })
    )
    vi.stubGlobal("fetch", fetcher)
    await loadKnowledgePage("p2")
    expect(fetcher.mock.calls[0][0]).toBe("/api/neev/knowledge/pages/p2")
    await expect(loadKnowledgePage("../settings")).rejects.toThrow()
    expect(fetcher).toHaveBeenCalledTimes(1)
  })
})

describe("log", () => {
  it("accepts entries whose page is not readable (no title)", () => {
    expect(
      parseKnowledgeLog([
        {
          seq: 3,
          at: 1,
          event: "batch_accepted",
          actorId: 1,
          pageId: "p2",
          pageTitle: null,
          detail: "pages=2",
        },
      ])
    ).toHaveLength(1)
  })
})

describe("markdown for the preview", () => {
  const readableCite = {
    key: "k-0123456789ab",
    withheld: false as const,
    sourceId: "r1",
    title: "Release notes",
    kind: "document" as const,
    nodeId: 7,
    segment: 1,
  }
  const citations = [
    readableCite,
    { key: "k-ffffffffffff", withheld: true as const },
  ]
  it("turns readable citations into in-app anchors labelled by the source", () => {
    const out = prepareKnowledgeMarkdown(
      "Ship on Fridays.[^k-0123456789ab]",
      citations,
      new Set()
    )
    expect(out).toBe(
      "Ship on Fridays.[Release notes](#neev-cite-k-0123456789ab)"
    )
  })
  it("names nothing for a citation whose source is no longer readable", () => {
    const out = prepareKnowledgeMarkdown(
      "A claim.[^k-ffffffffffff]",
      citations,
      new Set()
    )
    expect(out).not.toContain("k-ffffffffffff](")
    // Escaped, so a `[withheld]: url` definition elsewhere cannot make it a link.
    expect(out).toBe("A claim.\\[withheld\\]")
  })
  it("links [[slug]] only among readable pages", () => {
    const out = prepareKnowledgeMarkdown(
      "See [[beta]] and [[gone]].",
      [],
      new Set(["beta"])
    )
    expect(out).toBe("See [beta](#neev-page-beta) and gone.")
  })
  it("cannot be steered into an outside link by a crafted title", () => {
    const out = prepareKnowledgeMarkdown(
      "x[^k-0123456789ab]",
      [{ ...readableCite, title: "a](https://evil.example) [b" }],
      new Set()
    )
    expect(out).not.toContain("](https://evil.example)")
    expect(out).toContain("(#neev-cite-k-0123456789ab)")
  })
})

describe("requests", () => {
  it("saves capture with the revision it read", async () => {
    const fetcher = vi.fn<
      (path: string, init?: RequestInit) => Promise<Response>
    >(async () => json({ captureEnabled: true, revision: 4 }))
    vi.stubGlobal("fetch", fetcher)
    await saveKnowledgeSettings(true, 3, "csrf")
    const [path, init] = fetcher.mock.calls[0]
    expect(path).toBe("/api/neev/knowledge/settings")
    expect(init?.method).toBe("PUT")
    expect(JSON.parse(String(init?.body))).toEqual({
      captureEnabled: true,
      expectedRevision: 3,
    })
  })
  it("starts a job with exactly mode, provider and model", async () => {
    const fetcher = vi.fn<
      (path: string, init?: RequestInit) => Promise<Response>
    >(async () => json(job()))
    vi.stubGlobal("fetch", fetcher)
    await startKnowledgeJob("rebuild", "claude", "default", "csrf")
    expect(JSON.parse(String(fetcher.mock.calls[0][1]?.body))).toEqual({
      mode: "rebuild",
      provider: "claude",
      model: "default",
    })
  })
  it("estimates batches of twelve units", () => {
    expect(batchesFor(0)).toBe(0)
    expect(batchesFor(12)).toBe(1)
    expect(batchesFor(13)).toBe(2)
  })
  it("exports from the fixed route", () => {
    expect(knowledgeExportUrl()).toBe("/api/neev/knowledge/export")
  })
})
