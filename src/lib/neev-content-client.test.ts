import { afterEach, describe, expect, it, vi } from "vitest"
import { NativeClientError } from "./neev-client"
import {
  contentGet,
  contentMutation,
  listNativeResources,
  parseMessage,
  parseResource,
  parseResourcePage,
  parseRevision,
} from "./neev-content-client"

const resource = {
  id: "ce39f83c-4511-4ff7-80d3-c06184ee8c50",
  orgId: 7,
  nodeId: 8,
  kind: "document",
  creatorId: 9,
  title: "Plan",
  visibility: "private",
  originChatId: null,
  authRevision: "1",
  contentRevision: "0",
  archived: false,
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
}

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe("native content wire validation", () => {
  it("accepts revision zero only for resource contentRevision", () => {
    expect(parseResource(resource).visibility).toBe("private")
    expect(() => parseResource({ ...resource, authRevision: "01" })).toThrow()
    expect(() => parseResource({ ...resource, orgId: -1 })).toThrow()
    expect(() =>
      parseResource({ ...resource, contentRevision: "01" })
    ).toThrow()
  })

  it.each([
    { ...resource, id: "CE39F83C-4511-4FF7-80D3-C06184EE8C50" },
    { ...resource, id: "not-a-uuid" },
    { ...resource, originChatId: "not-a-uuid" },
    { ...resource, nodeId: 0 },
    { ...resource, kind: "file" },
    { ...resource, visibility: "public" },
  ])("rejects malformed resource identities and enums", (value) => {
    expect(() => parseResource(value)).toThrow()
  })

  it("validates message and revision integers as canonical strings", () => {
    expect(
      parseMessage({
        id: "f4713f49-d18b-4d49-a586-aab9d4eedba6",
        sequence: "1",
        authorId: 9,
        authorName: "Sagar",
        body: "hello",
        createdAt: 1_700_000_000,
      }).sequence
    ).toBe("1")
    expect(() =>
      parseMessage({
        id: "f4713f49-d18b-4d49-a586-aab9d4eedba6",
        sequence: "0",
        authorId: 9,
        authorName: "Sagar",
        body: "hello",
        createdAt: 1_700_000_000,
      })
    ).toThrow()
    expect(
      parseRevision({
        revision: "1",
        authorId: 9,
        authorName: "Sagar",
        byteLength: "0",
        sha256: "a".repeat(64),
        createdAt: 1_700_000_000,
        parentRevision: null,
        restoredFrom: null,
      }).byteLength
    ).toBe("0")
  })

  it("accepts only bounded opaque resource cursors", () => {
    expect(
      parseResourcePage({ items: [], nextAfter: null }).nextAfter
    ).toBeNull()
    expect(() =>
      parseResourcePage({ items: [], nextAfter: "cursor/value" })
    ).toThrow()
  })

  it("classifies resource 404 separately from session 401 without reading bodies", async () => {
    const json = vi.spyOn(Response.prototype, "json")
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(
          new Response("private path leaked", { status: 404 })
        )
        .mockResolvedValueOnce(new Response("expired session", { status: 401 }))
    )
    await expect(
      contentGet("/api/neev/resources/ce39f83c-4511-4ff7-80d3-c06184ee8c50")
    ).rejects.toMatchObject({
      kind: "not_found",
    })
    await expect(
      contentGet("/api/neev/resources/ce39f83c-4511-4ff7-80d3-c06184ee8c50")
    ).rejects.toMatchObject({
      kind: "unauthorized",
    })
    expect(json).not.toHaveBeenCalled()
  })

  it("uses the session CSRF contract for mutations", async () => {
    const fetcher = vi.fn(
      async () =>
        new Response(JSON.stringify(resource), {
          headers: { "Content-Type": "application/json" },
        })
    )
    vi.stubGlobal("fetch", fetcher)
    await contentMutation(
      "/api/neev/resources/ce39f83c-4511-4ff7-80d3-c06184ee8c50",
      { expectedAuthRevision: "1" },
      "csrf",
      undefined,
      "PATCH"
    )
    expect(fetcher).toHaveBeenCalledWith(
      expect.any(String),
      expect.objectContaining({
        method: "PATCH",
        credentials: "same-origin",
        cache: "no-store",
        headers: expect.objectContaining({ "x-neev-csrf": "csrf" }),
      })
    )
  })

  it("never includes response bodies in thrown errors", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("secret markdown", { status: 503 }))
    )
    let error: unknown
    try {
      await contentGet(
        "/api/neev/resources/ce39f83c-4511-4ff7-80d3-c06184ee8c50"
      )
    } catch (caught) {
      error = caught
    }
    expect(error).toBeInstanceOf(NativeClientError)
    expect(String(error)).not.toContain("secret markdown")
  })

  it("rejects a response outside the requested organization before returning it", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({
              items: [{ ...resource, orgId: 99 }],
              nextAfter: null,
            }),
            { headers: { "Content-Type": "application/json" } }
          )
      )
    )
    await expect(listNativeResources(7, 8, "document")).rejects.toMatchObject({
      kind: "invalid_response",
    })
  })
})
