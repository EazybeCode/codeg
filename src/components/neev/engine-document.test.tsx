import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { EngineDocument } from "./engine-document"
import { DocumentChromeSlot } from "./document-chrome-slot"
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
  // Undo any activeElement stub, or the next test inherits a frame that is
  // permanently "focused".
  delete (document as unknown as { activeElement?: unknown }).activeElement
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

/**
 * A frame that does not come up must read as retryable rather than as a blank
 * panel with nothing to press.
 *
 * Driven through the marker the proxy puts on its error pages, because that is
 * the real failure: an HTTP error is a response, so the frame loads it and
 * `onError` never fires. An earlier version waited for a load that never came,
 * which only held while jsdom happened not to fire one.
 */
it("offers a retry when the frame loads an engine error page", async () => {
  alone()
  const frame = (await screen.findByTitle(
    "Design document"
  )) as HTMLIFrameElement
  // Stand in for an error page carrying the marker the component looks for.
  Object.defineProperty(frame, "contentDocument", {
    configurable: true,
    value: {
      querySelector: (selector: string) =>
        selector.includes("neev-document-engine-error") ? {} : null,
    },
  })
  await act(async () => {
    fireEvent.load(frame)
  })
  expect(await screen.findByRole("alert")).toBeInTheDocument()
  expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument()
})

it("stays ready when the frame loads an ordinary page", async () => {
  alone()
  const frame = (await screen.findByTitle(
    "Design document"
  )) as HTMLIFrameElement
  Object.defineProperty(frame, "contentDocument", {
    configurable: true,
    value: { querySelector: () => null },
  })
  await act(async () => {
    fireEvent.load(frame)
  })
  // No marker, no alert — a document that merely mentions an error is still a
  // document, which is why the component reads one marker and never the body.
  expect(screen.queryByRole("alert")).not.toBeInTheDocument()
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

/**
 * The workspace shell keeps the sidebar and the tab strip on screen the whole
 * time, so a Back control there only restated them and cost a row of height.
 * The standalone folder view hides its listing while a resource is open, so
 * there it is still the only way out — which is why this is a prop and not a
 * deletion.
 */
it("offers no way back in the shell, where the sidebar and tabs are the way back", async () => {
  mockNativeSession()
  render(
    <NativeTestRoot>
      <EngineDocument resource={resource} onBack={vi.fn()} />
    </NativeTestRoot>
  )
  await screen.findByTitle("Design document")
  expect(
    screen.queryByRole("button", { name: "Back to folder" })
  ).not.toBeInTheDocument()
})

it("goes back without touching the document, where a way back is asked for", async () => {
  const user = userEvent.setup()
  const onBack = vi.fn()
  mockNativeSession()
  render(
    <NativeTestRoot>
      <EngineDocument resource={resource} onBack={onBack} showBack />
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
  /**
   * Focus moves into the frame: the parent window blurs while the iframe is
   * what holds focus.
   *
   * `activeElement` is asserted rather than assumed. A real browser sets it to
   * the iframe when focus moves inside, but `HTMLElement.focus()` on an iframe
   * does not reliably do so in jsdom — and when it does not, the component
   * correctly decides focus never entered the frame and this test passes while
   * exercising nothing. Stating it keeps the test about the logic.
   */
  Object.defineProperty(document, "activeElement", {
    configurable: true,
    get: () => frame,
  })
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

/**
 * The race that made renaming feel broken: rename, then immediately click
 * something in Neev. The focus-driven read is issued before the save lands, so
 * it answers with the OLD title — and if the editor's confirmation is dropped
 * because that read is still in flight, the stale title is the last word.
 */
it("re-reads again when a confirmation arrives during an in-flight read", async () => {
  const onChanged = vi.fn()
  let release: () => void = () => {}
  const deferred = new Promise<void>((resolve) => (release = resolve))
  let call = 0
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: async () => {
      call += 1
      // The first read is the focus one: issued before the save landed, so it
      // answers with the old title — and only after the confirmation arrives.
      if (call === 1) {
        await deferred
        return json({ ...resource, title: "Design document" })
      }
      return json({ ...resource, title: "Renamed in the editor" })
    },
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

  // Focus leaves and returns: a read starts and does not finish yet.
  frame.focus()
  await act(async () => {
    window.dispatchEvent(new Event("blur"))
  })
  await act(async () => {
    window.dispatchEvent(new Event("focus"))
  })

  // The editor confirms the rename WHILE that read is still in flight.
  await act(async () => {
    window.dispatchEvent(
      new MessageEvent("message", {
        data: { type: "neev-doc-metadata-updated", resourceId: resource.id },
        source: (frame as HTMLIFrameElement).contentWindow,
        origin: window.location.origin,
      })
    )
  })

  // Now let the stale read finish.
  await act(async () => {
    release()
    await Promise.resolve()
  })

  await waitFor(() =>
    expect(onChanged).toHaveBeenLastCalledWith(
      expect.objectContaining({ title: "Renamed in the editor" })
    )
  )
})

it("re-reads on a confirmation from the frame, without waiting for focus", async () => {
  const onChanged = vi.fn()
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () =>
      json({ ...resource, title: "Renamed in the editor" }),
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
  const frame = (await screen.findByTitle(
    "Design document"
  )) as HTMLIFrameElement
  // The frame's window is what the component compares against, and jsdom does
  // not always have it ready the instant the element exists. Waiting for it
  // keeps this a test of the origin check rather than of load timing.
  await waitFor(() => expect(frame.contentWindow).toBeTruthy())
  await act(async () => {
    window.dispatchEvent(
      new MessageEvent("message", {
        data: { type: "neev-doc-metadata-updated", resourceId: resource.id },
        source: frame.contentWindow,
        origin: window.location.origin,
      })
    )
  })
  await waitFor(() =>
    expect(onChanged).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Renamed in the editor" })
    )
  )
})

type SpoofedMessage = {
  origin: string | null
  useSource: boolean
  id?: string
}

it.each<[string, SpoofedMessage]>([
  [
    "a different origin",
    { origin: "https://evil.example.com", useSource: true },
  ],
  ["a window that is not the frame", { origin: null, useSource: false }],
  [
    "a different document's id",
    {
      origin: null,
      useSource: true,
      id: "22222222-2222-4222-8222-222222222222",
    },
  ],
])("ignores a confirmation from %s", async (_label, options) => {
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
  const frame = await screen.findByTitle("Design document")
  await act(async () => {
    window.dispatchEvent(
      new MessageEvent("message", {
        data: {
          type: "neev-doc-metadata-updated",
          resourceId: options.id ?? resource.id,
        },
        source: options.useSource
          ? (frame as HTMLIFrameElement).contentWindow
          : window,
        origin: options.origin ?? window.location.origin,
      })
    )
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
    await screen.findByRole("menuitem", { name: "Move to trash" })
  ).toBeInTheDocument()
  expect(screen.getByRole("menuitem", { name: "Move" })).toBeInTheDocument()
})

/**
 * Rename used to be absent here, on the grounds that upstream owns the title
 * and a second writer for one name is a conflict. That is no longer what this
 * is: the backend projects a native title change into Docs and reconciles on
 * open, so the PATCH is the same writer. What changed the decision was that a
 * reader looking for "rename" looks in a menu, and an empty one reads as "you
 * cannot".
 */
it("offers rename, now that Neev's PATCH is the same writer and not a second one", async () => {
  const user = userEvent.setup()
  await openEngineDocument(user)
  await openContentMenu()
  expect(
    await screen.findByRole("menuitem", { name: "Rename" })
  ).toBeInTheDocument()
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

/** Renders the frame and hands back the frame plus a rerender for the prop. */
function framed(value: NativeResource = resource) {
  mockNativeSession()
  const view = render(
    <NativeTestRoot>
      <EngineDocument resource={value} onBack={vi.fn()} onChanged={vi.fn()} />
    </NativeTestRoot>
  )
  const rerenderWith = (next: NativeResource) =>
    view.rerender(
      <NativeTestRoot>
        <EngineDocument resource={next} onBack={vi.fn()} onChanged={vi.fn()} />
      </NativeTestRoot>
    )
  return { rerenderWith }
}

/** Stand in for the editor announcing that its listener is attached. */
async function announceReady(
  frame: HTMLIFrameElement,
  resourceId = resource.id,
  origin = window.location.origin,
  source: unknown = frame.contentWindow
) {
  await act(async () => {
    window.dispatchEvent(
      new MessageEvent("message", {
        data: { type: "neev-doc-frame-ready", resourceId },
        origin,
        source: source as MessageEventSource,
      })
    )
  })
}

/**
 * A rename from Neev's own chrome must reach the editor, which read the title
 * once when it booted and has no other way to hear about it.
 *
 * Asserted on the message rather than on a reload, because reloading is the
 * thing this must not do: the frame is where the writing is, and a reload
 * during an unsaved edit costs a paragraph to fix a name.
 */
it("asks the frame to re-read after an external rename, without reloading it", async () => {
  const { rerenderWith } = framed()
  const frame = (await screen.findByTitle(
    "Design document"
  )) as HTMLIFrameElement
  const post = vi.fn()
  Object.defineProperty(frame, "contentWindow", {
    configurable: true,
    value: { postMessage: post },
  })
  await act(async () => {
    fireEvent.load(frame)
  })
  await announceReady(frame)

  await act(async () => {
    rerenderWith({ ...resource, title: "Renamed elsewhere", authRevision: "2" })
  })

  expect(post).toHaveBeenCalledWith(
    { type: "neev-doc-refresh-metadata", resourceId: resource.id },
    window.location.origin
  )
  // The frame was not torn down and re-requested.
  expect(screen.getByTitle("Renamed elsewhere")).toBe(frame)
})

/**
 * The rename can land while the editor is still booting, and `load` fires
 * before it has hydrated — so there is a window with no listener on the other
 * side. A fixed delay would be a guess that a slow machine beats; the frame
 * says when it is ready and the held refresh goes then.
 */
it("holds a rename that arrives before the frame is listening, and flushes it on the ready signal", async () => {
  const { rerenderWith } = framed()
  const frame = (await screen.findByTitle(
    "Design document"
  )) as HTMLIFrameElement
  const post = vi.fn()
  Object.defineProperty(frame, "contentWindow", {
    configurable: true,
    value: { postMessage: post },
  })

  // Renamed during boot: loaded, but the editor has not announced itself.
  await act(async () => {
    fireEvent.load(frame)
  })
  await act(async () => {
    rerenderWith({ ...resource, title: "Renamed in flight", authRevision: "2" })
  })
  expect(post).not.toHaveBeenCalled()

  await announceReady(frame)
  expect(post).toHaveBeenCalledTimes(1)
  expect(post).toHaveBeenCalledWith(
    { type: "neev-doc-refresh-metadata", resourceId: resource.id },
    window.location.origin
  )
})

/**
 * The handshake is a message like any other, so it gets the same three checks.
 * Anything else on the page can post, and being believed here would let it
 * drive this frame.
 */
it("ignores a ready signal from the wrong origin, window or document", async () => {
  const { rerenderWith } = framed()
  const frame = (await screen.findByTitle(
    "Design document"
  )) as HTMLIFrameElement
  const post = vi.fn()
  Object.defineProperty(frame, "contentWindow", {
    configurable: true,
    value: { postMessage: post },
  })
  await act(async () => {
    fireEvent.load(frame)
  })
  await act(async () => {
    rerenderWith({ ...resource, title: "Renamed", authRevision: "2" })
  })

  await announceReady(frame, resource.id, "https://evil.example")
  await announceReady(frame, resource.id, window.location.origin, {})
  await announceReady(frame, "99999999-9999-4999-8999-999999999999")
  expect(post).not.toHaveBeenCalled()

  // And the real one still works, so the test cannot pass by never flushing.
  await announceReady(frame)
  expect(post).toHaveBeenCalledTimes(1)
})

it("starts the framed editor's body from the family its title uses", async () => {
  alone()
  const frame = (await screen.findByTitle(
    "Design document"
  )) as HTMLIFrameElement
  const inner = document.implementation.createHTMLDocument("engine")
  Object.defineProperty(frame, "contentDocument", {
    configurable: true,
    value: inner,
  })
  await act(async () => {
    fireEvent.load(frame)
  })

  const style = inner.getElementById("neev-frame-typography")
  expect(style?.textContent).toContain('"Inter Variable"')
  // A default, not an override: anything the editor or the writer states about
  // a particular block still wins.
  expect(style?.textContent).not.toContain("!important")
  // A reload or a retry re-applies it without stacking copies.
  await act(async () => {
    fireEvent.load(frame)
  })
  expect(inner.querySelectorAll("style#neev-frame-typography")).toHaveLength(1)
})

it("renders the frame unchanged when its document cannot be read", async () => {
  alone()
  const frame = (await screen.findByTitle(
    "Design document"
  )) as HTMLIFrameElement
  // What a cross-origin frame does on access. The editor must still be ready.
  Object.defineProperty(frame, "contentDocument", {
    configurable: true,
    get() {
      throw new DOMException("cross-origin", "SecurityError")
    },
  })
  await act(async () => {
    fireEvent.load(frame)
  })
  expect(screen.queryByRole("alert")).not.toBeInTheDocument()
})

// ---------------------------------------------------------------------------
// One row of controls, one menu.
// ---------------------------------------------------------------------------

/**
 * A stand-in for the editor's page, with the options button the embed build
 * draws. Same-origin in the app, so the component reads and drives it directly.
 */
function engineDocumentWith({ trigger = true } = {}) {
  const inner = document.implementation.createHTMLDocument("engine")
  inner.body.innerHTML = trigger
    ? `<div class="--docs--floating-bar"><div class="--docs--card-floating-bar">` +
      `<button class="--docs--drop-button" aria-haspopup="menu"><button>…</button></button>` +
      `</div></div>`
    : `<div class="--docs--floating-bar"></div>`
  const click = vi.fn()
  inner
    .querySelector<HTMLElement>(".--docs--drop-button")
    ?.addEventListener("click", click)
  return { inner, click }
}

async function loadFrameWith(inner: Document) {
  const frame = (await screen.findByTitle(
    "Design document"
  )) as HTMLIFrameElement
  const focus = vi.fn()
  Object.defineProperty(frame, "contentDocument", {
    configurable: true,
    value: inner,
  })
  Object.defineProperty(frame, "contentWindow", {
    configurable: true,
    value: { focus, postMessage: vi.fn() },
  })
  await act(async () => {
    fireEvent.load(frame)
  })
  return { focus }
}

/**
 * The editor's own "…" and Neev's sat a hand's width apart, each holding half
 * of what a reader can do to the document. Neev's is now the one: choosing the
 * editor's options from it opens the editor's menu, and download, print and
 * history are exactly as reachable as before.
 */
it("opens the editor's own options from the one document menu", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true })
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime })
  await openEngineDocument(user)
  const { inner, click } = engineDocumentWith()
  const { focus } = await loadFrameWith(inner)

  await openContentMenu()
  const item = await screen.findByRole("menuitem", {
    name: "Download, print & history…",
  })
  expect(item).not.toHaveAttribute("aria-disabled", "true")
  await act(async () => {
    fireEvent.click(item)
  })
  await act(async () => {
    vi.advanceTimersByTime(1)
  })
  // Focus goes into the editor first, so its menu is not closed by the focus
  // Neev's menu would otherwise restore.
  expect(focus).toHaveBeenCalled()
  expect(click).toHaveBeenCalledTimes(1)
})

/**
 * The editor's button is hidden by the same selector Neev drives, so the two
 * cannot drift apart: what is hidden is exactly what the menu opens.
 */
it("hides the editor's own button, and only that", async () => {
  const user = userEvent.setup()
  await openEngineDocument(user)
  const { inner } = engineDocumentWith()
  await loadFrameWith(inner)
  const style = inner.getElementById("neev-frame-chrome")
  expect(style?.textContent).toContain(
    ".--docs--card-floating-bar .--docs--drop-button{visibility:hidden"
  )
  // The title gets a little room back now that no band sits above the frame.
  expect(style?.textContent).toContain(".--docs--doc-header{padding-top:12px}")
  await act(async () => {
    fireEvent.load(screen.getByTitle("Design document"))
  })
  expect(inner.querySelectorAll("style#neev-frame-chrome")).toHaveLength(1)
})

/**
 * When the editor draws no options button — still loading the document, or a
 * document in the trash — the item says so rather than doing nothing.
 */
it("offers the editor's options only when there is a button to open", async () => {
  const user = userEvent.setup()
  await openEngineDocument(user)
  const { inner } = engineDocumentWith({ trigger: false })
  await loadFrameWith(inner)
  await openContentMenu()
  expect(
    await screen.findByRole("menuitem", { name: "Download, print & history…" })
  ).toHaveAttribute("aria-disabled", "true")
})

/**
 * A member has none of Neev's creator actions, but download, print and history
 * are theirs as much as anyone's — before, they reached them through the
 * editor's own button, which is now hidden.
 */
it("gives an organization member the editor's options and nothing else", async () => {
  const user = userEvent.setup()
  await openEngineDocument(user, { ...resource, creatorId: 2 })
  const { inner } = engineDocumentWith()
  await loadFrameWith(inner)
  const menu = await openContentMenu()
  const items = Array.from(menu.querySelectorAll('[role="menuitem"]')).map(
    (item) => item.textContent
  )
  expect(items).toEqual(["Download, print & history…"])
})

it("puts the controls in the workspace's top row when it offers one", async () => {
  mockNativeSession()
  const slot = document.createElement("div")
  document.body.appendChild(slot)
  render(
    <NativeTestRoot>
      <DocumentChromeSlot.Provider value={slot}>
        <EngineDocument resource={resource} onBack={vi.fn()} showBack />
      </DocumentChromeSlot.Provider>
    </NativeTestRoot>
  )
  const frame = await screen.findByTitle("Design document")
  expect(slot).toContainElement(screen.getByRole("img", { name: "Private" }))
  // No band of its own, and no Back: the workspace has both covered.
  expect(frame.closest("section")?.querySelector("header")).toBeNull()
  expect(
    screen.queryByRole("button", { name: "Back to folder" })
  ).not.toBeInTheDocument()
  slot.remove()
})

it("draws no controls at all for a tab that is not in front", async () => {
  mockNativeSession()
  render(
    <NativeTestRoot>
      <DocumentChromeSlot.Provider value={null}>
        <EngineDocument resource={resource} onBack={vi.fn()} />
      </DocumentChromeSlot.Provider>
    </NativeTestRoot>
  )
  await screen.findByTitle("Design document")
  expect(screen.queryByRole("img", { name: "Private" })).not.toBeInTheDocument()
})
