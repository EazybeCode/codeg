import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { useState } from "react"
import { NativeMentionsIndicator } from "./native-mentions"
import { NativeChat } from "./native-chat"
import { NativeTabPanel } from "./native-suspended-surface"
import { json, mockNativeSession, NativeTestRoot } from "./test-helpers"
import type { NativeResource } from "@/lib/neev-content-client"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

afterEach(() => vi.unstubAllGlobals())

const CHAT = "11111111-1111-4111-8111-111111111111"
const MESSAGE = "22222222-2222-4222-8222-222222222222"
const notice = {
  messageId: MESSAGE,
  chatId: CHAT,
  chatTitle: "Design chat",
  authorName: "Ana Duarte",
  excerpt: "ping @Sagar about the spacing",
  createdAt: 1_700_000_000,
  read: false,
}
const resource: NativeResource = {
  id: CHAT,
  orgId: 3,
  nodeId: 4,
  kind: "chat",
  creatorId: 2,
  title: "Design chat",
  visibility: "organization",
  originChatId: null,
  authRevision: "1",
  contentRevision: "0",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
}

/**
 * The indicator, and a chat panel that may or may not be the one on screen.
 *
 * Both live in tabs, because that is the only place either one exists: a tab
 * stays mounted while hidden, which is exactly the case a receipt must not be
 * written for.
 */
function Harness({
  onOpen,
  withChat = false,
  chatHidden = false,
}: {
  onOpen: (chatId: string) => boolean | Promise<boolean>
  withChat?: boolean
  chatHidden?: boolean
}) {
  const [hidden, setHidden] = useState(chatHidden)
  return (
    <>
      {/* The popover portals into the panel it belongs to, exactly as it does
          in the shell — outside one it would render nowhere. */}
      <NativeTabPanel hidden={false}>
        <NativeMentionsIndicator onOpen={onOpen} />
        {withChat && hidden && (
          <button onClick={() => setHidden(false)}>Bring it forward</button>
        )}
      </NativeTabPanel>
      {withChat && (
        <NativeTabPanel hidden={hidden}>
          <NativeChat
            resource={resource}
            onBack={() => {}}
            onChanged={() => {}}
          />
        </NativeTabPanel>
      )}
    </>
  )
}

function mount(props: Parameters<typeof Harness>[0] & Record<string, unknown>) {
  render(
    <NativeTestRoot>
      <Harness {...(props as Parameters<typeof Harness>[0])} />
    </NativeTestRoot>
  )
}

let threadReads = 0
function chatRoutes() {
  threadReads = 0
  return {
    [`GET /api/neev/resources/${CHAT}`]: () => json(resource),
    [`GET /api/neev/chats/${CHAT}/messages`]: () => {
      threadReads += 1
      return json({ items: [], nextAfter: null })
    },
    "GET /api/neev/organization/members?limit=100&offset=0": () =>
      json({ members: [] }),
  }
}

it("writes the receipt only once the conversation is the one on screen", async () => {
  const marked: Record<string, unknown>[] = []
  const opened: string[] = []
  mockNativeSession(undefined, {
    ...chatRoutes(),
    "GET /api/neev/mentions": () =>
      json({ items: [notice], unread: marked.length ? 0 : 1 }),
    "POST /api/neev/mentions/read": (init) => {
      marked.push(JSON.parse(init.body as string))
      return json({ items: [{ ...notice, read: true }], unread: 0 })
    },
  })
  mount({
    withChat: true,
    chatHidden: true,
    onOpen: (chatId) => {
      opened.push(chatId)
      return true
    },
  })
  const user = userEvent.setup()
  await user.click(
    await screen.findByRole("button", { name: "Open mentions — 1 unread" })
  )
  await user.click(await screen.findByText("Ana Duarte mentioned you"))
  expect(opened).toEqual([CHAT])

  // The chat is open — mounted, loaded, holding its draft — and behind another
  // tab. Nobody has seen it, so nothing is marked read. (A hidden panel's
  // contents are hidden from the accessibility tree too, which is why this
  // waits on the thread's own read rather than on a control.)
  await waitFor(() => expect(threadReads).toBeGreaterThan(0))
  expect(marked).toEqual([])

  await user.click(screen.getByRole("button", { name: "Bring it forward" }))
  await waitFor(() => expect(marked).toHaveLength(1))
  expect(marked[0]).toEqual({ messageIds: [MESSAGE] })
  // And the count catches up from the server rather than being guessed at.
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Open mentions" })).toBeVisible()
  )
})

it("claims a receipt for the conversation already in front", async () => {
  const marked: Record<string, unknown>[] = []
  mockNativeSession(undefined, {
    ...chatRoutes(),
    "GET /api/neev/mentions": () => json({ items: [notice], unread: 1 }),
    "POST /api/neev/mentions/read": (init) => {
      marked.push(JSON.parse(init.body as string))
      return json({ items: [{ ...notice, read: true }], unread: 0 })
    },
  })
  // Already open and already visible: following the notice changes nothing
  // about the panel, so the receipt has nothing to ride in on but the staging
  // itself.
  mount({ withChat: true, onOpen: () => true })
  const user = userEvent.setup()
  await screen.findByRole("textbox", { name: "Message" })
  await user.click(
    await screen.findByRole("button", { name: "Open mentions — 1 unread" })
  )
  await user.click(await screen.findByText("Ana Duarte mentioned you"))

  await waitFor(() => expect(marked).toHaveLength(1))
  expect(marked[0]).toEqual({ messageIds: [MESSAGE] })
})

it("keeps a mention unread when the navigation never happens", async () => {
  const marked: unknown[] = []
  mockNativeSession(undefined, {
    "GET /api/neev/mentions": () => json({ items: [notice], unread: 1 }),
    "POST /api/neev/mentions/read": (init) => {
      marked.push(init)
      return json({ items: [], unread: 0 })
    },
  })
  // What the workspace does when an unsaved-message question is answered
  // "stay", or when the strip moved while it was up: the opening was asked
  // for, and no chat panel ever arrives.
  mount({ onOpen: () => true })
  const user = userEvent.setup()
  await user.click(
    await screen.findByRole("button", { name: "Open mentions — 1 unread" })
  )
  await user.click(await screen.findByText("Ana Duarte mentioned you"))

  expect(marked).toEqual([])
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Open mentions — 1 unread" })
    ).toBeVisible()
  )
})

it("says so when the conversation cannot be opened", async () => {
  const marked: unknown[] = []
  mockNativeSession(undefined, {
    "GET /api/neev/mentions": () => json({ items: [notice], unread: 1 }),
    "POST /api/neev/mentions/read": (init) => {
      marked.push(init)
      return json({ items: [], unread: 0 })
    },
  })
  mount({ onOpen: () => false })
  const user = userEvent.setup()
  await user.click(
    await screen.findByRole("button", { name: "Open mentions — 1 unread" })
  )
  await user.click(await screen.findByText("Ana Duarte mentioned you"))

  expect(
    await screen.findByText("This conversation could not be opened here.")
  ).toBeVisible()
  expect(marked).toEqual([])
  // The list stays up with the reason in it, so the count is behind an open
  // modal menu and out of the accessibility tree — still saying one unread.
  expect(
    document.querySelector('[data-slot="dropdown-menu-trigger"]')
  ).toHaveAttribute("aria-label", "Open mentions — 1 unread")
})

it("reports a refused inbox instead of showing an empty one", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/mentions": () => json({ code: "forbidden" }, 403),
  })
  mount({ onOpen: () => true })
  const user = userEvent.setup()
  await user.click(await screen.findByRole("button", { name: "Open mentions" }))

  // "No mentions yet" would be a claim about this reader's inbox that the
  // server never made.
  expect(await screen.findByRole("alert")).toBeVisible()
  expect(screen.queryByText("No mentions yet")).toBeNull()
})
