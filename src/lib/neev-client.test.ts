import { afterEach, describe, expect, it, vi } from "vitest"
import {
  loadNativeSession,
  loadServerCapabilities,
  nativeRequest,
  parseNode,
  parseOrganization,
  NativeClientError,
} from "./neev-client"

const session = {
  user: { id: 1, displayName: "Sagar", avatarUrl: null },
  organization: { id: 3, name: "Neev demo", avatarUrl: null },
  role: "admin",
  scope: "organization",
  csrf: "synthetic-csrf",
}
afterEach(() => {
  vi.unstubAllGlobals()
  vi.useRealTimers()
})
function reply(value: unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn(
      async () =>
        new Response(JSON.stringify(value), {
          headers: { "Content-Type": "application/json" },
        })
    )
  )
}
describe("native wire contracts", () => {
  it("accepts the server bootstrap without a GitHub identity", async () => {
    reply(session)
    expect(await loadNativeSession()).toEqual(session)
  })
  it.each([
    { ...session, scope: "future" },
    { ...session, scope: "identity" },
    { ...session, organization: null },
    { ...session, role: "owner" },
    { ...session, user: { ...session.user, id: 2147483648 } },
    { ...session, user: { ...session.user, id: 0 } },
    { ...session, csrf: "" },
  ])(
    "rejects inconsistent authority instead of downgrading to onboarding",
    async (value) => {
      reply(value)
      await expect(loadNativeSession()).rejects.toMatchObject({
        kind: "invalid_response",
      })
    }
  )
  it("discriminates native capabilities without a legacy mode field", async () => {
    reply({
      version: 1,
      identityMode: "native",
      runtimeEnabled: false,
      providers: { google: true, email: true, github: false },
      catalog: {
        nodeKinds: ["project", "folder"],
        repositoryDefaults: false,
        chats: false,
      },
    })
    expect(await loadServerCapabilities()).toMatchObject({
      identityMode: "native",
    })
  })
  it("fails closed for an unknown identity mode even with operator mode", async () => {
    reply({
      version: 1,
      identityMode: "future",
      mode: "operator",
      runtimeEnabled: true,
    })
    await expect(loadServerCapabilities()).rejects.toBeInstanceOf(
      NativeClientError
    )
  })
  it.each([1, "01", "0", "-1", "9223372036854775808"])(
    "rejects noncanonical or out-of-range revision %s",
    (revision) => {
      expect(() =>
        parseNode({
          id: 2,
          orgId: 3,
          parentId: null,
          kind: "project",
          name: "Work",
          revision,
        })
      ).toThrow()
      expect(() =>
        parseOrganization({
          id: 3,
          name: "Work",
          avatarUrl: null,
          ownerUserId: 1,
          brandingRevision: revision,
        })
      ).toThrow()
    }
  )
  it("aborts an unresponsive request at 65 seconds", async () => {
    vi.useFakeTimers()
    let signal: AbortSignal | undefined
    vi.stubGlobal(
      "fetch",
      vi.fn(
        (_url, init) =>
          new Promise((_resolve, reject) => {
            signal = init.signal
            signal!.addEventListener("abort", () =>
              reject(new DOMException("Aborted", "AbortError"))
            )
          })
      )
    )
    const pending = expect(
      nativeRequest("/api/neev/session")
    ).rejects.toMatchObject({ kind: "unavailable" })
    await vi.advanceTimersByTimeAsync(65000)
    await pending
    expect(signal?.aborted).toBe(true)
  })
  it("never sends a native request to a legacy or external route", async () => {
    const fetcher = vi.fn()
    vi.stubGlobal("fetch", fetcher)
    await expect(nativeRequest("/api/team/session")).rejects.toMatchObject({
      kind: "invalid_request",
    })
    expect(fetcher).not.toHaveBeenCalled()
  })
})
