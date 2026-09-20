import { renderHook, waitFor } from "@testing-library/react"
import { afterEach, expect, it, vi } from "vitest"
import { useNeevMessageStream } from "./use-neev-message-stream"

const CHAT = "11111111-1111-4111-8111-111111111111"

afterEach(() => {
  vi.unstubAllGlobals()
})

/** A server-controlled SSE body, so a test can push frames one at a time. */
function eventStream() {
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

const FAST_RETRIES = [10, 10, 10, 10] as const

function mount(onNotify: () => void, enabled = true) {
  return renderHook(() =>
    useNeevMessageStream({
      chatId: CHAT,
      enabled,
      onNotify,
      retryDelays: FAST_RETRIES,
    })
  )
}

it("claims live only while the stream is open and reconciles each advance", async () => {
  const stream = eventStream()
  const fetcher = vi.fn(async () => stream.response)
  vi.stubGlobal("fetch", fetcher)
  const onNotify = vi.fn()
  const { result } = mount(onNotify)
  stream.push({ type: "sync", sequence: "0" })
  await waitFor(() => expect(result.current.status).toBe("live"))
  // The opening sync closes any gap opened while the client was disconnected.
  await waitFor(() => expect(onNotify).toHaveBeenCalledTimes(1))
  expect(fetcher).toHaveBeenCalledWith(
    `/api/neev/chats/${CHAT}/stream`,
    expect.objectContaining({ cache: "no-store", credentials: "same-origin" })
  )
  // An idle heartbeat repeating the same cursor must not cause a refetch.
  stream.push({ type: "sync", sequence: "0" })
  stream.push({ type: "message", sequence: "1" })
  await waitFor(() => expect(onNotify).toHaveBeenCalledTimes(2))
  // A dropped-behind subscriber is told to re-read from its own cursor.
  stream.push({ type: "resync" })
  await waitFor(() => expect(onNotify).toHaveBeenCalledTimes(3))
  expect(result.current.status).toBe("live")
})

it("stops on revocation and asks for exactly one reconciling read", async () => {
  const stream = eventStream()
  const fetcher = vi.fn(async () => stream.response)
  vi.stubGlobal("fetch", fetcher)
  const onNotify = vi.fn()
  const { result } = mount(onNotify)
  stream.push({ type: "sync", sequence: "3" })
  await waitFor(() => expect(result.current.status).toBe("live"))
  stream.push({ type: "revoked" })
  // The refetch is what surfaces the denial; the hook never keeps delivering.
  await waitFor(() => expect(result.current.status).toBe("off"))
  expect(onNotify).toHaveBeenCalledTimes(2)
  stream.push({ type: "message", sequence: "4" })
  await new Promise((resolve) => setTimeout(resolve, 20))
  expect(onNotify).toHaveBeenCalledTimes(2)
  expect(fetcher).toHaveBeenCalledTimes(1)
})

it("falls back to polling when the deployment does not push", async () => {
  const fetcher = vi.fn(
    async () => new Response('{"code":"not_found"}', { status: 404 })
  )
  vi.stubGlobal("fetch", fetcher)
  const onNotify = vi.fn()
  const { result } = mount(onNotify)
  await waitFor(() => expect(result.current.status).toBe("polling"))
  // A rolled-back server is not retried in a loop.
  await new Promise((resolve) => setTimeout(resolve, 20))
  expect(fetcher).toHaveBeenCalledTimes(1)
  expect(onNotify).not.toHaveBeenCalled()
})

it("falls back to polling when the route answers with something else", async () => {
  // The SPA fallback can serve HTML where the stream route used to be.
  const fetcher = vi.fn(
    async () =>
      new Response("<!doctype html>", {
        status: 200,
        headers: { "Content-Type": "text/html" },
      })
  )
  vi.stubGlobal("fetch", fetcher)
  const { result } = mount(vi.fn())
  await waitFor(() => expect(result.current.status).toBe("polling"))
})

it("reconnects immediately when a healthy stream reaches its lifetime", async () => {
  const first = eventStream()
  const second = eventStream()
  const fetcher = vi
    .fn()
    .mockResolvedValueOnce(first.response)
    .mockResolvedValue(second.response)
  vi.stubGlobal("fetch", fetcher)
  const onNotify = vi.fn()
  const { result } = mount(onNotify)
  first.push({ type: "sync", sequence: "2" })
  await waitFor(() => expect(result.current.status).toBe("live"))
  first.close()
  await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(2))
  // The reconnect re-syncs, which is how a gap during the drop is closed.
  second.push({ type: "sync", sequence: "5" })
  await waitFor(() => expect(onNotify).toHaveBeenCalledTimes(2))
})

it("reports reconnecting and then polling when the stream keeps failing", async () => {
  const fetcher = vi.fn(async () => {
    throw new TypeError("network down")
  })
  vi.stubGlobal("fetch", fetcher)
  const { result } = mount(vi.fn())
  // A transient failure is announced honestly, never hidden behind "live".
  await waitFor(() => expect(result.current.status).toBe("reconnecting"))
  await waitFor(() => expect(result.current.status).toBe("polling"))
  // One attempt plus one retry per backoff step, then it gives up for good.
  expect(fetcher).toHaveBeenCalledTimes(FAST_RETRIES.length + 1)
})

it("opens nothing while disabled and cancels the request on unmount", async () => {
  const stream = eventStream()
  let init: RequestInit | undefined
  const fetcher = vi.fn(async (url: string, options: RequestInit) => {
    expect(url).toBe(`/api/neev/chats/${CHAT}/stream`)
    init = options
    return stream.response
  })
  vi.stubGlobal("fetch", fetcher)
  const disabled = mount(vi.fn(), false)
  expect(disabled.result.current.status).toBe("off")
  expect(fetcher).not.toHaveBeenCalled()
  disabled.unmount()

  const live = mount(vi.fn())
  await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1))
  expect(init?.signal?.aborted).toBe(false)
  live.unmount()
  // Cancelling the component tears the request down instead of leaking it.
  expect(init?.signal?.aborted).toBe(true)
})
