import {
  render,
  screen,
  waitFor,
  within,
  act,
  fireEvent,
} from "@testing-library/react"
import { StrictMode } from "react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeContent } from "./native-content"
import { NativeBoundary } from "./native-boundary"
import { NativeWorkspace } from "./native-workspace"
import { useNativeAuth } from "./native-auth-provider"
import {
  json,
  mockNativeSession,
  nativeCapabilities,
  NativeTestRoot,
  orgSession,
  composerText,
  composerEditable,
  typeInComposer,
  setComposerText,
} from "./test-helpers"
import type { NativeResource } from "@/lib/neev-content-client"
import { NativeClientError } from "@/lib/neev-client"
// The real rich editor uses hit testing; jsdom has no layout geometry.
document.elementsFromPoint ??= () => []
vi.mock("@/hooks/use-mobile", () => ({ useIsMobile: () => false }))
const push = vi.hoisted(() => vi.fn())
vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push }),
}))
const node = {
  id: 4,
  orgId: 3,
  kind: "folder" as const,
  parentId: 2,
  name: "Research",
  revision: "1",
}
const chat: NativeResource = {
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
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
  push.mockClear()
  localStorage.clear()
})
function mount() {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeContent
          node={node}
          onRequestNavigationGuard={() => {}}
          autosaveMs={250}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
}
// Open the compact "Content actions" header menu via keyboard so it works for both
// userEvent and the fireEvent-based visibility harness (Radix opens on pointer/keys,
// not a bare fireEvent.click).
async function openContentMenu() {
  await act(async () => {
    fireEvent.keyDown(screen.getByRole("button", { name: "Content actions" }), {
      key: "Enter",
    })
  })
  return screen.findByRole("menu")
}
// Rename/Archive/Reopen/Move live in that menu; Share/Make private stay visible
// header buttons.
async function triggerAction(
  user: { click: (element: Element) => Promise<void> },
  name: string
) {
  if (name === "Make private" || name === "Share with organization") {
    await user.click(screen.getByRole("button", { name }))
    return
  }
  await openContentMenu()
  const item = await screen.findByRole("menuitem", { name })
  await act(async () => {
    fireEvent.click(item)
  })
}
it("opens a chat as a page and restores the folder listing on Back", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [chat], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(chat),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
  })
  mount()
  const user = userEvent.setup()
  await user.click(await screen.findByRole("button", { name: /Design chat/ }))
  expect(await screen.findByRole("textbox", { name: "Message" })).toBeVisible()
  expect(screen.queryByRole("tablist")).toBeNull()
  expect(screen.queryByRole("heading", { name: "Research" })).toBeNull()
  await user.click(screen.getByRole("button", { name: "Back to folder" }))
  expect(await screen.findByRole("tablist")).toBeVisible()
})
/**
 * Whether the editor under test will take typing.
 *
 * The chat composer says so with `contenteditable`; the document editor is
 * BlockNote's, which this suite does not own and which says it its own way.
 */
function writable(box: HTMLElement, kind: "chat" | "document"): boolean {
  return kind === "chat"
    ? composerEditable(box)
    : box.getAttribute("contenteditable") !== "false"
}

// Activity can suspend synchronously when hiding. Await React's event flush
// explicitly; user-event's synchronous act wrapper cannot await that suspension.
const visibilityActions = {
  click: async (element: Element) => {
    await act(async () => {
      fireEvent.click(element)
    })
  },
  // The chat composer is a contenteditable now, not a form control, so it has
  // no value to set — everything else here still does.
  // An editor is not a form control: it has no value to set, and no separate
  // clear — writing to it replaces what it holds, which is what every use of
  // these two here does anyway.
  type: async (element: Element, value: string) => {
    const box = element as HTMLElement
    if (box.getAttribute("contenteditable") === "true")
      return setComposerText(box, value)
    await act(async () => {
      fireEvent.change(element, {
        target: { value: (element as HTMLInputElement).value + value },
      })
    })
  },
  clear: async (element: Element) => {
    const box = element as HTMLElement
    if (box.getAttribute("contenteditable") === "true")
      return setComposerText(box, "")
    await act(async () => {
      fireEvent.change(element, { target: { value: "" } })
    })
  },
}
it.each([false, true])(
  "retains a dirty draft and hides its actual dialog until same-session resource revalidation (StrictMode=%s)",
  async (strict) => {
    let revalidating = false
    let releaseSession!: (response: Response) => void
    let releaseResource!: (response: Response) => void
    const sessionReply = new Promise<Response>((resolve) => {
      releaseSession = resolve
    })
    const resourceReply = new Promise<Response>((resolve) => {
      releaseResource = resolve
    })
    const mock = mockNativeSession(undefined, {
      "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
      "GET /api/neev/nodes/4/documents": () =>
        json({ items: [], nextAfter: null }),
      "GET /api/neev/session": () =>
        revalidating ? sessionReply : json(orgSession),
      "GET /api/neev/nodes/4/chats": () =>
        json({ items: [chat], nextAfter: null }),
      [`GET /api/neev/resources/${chat.id}`]: () =>
        revalidating ? resourceReply : json(chat),
      [`GET /api/neev/chats/${chat.id}/messages`]: () =>
        json({ items: [], nextAfter: null }),
    })
    const content = (
      <NativeTestRoot>
        <NativeBoundary>
          <NativeContent
            node={node}
            onRequestNavigationGuard={() => {}}
            autosaveMs={250}
          />
        </NativeBoundary>
      </NativeTestRoot>
    )
    render(strict ? <StrictMode>{content}</StrictMode> : content)
    const user = visibilityActions
    await user.click(await screen.findByRole("button", { name: /Design chat/ }))
    const draft = await screen.findByRole("textbox", { name: "Message" })
    await typeInComposer(draft, "private retained thought")
    await user.click(screen.getByRole("button", { name: "Back to folder" }))
    const dialog = screen.getByRole("dialog")
    expect(dialog).toBeVisible()
    revalidating = true
    await act(async () => document.dispatchEvent(new Event("visibilitychange")))
    expect(draft).not.toBeVisible()
    expect(dialog).not.toBeVisible()
    expect(dialog.closest("[inert]")).not.toBeNull()
    expect(screen.queryByRole("button", { name: "Discard changes" })).toBeNull()
    await act(async () => releaseSession(json(orgSession)))
    expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull()
    expect(dialog).not.toBeVisible()
    expect(dialog.closest("[inert]")).not.toBeNull()
    revalidating = false
    await act(async () => releaseResource(json(chat)))
    expect(await screen.findByRole("dialog")).toBeVisible()
    expect(screen.getByText("private retained thought")).toBeInTheDocument()
    if (strict) {
      await user.click(screen.getByRole("button", { name: "Keep editing" }))
      expect(
        composerText(screen.getByRole("textbox", { name: "Message" }))
      ).toBe("private retained thought")
    } else {
      await user.click(screen.getByRole("button", { name: "Discard changes" }))
      expect(
        await screen.findByRole("button", { name: /Design chat/ })
      ).toBeVisible()
    }
    expect(mock.unknown).toEqual([])
  }
)
it.each(["Make private", "Archive", "Reopen"] as const)(
  "executes retained %s intent on its original target with fresh metadata after visibility recovery",
  async (action) => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
    let latest: NativeResource = { ...chat, visibility: "organization" }
    let revalidating = false
    let release!: (response: Response) => void
    const bootstrap = new Promise<Response>((resolve) => {
      release = resolve
    })
    const bodies: Record<string, unknown>[] = []
    const mock = mockNativeSession(undefined, {
      "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
      "GET /api/neev/nodes/4/documents": () =>
        json({ items: [], nextAfter: null }),
      "GET /api/neev/session": () =>
        revalidating ? bootstrap : json(orgSession),
      "GET /api/neev/nodes/4/chats": () =>
        json({ items: [latest], nextAfter: null }),
      [`GET /api/neev/resources/${chat.id}`]: () => json(latest),
      [`GET /api/neev/chats/${chat.id}/messages`]: () =>
        json({ items: [], nextAfter: null }),
      [`PATCH /api/neev/resources/${chat.id}`]: (init) => {
        const { expectedAuthRevision, ...change } = JSON.parse(
          init.body as string
        )
        bodies.push({ expectedAuthRevision, ...change })
        latest = { ...latest, ...change, authRevision: "3" }
        return json(latest)
      },
    })
    mount()
    await visibilityActions.click(
      await screen.findByRole("button", { name: /Design chat/ })
    )
    await visibilityActions.type(
      await screen.findByRole("textbox", { name: "Message" }),
      "draft before privacy action"
    )
    if (action === "Reopen") {
      latest = { ...latest, archived: true }
      await act(async () => {
        await vi.advanceTimersByTimeAsync(5000)
      })
      expect(
        composerEditable(screen.getByRole("textbox", { name: "Message" }))
      ).toBe(false)
    }
    if (action === "Make private") {
      expect(
        screen.getByRole("button", { name: "Make private" })
      ).toHaveAccessibleDescription(
        "Making this private blocks future access, but copies that people already downloaded cannot be recalled."
      )
    }
    await triggerAction(visibilityActions, action)
    const dialog = screen.getByRole("dialog")
    revalidating = true
    await act(async () => document.dispatchEvent(new Event("visibilitychange")))
    expect(dialog).not.toBeVisible()
    latest = {
      ...latest,
      title: "Fresh title",
      authRevision: "2",
      visibility: action === "Make private" ? "organization" : "private",
      archived: action === "Archive",
    }
    revalidating = false
    await act(async () => release(json(orgSession)))
    expect(await screen.findByRole("dialog")).toBeVisible()
    expect(bodies).toEqual([])
    await visibilityActions.click(
      screen.getByRole("button", { name: "Discard changes" })
    )
    // The document's own pause before it saves is deliberately unhurried.
    await waitFor(() => expect(bodies).toHaveLength(1), { timeout: 5000 })
    expect(bodies[0]).toEqual({
      expectedAuthRevision: "2",
      title: "Fresh title",
      nodeId: 4,
      visibility: "private",
      archived: action === "Archive",
    })
    expect(screen.getByRole("heading", { name: "Fresh title" })).toBeVisible()
    expect(
      screen.getByRole("button", { name: "Share with organization" })
    ).toBeVisible()
    if (action === "Archive") {
      expect(
        composerEditable(screen.getByRole("textbox", { name: "Message" }))
      ).toBe(false)
    } else {
      expect(
        composerEditable(screen.getByRole("textbox", { name: "Message" }))
      ).toBe(true)
    }
    expect(composerText(screen.getByRole("textbox", { name: "Message" }))).toBe(
      ""
    )
    // The opposite action is now offered in the Content actions menu.
    await openContentMenu()
    expect(
      await screen.findByRole("menuitem", {
        name: action === "Archive" ? "Reopen" : "Archive",
      })
    ).toBeVisible()
    expect(mock.unknown).toEqual([])
  }
)

it("drops a retained mutation intent when a parent creation request supersedes its selection", async () => {
  const shared = { ...chat, visibility: "organization" as const }
  const bodies: unknown[] = []
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [shared], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(shared),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    [`PATCH /api/neev/resources/${chat.id}`]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json(shared)
    },
  })
  const tree = (create = false) => (
    <NativeTestRoot>
      <NativeBoundary>
        <NativeContent
          node={node}
          onRequestNavigationGuard={() => {}}
          creation={create ? { kind: "chat", request: 1 } : undefined}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
  const view = render(tree())
  await visibilityActions.click(
    await screen.findByRole("button", { name: /Design chat/ })
  )
  await visibilityActions.type(
    await screen.findByRole("textbox", { name: "Message" }),
    "original target draft"
  )
  await visibilityActions.click(
    screen.getByRole("button", { name: "Make private" })
  )
  await act(async () => document.dispatchEvent(new Event("visibilitychange")))
  await screen.findByRole("dialog")
  await act(async () => {
    view.rerender(tree(true))
  })
  await visibilityActions.click(screen.getByRole("button", { name: "Cancel" }))
  await visibilityActions.click(
    screen.getByRole("button", { name: "Discard changes" })
  )
  expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull()
  expect(screen.queryByRole("dialog")).toBeNull()
  expect(screen.getByRole("button", { name: "New chat" })).toBeVisible()
  expect(bodies).toEqual([])
  expect(mock.unknown).toEqual([])
})

it.each([
  ["csrf", { ...orgSession, csrf: "rotated-proof" }],
  ["account", { ...orgSession, user: { ...orgSession.user, id: 2 } }],
  [
    "organization",
    { ...orgSession, organization: { ...orgSession.organization, id: 9 } },
  ],
  ["role", { ...orgSession, role: "member" }],
  [
    "scope",
    { ...orgSession, scope: "identity", organization: null, role: null },
  ],
  ["401", 401],
  ["403", 403],
  // A transient failure is deliberately NOT here. An unanswered check is not a
  // refusal, and treating it as one destroyed drafts every time the network
  // hiccuped; the session is held instead, and the work with it. See
  // native-auth-provider.test.tsx.
])(
  "hard-clears actual private drafts on visibility %s mismatch or refusal",
  async (_label, response) => {
    let refreshed = false
    const mock = mockNativeSession(undefined, {
      "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
      "GET /api/neev/nodes/4/documents": () =>
        json({ items: [], nextAfter: null }),
      "GET /api/neev/session": () =>
        !refreshed
          ? json(orgSession)
          : typeof response === "number"
            ? json({ code: "unavailable" }, response)
            : json(response),
      "GET /api/neev/nodes/4/chats": () =>
        json({ items: [chat], nextAfter: null }),
      [`GET /api/neev/resources/${chat.id}`]: () => json(chat),
      [`GET /api/neev/chats/${chat.id}/messages`]: () =>
        json({ items: [], nextAfter: null }),
    })
    mount()
    const user = visibilityActions
    await user.click(await screen.findByRole("button", { name: /Design chat/ }))
    await typeInComposer(
      await screen.findByRole("textbox", { name: "Message" }),
      "must be destroyed"
    )
    refreshed = true
    await act(async () => document.dispatchEvent(new Event("visibilitychange")))
    await waitFor(() =>
      expect(screen.queryByText("must be destroyed")).toBeNull()
    )
    expect(document.body.innerHTML).not.toContain("rotated-proof")
    expect(mock.unknown).toEqual([])
  }
)

it.each([403, 404, 503])(
  "clears a retained draft before revealing a resource revoked or unavailable while hidden (%s)",
  async (status) => {
    let revalidating = false
    let release!: (response: Response) => void
    const pending = new Promise<Response>((resolve) => {
      release = resolve
    })
    const mock = mockNativeSession(undefined, {
      "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
      "GET /api/neev/nodes/4/documents": () =>
        json({ items: [], nextAfter: null }),
      "GET /api/neev/nodes/4/chats": () =>
        json({ items: revalidating ? [] : [chat], nextAfter: null }),
      [`GET /api/neev/resources/${chat.id}`]: () =>
        revalidating ? pending : json(chat),
      [`GET /api/neev/chats/${chat.id}/messages`]: () =>
        json({ items: [], nextAfter: null }),
    })
    mount()
    const user = visibilityActions
    await user.click(await screen.findByRole("button", { name: /Design chat/ }))
    const draft = await screen.findByRole("textbox", { name: "Message" })
    await typeInComposer(draft, "revoked private thought")
    revalidating = true
    await act(async () => document.dispatchEvent(new Event("visibilitychange")))
    expect(draft).not.toBeVisible()
    await act(async () => release(json({ code: "unavailable" }, status)))
    await waitFor(() =>
      expect(screen.queryByText("revoked private thought")).toBeNull()
    )
    expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull()
    expect(mock.unknown).toEqual([])
  }
)

it.each(["manual", "broadcast", "401", "logout"])(
  "%s hard invalidation destroys a draft during soft revalidation and rejects its late bootstrap",
  async (action) => {
    let phase = "initial"
    let release!: (response: Response) => void
    let receive!: (event: MessageEvent) => void
    const pending = new Promise<Response>((resolve) => {
      release = resolve
    })
    class Channel {
      addEventListener(_name: string, listener: (event: MessageEvent) => void) {
        receive = listener
      }
      removeEventListener() {}
      close() {}
      postMessage() {}
    }
    vi.stubGlobal("BroadcastChannel", Channel)
    const mock = mockNativeSession(undefined, {
      "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
      "GET /api/neev/nodes/4/documents": () =>
        json({ items: [], nextAfter: null }),
      "GET /api/neev/session": () =>
        phase === "soft" ? pending : json(orgSession),
      "GET /api/neev/nodes/4/chats": () =>
        json({ items: [chat], nextAfter: null }),
      [`GET /api/neev/resources/${chat.id}`]: () => json(chat),
      [`GET /api/neev/chats/${chat.id}/messages`]: () =>
        json({ items: [], nextAfter: null }),
      "POST /api/neev/logout": () => json({ code: "unavailable" }, 503),
    })
    function Controls() {
      const auth = useNativeAuth()
      return (
        <button
          onClick={() => {
            if (action === "manual") void auth.refresh()
            if (action === "broadcast")
              receive({
                data: { type: "invalidate", source: "other-tab" },
              } as MessageEvent)
            if (action === "401")
              auth.handleError(new NativeClientError("unauthorized"))
            if (action === "logout") void auth.logout()
          }}
        >
          Invalidate fixture
        </button>
      )
    }
    render(
      <NativeTestRoot>
        <Controls />
        <NativeBoundary>
          <NativeContent
            node={node}
            onRequestNavigationGuard={() => {}}
            autosaveMs={250}
          />
        </NativeBoundary>
      </NativeTestRoot>
    )
    const user = visibilityActions
    await user.click(await screen.findByRole("button", { name: /Design chat/ }))
    const box = await screen.findByRole("textbox", { name: "Message" })
    await typeInComposer(box, "never revive this draft")
    phase = "soft"
    await act(async () => document.dispatchEvent(new Event("visibilitychange")))
    // The composer's own element goes with its tab — the editor holds the draft
    // while it is away, which is what "retained" means here.
    expect(composerText(box)).toBe("never revive this draft")
    expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull()
    phase = "hard"
    await act(async () => {
      await user.click(
        screen.getByRole("button", { name: "Invalidate fixture" })
      )
    })
    expect(screen.queryByText("never revive this draft")).toBeNull()
    await act(async () => release(json(orgSession)))
    expect(screen.queryByText("never revive this draft")).toBeNull()
    expect(mock.unknown).toEqual([])
  }
)

it.each(["chat", "document"] as const)(
  "preserves pinned %s preconditions and ambiguous retry after identical-session recovery, ignoring old completion",
  async (kind) => {
    const resource = {
      ...chat,
      kind,
      contentRevision: kind === "document" ? "1" : "0",
    }
    let refreshed = false
    const bodies: Record<string, unknown>[] = []
    const releases: Array<(response: Response) => void> = []
    const revision = {
      format: "markdown",
      formatVersion: 1,
      revision: "1",
      authorId: 1,
      authorName: "Sagar",
      byteLength: "4",
      sha256: "a".repeat(64),
      createdAt: 1700000000,
      parentRevision: null,
      restoredFrom: null,
    }
    const message = {
      id: "22222222-2222-4222-8222-222222222222",
      sequence: "1",
      authorId: 1,
      authorName: "Sagar",
      body: "retained draft",
      createdAt: 1700000000,
    }
    const mock = mockNativeSession(undefined, {
      "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
      "GET /api/neev/nodes/4/chats": () =>
        json({ items: kind === "chat" ? [resource] : [], nextAfter: null }),
      "GET /api/neev/nodes/4/documents": () =>
        json({ items: [resource], nextAfter: null }),
      [`GET /api/neev/resources/${chat.id}`]: () =>
        json(
          refreshed
            ? {
                ...resource,
                authRevision: "2",
                contentRevision: kind === "document" ? "2" : "0",
                visibility: "organization",
              }
            : resource
        ),
      [`GET /api/neev/chats/${chat.id}/messages`]: () =>
        json({ items: [], nextAfter: null }),
      [`GET /api/neev/documents/${chat.id}/revisions`]: () =>
        json({ items: [revision], nextAfter: null }),
      [`GET /api/neev/documents/${chat.id}/revisions?after=1`]: () =>
        json({ items: [{ ...revision, revision: "2" }], nextAfter: null }),
      [`GET /api/neev/documents/${chat.id}/content?revision=1`]: () =>
        json({ document: null, revision, markdown: "seed" }),
      [`POST /api/neev/${kind === "chat" ? "chats" : "documents"}/${chat.id}/${kind === "chat" ? "messages" : "revisions"}`]:
        (init) => {
          bodies.push(JSON.parse(init.body as string))
          return new Promise<Response>((resolve) => {
            releases.push(resolve)
          })
        },
    })
    mount()
    const user = visibilityActions
    await screen.findByRole("tab", { name: "Documents" })
    if (kind === "document")
      await user.click(screen.getByRole("tab", { name: "Documents" }))
    await user.click(await screen.findByRole("button", { name: /Design chat/ }))
    const name = kind === "chat" ? "Message" : "Document content"
    const draft = await screen.findByRole("textbox", { name })
    await user.clear(draft)
    await user.type(draft, "retained draft")
    // A chat is sent by hand; a document writes itself down shortly after the
    // typing stops, so there is no button to press for it.
    if (kind === "chat")
      await user.click(screen.getByRole("button", { name: "Send" }))
    await waitFor(() => expect(bodies).toHaveLength(1))
    refreshed = true
    await act(async () => document.dispatchEvent(new Event("visibilitychange")))
    expect(composerText(screen.getByRole("textbox", { name }))).toBe(
      "retained draft"
    )
    if (kind === "chat") {
      // A message is sent by hand, so the retry is a second press — and it
      // must carry the SAME request id rather than write twice.
      const submit = await screen.findByRole("button", { name: "Send" })
      await waitFor(() => expect(submit).toBeEnabled())
      await user.click(submit)
      await waitFor(() => expect(bodies).toHaveLength(2))
      expect(bodies[1]).toEqual(bodies[0])
    } else {
      // A document has no button to press: it is already saving. If a
      // revalidation makes it try again, that attempt must be a REPLAY — the
      // same request id and the same preconditions — so the server dedupes it
      // instead of writing the draft twice.
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 60))
      })
      for (const body of bodies.slice(1)) expect(body).toEqual(bodies[0])
    }
    expect(bodies[bodies.length - 1]).toEqual(
      kind === "chat"
        ? {
            body: "retained draft",
            expectedAuthRevision: "1",
            requestId: expect.any(String),
          }
        : {
            markdown: "retained draft",
            expectedContentRevision: "1",
            expectedAuthRevision: "1",
            requestId: expect.any(String),
          }
    )
    await act(async () =>
      releases[0](
        json(
          kind === "chat"
            ? message
            : { ...revision, revision: "2", parentRevision: "1" },
          201
        )
      )
    )
    expect(composerText(screen.getByRole("textbox", { name }))).toBe(
      "retained draft"
    )
    // A chat's composer stays usable while a send is in flight — an agent run
    // shares this box and can take minutes. What that costs is a race the
    // clear has to win honestly: text typed before the server answers is newer
    // than the send, and survives it.
    if (kind === "chat")
      expect(writable(screen.getByRole("textbox", { name }), kind)).toBe(true)
    if (releases[1])
      await act(async () =>
        releases[1](
          json(
            kind === "chat"
              ? message
              : { ...revision, revision: "2", parentRevision: "1" },
            201
          )
        )
      )
    await waitFor(() =>
      expect(writable(screen.getByRole("textbox", { name }), kind)).toBe(true)
    )
    expect(mock.unknown).toEqual([])
  }
)
it("waits for the entire loaded resource window before revealing a retained draft", async () => {
  const resources = [
    chat,
    ...Array.from({ length: 101 }, (_, i) => ({
      ...chat,
      id: `${(i + 2).toString(16).padStart(8, "0")}-1111-4111-8111-111111111111`,
      title: i === 0 ? "Revoked entry" : `Resource ${i + 2}`,
    })),
  ]
  let resumed = false
  let approved = false
  let release!: (response: Response) => void
  const pending = new Promise<Response>((resolve) => {
    release = resolve
  })
  const fresh = resources.filter(
    (resource) => resource.title !== "Revoked entry"
  )
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () =>
      json({
        items: (resumed ? fresh : resources).slice(0, 100),
        nextAfter: resumed ? "fresh" : "initial",
      }),
    "GET /api/neev/nodes/4/chats?after=initial": () =>
      json({ items: resources.slice(100), nextAfter: null }),
    "GET /api/neev/nodes/4/chats?after=fresh": () =>
      approved ? json({ items: fresh.slice(100), nextAfter: null }) : pending,
    [`GET /api/neev/resources/${chat.id}`]: () => json(chat),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
  })
  await act(async () => {
    mount()
  })
  const more = await screen.findByRole("button", { name: "Load more" })
  await act(async () => {
    fireEvent.click(more)
  })
  await screen.findByRole("button", { name: /Resource 102/ })
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: /Design chat/ }))
  })
  const editor = await screen.findByRole("textbox", { name: "Message" })
  await setComposerText(editor, "retained across full window")
  resumed = true
  await act(async () => document.dispatchEvent(new Event("visibilitychange")))
  expect(composerText(editor)).toBe("retained across full window")
  expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull()
  approved = true
  await act(async () =>
    release(json({ items: fresh.slice(100), nextAfter: null }))
  )
  expect(
    composerText(await screen.findByRole("textbox", { name: "Message" }))
  ).toBe("retained across full window")
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Back to folder" }))
  })
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Discard changes" }))
  })
  expect(
    await screen.findByRole("button", { name: /Resource 102/ })
  ).toBeVisible()
  expect(screen.queryByRole("button", { name: /Revoked entry/ })).toBeNull()
  expect(mock.unknown).toEqual([])
})

it("hard-clears the scope when the loaded window loses authority during revalidation", async () => {
  let reads = 0
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () =>
      ++reads === 2
        ? json({ code: "forbidden" }, 403)
        : json({ items: reads === 1 ? [chat] : [], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(chat),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
  })
  await act(async () => {
    mount()
  })
  const select = await screen.findByRole("button", { name: /Design chat/ })
  await act(async () => {
    fireEvent.click(select)
  })
  const editor = await screen.findByRole("textbox", { name: "Message" })
  await act(async () => {
    await setComposerText(editor, "window revoked draft")
  })
  await act(async () => document.dispatchEvent(new Event("visibilitychange")))
  expect(await screen.findByRole("button", { name: "New chat" })).toBeEnabled()
  expect(screen.queryByText("window revoked draft")).toBeNull()
  expect(mock.unknown).toEqual([])
})

it("loads native content after React Strict Mode replays its mount effects", async () => {
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [chat], nextAfter: null }),
  })
  render(
    <StrictMode>
      <NativeTestRoot>
        <NativeBoundary>
          <NativeContent
            node={node}
            onRequestNavigationGuard={() => {}}
            autosaveMs={250}
          />
        </NativeBoundary>
      </NativeTestRoot>
    </StrictMode>
  )
  expect(
    await screen.findByRole("button", { name: /Design chat/ })
  ).toBeVisible()
  expect(mock.unknown).toEqual([])
})
it("creates private content only inside the selected node and confirms sharing its entire history", async () => {
  let latest = chat
  const bodies: unknown[] = []
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
    "POST /api/neev/nodes/4/chats": (init) => {
      bodies.push(JSON.parse(init.body as string))
      return json(chat, 201)
    },
    [`GET /api/neev/resources/${chat.id}`]: () => json(latest),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    [`PATCH /api/neev/resources/${chat.id}`]: (init) => {
      const body = JSON.parse(init.body as string)
      bodies.push(body)
      latest = { ...latest, ...body, authRevision: "2" }
      delete (latest as unknown as Record<string, unknown>).expectedAuthRevision
      return json(latest)
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: "New chat" }))
  expect(screen.getByRole("dialog")).toHaveTextContent("Research")
  await user.type(screen.getByRole("textbox", { name: "Title" }), chat.title)
  await user.click(screen.getByRole("button", { name: "Create" }))
  await screen.findByRole("textbox", { name: "Message" })
  expect(bodies[0]).toEqual({
    title: "Design chat",
    requestId: expect.any(String),
  })
  await user.click(
    screen.getByRole("button", { name: "Share with organization" })
  )
  expect(screen.getByRole("dialog")).toHaveTextContent("Everyone in Neev demo")
  expect(screen.getByRole("dialog")).toHaveTextContent("full history")
  expect(bodies).toHaveLength(1)
  await user.click(screen.getByRole("button", { name: "Share full history" }))
  await waitFor(() => expect(bodies).toHaveLength(2))
  expect(bodies[1]).toEqual({
    expectedAuthRevision: "1",
    title: "Design chat",
    nodeId: 4,
    visibility: "organization",
    archived: false,
  })
  expect(mock.unknown).toEqual([])
})

it(
  "guards leaving the workspace, and never asks when only switching tabs",
  { timeout: 20000 },
  async () => {
    const nodes = [
      { ...node, id: 2, kind: "project", parentId: null, name: "Launch" },
      node,
      { ...node, id: 5, kind: "project", parentId: null, name: "Other" },
    ]
    const doc = {
      ...chat,
      kind: "document",
      title: "Draft document",
      contentRevision: "0",
    }
    const STARTED_ID = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
    const started: NativeResource[] = []
    const mock = mockNativeSession(undefined, {
      "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
      "GET /api/neev/nodes/2/chats": () =>
        json({ items: started.length ? [started[0]] : [], nextAfter: null }),
      "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
      "GET /api/neev/nodes/5/chats": () => json({ items: [], nextAfter: null }),
      // Starting an agent chat creates an ordinary chat resource under Launch.
      "POST /api/neev/nodes/2/chats": () => {
        started.push({ ...chat, id: STARTED_ID, nodeId: 2, title: "New chat" })
        return json(started[0], 201)
      },
      [`GET /api/neev/resources/${STARTED_ID}`]: () => json(started[0]),
      [`GET /api/neev/chats/${STARTED_ID}/messages`]: () =>
        json({ items: [], nextAfter: null }),
      "GET /api/neev/nodes": () => json({ nodes, nextAfter: null }),
      "GET /api/neev/nodes/2/documents": () =>
        json({ items: [], nextAfter: null }),
      "GET /api/neev/nodes/4/documents": () =>
        json({ items: [doc], nextAfter: null }),
      "GET /api/neev/nodes/5/documents": () =>
        json({ items: [], nextAfter: null }),
      // The draft cannot be written down here, which is what makes leaving a
      // question rather than a save.
      [`POST /api/neev/documents/${chat.id}/revisions`]: () =>
        json({ code: "unavailable" }, 503),
      [`GET /api/neev/resources/${chat.id}`]: () => json(doc),
      [`GET /api/neev/documents/${chat.id}/revisions`]: () =>
        json({ items: [], nextAfter: null }),
      "POST /api/neev/logout": () => json({ ok: true }),
    })
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )
    // Documents is the default section; open the doc directly from the sidebar tree.
    await user.click(
      await screen.findByRole("button", { name: "Expand Launch" })
    )
    await user.click(
      await screen.findByRole("button", { name: "Expand Research" })
    )
    await user.click(
      await screen.findByRole("button", { name: /^Draft document/ })
    )
    await user.type(
      // Loading the real code-split editor is not subject to a one-second SLA.
      await screen.findByRole(
        "textbox",
        { name: "Document content" },
        { timeout: 15000 }
      ),
      "secret draft"
    )
    const unload = new Event("beforeunload", { cancelable: true })
    window.dispatchEvent(unload)
    expect(unload.defaultPrevented).toBe(true)
    // Asked for somewhere to put it: starting a chat fills the tab in front,
    // and the blank one is in front now, so the draft is not in danger and is
    // not asked about — it is still there, behind its own tab.
    await user.click(screen.getByRole("button", { name: "New tab" }))
    await user.click(
      screen.getAllByRole("button", { name: "Actions for Launch" })[0]
    )
    await user.click(screen.getByRole("menuitem", { name: "New agent chat" }))
    expect(screen.queryByRole("dialog")).toBeNull()
    await user.click(screen.getByRole("tab", { name: /^Draft document/ }))
    expect(
      screen.getAllByRole("textbox", { name: "Document content" })[0]
    ).toHaveTextContent("secret draft")
    // Leaving the workspace altogether DOES ask: every mounted panel is about to
    // go, including this one.
    await user.click(screen.getByRole("button", { name: "Sagar" }))
    await user.click(screen.getByRole("menuitem", { name: "Settings" }))
    expect(screen.getByRole("dialog")).toBeVisible()
    expect(push).not.toHaveBeenCalled()
    await user.click(screen.getByRole("button", { name: "Keep editing" }))
    await user.click(screen.getByRole("button", { name: "Sagar" }))
    await user.click(screen.getByRole("menuitem", { name: "Sign out" }))
    // Leaving tries to SAVE first. Only a save that will not land turns into a
    // question — here it cannot land, so the reader is asked rather than having
    // the draft thrown away on their behalf.
    expect(await screen.findByRole("dialog")).toHaveTextContent(
      "Discard unsaved changes?"
    )
    await user.click(screen.getByRole("button", { name: "Discard changes" }))
    await waitFor(() => expect(screen.queryByText("secret draft")).toBeNull())
    expect(mock.unknown).toEqual([])
  }
)

it(
  "shows the draft it is asking about when closing a hidden tab",
  { timeout: 20000 },
  async () => {
    // A panel's confirmation renders inside that panel, which is what stops a
    // hidden tab's menus covering the one in front. The same containment would
    // hide a confirmation we asked for — an invisible prompt and a Close button
    // that appears to do nothing.
    const nodes = [
      {
        ...node,
        id: 2,
        kind: "project" as const,
        parentId: null,
        name: "Launch",
      },
      node,
      {
        ...node,
        id: 5,
        kind: "project" as const,
        parentId: null,
        name: "Other",
      },
    ]
    const doc = { ...chat, kind: "document" as const, title: "Draft document" }
    const mock = mockNativeSession(undefined, {
      "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
      "GET /api/neev/nodes": () => json({ nodes, nextAfter: null }),
      "GET /api/neev/nodes/2/documents": () =>
        json({ items: [], nextAfter: null }),
      "GET /api/neev/nodes/2/chats": () => json({ items: [], nextAfter: null }),
      "GET /api/neev/nodes/4/documents": () =>
        json({ items: [doc], nextAfter: null }),
      "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
      "GET /api/neev/nodes/5/documents": () =>
        json({ items: [], nextAfter: null }),
      "GET /api/neev/nodes/5/chats": () => json({ items: [], nextAfter: null }),
      [`GET /api/neev/resources/${chat.id}`]: () => json(doc),
      [`POST /api/neev/documents/${chat.id}/revisions`]: () =>
        json({ code: "unavailable" }, 503),
      [`GET /api/neev/documents/${chat.id}/revisions`]: () =>
        json({ items: [], nextAfter: null }),
    })
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeWorkspace />
      </NativeTestRoot>
    )

    // Open the document and start writing in it.
    await user.click(
      await screen.findByRole("button", { name: "Expand Launch" })
    )
    await user.click(
      await screen.findByRole("button", { name: "Expand Research" })
    )
    await user.click(
      await screen.findByRole("button", { name: /^Draft document/ })
    )
    const body = await screen.findByRole(
      "textbox",
      { name: "Document content" },
      { timeout: 5000 }
    )
    await user.click(body)
    await user.keyboard("hidden work")

    // Leave it for another project, in a tab asked for first: no prompt,
    // because the draft keeps its own tab and nothing is destroyed.
    await user.click(screen.getByRole("button", { name: "New tab" }))
    await user.click(
      within(screen.getByRole("navigation", { name: "Projects" })).getByRole(
        "button",
        { name: "Other" }
      )
    )
    expect(screen.queryByRole("dialog")).toBeNull()

    // Now close the tab that is NOT in front. Its confirmation must be on screen.
    await user.click(
      screen.getByRole("button", { name: /Close Draft document/ })
    )
    const prompt = await screen.findByRole("dialog")
    expect(prompt).toHaveTextContent("Discard unsaved changes?")
    expect(prompt).toBeVisible()
    // Cancelling keeps the tab, the words in it, and brings it to the front —
    // the reader is looking at the draft they were asked about.
    await user.click(screen.getByRole("button", { name: "Keep editing" }))
    expect(
      await screen.findByRole("tab", { name: /^Draft document/ })
    ).toHaveAttribute("aria-selected", "true")
    expect(
      screen.getByRole("textbox", { name: "Document content" })
    ).toHaveTextContent("hidden work")
    expect(mock.unknown).toEqual([])
  }
)

it("immediately applies creator archive/reopen actions and moves with the current authority revision", async () => {
  let latest = chat
  const bodies: Record<string, unknown>[] = []
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: latest.nodeId === 4 ? [latest] : [], nextAfter: null }),
    "GET /api/neev/nodes": () =>
      json({
        nodes: [node, { ...node, id: 5, name: "Destination" }],
        nextAfter: null,
      }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(latest),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    [`PATCH /api/neev/resources/${chat.id}`]: (init) => {
      const { expectedAuthRevision, ...patch } = JSON.parse(init.body as string)
      bodies.push({ expectedAuthRevision, ...patch })
      latest = {
        ...latest,
        ...patch,
        authRevision: String(Number(expectedAuthRevision) + 1),
      }
      return json(latest)
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: /Design chat/ }))
  await screen.findByRole("textbox", { name: "Message" })
  await triggerAction(user, "Archive")
  await waitFor(() =>
    expect(
      composerEditable(screen.getByRole("textbox", { name: "Message" }))
    ).toBe(false)
  )
  await user.click(screen.getByRole("button", { name: "Content actions" }))
  const reopen = await screen.findByRole("menuitem", { name: "Reopen" })
  expect(reopen).toBeEnabled()
  await user.click(reopen)
  await waitFor(() =>
    expect(
      composerEditable(screen.getByRole("textbox", { name: "Message" }))
    ).toBe(true)
  )
  await triggerAction(user, "Move")
  await screen.findByRole("option", { name: "Destination · #5" })
  await user.selectOptions(
    screen.getByRole("combobox", { name: "Destination folder" }),
    "5"
  )
  await user.click(screen.getByRole("button", { name: "Save" }))
  await waitFor(() =>
    expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull()
  )
  expect(
    bodies.map((body) => [
      body.expectedAuthRevision,
      body.archived,
      body.nodeId,
    ])
  ).toEqual([
    ["1", true, 4],
    ["2", false, 4],
    ["3", false, 5],
  ])
  expect(mock.unknown).toEqual([])
})

it("never offers independent sharing for a source-bound document", async () => {
  const doc = {
    ...chat,
    kind: "document",
    originChatId: "33333333-3333-4333-8333-333333333333",
  }
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [doc], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(doc),
    [`GET /api/neev/documents/${chat.id}/revisions`]: () =>
      json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("tab", { name: "Documents" }))
  await user.click(await screen.findByRole("button", { name: /Design chat/ }))
  await screen.findByRole("textbox", { name: "Document content" })
  expect(screen.getByText(/Audience follows source chat/)).toBeVisible()
  expect(
    screen.queryByRole("button", { name: "Share with organization" })
  ).toBeNull()
  expect(screen.queryByRole("button", { name: "Make private" })).toBeNull()
  expect(mock.unknown).toEqual([])
})

it("discards the actual draft before opening a creator action on the same resource", async () => {
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [chat], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(chat),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: /Design chat/ }))
  await typeInComposer(
    await screen.findByRole("textbox", { name: "Message" }),
    "discard me"
  )
  await user.click(
    screen.getByRole("button", { name: "Share with organization" })
  )
  await user.click(screen.getByRole("button", { name: "Discard changes" }))
  expect(screen.queryByText("discard me")).toBeNull()
  expect(screen.getByRole("dialog")).toHaveTextContent("Share full history?")
  await user.click(screen.getByRole("button", { name: "Cancel" }))
  expect(
    composerText(await screen.findByRole("textbox", { name: "Message" }))
  ).toBe("")
  expect(mock.unknown).toEqual([])
})

it("guards leaving a dirty page before keyboard switching folder tabs", async () => {
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [chat], nextAfter: null }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(chat),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: /Design chat/ }))
  await typeInComposer(
    await screen.findByRole("textbox", { name: "Message" }),
    "dirty"
  )
  expect(screen.queryByRole("tablist")).toBeNull()
  await user.click(screen.getByRole("button", { name: "Back to folder" }))
  await user.click(screen.getByRole("button", { name: "Discard changes" }))
  expect(screen.queryByText("dirty")).toBeNull()
  screen.getByRole("tab", { name: "Team chat" }).focus()
  await user.keyboard("{ArrowRight}")
  await waitFor(() =>
    expect(screen.getByRole("tab", { name: "Agent chats" })).toHaveAttribute(
      "aria-selected",
      "true"
    )
  )
  expect(screen.getByRole("tab", { name: "Agent chats" })).toHaveFocus()
  await user.keyboard("{End}")
  await waitFor(() =>
    expect(screen.getByRole("tab", { name: "Documents" })).toHaveAttribute(
      "aria-selected",
      "true"
    )
  )
  expect(screen.getByRole("tab", { name: "Documents" })).toHaveFocus()
  expect(mock.unknown).toEqual([])
})

it("shows a truthful Agent chats placeholder without faking a list or fetching", async () => {
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/session": () => json(orgSession),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [chat], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("tab", { name: "Agent chats" }))
  expect(
    screen.getByRole("tabpanel", { name: "Agent chats" })
  ).toHaveTextContent(
    "Agent chats aren't available yet. They'll appear here once agent sessions ship."
  )
  // No faked session list and no create affordance while unavailable.
  expect(screen.queryByRole("button", { name: "New chat" })).toBeNull()
  expect(screen.queryByRole("button", { name: "New document" })).toBeNull()
  expect(mock.unknown).toEqual([])
})

it("auth invalidation clears a private draft even while its Settings guard is open", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  let expired = false
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    // The link to Settings lives in the agent composer now, and only appears
    // when there is a runtime and a provider that is not connected yet.
    "GET /api/workos/capabilities": () =>
      json({ ...nativeCapabilities, runtimeEnabled: true }),
    "GET /api/neev/agent/connections": () =>
      json({
        runtimeEnabled: true,
        connections: [
          {
            provider: "claude",
            state: "not_signed_in",
            accountLabel: null,
            checkedAt: null,
            errorCode: null,
          },
        ],
      }),
    // With a runtime bound, the conversation asks whether this reader has ever
    // invoked here. They have not, and an empty history is a normal history.
    [`GET /api/neev/chats/${chat.id}/agent?provider=claude`]: () =>
      json({ code: "not_found" }, 404),
    [`GET /api/neev/chats/${chat.id}/agent?provider=codex`]: () =>
      json({ code: "not_found" }, 404),
    "GET /api/neev/session": () =>
      json(
        expired
          ? {
              ...orgSession,
              organization: {
                id: 9,
                name: "New organization",
                avatarUrl: null,
              },
            }
          : orgSession
      ),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [chat], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () =>
      expired ? json({ code: "unauthorized" }, 401) : json(chat),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: /Design chat/ }))
  await typeInComposer(
    await screen.findByRole("textbox", { name: "Message" }),
    "secret pending draft"
  )
  // The link lives with the agent composer now: the mode is the thing that
  // decides what Send does, so what it needs to work is stated next to it.
  await user.click(screen.getByRole("button", { name: "Ask agent" }))
  await user.click(
    await screen.findByRole("link", { name: "Personal AI connections" })
  )
  expect(screen.getByRole("dialog")).toHaveTextContent(
    "Discard unsaved changes?"
  )
  expired = true
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5000)
  })
  expect(screen.queryByRole("dialog")).toBeNull()
  expect(screen.queryByText("secret pending draft")).toBeNull()
  expect(push).not.toHaveBeenCalled()
  expect(mock.unknown).toEqual([])
})

it("ignores a delayed PATCH for A after navigating to B and writing a draft", async () => {
  const other = {
    ...chat,
    id: "22222222-2222-4222-8222-222222222222",
    title: "Chat B",
  }
  let release!: (value: Response) => void
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [chat, other], nextAfter: null }),
    [`GET /api/neev/resources/${chat.id}`]: () => json(chat),
    [`GET /api/neev/resources/${other.id}`]: () => json(other),
    [`GET /api/neev/chats/${chat.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    [`GET /api/neev/chats/${other.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    [`PATCH /api/neev/resources/${chat.id}`]: (init) => {
      expect(JSON.parse(init.body as string)).toEqual({
        expectedAuthRevision: "1",
        title: "Design chat",
        nodeId: 4,
        visibility: "private",
        archived: true,
      })
      return new Promise((resolve) => {
        release = resolve
      })
    },
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: /Design chat/ }))
  await screen.findByRole("textbox", { name: "Message" })
  await triggerAction(user, "Archive")
  await user.click(screen.getByRole("button", { name: "Back to folder" }))
  await user.click(await screen.findByRole("button", { name: /Chat B/ }))
  await typeInComposer(
    await screen.findByRole("textbox", { name: "Message" }),
    "B's unsaved draft"
  )
  await act(async () => {
    release(json({ ...chat, archived: true, authRevision: "2" }))
  })
  expect(screen.getByRole("heading", { name: "Chat B" })).toBeVisible()
  expect(composerText(screen.getByRole("textbox", { name: "Message" }))).toBe(
    "B's unsaved draft"
  )
  expect(mock.unknown).toEqual([])
})

it("rejects delayed move destinations from a previously selected resource", async () => {
  const other = {
    ...chat,
    id: "22222222-2222-4222-8222-222222222222",
    title: "Other chat",
  }
  let release!: (value: Response) => void
  const delayed = new Promise<Response>((resolve) => {
    release = resolve
  })
  let moves = 0
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [chat, other], nextAfter: null }),
    "GET /api/neev/nodes": () =>
      ++moves === 1
        ? delayed
        : json({
            nodes: [{ ...node, id: 6, name: "Current destination" }],
            nextAfter: null,
          }),
    ...Object.fromEntries(
      [chat, other].flatMap((resource) => [
        [`GET /api/neev/resources/${resource.id}`, () => json(resource)],
        [
          `GET /api/neev/chats/${resource.id}/messages`,
          () => json({ items: [], nextAfter: null }),
        ],
      ])
    ),
  })
  mount()
  const user = userEvent.setup()
  await user.click(await screen.findByRole("button", { name: /Design chat/ }))
  await screen.findByRole("textbox", { name: "Message" })
  await triggerAction(user, "Move")
  await user.click(screen.getByRole("button", { name: "Cancel" }))
  await user.click(screen.getByRole("button", { name: "Back to folder" }))
  await user.click(await screen.findByRole("button", { name: /Other chat/ }))
  await screen.findByRole("textbox", { name: "Message" })
  await triggerAction(user, "Move")
  await screen.findByRole("option", { name: "Current destination · #6" })
  await act(async () =>
    release(
      json({
        nodes: [{ ...node, id: 5, name: "Old destination" }],
        nextAfter: null,
      })
    )
  )
  expect(
    screen.getByRole("option", { name: "Current destination · #6" })
  ).toBeInTheDocument()
  expect(screen.queryByRole("option", { name: /Old destination/ })).toBeNull()
  expect(mock.unknown).toEqual([])
})

it("refreshes the loaded resource page window and removes revoked entries on an actual poll", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  const resources = Array.from({ length: 102 }, (_, i) => ({
    ...chat,
    id: `${(i + 1).toString(16).padStart(8, "0")}-1111-4111-8111-111111111111`,
    title: i === 0 ? "Revoked entry" : `Resource ${i + 1}`,
  }))
  let polled = false
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () =>
      json({
        items: polled ? resources.slice(1, 101) : resources.slice(0, 100),
        nextAfter: polled ? "fresh2" : "page2",
      }),
    "GET /api/neev/nodes/4/chats?after=page2": () =>
      json({ items: resources.slice(100), nextAfter: null }),
    "GET /api/neev/nodes/4/chats?after=fresh2": () =>
      json({ items: resources.slice(101), nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("button", { name: "Load more" }))
  expect(
    await screen.findByRole("button", { name: /Resource 102/ })
  ).toBeVisible()
  polled = true
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5000)
  })
  expect(screen.getByRole("button", { name: /Resource 102/ })).toBeVisible()
  expect(screen.queryByRole("button", { name: /Revoked entry/ })).toBeNull()
  expect(mock.unknown).toEqual([])
})

function SwitchScope() {
  const auth = useNativeAuth()
  return <button onClick={() => void auth.refresh()}>Switch scope</button>
}
it("rejects a late private list response after auth generation changes even when transport ignores abort", async () => {
  let release!: (response: Response) => void
  let switched = false
  const mock = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/session": () =>
      json(
        switched
          ? {
              ...orgSession,
              organization: {
                id: 9,
                name: "New organization",
                avatarUrl: null,
              },
            }
          : orgSession
      ),
    "GET /api/neev/nodes/4/chats": () =>
      new Promise((resolve) => {
        release = resolve
      }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <SwitchScope />
      <NativeBoundary>
        <NativeContent
          node={node}
          onRequestNavigationGuard={() => {}}
          autosaveMs={250}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
  await screen.findByRole("tab", { name: "Team chat" })
  await waitFor(() => expect(release).toBeDefined())
  switched = true
  await user.click(screen.getByRole("button", { name: "Switch scope" }))
  await act(async () => {
    release(
      json({
        items: [{ ...chat, title: "other organization's secret" }],
        nextAfter: null,
      })
    )
  })
  expect(screen.queryByText("other organization's secret")).toBeNull()
  expect(mock.unknown).toEqual([])
})
