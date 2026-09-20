import { act, renderHook, waitFor } from "@testing-library/react"
import { afterEach, expect, it, vi } from "vitest"
import { useNeevMessageStream } from "./use-neev-message-stream"

const CHAT = "11111111-1111-4111-8111-111111111111"

afterEach(() => {
  vi.useRealTimers()
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

/** An accepted stream that hangs up at once, optionally after a valid frame. */
function hangsUpAtOnce(...frames: unknown[]) {
  const encoder = new TextEncoder()
  return new Response(
    new ReadableStream<Uint8Array>({
      start(c) {
        for (const frame of frames)
          c.enqueue(encoder.encode(`data: ${JSON.stringify(frame)}\n\n`))
        c.close()
      },
    }),
    { status: 200, headers: { "Content-Type": "text/event-stream" } }
  )
}

/** Let pending promises run without letting the clock move at all. */
async function settle(times = 20) {
  await act(async () => {
    for (let index = 0; index < times; index += 1)
      await vi.advanceTimersByTimeAsync(0)
  })
}

const FAST_RETRIES = [10, 10, 10, 10] as const

function mount(
  onNotify: () => void,
  options: { enabled?: boolean; retryDelays?: readonly number[] } = {}
) {
  const { enabled = true, retryDelays = FAST_RETRIES } = options
  return renderHook(() =>
    useNeevMessageStream({ chatId: CHAT, enabled, onNotify, retryDelays })
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

it("backs off and gives up when the server accepts and hangs up at once", async () => {
  // Nothing here throws: the request succeeds, the content type is right, the
  // body is simply empty and closed. Reconnecting on that without counting it
  // as a failure would spin the loop at full speed.
  const fetcher = vi.fn(async () => hangsUpAtOnce())
  vi.stubGlobal("fetch", fetcher)
  const { result } = mount(vi.fn())
  await waitFor(() => expect(result.current.status).toBe("polling"))
  expect(fetcher).toHaveBeenCalledTimes(FAST_RETRIES.length + 1)
})

it("backs off when each connection sends a valid sync and then hangs up", async () => {
  const fetcher = vi.fn(async () =>
    hangsUpAtOnce({ type: "sync", sequence: "1" })
  )
  vi.stubGlobal("fetch", fetcher)
  const onNotify = vi.fn()
  const { result } = mount(onNotify)
  await waitFor(() => expect(result.current.status).toBe("polling"))
  // A delivered frame does not make a one-shot connection healthy.
  expect(fetcher).toHaveBeenCalledTimes(FAST_RETRIES.length + 1)
  // The cursor is remembered across attempts, so the repeats cost one read.
  expect(onNotify).toHaveBeenCalledTimes(1)
})

it("reconnects with no backoff after a connection that stayed healthy", async () => {
  vi.useFakeTimers()
  const first = eventStream()
  const second = eventStream()
  const fetcher = vi
    .fn()
    .mockResolvedValueOnce(first.response)
    .mockResolvedValue(second.response)
  vi.stubGlobal("fetch", fetcher)
  const onNotify = vi.fn()
  const { result } = mount(onNotify)
  await settle()
  first.push({ type: "sync", sequence: "2" })
  await settle()
  expect(result.current.status).toBe("live")
  // It lives well past the healthy floor, then ends the way the server's
  // bounded lifetime ends it.
  await act(async () => {
    await vi.advanceTimersByTimeAsync(6_000)
  })
  first.close()
  await settle()
  // `settle` never moves the clock, so a backoff timer could not have fired:
  // the reconnect happened straight away.
  expect(fetcher).toHaveBeenCalledTimes(2)
  second.push({ type: "sync", sequence: "5" })
  await settle()
  expect(onNotify).toHaveBeenCalledTimes(2)
  expect(result.current.status).toBe("live")
})

it("reports reconnecting between backed-off attempts, then polling", async () => {
  vi.useFakeTimers()
  const fetcher = vi.fn(async () => {
    throw new TypeError("network down")
  })
  vi.stubGlobal("fetch", fetcher)
  const { result } = mount(vi.fn(), { retryDelays: [1_000, 1_000] })
  await settle()
  expect(fetcher).toHaveBeenCalledTimes(1)
  // A transient failure is announced honestly, never hidden behind "live", and
  // the next attempt genuinely waits out its backoff.
  expect(result.current.status).toBe("reconnecting")
  await act(async () => {
    await vi.advanceTimersByTimeAsync(1_000)
  })
  await settle()
  expect(fetcher).toHaveBeenCalledTimes(2)
  expect(result.current.status).toBe("reconnecting")
  await act(async () => {
    await vi.advanceTimersByTimeAsync(1_000)
  })
  await settle()
  // One attempt per backoff step plus the first, then it gives up for good.
  expect(fetcher).toHaveBeenCalledTimes(3)
  expect(result.current.status).toBe("polling")
})

it("settles its pending backoff on unmount instead of stranding it", async () => {
  vi.useFakeTimers()
  const fetcher = vi.fn(async () => hangsUpAtOnce())
  vi.stubGlobal("fetch", fetcher)
  const view = mount(vi.fn(), { retryDelays: [10_000] })
  await settle()
  expect(fetcher).toHaveBeenCalledTimes(1)
  expect(vi.getTimerCount()).toBeGreaterThan(0)
  view.unmount()
  // Teardown clears the backoff timer and resolves the sleep, so nothing is
  // left pending and no further attempt is made.
  expect(vi.getTimerCount()).toBe(0)
  await act(async () => {
    await vi.advanceTimersByTimeAsync(60_000)
  })
  expect(fetcher).toHaveBeenCalledTimes(1)
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
  const disabled = mount(vi.fn(), { enabled: false })
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
