import { afterEach, describe, expect, it, vi } from "vitest"
import { NativeClientError } from "./neev-client"
import { NativeContentNotFoundError } from "./neev-content-client"
import {
  cancelAgentRun,
  followAgentRun,
  invokeTeamAgent,
  isSupportedModel,
  createAgentSession,
  listAgentSessions,
  listAgentTurns,
  loadAgentConnections,
  openAgentRunStream,
  parseAgentConnections,
  parseAgentSession,
  parseAgentTurn,
  parseRunEvent,
  parseStartedTurn,
  startAgentTurn,
  updateAgentSession,
  type NeevRunEvent,
} from "./neev-agent-sessions-client"

const SESSION_ID = "123e4567-e89b-42d3-a456-426614174000"
const TURN_ID = "223e4567-e89b-42d3-a456-426614174001"
const RUN_ID = "0123456789abcdef0123456789abcdef"

const session = {
  id: SESSION_ID,
  orgId: 3,
  ownerId: 1,
  title: "Refactor the parser",
  provider: "claude",
  model: "default",
  boundChatId: null,
  archived: false,
  revision: "1",
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
}

const turn = {
  id: TURN_ID,
  sessionId: SESSION_ID,
  sequence: "1",
  runId: RUN_ID,
  prompt: "Explain this file",
  response: "",
  status: "running",
  errorCode: null,
  provider: "claude",
  model: "default",
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
  completedAt: null,
}

const settled = {
  ...turn,
  response: "Here is the summary",
  status: "completed",
  completedAt: 1_700_000_050,
}

function json(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  })
}

function sse(...frames: string[]) {
  return new Response(frames.map((frame) => `data: ${frame}\n\n`).join(""), {
    status: 200,
    headers: { "Content-Type": "text/event-stream" },
  })
}

afterEach(() => vi.unstubAllGlobals())

describe("agent session wire validation", () => {
  it("accepts the server's session and turn shapes", () => {
    expect(parseAgentSession(session)).toEqual(session)
    expect(parseAgentTurn(turn)).toEqual(turn)
    expect(parseAgentTurn(settled)).toEqual(settled)
  })

  it("refuses sessions and turns that could not have come from the server", () => {
    for (const bad of [
      { ...session, id: "not-a-uuid" },
      { ...session, revision: "0" },
      { ...session, title: "" },
      { ...session, archived: "no" },
      // An unexpected field means the contract moved; refuse rather than guess.
      { ...session, extra: 1 },
    ])
      expect(() => parseAgentSession(bad)).toThrow(NativeClientError)
    for (const bad of [
      { ...turn, status: "queued" },
      { ...turn, runId: "0123" },
      // A running turn that claims a completion time, and a settled turn that
      // claims none: either way the row cannot be rendered truthfully.
      { ...turn, completedAt: 1_700_000_050 },
      { ...settled, completedAt: null },
      { ...turn, errorCode: "Something went wrong" },
    ])
      expect(() => parseAgentTurn(bad)).toThrow(NativeClientError)
  })

  it("refuses a started-turn envelope whose run id is not the turn's", () => {
    expect(parseStartedTurn({ turn, runId: RUN_ID })).toEqual({
      turn,
      runId: RUN_ID,
    })
    expect(() =>
      parseStartedTurn({
        turn,
        runId: "ffffffffffffffffffffffffffffffff",
      })
    ).toThrow(NativeClientError)
  })

  it("never lets a disabled runtime report a usable connection", () => {
    const connected = {
      provider: "claude",
      state: "connected",
      accountLabel: "sagar@example.test",
      checkedAt: 1_700_000_000,
      errorCode: null,
    }
    expect(
      parseAgentConnections({ runtimeEnabled: true, connections: [connected] })
    ).toEqual({ runtimeEnabled: true, connections: [connected] })
    expect(() =>
      parseAgentConnections({
        runtimeEnabled: false,
        connections: [connected],
      })
    ).toThrow(NativeClientError)
    expect(
      parseAgentConnections({
        runtimeEnabled: false,
        connections: [{ ...connected, state: "disabled" }],
      }).connections[0].state
    ).toBe("disabled")
    expect(() =>
      parseAgentConnections({
        runtimeEnabled: true,
        connections: [{ ...connected, state: "ready" }],
      })
    ).toThrow(NativeClientError)
  })

  it("accepts every run event the server emits and drops the rest", () => {
    expect(
      parseRunEvent(
        `{"type":"started","runId":"${RUN_ID}","turnId":"${TURN_ID}","sequence":"2"}`
      )
    ).toEqual({
      type: "started",
      runId: RUN_ID,
      turnId: TURN_ID,
      sequence: "2",
    })
    // A live reattach omits the sequence.
    expect(
      parseRunEvent(
        `{"type":"started","runId":"${RUN_ID}","turnId":"${TURN_ID}"}`
      )
    ).toEqual({
      type: "started",
      runId: RUN_ID,
      turnId: TURN_ID,
      sequence: null,
    })
    expect(parseRunEvent('{"type":"chunk","text":"hi"}')).toEqual({
      type: "chunk",
      text: "hi",
    })
    expect(
      parseRunEvent(
        '{"type":"finished","status":"failed","errorCode":"disabled"}'
      )
    ).toEqual({ type: "finished", status: "failed", errorCode: "disabled" })
    expect(
      parseRunEvent('{"type":"finished","status":"completed","errorCode":null}')
    ).toEqual({ type: "finished", status: "completed", errorCode: null })
    for (const bad of [
      "nonsense",
      "[]",
      '{"type":"progress"}',
      '{"type":"finished","status":"queued","errorCode":null}',
      '{"type":"started","runId":"short","turnId":"x"}',
    ])
      expect(parseRunEvent(bad)).toBeNull()
  })
})

describe("agent session requests", () => {
  it("lists sessions and asks for archived ones explicitly", async () => {
    const urls: string[] = []
    // A Response body reads once, so build a fresh one per call.
    const fetcher = vi.fn(async (url: string) => {
      urls.push(url)
      return json({ sessions: [session] })
    })
    vi.stubGlobal("fetch", fetcher)
    expect(await listAgentSessions()).toEqual([session])
    await listAgentSessions(true)
    expect(urls).toEqual([
      "/api/neev/agent/sessions",
      "/api/neev/agent/sessions?archived=1",
    ])
  })

  it("omits the optional binding rather than sending an unexpected null", async () => {
    const fetcher = vi.fn().mockResolvedValue(json(session, 201))
    vi.stubGlobal("fetch", fetcher)
    await createAgentSession(
      {
        title: "New",
        provider: "claude",
        model: "default",
        requestId: SESSION_ID,
      },
      "csrf"
    )
    // The server declares `deny_unknown_fields`; an absent optional is fine but
    // a stray key is a 400.
    expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
      title: "New",
      provider: "claude",
      model: "default",
      requestId: SESSION_ID,
    })
  })

  it("refuses an unsupported provider before any request", async () => {
    const fetcher = vi.fn()
    vi.stubGlobal("fetch", fetcher)
    await expect(
      createAgentSession(
        {
          title: "New",
          // Only the providers the runtime can dispatch may be offered.
          provider: "gemini" as "claude",
          model: "default",
          requestId: SESSION_ID,
        },
        "csrf"
      )
    ).rejects.toBeInstanceOf(NativeClientError)
    expect(fetcher).not.toHaveBeenCalled()
  })

  it("sends exactly one changed field with the expected revision", async () => {
    const calls: { url: string; init: RequestInit }[] = []
    const fetcher = vi.fn(async (url: string, init: RequestInit) => {
      calls.push({ url, init })
      return json({ ...session, revision: "2" })
    })
    vi.stubGlobal("fetch", fetcher)
    await updateAgentSession(
      SESSION_ID,
      { title: "Renamed", expectedRevision: "1" },
      "csrf"
    )
    expect(calls[0].init.method).toBe("PATCH")
    expect(JSON.parse(calls[0].init.body as string)).toEqual({
      title: "Renamed",
      expectedRevision: "1",
    })
    await updateAgentSession(
      SESSION_ID,
      { archived: true, expectedRevision: "1" },
      "csrf"
    )
    expect(JSON.parse(calls[1].init.body as string)).toEqual({
      archived: true,
      expectedRevision: "1",
    })
    // A combined rename+archive would need two revisions and could half-apply.
    await expect(
      updateAgentSession(
        SESSION_ID,
        { title: "x", archived: true, expectedRevision: "1" } as never,
        "csrf"
      )
    ).rejects.toBeInstanceOf(NativeClientError)
  })

  it("returns a refused turn as a settled failure rather than throwing", async () => {
    const refused = {
      ...turn,
      status: "failed",
      errorCode: "disabled",
      completedAt: 1_700_000_001,
    }
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(json({ turn: refused, runId: RUN_ID }, 201))
    )
    const started = await startAgentTurn(SESSION_ID, "hello", "csrf")
    // The prompt is persisted before the launch, so the UI shows a real turn
    // with a real reason instead of a request that disappeared.
    expect(started.turn.status).toBe("failed")
    expect(started.turn.errorCode).toBe("disabled")
    expect(started.turn.prompt).toBe("Explain this file")
  })

  it("refuses an empty or oversized prompt before any request", async () => {
    const fetcher = vi.fn()
    vi.stubGlobal("fetch", fetcher)
    for (const prompt of ["", "x".repeat(32_769)])
      await expect(
        startAgentTurn(SESSION_ID, prompt, "csrf")
      ).rejects.toBeInstanceOf(NativeClientError)
    expect(fetcher).not.toHaveBeenCalled()
  })

  it("reports an honest cancel outcome with the stored status", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(json({ canceled: false, status: "running" }))
    )
    // A run owned by another instance cannot be cancelled here; saying so beats
    // claiming a cancel that did not happen.
    expect(await cancelAgentRun(RUN_ID, "csrf")).toEqual({
      canceled: false,
      status: "running",
    })
  })

  it("rejects a malformed run id before it reaches a URL", async () => {
    const fetcher = vi.fn()
    vi.stubGlobal("fetch", fetcher)
    await expect(cancelAgentRun("../sessions", "csrf")).rejects.toBeInstanceOf(
      NativeClientError
    )
    await expect(
      openAgentRunStream("../sessions", () => {})
    ).rejects.toBeInstanceOf(NativeClientError)
    expect(fetcher).not.toHaveBeenCalled()
  })

  it("rejects a turn page that belongs to another session", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        json({
          items: [
            { ...turn, sessionId: "323e4567-e89b-42d3-a456-426614174002" },
          ],
          nextAfter: null,
        })
      )
    )
    await expect(listAgentTurns(SESSION_ID)).rejects.toBeInstanceOf(
      NativeClientError
    )
  })

  it("loads connections without ever receiving a credential", async () => {
    const payload = {
      runtimeEnabled: true,
      connections: [
        {
          provider: "claude",
          state: "reconnect_required",
          accountLabel: "sagar@example.test",
          checkedAt: 1_700_000_000,
          errorCode: "unauthorized",
        },
      ],
    }
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(json(payload)))
    expect(await loadAgentConnections()).toEqual(payload)
  })
})

describe("team invocation", () => {
  const CHAT_ID = "423e4567-e89b-42d3-a456-426614174003"
  const ask = {
    id: "523e4567-e89b-42d3-a456-426614174004",
    sequence: "1",
    authorId: 9,
    authorName: "Sagar",
    body: "summarise the incident",
    createdAt: 1_700_000_000,
    authorKind: "human",
    agentLabel: null,
  }

  it("posts the prompt with its audience guard and idempotent request id", async () => {
    const calls: { url: string; init: RequestInit }[] = []
    const fetcher = vi.fn(async (url: string, init: RequestInit) => {
      calls.push({ url, init })
      return json({ message: ask, turn, runId: RUN_ID }, 201)
    })
    vi.stubGlobal("fetch", fetcher)
    const invocation = await invokeTeamAgent(
      CHAT_ID,
      {
        prompt: "summarise the incident",
        provider: "claude",
        model: "default",
        requestId: SESSION_ID,
        expectedAuthRevision: "2",
      },
      "csrf"
    )
    expect(calls[0].url).toBe(`/api/neev/chats/${CHAT_ID}/agent`)
    expect(JSON.parse(calls[0].init.body as string)).toEqual({
      prompt: "summarise the incident",
      provider: "claude",
      model: "default",
      requestId: SESSION_ID,
      expectedAuthRevision: "2",
    })
    // The ask comes back already persisted as an ordinary human message, and
    // the run id is the turn's, so the caller streams the run it just started.
    expect(invocation.message.authorKind).toBe("human")
    expect(invocation.runId).toBe(turn.runId)
  })

  it("refuses an unsupported provider, model or revision before any request", async () => {
    const fetcher = vi.fn()
    vi.stubGlobal("fetch", fetcher)
    const valid = {
      prompt: "hi",
      provider: "claude" as const,
      model: "default",
      requestId: SESSION_ID,
      expectedAuthRevision: "2",
    }
    for (const bad of [
      { ...valid, provider: "gemini" as "claude" },
      // A model that could be read as a flag is refused by the runtime too.
      { ...valid, model: "--dangerous" },
      { ...valid, model: "" },
      { ...valid, expectedAuthRevision: "0" },
      { ...valid, prompt: "" },
    ])
      await expect(
        invokeTeamAgent(CHAT_ID, bad, "csrf")
      ).rejects.toBeInstanceOf(NativeClientError)
    expect(fetcher).not.toHaveBeenCalled()
  })

  it("refuses a reply echoed back as the invoker's own ask", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        json(
          {
            message: { ...ask, authorKind: "agent", agentLabel: "claude" },
            turn,
            runId: RUN_ID,
          },
          201
        )
      )
    )
    // The prompt row must be human; an agent-authored one means the wrong
    // message came back and the thread would be mislabelled.
    await expect(
      invokeTeamAgent(
        CHAT_ID,
        {
          prompt: "hi",
          provider: "claude",
          model: "default",
          requestId: SESSION_ID,
          expectedAuthRevision: "2",
        },
        "csrf"
      )
    ).rejects.toBeInstanceOf(NativeClientError)
  })

  it("accepts exactly the models the runtime would accept", () => {
    // Mirrors `safe_model` in execution.rs: `default` means no --model at all.
    expect(isSupportedModel("default")).toBe(true)
    expect(isSupportedModel("claude-opus-4-1")).toBe(true)
    expect(isSupportedModel("gpt-5.1_codex")).toBe(true)
    expect(isSupportedModel("--model")).toBe(false)
    expect(isSupportedModel("")).toBe(false)
    expect(isSupportedModel("has space")).toBe(false)
    expect(isSupportedModel("x".repeat(81))).toBe(false)
  })
})

describe("agent run stream", () => {
  it("replays a settled run exactly as the server stored it", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValue(
          sse(
            `{"type":"started","runId":"${RUN_ID}","turnId":"${TURN_ID}","sequence":"1"}`,
            '{"type":"chunk","text":"Here is "}',
            '{"type":"chunk","text":"the summary"}',
            '{"type":"finished","status":"completed","errorCode":null}'
          )
        )
    )
    const events: NeevRunEvent[] = []
    await openAgentRunStream(RUN_ID, (event) => events.push(event))
    expect(events.map((event) => event.type)).toEqual([
      "started",
      "chunk",
      "chunk",
      "finished",
    ])
    expect(
      events
        .filter((event) => event.type === "chunk")
        .map((event) => (event as { text: string }).text)
        .join("")
    ).toBe("Here is the summary")
  })

  it("reads the run events endpoint, never the turn-start endpoint", async () => {
    const fetcher = vi
      .fn()
      .mockResolvedValue(sse('{"type":"chunk","text":"x"}'))
    vi.stubGlobal("fetch", fetcher)
    await openAgentRunStream(RUN_ID, () => {})
    // Reattaching must never be able to start a run: it is a GET on the run.
    expect(fetcher.mock.calls[0][0]).toBe(
      `/api/neev/agent/runs/${RUN_ID}/events`
    )
    expect(fetcher.mock.calls[0][1].method).toBeUndefined()
  })

  it("reattaches across a drop and never re-runs the prompt", async () => {
    const attempts: string[] = []
    const open = vi
      .fn()
      .mockImplementationOnce(async (id: string) => {
        attempts.push(id)
        throw new TypeError("connection reset")
      })
      .mockImplementationOnce(
        async (id: string, onEvent: (event: NeevRunEvent) => void) => {
          attempts.push(id)
          onEvent({ type: "chunk", text: "resumed" })
          onEvent({ type: "finished", status: "completed", errorCode: null })
        }
      )
    const phases: string[] = []
    const events: NeevRunEvent[] = []
    await followAgentRun({
      runId: RUN_ID,
      signal: new AbortController().signal,
      onEvent: (event) => events.push(event),
      onPhase: (phase) => phases.push(phase),
      retryDelays: [0],
      open: open as never,
    })
    // Both attempts are reads of the same run; nothing was started again.
    expect(attempts).toEqual([RUN_ID, RUN_ID])
    expect(phases).toEqual(["streaming", "reconnecting", "streaming"])
    expect(events[events.length - 1]).toEqual({
      type: "finished",
      status: "completed",
      errorCode: null,
    })
  })

  it("stops following, rather than spinning, when attachments keep dying", async () => {
    // Resolves without a terminal event every time: the loop must treat that as
    // a fault and give up, not reattach at full speed forever.
    const open = vi.fn(async () => {})
    const phases: string[] = []
    await followAgentRun({
      runId: RUN_ID,
      signal: new AbortController().signal,
      onEvent: () => {},
      onPhase: (phase) => phases.push(phase),
      retryDelays: [0, 0],
      open: open as never,
    })
    expect(open).toHaveBeenCalledTimes(3)
    expect(phases[phases.length - 1]).toBe("lost")
  })

  it("reports a vanished run as lost without retrying it", async () => {
    const open = vi.fn(async () => {
      throw new NativeContentNotFoundError()
    })
    const phases: string[] = []
    await followAgentRun({
      runId: RUN_ID,
      signal: new AbortController().signal,
      onEvent: () => {},
      onPhase: (phase) => phases.push(phase),
      open: open as never,
    })
    // §5.2: unrecoverable means an explicit new turn, never an auto re-run.
    expect(open).toHaveBeenCalledTimes(1)
    expect(phases).toEqual(["streaming", "lost"])
  })

  it("stops following as soon as its caller aborts", async () => {
    const controller = new AbortController()
    const open = vi.fn(async () => {
      controller.abort()
    })
    await followAgentRun({
      runId: RUN_ID,
      signal: controller.signal,
      onEvent: () => {},
      onPhase: () => {},
      retryDelays: [10_000],
      open: open as never,
    })
    expect(open).toHaveBeenCalledTimes(1)
  })

  it("surfaces a run that can no longer be followed", async () => {
    for (const [status, kind] of [
      [404, "not_found"],
      [401, "unauthorized"],
      [403, "forbidden"],
      [503, "unavailable"],
    ] as const) {
      vi.stubGlobal(
        "fetch",
        vi.fn().mockResolvedValue(new Response("{}", { status }))
      )
      await expect(openAgentRunStream(RUN_ID, () => {})).rejects.toMatchObject({
        kind,
      })
    }
  })
})
