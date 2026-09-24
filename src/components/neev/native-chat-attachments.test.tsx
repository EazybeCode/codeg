import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, beforeEach, expect, it, vi } from "vitest"
import { NativeChat } from "./native-chat"
import { NativeBoundary } from "./native-boundary"
import {
  json,
  mockNativeSession,
  nativeCapabilities,
  NativeTestRoot,
  typeInComposer,
} from "./test-helpers"
import type { NativeResource } from "@/lib/neev-content-client"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

const resource: NativeResource = {
  id: "11111111-1111-4111-8111-111111111111",
  orgId: 3,
  nodeId: 4,
  kind: "chat",
  creatorId: 1,
  title: "Design chat",
  visibility: "private",
  originChatId: null,
  authRevision: "1",
  contentRevision: "0",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
}
const KEY = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
const messagesPath = `/api/neev/chats/${resource.id}/messages`
const uploadPath = `/api/neev/chats/${resource.id}/attachments`
const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1])

function png(name = "shot.png"): File {
  return new File([PNG.buffer.slice(0)], name, { type: "image/png" })
}

/** A drag carrying files, as the browser hands one over. */
function fileDrag(files: File[]) {
  return {
    types: ["Files"],
    files,
    items: files.map((file) => ({ kind: "file", getAsFile: () => file })),
    dropEffect: "none",
  }
}

const attachment = {
  key: KEY,
  fileName: "shot.png",
  contentType: "image/png",
  byteLength: PNG.length,
}
const sent = {
  id: "22222222-2222-4222-8222-222222222222",
  sequence: "1",
  authorId: 1,
  authorName: "Sagar",
  body: "Look at this",
  createdAt: 1_700_000_000,
  attachments: [attachment],
}

beforeEach(() => localStorage.clear())
afterEach(() => vi.unstubAllGlobals())

function mount() {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeChat
          resource={resource}
          onBack={() => {}}
          onChanged={() => {}}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

async function ready() {
  const box = await screen.findByRole("textbox", { name: "Message" })
  await waitFor(() =>
    expect(screen.queryByText("Loading this conversation…")).toBeNull()
  )
  return box
}

it("uploads a dropped file and sends it inside the sentence it was written in", async () => {
  const uploads: RequestInit[] = []
  const posted: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${messagesPath}`]: () => json({ items: [], nextAfter: null }),
    [`POST ${uploadPath}`]: (init) => {
      uploads.push(init)
      return json(attachment, 201)
    },
    [`POST ${messagesPath}`]: (init) => {
      posted.push(JSON.parse(init.body as string))
      return json(sent, 201)
    },
  })
  mount()
  const box = await ready()
  fireEvent.drop(box, { dataTransfer: fileDrag([png()]) })

  // The bytes go up on their own, before anything is sent.
  await waitFor(() => expect(uploads).toHaveLength(1))
  const headers = uploads[0].headers as Record<string, string>
  expect(headers["X-Neev-Filename"]).toBe("shot.png")
  expect(headers["x-neev-csrf"]).toBe("synthetic-csrf")
  expect(uploads[0].body).toBeInstanceOf(File)
  await screen.findByRole("button", { name: "Remove shot.png" })

  // Words on BOTH sides of the file, which is the whole point: the file is in
  // the sentence, not in a tray under it.
  await typeInComposer(box, "Look at ", "start")
  await typeInComposer(box, "and tell me", "end")
  const user = userEvent.setup()
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(posted).toHaveLength(1))
  // The marker sits where the file sat, and its number is the file's place in
  // the key list the server stores in order.
  expect(posted[0].body).toBe("Look at [attachment 1: shot.png] and tell me")
  expect(posted[0].attachmentKeys).toEqual([KEY])
  // Sent means gone from the composer, and shown from the server's own row.
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: "Remove shot.png" })).toBeNull()
  )
  // A compact file reference now, image or not: named, downloadable, and
  // addressed by the key the server gave it — with the thumbnail inside it.
  const reference = await screen.findByRole("link", { name: /shot\.png/ })
  expect(reference).toHaveAttribute("href", `/api/neev/attachments/${KEY}`)
  expect(reference).toHaveAttribute("download", "shot.png")
})

it("sends files with no words as the reference alone plus keys", async () => {
  const posted: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${messagesPath}`]: () => json({ items: [], nextAfter: null }),
    [`POST ${uploadPath}`]: () => json(attachment, 201),
    [`POST ${messagesPath}`]: (init) => {
      posted.push(JSON.parse(init.body as string))
      return json({ ...sent, body: "[attachment 1: shot.png]" }, 201)
    },
  })
  mount()
  const box = await ready()
  fireEvent.drop(box, { dataTransfer: fileDrag([png()]) })
  await screen.findByRole("button", { name: "Remove shot.png" })
  const send = screen.getByRole("button", { name: "Send" })
  expect(send).toBeEnabled()
  await userEvent.setup().click(send)
  await waitFor(() => expect(posted).toHaveLength(1))
  // Not empty any more: a message that is only a file still SAYS which file,
  // and that sentence is what the transcript renders the reference from.
  expect(posted[0].body).toBe("[attachment 1: shot.png]")
  expect(posted[0].attachmentKeys).toEqual([KEY])
})

it("keeps the files and the request id when the send itself fails", async () => {
  const posted: Record<string, unknown>[] = []
  let uploads = 0
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${messagesPath}`]: () => json({ items: [], nextAfter: null }),
    [`POST ${uploadPath}`]: () => {
      uploads += 1
      return json(attachment, 201)
    },
    [`POST ${messagesPath}`]: (init) => {
      posted.push(JSON.parse(init.body as string))
      if (posted.length === 1) throw new TypeError("lost response")
      return json(sent, 201)
    },
  })
  mount()
  const box = await ready()
  fireEvent.drop(box, { dataTransfer: fileDrag([png()]) })
  await screen.findByRole("button", { name: "Remove shot.png" })
  const user = userEvent.setup()
  await typeInComposer(box, "Look at this")
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(posted).toHaveLength(1))
  // The file is still attached and still visible; nothing was silently dropped.
  expect(screen.getByRole("button", { name: "Remove shot.png" })).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(posted).toHaveLength(2))
  // Same message, so the same request id and the same key — retried, not resent.
  expect(posted[1].requestId).toBe(posted[0].requestId)
  expect(posted[1].attachmentKeys).toEqual([KEY])
  expect(uploads).toBe(1)
})

it("will not send while a file is still going up", async () => {
  let release!: () => void
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${messagesPath}`]: () => json({ items: [], nextAfter: null }),
    [`POST ${uploadPath}`]: () =>
      new Promise((resolve) => {
        release = () => resolve(json(attachment, 201))
      }),
  })
  mount()
  const box = await ready()
  await userEvent.setup().type(box, "Look at this")
  fireEvent.drop(box, { dataTransfer: fileDrag([png()]) })
  await screen.findByText("Uploading…")
  expect(screen.getByRole("button", { name: "Send" })).toBeDisabled()
  // Held, and it says why rather than looking ready and doing nothing.
  expect(
    screen.getByText("Waiting for the attached files to finish uploading.")
  ).toBeVisible()
  release()
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Send" })).toBeEnabled()
  )
})

it("ignores a drag that is not carrying files", async () => {
  const fetched: string[] = []
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${messagesPath}`]: () => json({ items: [], nextAfter: null }),
  })
  mount()
  const box = await ready()
  fireEvent.drop(box, {
    dataTransfer: { types: ["text/plain"], files: [], items: [] },
  })
  await Promise.resolve()
  for (const call of mock.fetcher.mock.calls) fetched.push(call[0])
  expect(fetched.some((path) => path.endsWith("/attachments"))).toBe(false)
  expect(screen.queryByRole("list", { name: "Attachments" })).toBeNull()
})

it("re-mints the ask's request id once its files change after a failure", async () => {
  // The server hashes the prompt AND the keys into the receipt, so reusing the
  // id after removing a file would be refused forever rather than retried.
  const asks: Record<string, unknown>[] = []
  const teamSession = `GET /api/neev/chats/${resource.id}/agent?provider=`
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${messagesPath}`]: () => json({ items: [], nextAfter: null }),
    "GET /api/workos/capabilities": () =>
      json({ ...nativeCapabilities, runtimeEnabled: true }),
    "GET /api/neev/agent/connections": () =>
      json({
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
      }),
    [`${teamSession}claude`]: () => json({ code: "not_found" }, 404),
    [`${teamSession}codex`]: () => json({ code: "not_found" }, 404),
    [`POST ${uploadPath}`]: () => json(attachment, 201),
    [`POST /api/neev/chats/${resource.id}/agent`]: (init) => {
      asks.push(JSON.parse(init.body as string))
      throw new TypeError("lost response")
    },
  })
  mount()
  const box = await ready()
  const user = userEvent.setup()
  await user.click(screen.getByRole("button", { name: "Ask agent" }))
  await typeInComposer(box, "Look at this")
  fireEvent.drop(box, { dataTransfer: fileDrag([png()]) })
  await screen.findByRole("button", { name: "Remove shot.png" })
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(asks).toHaveLength(1))
  expect(asks[0].attachmentKeys).toEqual([KEY])

  // Same words, same file: still the same ask.
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(asks).toHaveLength(2))
  expect(asks[1].requestId).toBe(asks[0].requestId)

  // Remove the file and try again: that is a different ask.
  await user.click(screen.getByRole("button", { name: "Remove shot.png" }))
  await user.click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(asks).toHaveLength(3))
  expect(asks[2].attachmentKeys).toBeUndefined()
  expect(asks[2].requestId).not.toBe(asks[0].requestId)
})
