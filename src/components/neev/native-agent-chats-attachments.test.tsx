import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeAgentChats } from "./native-agent-chats"
import { NativeBoundary } from "./native-boundary"
import { json, mockNativeSession, NativeTestRoot } from "./test-helpers"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

const SESSION_ID = "11111111-1111-4111-8111-111111111111"
const TURN_ID = "22222222-2222-4222-8222-222222222222"
const RUN_ID = "0123456789abcdef0123456789abcdef"
const KEY = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1])

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
const attachment = {
  key: KEY,
  fileName: "shot.png",
  contentType: "image/png",
  byteLength: PNG.length,
}
const runningTurn = {
  id: TURN_ID,
  sessionId: SESSION_ID,
  sequence: "1",
  runId: RUN_ID,
  prompt: "",
  response: "",
  activity: [],
  status: "running",
  errorCode: null,
  provider: "claude",
  model: "default",
  attachments: [attachment],
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
  completedAt: null,
}
const connected = {
  runtimeEnabled: true,
  connections: [
    {
      provider: "claude",
      state: "connected",
      accountLabel: "sagar@example.test",
      checkedAt: 1_700_000_000,
      errorCode: null,
    },
  ],
}

const TURNS = `GET /api/neev/agent/sessions/${SESSION_ID}/turns`
const START = `POST /api/neev/agent/sessions/${SESSION_ID}/turns`
const UPLOAD = `POST /api/neev/agent/sessions/${SESSION_ID}/attachments`

function png(name = "shot.png"): File {
  return new File([PNG.buffer.slice(0)], name, { type: "image/png" })
}

function fileDrag(files: File[]) {
  return {
    types: ["Files"],
    files,
    items: files.map((file) => ({ kind: "file", getAsFile: () => file })),
    dropEffect: "none",
  }
}

function composer(onSessionCreated = vi.fn()) {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentChats
          nodeId={7}
          nodeName="Launch"
          draftProvider="claude"
          onSessionCreated={onSessionCreated}
          onDraftCancelled={vi.fn()}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

function openSession() {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentChats nodeId={null} sessionId={SESSION_ID} />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
})

it("starts a first chat from a dropped file alone", async () => {
  let created: Record<string, unknown> | null = null
  let started: Record<string, unknown> | null = null
  const uploads: RequestInit[] = []
  const handed = vi.fn()
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () => json(connected),
    "POST /api/neev/agent/sessions": (init) => {
      created = JSON.parse(init.body as string)
      return json({ ...session, nodeId: 7, title: "shot.png" }, 201)
    },
    [UPLOAD]: (init) => {
      // The session must exist before anything is uploaded to it.
      expect(created).not.toBeNull()
      uploads.push(init)
      return json(attachment, 201)
    },
    [START]: (init) => {
      started = JSON.parse(init.body as string)
      return json({ turn: runningTurn, runId: RUN_ID }, 201)
    },
    [TURNS]: () => json({ items: [runningTurn], nextAfter: null }),
    [`GET /api/neev/agent/runs/${RUN_ID}/events`]: () =>
      new Response("", {
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
      }),
  })
  composer(handed)
  const box = await screen.findByRole("textbox", { name: "Message" })
  await waitFor(() => expect(box).toBeEnabled())
  fireEvent.drop(box, { dataTransfer: fileDrag([png()]) })
  await screen.findByRole("button", { name: "Remove shot.png" })
  // Nothing has gone anywhere yet: there is no session to upload to.
  expect(uploads).toHaveLength(0)

  const send = screen.getByRole("button", { name: "Send" })
  expect(send).toBeEnabled()
  await userEvent.setup().click(send)

  await waitFor(() => expect(started).not.toBeNull())
  // A chat started from files alone is named after the first of them.
  expect(created).toMatchObject({ title: "shot.png" })
  expect(uploads).toHaveLength(1)
  expect(
    (uploads[0].headers as Record<string, string>)["X-Neev-Filename"]
  ).toBe("shot.png")
  // Empty prompt, real keys: the server is what labels an attachment-only turn.
  expect(started).toEqual({ prompt: "", attachmentKeys: [KEY] })
  // Handed over only after everything was persisted.
  expect(handed).toHaveBeenCalled()
})

it("keeps the session and the uploaded file when the first turn fails", async () => {
  let creations = 0
  let uploads = 0
  let attempts = 0
  const started: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () => json(connected),
    "POST /api/neev/agent/sessions": () => {
      creations += 1
      return json({ ...session, nodeId: 7 }, 201)
    },
    [UPLOAD]: () => {
      uploads += 1
      return json(attachment, 201)
    },
    [START]: (init) => {
      started.push(JSON.parse(init.body as string))
      attempts += 1
      if (attempts === 1) throw new TypeError("lost response")
      return json({ turn: runningTurn, runId: RUN_ID }, 201)
    },
    [TURNS]: () => json({ items: [], nextAfter: null }),
    [`GET /api/neev/agent/runs/${RUN_ID}/events`]: () =>
      new Response("", {
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
      }),
  })
  const user = userEvent.setup()
  composer()
  const box = await screen.findByRole("textbox", { name: "Message" })
  await waitFor(() => expect(box).toBeEnabled())
  await user.type(box, "Read this")
  fireEvent.drop(box, { dataTransfer: fileDrag([png()]) })
  await screen.findByRole("button", { name: "Remove shot.png" })
  await user.click(screen.getByRole("button", { name: "Send" }))

  await waitFor(() => expect(attempts).toBe(1))
  // Words and file both survive a failed turn.
  expect(box).toHaveValue("Read this")
  expect(screen.getByRole("button", { name: "Remove shot.png" })).toBeVisible()

  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(attempts).toBe(2))
  // One session, one upload: a retry repeats neither.
  expect(creations).toBe(1)
  expect(uploads).toBe(1)
  expect(started[1]).toEqual({ prompt: "Read this", attachmentKeys: [KEY] })
})

it("uploads into the open session and shows what a stored turn carried", async () => {
  const uploads: string[] = []
  let started: Record<string, unknown> | null = null
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
    "GET /api/neev/agent/connections": () => json(connected),
    [UPLOAD]: (init) => {
      uploads.push((init.headers as Record<string, string>)["X-Neev-Filename"])
      return json(attachment, 201)
    },
    [START]: (init) => {
      started = JSON.parse(init.body as string)
      return json({ turn: runningTurn, runId: RUN_ID }, 201)
    },
    [TURNS]: () =>
      json({
        items: [
          {
            ...runningTurn,
            id: "44444444-4444-4444-8444-444444444444",
            prompt: "Earlier ask",
            status: "completed",
            response: "done",
            completedAt: 1_700_000_050,
          },
        ],
        nextAfter: null,
      }),
    [`GET /api/neev/agent/runs/${RUN_ID}/events`]: () =>
      new Response("", {
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
      }),
  })
  openSession()
  const box = await screen.findByRole("textbox", { name: "Ask the agent" })
  await waitFor(() => expect(box).toBeEnabled())
  // The stored turn's own attachments, read back from the server.
  // A compact file reference now, image or not: named, downloadable, and
  // addressed by the key the server gave it — with the thumbnail inside it.
  const reference = await screen.findByRole("link", { name: /shot\.png/ })
  expect(reference).toHaveAttribute("href", `/api/neev/attachments/${KEY}`)
  expect(reference).toHaveAttribute("download", "shot.png")

  fireEvent.drop(box, { dataTransfer: fileDrag([png("diagram.png")]) })
  await waitFor(() => expect(uploads).toEqual(["diagram.png"]))
  await userEvent.setup().click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(started).not.toBeNull())
  expect(started).toEqual({ prompt: "", attachmentKeys: [KEY] })
})

it("asks before leaving with a file staged and nothing typed", async () => {
  const held: { guard: ((action: () => void) => void) | null } = { guard: null }
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/agent/connections": () => json(connected),
  })
  render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentChats
          nodeId={7}
          draftProvider="claude"
          onRequestNavigationGuard={(value) => {
            held.guard = value
          }}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
  const box = await screen.findByRole("textbox", { name: "Message" })
  await waitFor(() => expect(box).toBeEnabled())
  const leave = vi.fn()
  held.guard?.(leave)
  expect(leave).toHaveBeenCalledTimes(1)

  fireEvent.drop(box, { dataTransfer: fileDrag([png()]) })
  await screen.findByRole("button", { name: "Remove shot.png" })
  const confirm = vi.spyOn(window, "confirm").mockReturnValue(false)
  held.guard?.(leave)
  expect(confirm).toHaveBeenCalled()
  expect(leave).toHaveBeenCalledTimes(1)
  confirm.mockRestore()
})

it("never carries a file into the chat that was opened next", async () => {
  const SECOND_ID = "33333333-3333-4333-8333-333333333333"
  const second = { ...session, id: SECOND_ID, title: "Audit the parser" }
  let release!: () => void
  let started: Record<string, unknown> | null = null
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session, second] }),
    "GET /api/neev/agent/connections": () => json(connected),
    [UPLOAD]: () =>
      new Promise((resolve) => {
        release = () => resolve(json(attachment, 201))
      }),
    [TURNS]: () => json({ items: [], nextAfter: null }),
    [`GET /api/neev/agent/sessions/${SECOND_ID}/turns`]: () =>
      json({ items: [], nextAfter: null }),
    [`POST /api/neev/agent/sessions/${SECOND_ID}/turns`]: (init) => {
      started = JSON.parse(init.body as string)
      return json(
        { turn: { ...runningTurn, sessionId: SECOND_ID }, runId: RUN_ID },
        201
      )
    },
    [`GET /api/neev/agent/runs/${RUN_ID}/events`]: () =>
      new Response("", {
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
      }),
  })
  const view = render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentChats nodeId={null} sessionId={SESSION_ID} />
      </NativeBoundary>
    </NativeTestRoot>
  )
  const box = await screen.findByRole("textbox", { name: "Ask the agent" })
  await waitFor(() => expect(box).toBeEnabled())
  fireEvent.drop(box, { dataTransfer: fileDrag([png()]) })
  await screen.findByRole("button", { name: "Remove shot.png" })

  // The reader moves to another chat while the upload is still going.
  view.rerender(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentChats nodeId={null} sessionId={SECOND_ID} />
      </NativeBoundary>
    </NativeTestRoot>
  )
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: "Remove shot.png" })).toBeNull()
  )
  release()

  const user = userEvent.setup()
  await user.type(
    screen.getByRole("textbox", { name: "Ask the agent" }),
    "Different question"
  )
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(started).not.toBeNull())
  // The other chat's file is nowhere near this turn.
  expect(started).toEqual({ prompt: "Different question" })
})
