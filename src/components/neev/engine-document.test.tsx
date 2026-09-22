import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { EngineDocument } from "./engine-document"
import { NativeContent } from "./native-content"
import { NativeBoundary } from "./native-boundary"
import { json, mockNativeSession, NativeTestRoot } from "./test-helpers"
import { parseResource } from "@/lib/neev-content-client"
import type { NativeResource } from "@/lib/neev-content-client"

// The native document branch mounts the rich editor, which hit-tests.
document.elementsFromPoint ??= () => []
vi.mock("@/hooks/use-mobile", () => ({ useIsMobile: () => false }))
vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

const node = {
  id: 4,
  orgId: 3,
  kind: "folder" as const,
  parentId: 2,
  name: "Research",
  revision: "1",
}

const resource: NativeResource = {
  id: "11111111-1111-4111-8111-111111111111",
  orgId: 3,
  nodeId: 4,
  kind: "document",
  creatorId: 1,
  title: "Design document",
  visibility: "private",
  originChatId: null,
  authRevision: "1",
  contentRevision: "1",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
  engine: "lasuite",
}

// Radix opens its menu on pointer/key events, not on a bare click in jsdom.
async function openContentMenu() {
  await act(async () => {
    fireEvent.keyDown(screen.getByRole("button", { name: "Content actions" }), {
      key: "Enter",
    })
  })
  return screen.findByRole("menu")
}

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
  localStorage.clear()
})

function alone(value: NativeResource = resource, loadTimeoutMs?: number) {
  const { fetcher } = mockNativeSession()
  render(
    <NativeTestRoot>
      <EngineDocument
        resource={value}
        onBack={vi.fn()}
        loadTimeoutMs={loadTimeoutMs}
      />
    </NativeTestRoot>
  )
  return fetcher
}

it("frames the document at the engine route", async () => {
  alone()
  const frame = await screen.findByTitle("Design document")
  // The resource id IS the upstream document id. There is no second id and no
  // mapping, so there is nothing that can fall out of sync.
  expect(frame).toHaveAttribute("src", `/document-engine/docs/${resource.id}/`)
})

/**
 * The point of the whole component.
 *
 * Neev must not read, write, poll or autosave content for a document the
 * upstream editor owns — two writers against one document is the failure this
 * design exists to avoid. Asserted against every request the component causes
 * rather than a list of paths guessed in advance, so a fetch added later fails
 * this test too instead of slipping past a stale list.
 */
it("makes no content, revision or autosave request at all", async () => {
  const fetcher = alone()
  await screen.findByTitle("Design document")
  await act(async () => {
    await Promise.resolve()
  })
  const calls = fetcher.mock.calls.map(
    ([path, init]) => `${(init as RequestInit)?.method ?? "GET"} ${path}`
  )
  // Session and capabilities belong to the auth provider the test root mounts,
  // not to this component. Everything else would be this component's doing.
  const owned = calls.filter(
    (call) =>
      !call.startsWith("GET /api/neev/session") &&
      !call.startsWith("GET /api/workos/capabilities")
  )
  expect(owned).toEqual([])
})

it("renders no second title, because upstream owns the title", async () => {
  alone()
  await screen.findByTitle("Design document")
  // The frame's accessible name carries the title. Nothing above it repeats it
  // as a heading or offers a second editable field backed by a different store.
  expect(screen.queryByRole("heading")).not.toBeInTheDocument()
  expect(screen.queryByRole("textbox")).not.toBeInTheDocument()
})

it("offers a retry rather than a blank panel when the frame never loads", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true })
  alone(resource, 1000)
  await screen.findByTitle("Design document")
  await act(async () => {
    vi.advanceTimersByTime(1500)
  })
  expect(await screen.findByRole("alert")).toBeInTheDocument()
  expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument()
})

it("renders nothing for a resource from another organization", async () => {
  mockNativeSession()
  const { container } = render(
    <NativeTestRoot>
      <EngineDocument resource={{ ...resource, orgId: 99 }} onBack={vi.fn()} />
    </NativeTestRoot>
  )
  await act(async () => {
    await Promise.resolve()
  })
  expect(container.querySelector("iframe")).toBeNull()
})

it("goes back without touching the document", async () => {
  const user = userEvent.setup()
  const onBack = vi.fn()
  mockNativeSession()
  render(
    <NativeTestRoot>
      <EngineDocument resource={resource} onBack={onBack} />
    </NativeTestRoot>
  )
  await user.click(
    await screen.findByRole("button", { name: "Back to folder" })
  )
  expect(onBack).toHaveBeenCalledTimes(1)
})

// ---------------------------------------------------------------------------
// The title is upstream's, so Neev has to go and ask what it became.
// ---------------------------------------------------------------------------

/**
 * Renaming happens inside the frame, where Neev cannot see it. Coming back out
 * of the editor is the cue to re-read the resource, so the tab and the sidebar
 * stop showing the old name without anyone reloading the page.
 */
it("re-reads the title after focus comes back out of the frame", async () => {
  const onChanged = vi.fn()
  const renamed = { ...resource, title: "Renamed upstream" }
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(renamed),
  })
  render(
    <NativeTestRoot>
      <EngineDocument
        resource={resource}
        onBack={vi.fn()}
        onChanged={onChanged}
      />
    </NativeTestRoot>
  )
  const frame = await screen.findByTitle("Design document")
  // Focus moves into the frame: the parent window blurs while the iframe is
  // what holds focus.
  frame.focus()
  await act(async () => {
    window.dispatchEvent(new Event("blur"))
  })
  expect(onChanged).not.toHaveBeenCalled()
  // And back out again.
  await act(async () => {
    window.dispatchEvent(new Event("focus"))
  })
  await waitFor(() =>
    expect(onChanged).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Renamed upstream" })
    )
  )
})

it("does not re-read anything when focus never entered the frame", async () => {
  const onChanged = vi.fn()
  const { fetcher } = mockNativeSession()
  render(
    <NativeTestRoot>
      <EngineDocument
        resource={resource}
        onBack={vi.fn()}
        onChanged={onChanged}
      />
    </NativeTestRoot>
  )
  await screen.findByTitle("Design document")
  // A window focus event on its own is somebody returning to the browser tab,
  // not somebody leaving the editor. Refreshing on it would turn every tab
  // switch into a request.
  await act(async () => {
    window.dispatchEvent(new Event("focus"))
  })
  expect(onChanged).not.toHaveBeenCalled()
  expect(
    fetcher.mock.calls.some(([path]) =>
      String(path).includes(`/resources/${resource.id}`)
    )
  ).toBe(false)
})

it("ignores a title the frame tries to announce itself", async () => {
  const onChanged = vi.fn()
  mockNativeSession()
  render(
    <NativeTestRoot>
      <EngineDocument
        resource={resource}
        onBack={vi.fn()}
        onChanged={onChanged}
      />
    </NativeTestRoot>
  )
  await screen.findByTitle("Design document")
  // Believing this would let a document's contents rename it in Neev's tabs
  // and sidebar. The title is read back from Neev's own API or not at all.
  await act(async () => {
    window.dispatchEvent(
      new MessageEvent("message", {
        data: { type: "title", title: "Attacker chosen" },
      })
    )
  })
  expect(onChanged).not.toHaveBeenCalled()
})

// ---------------------------------------------------------------------------
// Sharing stays on the Neev side, because upstream's is hidden in embed mode.
// ---------------------------------------------------------------------------

/**
 * The embedded build hides Docs' own sharing and public-link UI, so if Neev did
 * not offer sharing here a newly-created private document could not be shared
 * with the organization at all — it would simply be unreachable by anyone else.
 */
/**
 * The embedded build hides Docs' own sharing and public-link UI, so if Neev did
 * not offer sharing here a newly-created private document could not be shared
 * with the organization at all — it would simply be unreachable by anyone else.
 *
 * Driven through the content view rather than the component alone, because the
 * menu portals into a container the surrounding surface provides.
 */
it("keeps the sharing, archive and move actions Neev owns", async () => {
  const user = userEvent.setup()
  await openEngineDocument(user)
  expect(
    screen.getByRole("button", { name: "Share with organization" })
  ).toBeInTheDocument()
  await openContentMenu()
  expect(
    await screen.findByRole("menuitem", { name: "Archive" })
  ).toBeInTheDocument()
  expect(screen.getByRole("menuitem", { name: "Move" })).toBeInTheDocument()
})

it("offers no rename, because renaming belongs to the editor", async () => {
  const user = userEvent.setup()
  await openEngineDocument(user)
  await openContentMenu()
  await screen.findByRole("menuitem", { name: "Move" })
  // Two ways to rename one document, backed by two different stores, is two
  // names that can disagree.
  expect(
    screen.queryByRole("menuitem", { name: "Rename" })
  ).not.toBeInTheDocument()
})

/**
 * Archiving and moving are refused server-side for anyone but the creator, so
 * offering them to an organization member is offering controls that can only
 * fail. The native document gates them the same way.
 */
it("shows an organization member no creator-only actions", async () => {
  const user = userEvent.setup()
  // creatorId 2 is not the signed-in user, so none of those actions are theirs.
  await openEngineDocument(user, { ...resource, creatorId: 2 })
  expect(
    screen.queryByRole("button", { name: "Content actions" })
  ).not.toBeInTheDocument()
  expect(
    screen.queryByRole("button", { name: "Share with organization" })
  ).not.toBeInTheDocument()
})

// ---------------------------------------------------------------------------
// Which branch the content view picks. The server decides; the client obeys.
// ---------------------------------------------------------------------------

function openIn(document_: NativeResource) {
  const { fetcher } = mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [document_], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () => json({ items: [], nextAfter: null }),
    [`GET /api/neev/resources/${document_.id}`]: () => json(document_),
    [`GET /api/neev/documents/${document_.id}/revisions`]: () =>
      json({ items: [], nextAfter: null }),
    [`GET /api/neev/documents/${document_.id}/content`]: () =>
      json({
        revision: {
          format: "markdown",
          formatVersion: 1,
          revision: "1",
          authorId: 1,
          authorName: "Sagar",
          byteLength: "4",
          sha256: "a".repeat(64),
          createdAt: 1_700_000_000,
          parentRevision: null,
          restoredFrom: null,
        },
        markdown: "text",
        document: null,
      }),
  })
  render(
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
  return fetcher
}

/**
 * Open an engine document through the content view, which is what gives the
 * header menu the portal container it renders into.
 */
async function openEngineDocument(
  user: ReturnType<typeof userEvent.setup>,
  value: NativeResource = resource
) {
  const fetcher = openIn(value)
  await user.click(await screen.findByRole("tab", { name: "Documents" }))
  await user.click(
    await screen.findByRole("button", { name: /Design document/ })
  )
  await screen.findByTitle(value.title)
  return fetcher
}

it("opens an engine document in the frame, not the native editor", async () => {
  const user = userEvent.setup()
  openIn(resource)
  await user.click(await screen.findByRole("tab", { name: "Documents" }))
  await user.click(
    await screen.findByRole("button", { name: /Design document/ })
  )
  expect(await screen.findByTitle("Design document")).toHaveAttribute(
    "src",
    `/document-engine/docs/${resource.id}/`
  )
})

/**
 * Legacy documents are never migrated, so the native path has to keep working
 * unchanged — both when the server says `native` and when it says nothing at
 * all, which is what an older server does.
 */
async function opensNatively(legacy: NativeResource) {
  const user = userEvent.setup()
  openIn(legacy)
  await user.click(await screen.findByRole("tab", { name: "Documents" }))
  await user.click(
    await screen.findByRole("button", { name: /Design document/ })
  )
  // The native editor draws the title as a control of its own; the frame does
  // not exist at all.
  expect(await screen.findByDisplayValue("Design document")).toBeInTheDocument()
  expect(document.querySelector("iframe")).toBeNull()
}

it("opens a document the server calls native in the native editor", async () => {
  await opensNatively({ ...resource, engine: "native" })
})

it("opens a document with no engine at all in the native editor", async () => {
  // What an older server sends. It must degrade to the existing editor rather
  // than to a blank frame.
  const legacy: NativeResource = { ...resource }
  delete legacy.engine
  await opensNatively(legacy)
})

// ---------------------------------------------------------------------------
// The parser. Adding a field to a response must not break existing clients,
// and an engine this client cannot render must not be quietly mistaken for one
// it can.
// ---------------------------------------------------------------------------

const wire = {
  id: resource.id,
  orgId: 3,
  nodeId: 4,
  kind: "document",
  creatorId: 1,
  title: "Design document",
  visibility: "private",
  originChatId: null,
  authRevision: "1",
  contentRevision: "1",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
}

it("accepts a document carrying engine", () => {
  expect(parseResource({ ...wire, engine: "lasuite" }).engine).toBe("lasuite")
  expect(parseResource({ ...wire, engine: "native" }).engine).toBe("native")
})

it("accepts a chat carrying engine", () => {
  // Chats are never engine documents, but a server that stamps the field onto
  // every resource must not take chats down with it.
  const chat = { ...wire, kind: "chat", contentRevision: "0" }
  expect(parseResource({ ...chat, engine: "native" }).engine).toBe("native")
})

it("still accepts a response with no engine at all", () => {
  expect(parseResource(wire).engine).toBeUndefined()
})

it("refuses an engine it cannot render", () => {
  // Coercing an unknown engine to `native` would open Neev's editor against
  // content it cannot read — a blank document where one exists, which invites
  // saving over it. Failing loudly is the safe direction.
  expect(() => parseResource({ ...wire, engine: "quip" })).toThrow()
  expect(() => parseResource({ ...wire, engine: null })).toThrow()
})

it("still refuses a genuinely unexpected field", () => {
  // The strictness that makes the above safe has to survive it.
  expect(() => parseResource({ ...wire, surprise: 1 })).toThrow()
  expect(() =>
    parseResource({ ...wire, engine: "lasuite", surprise: 1 })
  ).toThrow()
})

it("still refuses a response missing a required field", () => {
  const missing: Record<string, unknown> = { ...wire }
  delete missing.title
  expect(() => parseResource(missing)).toThrow()
  expect(() => parseResource({ ...missing, engine: "lasuite" })).toThrow()
})
