// Direct (browser-callback) sign-in for Claude and Codex, as the client reads
// and sends it (`direct-signin-contract.md` §3–§4). Every refusal here is one
// the server also makes; making it first turns a wrong paste into a clear
// message instead of a round trip — and never into a request.
import { afterEach, describe, expect, it, vi } from "vitest"
import {
  callbackQueryFromAddress,
  isCallbackRejected,
  parseApproval,
  parseSigninFragment,
  startProviderConnect,
  submitConnectCallback,
  type NeevApproval,
} from "./neev-agent-sessions-client"
import { NativeClientError } from "./neev-client"

const ATTEMPT = "0123456789abcdef0123456789abcdef"
const CODEX_AUTH =
  "https://auth.openai.com/oauth/authorize?response_type=code&client_id=x&state=s"
const CLAUDE_AUTH = "https://claude.com/cai/oauth/authorize?code=true&state=s"

const codex = {
  kind: "browserCallback",
  verificationUrl: CODEX_AUTH,
  callbackPort: 1455,
  callbackPath: "/auth/callback",
  fallbackUrl: null,
  needsCode: false,
  userCode: null,
}
const claude = {
  ...codex,
  verificationUrl: CLAUDE_AUTH,
  callbackPort: 53117,
  callbackPath: "/callback",
  fallbackUrl: CLAUDE_AUTH + "&fallback=1",
}

function json(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  })
}

afterEach(() => vi.unstubAllGlobals())

describe("browser-callback approval", () => {
  it("reads Codex's and Claude's callback approvals", () => {
    expect(parseApproval(codex, "codex")).toEqual({
      kind: "browser_callback",
      verificationUrl: CODEX_AUTH,
      callbackPort: 1455,
      callbackPath: "/auth/callback",
      fallbackUrl: null,
    })
    expect(parseApproval(claude, "claude")).toMatchObject({
      kind: "browser_callback",
      callbackPort: 53117,
      callbackPath: "/callback",
      fallbackUrl: claude.fallbackUrl,
    })
  })

  it("keeps the older device and paste approvals working", () => {
    expect(
      parseApproval(
        {
          kind: "device",
          verificationUrl: "https://auth.openai.com/codex/device",
          userCode: "ABCD-EFGH",
          needsCode: false,
        },
        "codex"
      ).kind
    ).toBe("device")
    expect(
      parseApproval(
        {
          kind: "authorizationCode",
          verificationUrl: CLAUDE_AUTH,
          userCode: null,
          needsCode: true,
        },
        "claude"
      ).kind
    ).toBe("authorization_code")
  })

  it("refuses a callback approval that could aim the user or the relay wrong", () => {
    const refused: [unknown, "claude" | "codex" | "gemini"][] = [
      // The path is the CLI's own and pinned per provider.
      [{ ...codex, callbackPath: "/callback" }, "codex"],
      [{ ...claude, callbackPath: "/auth/callback" }, "claude"],
      [{ ...codex, callbackPath: "/auth/callback/../x" }, "codex"],
      // Privileged, out-of-range or non-integer ports.
      [{ ...codex, callbackPort: 80 }, "codex"],
      [{ ...codex, callbackPort: 65536 }, "codex"],
      [{ ...codex, callbackPort: 1455.5 }, "codex"],
      [{ ...codex, callbackPort: "1455" }, "codex"],
      // Only the pinned authorize endpoint — not the device page, not another
      // provider's, not a lookalike.
      [
        { ...codex, verificationUrl: "https://auth.openai.com/codex/device" },
        "codex",
      ],
      [{ ...codex, verificationUrl: CLAUDE_AUTH }, "codex"],
      [
        {
          ...claude,
          verificationUrl: "https://claude.com.evil.test/cai/oauth/authorize?x",
        },
        "claude",
      ],
      // Codex has no paste page; Claude's must be Claude's.
      [{ ...codex, fallbackUrl: CODEX_AUTH }, "codex"],
      [{ ...claude, fallbackUrl: "https://evil.test/" }, "claude"],
      // Nothing to show and nothing to paste.
      [{ ...codex, userCode: "ABCD" }, "codex"],
      [{ ...codex, needsCode: true }, "codex"],
      // Not a flow this provider has.
      [
        {
          ...codex,
          verificationUrl: "https://accounts.google.com/o/oauth2/v2/auth?x",
        },
        "gemini",
      ],
      // A field the contract does not have.
      [{ ...codex, expectedState: "s" }, "codex"],
    ]
    for (const [value, provider] of refused)
      expect(() => parseApproval(value, provider)).toThrow(NativeClientError)
  })
})

describe("pasted loopback address", () => {
  const approval = parseApproval(codex, "codex") as Extract<
    NeevApproval,
    { kind: "browser_callback" }
  >

  it("accepts this attempt's own loopback address on either host name", () => {
    expect(
      callbackQueryFromAddress(
        "  http://localhost:1455/auth/callback?code=ac_1&scope=openid&state=s1 ",
        approval
      )
    ).toBe("code=ac_1&scope=openid&state=s1")
    expect(
      callbackQueryFromAddress(
        "http://127.0.0.1:1455/auth/callback?state=s1&error=access_denied",
        approval
      )
    ).toBe("state=s1&error=access_denied")
  })

  it("refuses any other address", () => {
    for (const pasted of [
      "",
      "code=ac_1&state=s1",
      "https://localhost:1455/auth/callback?code=a&state=s",
      "http://localhost:1456/auth/callback?code=a&state=s",
      "http://localhost/auth/callback?code=a&state=s",
      "http://localhost:1455/callback?code=a&state=s",
      "http://[::1]:1455/auth/callback?code=a&state=s",
      "http://evil.test:1455/auth/callback?code=a&state=s",
      "http://u:p@localhost:1455/auth/callback?code=a&state=s",
      "http://localhost:1455/auth/callback?code=a&state=s#x",
      // Keys and shapes a provider redirect never carries.
      "http://localhost:1455/auth/callback?code=a",
      "http://localhost:1455/auth/callback?state=s",
      "http://localhost:1455/auth/callback?code=a&code=b&state=s",
      "http://localhost:1455/auth/callback?code=a&state=s&redirect=x",
      "http://localhost:1455/auth/callback?code=&state=s",
      `http://localhost:1455/auth/callback?code=${"a".repeat(5000)}&state=s`,
    ])
      expect(callbackQueryFromAddress(pasted, approval)).toBeNull()
  })
})

describe("/agents/signin fragment", () => {
  it("splits the attempt from the provider's own query", () => {
    expect(
      parseSigninFragment(
        `#attempt=${ATTEMPT}&provider=claude&code=ac_1&state=s1`
      )
    ).toEqual({
      provider: "claude",
      attemptId: ATTEMPT,
      query: "code=ac_1&state=s1",
    })
  })

  it("refuses a fragment that is not exactly one attempt's callback", () => {
    for (const hash of [
      "",
      "#",
      `#attempt=${ATTEMPT.toUpperCase()}&provider=codex&code=a&state=s`,
      `#attempt=${ATTEMPT.slice(1)}&provider=codex&code=a&state=s`,
      `#attempt=${ATTEMPT}&attempt=${ATTEMPT}&provider=codex&code=a&state=s`,
      `#attempt=${ATTEMPT}&provider=gemini&code=a&state=s`,
      `#attempt=${ATTEMPT}&code=a&state=s`,
      `#attempt=${ATTEMPT}&provider=codex&code=a`,
      `#attempt=${ATTEMPT}&provider=codex&code=a&state=s&next=/x`,
    ])
      expect(parseSigninFragment(hash)).toBeNull()
  })
})

describe("direct sign-in requests", () => {
  it("starts in the server's default method unless Codex asks for a code", async () => {
    const bodies: unknown[] = []
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_url: string, init: RequestInit) => {
        bodies.push(JSON.parse(init.body as string))
        return json(
          {
            outcome: "pending",
            attemptId: ATTEMPT,
            expiresAt: 1_700_000_600,
            connection: null,
          },
          202
        )
      })
    )
    await startProviderConnect("claude", "k1", "csrf")
    await startProviderConnect("codex", "k2", "csrf", undefined, "device")
    // Absent, not "browser": an older server's body refuses unknown fields.
    expect(bodies).toEqual([
      { requestKey: "k1" },
      { requestKey: "k2", method: "device" },
    ])
    await expect(
      startProviderConnect("claude", "k3", "csrf", undefined, "device")
    ).rejects.toBeInstanceOf(NativeClientError)
    expect(bodies).toHaveLength(2)
  })

  it("posts the callback query to the attempt and reads the attempt back", async () => {
    const calls: { url: string; init: RequestInit }[] = []
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init: RequestInit) => {
        calls.push({ url, init })
        return json({
          state: "pending",
          expiresAt: 1_700_000_600,
          approval: null,
        })
      })
    )
    expect(
      await submitConnectCallback(
        "codex",
        ATTEMPT,
        "rk",
        "code=a&state=s",
        "csrf"
      )
    ).toEqual({ state: "pending", expiresAt: 1_700_000_600, approval: null })
    expect(calls[0].url).toBe(
      `/api/neev/agent/connections/codex/attempts/${ATTEMPT}/callback`
    )
    expect(JSON.parse(calls[0].init.body as string)).toEqual({
      requestKey: "rk",
      query: "code=a&state=s",
    })
    expect(
      (calls[0].init.headers as Record<string, string>)["x-neev-csrf"]
    ).toBe("csrf")
  })

  it("refuses a malformed callback before any request", async () => {
    const fetcher = vi.fn()
    vi.stubGlobal("fetch", fetcher)
    for (const args of [
      ["codex", "not-an-attempt", "rk", "code=a&state=s"],
      ["codex", ATTEMPT, "rk", ""],
      ["codex", ATTEMPT, "rk", "x".repeat(4097)],
      ["gemini", ATTEMPT, "rk", "code=a&state=s"],
      ["codex", ATTEMPT, "bad key!", "code=a&state=s"],
    ] as const)
      await expect(
        submitConnectCallback(args[0], args[1], args[2], args[3], "csrf")
      ).rejects.toBeInstanceOf(NativeClientError)
    expect(fetcher).not.toHaveBeenCalled()
  })

  it("tells a refused callback (422) apart from every other failure", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => json({ code: "callback_rejected" }, 422))
    )
    const error = await submitConnectCallback(
      "codex",
      ATTEMPT,
      "rk",
      "code=a&state=s",
      "csrf"
    ).catch((cause: unknown) => cause)
    expect(isCallbackRejected(error)).toBe(true)
    expect(isCallbackRejected(new NativeClientError("conflict", 409))).toBe(
      false
    )
  })
})

describe("the backend as built", () => {
  it("reads Codex's approval with no fallbackUrl key at all", () => {
    // `approval_dto` skips a None field rather than sending null.
    const { fallbackUrl: _omitted, ...wire } = codex
    void _omitted
    expect(parseApproval(wire, "codex")).toMatchObject({
      kind: "browser_callback",
      fallbackUrl: null,
    })
  })

  it("asks for the browser sign-in by name, and only where it exists", async () => {
    const bodies: unknown[] = []
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_url: string, init: RequestInit) => {
        bodies.push(JSON.parse(init.body as string))
        return json(
          {
            outcome: "pending",
            attemptId: ATTEMPT,
            expiresAt: 1_700_000_600,
            connection: null,
          },
          202
        )
      })
    )
    await startProviderConnect("claude", "k1", "csrf", undefined, "browser")
    await startProviderConnect("codex", "k2", "csrf", undefined, "browser")
    expect(bodies).toEqual([
      { requestKey: "k1", method: "browser" },
      { requestKey: "k2", method: "browser" },
    ])
    for (const provider of ["gemini", "cursor", "opencode"])
      await expect(
        startProviderConnect(provider, "k3", "csrf", undefined, "browser")
      ).rejects.toBeInstanceOf(NativeClientError)
    expect(bodies).toHaveLength(2)
  })
})

describe("callback failures (contract v2 §3)", () => {
  it("maps each status to what the UI must do", async () => {
    const { callbackFailure } = await import("./neev-agent-sessions-client")
    const { NativeContentNotFoundError } = await import("./neev-content-client")
    const at = (status: number) =>
      callbackFailure(new NativeClientError("unavailable", status))
    expect(at(400)).toBe("rejected")
    expect(at(422)).toBe("rejected")
    expect(at(409)).toBe("conflict")
    expect(at(429)).toBe("limited")
    expect(at(503)).toBe("unavailable")
    expect(callbackFailure(new NativeContentNotFoundError())).toBe("gone")
    // A timeout or no answer: the gate was not consumed either.
    expect(callbackFailure(new NativeClientError("unavailable"))).toBe(
      "unavailable"
    )
    // Refused before sending is ours, never mistaken for the server's 400.
    expect(callbackFailure(new NativeClientError("invalid_request"))).toBe(
      "invalid"
    )
    expect(callbackFailure(new Error("x"))).toBe("other")
  })
})

it("holds a callback query to the server's 1024-byte bound", async () => {
  const { NEEV_MAX_CALLBACK_QUERY_BYTES } =
    await import("./neev-agent-sessions-client")
  expect(NEEV_MAX_CALLBACK_QUERY_BYTES).toBe(1024)
  const approval = parseApproval(codex, "codex") as Extract<
    NeevApproval,
    { kind: "browser_callback" }
  >
  const at = (code: string) =>
    `http://localhost:1455/auth/callback?code=${code}&state=s`
  const fits = "a".repeat(1024 - "code=&state=s".length)
  expect(callbackQueryFromAddress(at(fits), approval)).not.toBeNull()
  expect(callbackQueryFromAddress(at(fits + "a"), approval)).toBeNull()
})
