import { afterEach, expect, it, vi } from "vitest"
import { NativeClientError } from "./neev-client"
import {
  forkAgentChat,
  forkChat,
  forkRefusal,
  parseAgentChatFork,
  parseChatFork,
} from "./neev-fork"

const A = "11111111-1111-4111-8111-111111111111"
const B = "22222222-2222-4222-8222-222222222222"
const C = "33333333-3333-4333-8333-333333333333"
const R = "44444444-4444-4444-8444-444444444444"

const chatFork = {
  chatId: B,
  sourceChatId: A,
  throughMessageId: C,
  nodeId: 7,
  copiedMessages: 2,
  copiedAttachments: 1,
  archivedAttachmentsNotCopied: 0,
  replayed: false,
}
const sessionFork = {
  sessionId: B,
  sourceSessionId: A,
  throughTurnId: C,
  copiedTurns: 2,
  copiedAttachments: 0,
  archivedAttachmentsNotCopied: 0,
  skippedIncompleteTurns: 1,
  replayed: false,
}

function respond(status: number, body: unknown) {
  const fetcher = vi.fn().mockResolvedValue(
    new Response(JSON.stringify(body), {
      status,
      headers: { "Content-Type": "application/json" },
    })
  )
  vi.stubGlobal("fetch", fetcher)
  return fetcher
}

afterEach(() => vi.unstubAllGlobals())

it("forks a chat with exactly the selection and request id, and parses the result", async () => {
  const fetcher = respond(201, chatFork)
  await expect(forkChat(A, C, R, "csrf")).resolves.toEqual(chatFork)
  const [path, init] = fetcher.mock.calls[0]
  expect(path).toBe(`/api/neev/chats/${A}/fork`)
  expect(init.method).toBe("POST")
  expect(init.headers["x-neev-csrf"]).toBe("csrf")
  expect(JSON.parse(init.body)).toEqual({ throughMessageId: C, requestId: R })
})

it("forks an Agent Chat at a turn", async () => {
  const fetcher = respond(201, sessionFork)
  await expect(forkAgentChat(A, C, R, "csrf")).resolves.toEqual(sessionFork)
  expect(fetcher.mock.calls[0][0]).toBe(`/api/neev/agent/sessions/${A}/fork`)
  expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
    throughTurnId: C,
    requestId: R,
  })
})

it("refuses malformed ids before making any request", async () => {
  const fetcher = respond(201, chatFork)
  for (const call of [
    () => forkChat("nope", C, R, "csrf"),
    () => forkChat(A, "../x", R, "csrf"),
    () => forkAgentChat(A, C, "not-a-uuid", "csrf"),
  ])
    await expect(call()).rejects.toMatchObject({ kind: "invalid_request" })
  expect(fetcher).not.toHaveBeenCalled()
})

it("names why a fork was refused, and nothing else", async () => {
  respond(409, { code: "fork_attachments_too_large" })
  const error = await forkChat(A, C, R, "csrf").catch((e: unknown) => e)
  expect(error).toBeInstanceOf(NativeClientError)
  expect(forkRefusal(error)).toBe("fork_attachments_too_large")
  respond(409, { code: "conflict" })
  expect(
    forkRefusal(await forkChat(A, C, R, "csrf").catch((e: unknown) => e))
  ).toBeNull()
  respond(404, { code: "not_found" })
  expect(
    forkRefusal(await forkChat(A, C, R, "csrf").catch((e: unknown) => e))
  ).toBeNull()
  expect(forkRefusal(new Error("x"))).toBeNull()
})

it("parses strictly: unknown fields, wrong types, a fork of itself or an empty fork are invalid", () => {
  expect(parseChatFork(chatFork)).toEqual(chatFork)
  for (const bad of [
    { ...chatFork, visibility: "organization" },
    { ...chatFork, chatId: A },
    { ...chatFork, copiedMessages: 0 },
    { ...chatFork, copiedAttachments: -1 },
    { ...chatFork, nodeId: 0 },
    { ...chatFork, replayed: "no" },
    { ...chatFork, throughMessageId: "x" },
    Object.fromEntries(
      Object.entries(chatFork).filter(([key]) => key !== "replayed")
    ),
    null,
    [],
  ])
    expect(() => parseChatFork(bad)).toThrow(NativeClientError)
  expect(parseAgentChatFork(sessionFork)).toEqual(sessionFork)
  for (const bad of [
    { ...sessionFork, sessionId: A },
    { ...sessionFork, copiedTurns: 0 },
    { ...sessionFork, skippedIncompleteTurns: 1.5 },
    { ...sessionFork, extra: 1 },
  ])
    expect(() => parseAgentChatFork(bad)).toThrow(NativeClientError)
})

it("names a target folder only when asked, and refuses a fork placed elsewhere", async () => {
  let fetcher = respond(201, chatFork)
  await forkChat(A, C, R, "csrf", undefined, 7)
  expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
    throughMessageId: C,
    requestId: R,
    nodeId: 7,
  })
  fetcher = respond(201, chatFork)
  await expect(forkChat(A, C, R, "csrf", undefined, 8)).rejects.toMatchObject({
    kind: "invalid_response",
  })
  fetcher = respond(201, chatFork)
  await expect(forkChat(A, C, R, "csrf", undefined, 0)).rejects.toMatchObject({
    kind: "invalid_request",
  })
  expect(fetcher).not.toHaveBeenCalled()
  respond(409, { code: "fork_no_location" })
  const refused = await forkChat(A, C, R, "csrf").catch((error) => error)
  expect(forkRefusal(refused)).toBe("fork_no_location")
})
