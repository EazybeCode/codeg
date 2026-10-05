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
  parseDocumentContent,
  saveNativeDocument,
  runNativeAgent,
  cancelNativeAgent,
  openNativeMessageStream,
  parseMessageStreamEvent,
  type NativeAgentEvent,
} from "./neev-content-client"

const revision = {
  revision: "1",
  authorId: 9,
  authorName: "Sagar",
  byteLength: "4",
  sha256: "19b25856e1c150ca834cffc8b59b23adbd0ec0389e58eb22b3b64768098d002b",
  createdAt: 1_700_000_000,
  parentRevision: null,
  restoredFrom: null,
  format: "markdown",
  formatVersion: 1,
}
const rich = {
  version: 1,
  blocks: [
    {
      type: "paragraph",
      id: "00000000-0000-0000-0000-000000000000",
      content: [{ text: "Hello", styles: [], href: null }],
      children: [],
    },
  ],
}

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
  it("aborts a pending rich save without returning a late response", async () => {
    const controller = new AbortController()
    vi.stubGlobal(
      "fetch",
      vi.fn(
        (_path, init: RequestInit) =>
          new Promise((_resolve, reject) => {
            init.signal?.addEventListener("abort", () =>
              reject(new DOMException("Aborted", "AbortError"))
            )
          })
      )
    )
    const pending = saveNativeDocument(
      resource.id,
      {
        requestId: resource.id,
        expectedContentRevision: "3",
        expectedAuthRevision: "2",
        document: rich,
      } as Parameters<typeof saveNativeDocument>[1],
      "csrf",
      controller.signal
    )
    controller.abort()
    await expect(pending).rejects.toMatchObject({ kind: "aborted" })
  })

  it("requires format agreement and canonical rich byte length, not export length", () => {
    expect(
      parseDocumentContent({ revision, markdown: "seed", document: null })
        .document
    ).toBeNull()
    const richRevision = {
      ...revision,
      format: "blocks",
      byteLength: "156",
      sha256:
        "8f05bf0f0f2475feff5738cafac55f3e4aea98a3be5ddd4a59d23697aef8e6ea",
    }
    expect(
      parseDocumentContent({
        revision: richRevision,
        markdown: "Hello\n",
        document: rich,
      }).document
    ).toEqual(rich)
    for (const body of [
      {
        revision: richRevision,
        markdown: "Hello\n",
        document: { version: 2, blocks: [] },
      },
      { revision, markdown: "seed", document: { version: 1, blocks: [] } },
      {
        revision: { ...richRevision, byteLength: "6" },
        markdown: "Hello\n",
        document: rich,
      },
      {
        revision: { ...revision, format: "html" },
        markdown: "seed",
        document: null,
      },
      {
        revision: { ...revision, formatVersion: 2 },
        markdown: "seed",
        document: null,
      },
      { revision: richRevision, markdown: "x".repeat(1048577), document: rich },
    ])
      expect(() => parseDocumentContent(body)).toThrow()
  })

  it("sends exactly one body with pinned identity, head, auth and abort signal", async () => {
    const fetcher = vi.fn(
      async () =>
        new Response(
          JSON.stringify({
            ...revision,
            format: "blocks",
            byteLength: "156",
            sha256:
              "8f05bf0f0f2475feff5738cafac55f3e4aea98a3be5ddd4a59d23697aef8e6ea",
          }),
          { headers: { "Content-Type": "application/json" } }
        )
    )
    vi.stubGlobal("fetch", fetcher)
    const input = {
      requestId: resource.id,
      expectedContentRevision: "0",
      expectedAuthRevision: "1",
      document: rich,
    }
    const signal = new AbortController().signal
    await saveNativeDocument(
      resource.id,
      input as Parameters<typeof saveNativeDocument>[1],
      "csrf",
      signal
    )
    expect(fetcher).toHaveBeenCalledWith(
      expect.any(String),
      expect.objectContaining({
        body: JSON.stringify(input),
        signal,
        cache: "no-store",
        headers: expect.objectContaining({ "x-neev-csrf": "csrf" }),
      })
    )
    for (const bad of [
      { ...input, markdown: "both" },
      { ...input, document: undefined },
      { ...input, extra: true },
    ]) {
      await expect(
        saveNativeDocument(
          resource.id,
          bad as Parameters<typeof saveNativeDocument>[1],
          "csrf"
        )
      ).rejects.toMatchObject({ kind: "invalid_request" })
    }
    expect(fetcher).toHaveBeenCalledTimes(1)
  })

  it("accepts member/team sharing as its own audience, and nothing unknown", () => {
    expect(
      parseResource({ ...resource, visibility: "shared" }).visibility
    ).toBe("shared")
    expect(() => parseResource({ ...resource, visibility: "public" })).toThrow()
  })

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

  it("labels agent-authored messages and tolerates pre-rollout human rows", () => {
    const base = {
      id: "f4713f49-d18b-4d49-a586-aab9d4eedba6",
      sequence: "1",
      authorId: 9,
      authorName: "Sagar",
      body: "hello",
      createdAt: 1_700_000_000,
    }
    // A server that has not shipped §5.3 yet sends six keys; that is a human
    // message, unambiguously.
    expect(parseMessage(base)).toMatchObject({
      authorKind: "human",
      agentLabel: null,
    })
    expect(
      parseMessage({ ...base, authorKind: "human", agentLabel: null })
    ).toMatchObject({ authorKind: "human", agentLabel: null })
    // An agent reply keeps the invoker as author of record and names the
    // provider that wrote it.
    expect(
      parseMessage({ ...base, authorKind: "agent", agentLabel: "claude" })
    ).toMatchObject({ authorKind: "agent", agentLabel: "claude", authorId: 9 })
    for (const bad of [
      // The pair must agree: a kind without its label cannot be rendered
      // truthfully, and neither can a label on a human row.
      { ...base, authorKind: "agent", agentLabel: null },
      { ...base, authorKind: "human", agentLabel: "claude" },
      { ...base, authorKind: "robot", agentLabel: "claude" },
      { ...base, authorKind: "agent", agentLabel: "Claude Opus!" },
      // Half-present authorship is a broken contract, not a legacy row.
      { ...base, authorKind: "agent" },
      { ...base, agentLabel: "claude" },
    ])
      expect(() => parseMessage(bad)).toThrow(NativeClientError)
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
        format: "markdown",
        formatVersion: 1,
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

describe("agent runtime client", () => {
  const chatId = "123e4567-e89b-42d3-a456-426614174000"

  it("streams started/chunk/completed events over SSE", async () => {
    const sse =
      'data: {"type":"started","run_id":"r1"}\n\n' +
      'data: {"type":"chunk","text":"hel"}\n\n' +
      'data: {"type":"chunk","text":"lo"}\n\n' +
      'data: {"type":"completed"}\n\n'
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(sse))
        controller.close()
      },
    })
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response(body, { status: 200 }))
    )
    const events: NativeAgentEvent[] = []
    await runNativeAgent(
      { chatId, prompt: "hi", provider: "claude", model: "m" },
      "csrf",
      (event) => events.push(event)
    )
    expect(events).toEqual([
      { type: "started", runId: "r1" },
      { type: "chunk", text: "hel" },
      { type: "chunk", text: "lo" },
      { type: "completed" },
    ])
  })

  it("throws unavailable when the runtime is disabled (503)", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response("{}", { status: 503 }))
    )
    await expect(
      runNativeAgent(
        { chatId, prompt: "hi", provider: "c", model: "m" },
        "csrf",
        () => {}
      )
    ).rejects.toMatchObject({ kind: "unavailable" })
  })

  it("cancelNativeAgent posts the run id and returns canceled", async () => {
    const fetcher = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ canceled: true }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      })
    )
    vi.stubGlobal("fetch", fetcher)
    await expect(cancelNativeAgent("r1", "csrf")).resolves.toBe(true)
    expect(fetcher).toHaveBeenCalledWith(
      "/api/neev/agent/cancel",
      expect.objectContaining({ method: "POST" })
    )
  })
})

describe("team message stream client", () => {
  const chatId = "123e4567-e89b-42d3-a456-426614174000"
  afterEach(() => vi.unstubAllGlobals())

  function sse(...frames: string[]) {
    return new Response(frames.map((frame) => `data: ${frame}\n\n`).join(""), {
      status: 200,
      headers: { "Content-Type": "text/event-stream" },
    })
  }

  it("accepts only the four content-free notice shapes", () => {
    expect(parseMessageStreamEvent('{"type":"sync","sequence":"0"}')).toEqual({
      type: "sync",
      sequence: "0",
    })
    expect(
      parseMessageStreamEvent('{"type":"message","sequence":"12"}')
    ).toEqual({ type: "message", sequence: "12" })
    expect(parseMessageStreamEvent('{"type":"resync"}')).toEqual({
      type: "resync",
    })
    expect(parseMessageStreamEvent('{"type":"revoked"}')).toEqual({
      type: "revoked",
    })
  })

  it("rejects malformed, unknown, and out-of-range notices", () => {
    for (const frame of [
      "not json",
      "[]",
      '{"type":"chunk","text":"hi"}',
      '{"type":"message"}',
      // A message sequence is always a positive canonical integer.
      '{"type":"message","sequence":"0"}',
      '{"type":"message","sequence":1}',
      '{"type":"message","sequence":"01"}',
      '{"type":"sync","sequence":"-1"}',
    ])
      expect(parseMessageStreamEvent(frame)).toBeNull()
  })

  it("streams notices and never carries message content", async () => {
    const fetcher = vi
      .fn()
      .mockResolvedValue(
        sse(
          '{"type":"sync","sequence":"2"}',
          '{"type":"message","sequence":"3"}',
          '{"type":"chunk","text":"secret"}',
          '{"type":"revoked"}'
        )
      )
    vi.stubGlobal("fetch", fetcher)
    const events: unknown[] = []
    await openNativeMessageStream(chatId, (event) => events.push(event))
    // The unknown "chunk" frame is dropped rather than surfaced as content.
    expect(events).toEqual([
      { type: "sync", sequence: "2" },
      { type: "message", sequence: "3" },
      { type: "revoked" },
    ])
    expect(fetcher).toHaveBeenCalledWith(
      `/api/neev/chats/${chatId}/stream`,
      expect.objectContaining({ credentials: "same-origin" })
    )
  })

  it("refuses a non-canonical chat id before any request", async () => {
    const fetcher = vi.fn()
    vi.stubGlobal("fetch", fetcher)
    await expect(
      openNativeMessageStream("../agent/run", () => {})
    ).rejects.toBeInstanceOf(NativeClientError)
    expect(fetcher).not.toHaveBeenCalled()
  })

  it("reports push as unavailable for capacity, denial, and rollback", async () => {
    for (const [status, kind] of [
      [503, "unavailable"],
      [401, "unauthorized"],
      [403, "forbidden"],
      [404, "not_found"],
    ] as const) {
      vi.stubGlobal(
        "fetch",
        vi.fn().mockResolvedValue(new Response("{}", { status }))
      )
      await expect(
        openNativeMessageStream(chatId, () => {})
      ).rejects.toMatchObject({ kind })
    }
  })
})
