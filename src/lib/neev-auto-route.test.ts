import { afterEach, describe, expect, it, vi } from "vitest"
import { NativeClientError } from "./neev-client"
import {
  parseRoutingStatus,
  readAutoPreference,
  recalledRoute,
  rememberRoute,
  routeNewAgentChat,
  writeAutoPreference,
} from "./neev-auto-route"
import type { NeevRoute } from "./neev-agent-sessions-client"

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  })
}

const routed = {
  provider: "codex",
  model: "gpt-a",
  label: "GPT A",
  effort: "high",
  fast: false,
  outcome: "routed",
  reason: null,
  router: "jev",
}

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

const EXTRA = { supervision: "off", usage: [] }

describe("routing status", () => {
  it("reads the three honest states", () => {
    expect(
      parseRoutingStatus({
        state: "off",
        router: null,
        maxTaskChars: 6000,
        maxPreviousChars: 2000,
        ...EXTRA,
      })
    ).toEqual({
      state: "off",
      router: null,
      maxTaskChars: 6000,
      maxPreviousChars: 2000,
      supervision: "off",
      usage: [],
    })
    expect(
      parseRoutingStatus({
        state: "missing_credential",
        router: "jev",
        maxTaskChars: 6000,
        maxPreviousChars: 2000,
        ...EXTRA,
      }).state
    ).toBe("missing_credential")
    expect(
      parseRoutingStatus({
        state: "ready",
        router: "jev",
        maxTaskChars: 6000,
        maxPreviousChars: 2000,
        ...EXTRA,
      }).state
    ).toBe("ready")
  })

  it("reads the caller's own usage and the supervision setting", () => {
    const usage = {
      provider: "codex",
      availability: "exhausted",
      until: 1_900_000_000,
      remaining: "low",
      nextResetAt: 1_900_000_000,
      observedAt: 1_899_990_000,
    }
    const status = parseRoutingStatus({
      state: "ready",
      router: "jev",
      maxTaskChars: 6000,
      maxPreviousChars: 2000,
      supervision: "plan_review",
      usage: [usage],
    })
    expect(status.supervision).toBe("plan_review")
    expect(status.usage).toEqual([usage])
  })

  it("refuses a status that could misreport the router or usage", () => {
    for (const bad of [
      {
        state: "ready",
        router: null,
        maxTaskChars: 6000,
        maxPreviousChars: 2000,
        ...EXTRA,
      },
      {
        state: "off",
        router: "jev",
        maxTaskChars: 6000,
        maxPreviousChars: 2000,
        ...EXTRA,
      },
      {
        state: "enabled",
        router: "jev",
        maxTaskChars: 6000,
        maxPreviousChars: 2000,
        ...EXTRA,
      },
      { state: "ready", router: "jev", ...EXTRA },
      {
        state: "ready",
        router: "jev",
        maxTaskChars: 6000,
        maxPreviousChars: 2000,
        key: "x",
        ...EXTRA,
      },
      {
        state: "ready",
        router: "Jev Pro!",
        maxTaskChars: 6000,
        maxPreviousChars: 2000,
        ...EXTRA,
      },
      {
        state: "ready",
        router: "jev",
        maxTaskChars: 6000,
        maxPreviousChars: 2000,
      },
      {
        state: "ready",
        router: "jev",
        maxTaskChars: 6000,
        maxPreviousChars: 2000,
        supervision: "always",
        usage: [],
      },
      {
        state: "ready",
        router: "jev",
        maxTaskChars: 6000,
        maxPreviousChars: 2000,
        supervision: "off",
        usage: [{ provider: "codex", usedPercent: 42 }],
      },
    ])
      expect(() => parseRoutingStatus(bad)).toThrow(NativeClientError)
  })
})

describe("routing a new chat", () => {
  it("sends the draft and its manual fallback, and reads the decision", async () => {
    const fetcher = vi.fn().mockResolvedValue(json(routed))
    vi.stubGlobal("fetch", fetcher)
    const route = await routeNewAgentChat(
      {
        prompt: "Build the importer",
        provider: "claude",
        model: "sonnet",
        effort: null,
        fast: false,
        attachedFiles: 2,
      },
      "csrf"
    )
    expect(route).toEqual({
      ...routed,
      routeId: null,
      withheld: [],
      supervision: null,
    })
    const [url, init] = fetcher.mock.calls[0]
    expect(url).toBe("/api/neev/agent/route")
    expect(JSON.parse(init.body)).toEqual({
      prompt: "Build the importer",
      provider: "claude",
      model: "sonnet",
      effort: null,
      fast: false,
      attachedFiles: 2,
    })
  })

  it("refuses an off-list provider in the answer", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(json({ ...routed, provider: "openrouter" }))
    )
    await expect(
      routeNewAgentChat(
        {
          prompt: "x",
          provider: "claude",
          model: "default",
          effort: null,
          fast: false,
          attachedFiles: 0,
        },
        "csrf"
      )
    ).rejects.toThrow(NativeClientError)
  })

  it("refuses an unsafe request before sending it", async () => {
    const fetcher = vi.fn()
    vi.stubGlobal("fetch", fetcher)
    await expect(
      routeNewAgentChat(
        {
          prompt: "x",
          provider: "claude",
          model: "--dangerously",
          effort: null,
          fast: false,
          attachedFiles: 0,
        },
        "csrf"
      )
    ).rejects.toThrow(NativeClientError)
    expect(fetcher).not.toHaveBeenCalled()
  })
})

describe("device-local state", () => {
  it("defaults Auto on and remembers an explicit switch-off", () => {
    localStorage.clear()
    expect(readAutoPreference()).toBe(true)
    writeAutoPreference(false)
    expect(readAutoPreference()).toBe(false)
    writeAutoPreference(true)
    expect(readAutoPreference()).toBe(true)
  })

  it("remembers a bounded number of last routes", () => {
    const route = routed as NeevRoute
    for (let index = 0; index < 60; index += 1)
      rememberRoute(`session-${index}`, route)
    expect(recalledRoute("session-0")).toBeNull()
    expect(recalledRoute("session-59")).toEqual(route)
  })
})
