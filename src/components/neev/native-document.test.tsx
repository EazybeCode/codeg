import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import type { Editor } from "@tiptap/core"
import { NativeDocument } from "./native-document"
import { NativeBoundary } from "./native-boundary"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  orgSession,
} from "./test-helpers"
import type { NativeResource } from "@/lib/neev-content-client"
import { importMarkdown } from "@/lib/neev-rich-adapter"
import { parseRichDocument } from "@/lib/neev-rich-document"
import { useNativeAuth } from "./native-auth-provider"
// jsdom has no hit-testing geometry; actual pointer acceptance runs in browser.
document.elementsFromPoint ??= () => []
vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))
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
}
const revision = {
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
}
const path = `/api/neev/documents/${resource.id}`
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})
function mount(current = resource) {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeDocument
          resource={current}
          onBack={() => {}}
          onChanged={() => {}}
          onDirtyChange={() => {}}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

function LogoutControl() {
  const auth = useNativeAuth()
  return (
    <button onClick={() => void auth.logout()}>Log out test session</button>
  )
}
it("places document toolbar before the editor in keyboard order", async () => {
  const empty = { ...resource, contentRevision: "0" }
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(empty),
    [`GET ${path}/revisions`]: () => json({ items: [], nextAfter: null }),
  })
  mount(empty)
  const editor = await screen.findByRole(
    "textbox",
    { name: "Document content" },
    { timeout: 5000 }
  )
  expect(
    screen
      .getByRole("button", { name: "History" })
      .compareDocumentPosition(editor) & Node.DOCUMENT_POSITION_FOLLOWING
  ).toBeTruthy()
  expect(screen.queryByRole("region", { name: "Preview" })).toBeNull()
})

it("toggles the per-user document width and persists it (default full canvas)", async () => {
  localStorage.clear()
  const empty = { ...resource, contentRevision: "0" }
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(empty),
    [`GET ${path}/revisions`]: () => json({ items: [], nextAfter: null }),
  })
  mount(empty)
  await screen.findByRole(
    "textbox",
    { name: "Document content" },
    { timeout: 5000 }
  )
  const section = screen.getByRole("region", { name: "Design document" })
  // Default is full canvas: the section does not impose the narrow 3xl column.
  expect(section).toHaveAttribute("data-doc-width", "full")
  const toggle = screen.getByRole("button", { name: "Focus width" })
  expect(toggle).toHaveAttribute("aria-pressed", "true")
  // Toggling to Focus restores the narrow reading column and persists per user.
  await userEvent.click(toggle)
  expect(section).toHaveAttribute("data-doc-width", "focus")
  expect(
    screen.getByRole("button", { name: "Full width" })
  ).toHaveAttribute("aria-pressed", "false")
  expect(localStorage.getItem("neev.ws.doc-width")).toBe("focus")
})

async function editRich(text: string) {
  // The real editor is code-split; cold transformation in the full suite can
  // exceed Testing Library's one-second default. Still wait for the actual UI.
  const editor = await screen.findByRole(
    "textbox",
    { name: "Document content" },
    { timeout: 5000 }
  )
  const paragraph = editor.querySelector("p")
  expect(paragraph).not.toBeNull()
  await act(async () => {
    paragraph!.textContent = text
    fireEvent.input(paragraph!, { inputType: "insertText", data: text })
  })
  return editor
}

it("opens an unsaved document as rich and saves/reloads its canonical body with the keyboard", async () => {
  let latest = { ...resource, contentRevision: "0" }
  let savedDocument: unknown
  let savedRevision = { ...revision, format: "blocks", byteLength: "0" }
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(latest),
    [`GET ${path}/revisions`]: () => json({ items: [], nextAfter: null }),
    [`POST ${path}/revisions`]: (init) => {
      const input = JSON.parse(init.body as string)
      expect(input).toMatchObject({
        expectedAuthRevision: "1",
        expectedContentRevision: "0",
      })
      expect(input).not.toHaveProperty("markdown")
      savedDocument = input.document
      savedRevision = {
        ...savedRevision,
        byteLength: String(
          new TextEncoder().encode(JSON.stringify(savedDocument)).length
        ),
      }
      latest = { ...latest, contentRevision: "1" }
      return json(savedRevision, 201)
    },
    [`GET ${path}/content?revision=1`]: () =>
      json({
        revision: savedRevision,
        document: savedDocument,
        markdown: "Fresh rich\n",
      }),
  })
  const user = userEvent.setup()
  const first = mount(latest)
  const editor = await editRich("Fresh rich")
  fireEvent.keyDown(editor, { key: "s", ctrlKey: true })
  expect(await screen.findByText("Saved")).toBeVisible()
  expect(savedDocument).toMatchObject({
    version: 1,
    blocks: [
      {
        type: "paragraph",
        content: [{ text: "Fresh rich", styles: [], href: null }],
      },
    ],
  })
  first.unmount()
  mount(latest)
  expect(
    await screen.findByRole("textbox", { name: "Document content" })
  ).toHaveTextContent("Fresh rich")
  await user.click(screen.getByRole("button", { name: "History" }))
  expect(mock.unknown).toEqual([])
})

it.each([409, 400, "lost"] as const)(
  "preserves rich draft/preconditions and exact retry after %s save",
  async (failure) => {
    const document = importMarkdown("Seed rich")
    const richRevision = {
      ...revision,
      format: "blocks",
      byteLength: String(
        new TextEncoder().encode(JSON.stringify(document)).length
      ),
    }
    const bodies: Record<string, unknown>[] = []
    mockNativeSession(undefined, {
      [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
      [`GET ${path}/content?revision=1`]: () =>
        json({ revision: richRevision, document, markdown: "Seed rich\n" }),
      [`GET ${path}/revisions`]: () =>
        json({ items: [richRevision], nextAfter: null }),
      [`POST ${path}/revisions`]: (init) => {
        bodies.push(JSON.parse(init.body as string))
        if (failure === "lost") throw new TypeError("lost response")
        return json(
          { code: failure === 409 ? "conflict" : "invalid_request" },
          failure
        )
      },
    })
    const user = userEvent.setup()
    mount()
    const editor = await editRich("My rich draft")
    await user.click(screen.getByRole("button", { name: "Save" }))
    expect(await screen.findByRole("alert")).toBeVisible()
    expect(editor).toHaveTextContent("My rich draft")
    await user.click(screen.getByRole("button", { name: "Save" }))
    expect(bodies).toHaveLength(2)
    expect(bodies[1]).toEqual(bodies[0])
    expect(bodies[1]).toMatchObject({
      expectedContentRevision: "1",
      expectedAuthRevision: "1",
      document: { blocks: [{ content: [{ text: "My rich draft" }] }] },
    })
  }
)

it.each([false, true])(
  "previews Markdown conversion without saving and refuses unsupported content (%s)",
  async (unsupported) => {
    const markdown = unsupported
      ? "![image](https://example.test/a)"
      : "# Original"
    let posted = false
    mockNativeSession(undefined, {
      [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
      [`GET ${path}/content?revision=1`]: () =>
        json({
          revision: { ...revision, byteLength: String(markdown.length) },
          document: null,
          markdown,
        }),
      [`GET ${path}/revisions`]: () =>
        json({ items: [revision], nextAfter: null }),
      [`POST ${path}/revisions`]: () => {
        posted = true
        return json({}, 500)
      },
    })
    const user = userEvent.setup()
    mount()
    await screen.findByRole("textbox", { name: "Document content" })
    await user.click(
      screen.getByRole("button", { name: "Convert to rich document" })
    )
    if (unsupported) {
      expect(await screen.findByRole("alert")).toHaveTextContent(
        /Conversion refused/
      )
      expect(screen.getByDisplayValue(markdown)).toHaveValue(markdown)
    } else {
      const dialog = await screen.findByRole("dialog")
      expect(
        within(dialog).getByRole("heading", { name: "Original" })
      ).toBeVisible()
      expect(screen.getByDisplayValue(markdown)).toHaveValue(markdown)
      await user.click(
        within(dialog).getByRole("button", { name: "Confirm conversion" })
      )
      expect(
        await screen.findByRole("textbox", { name: "Document content" })
      ).toHaveTextContent("Original")
      expect(screen.getByText("Unsaved changes")).toBeVisible()
    }
    expect(posted).toBe(false)
  }
)

it("copies the dirty rich JSON separately from explicitly labeled saved-only Markdown", async () => {
  const document = importMarkdown("Saved rich")
  const richRevision = {
    ...revision,
    format: "blocks",
    byteLength: String(
      new TextEncoder().encode(JSON.stringify(document)).length
    ),
  }
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}/content?revision=1`]: () =>
      json({ revision: richRevision, document, markdown: "Saved rich\n" }),
    [`GET ${path}/revisions`]: () =>
      json({ items: [richRevision], nextAfter: null }),
  })
  const user = userEvent.setup()
  const copy = vi.spyOn(navigator.clipboard, "writeText")
  mount()
  await editRich("Unsaved rich")
  await user.click(screen.getByRole("button", { name: "Copy structured JSON" }))
  expect(JSON.parse(copy.mock.lastCall![0])).toMatchObject({
    blocks: [{ content: [{ text: "Unsaved rich" }] }],
  })
  await user.click(
    screen.getByRole("button", {
      name: /Export saved Markdown.*revision 1.*unsaved changes excluded/i,
    })
  )
  expect(copy.mock.lastCall![0]).toBe("Saved rich\n")
})

it("copies a boundary-size dirty rich draft as bounded, valid recovery JSON", async () => {
  const current = { ...resource, contentRevision: "0" }
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(current),
    [`GET ${path}/revisions`]: () => json({ items: [], nextAfter: null }),
  })
  const user = userEvent.setup()
  const copy = vi.spyOn(navigator.clipboard, "writeText")
  mount(current)
  // One paragraph/run with a 36-character UUID has 151 bytes of JSON overhead.
  const text = "x".repeat(1_048_576 - 151)
  await editRich(text)
  await user.click(screen.getByRole("button", { name: "Copy structured JSON" }))
  const copied = copy.mock.lastCall![0]
  expect(new TextEncoder().encode(copied).length).toBe(1_048_576)
  expect(parseRichDocument(JSON.parse(copied)).blocks).toMatchObject([
    { type: "paragraph", content: [{ text, styles: [], href: null }] },
  ])
})

it("renders mixed history and restores the selected Markdown format while preserving source audience", async () => {
  const document = importMarkdown("Rich current")
  const richRevision = {
    ...revision,
    format: "blocks",
    revision: "2",
    parentRevision: "1",
    byteLength: String(
      new TextEncoder().encode(JSON.stringify(document)).length
    ),
  }
  const current = {
    ...resource,
    contentRevision: "2",
    visibility: "organization" as const,
    originChatId: "22222222-2222-4222-8222-222222222222",
  }
  let restore: unknown
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(current),
    [`GET ${path}/content?revision=2`]: () =>
      json({ revision: richRevision, document, markdown: "Rich current\n" }),
    [`GET ${path}/content?revision=1`]: () =>
      json({ revision, document: null, markdown: "seed" }),
    [`GET ${path}/revisions`]: () =>
      json({ items: [revision, richRevision], nextAfter: null }),
    [`POST ${path}/restore`]: (init) => {
      restore = JSON.parse(init.body as string)
      return json(
        { ...revision, revision: "3", parentRevision: "2", restoredFrom: "1" },
        201
      )
    },
  })
  const user = userEvent.setup()
  mount(current)
  await screen.findByRole("textbox", { name: "Document content" })
  expect(screen.getByText(/Audience follows source chat/)).toBeVisible()
  await user.click(screen.getByRole("button", { name: "History" }))
  const history = screen.getByRole("region", { name: "History" })
  expect(history).toHaveTextContent("Rich document")
  expect(history).toHaveTextContent("Markdown")
  await user.click(screen.getByRole("button", { name: "Compare revision 1" }))
  expect(await screen.findByRole("dialog")).toHaveTextContent("Rich current")
  await user.click(
    screen.getByRole("button", { name: "Restore as new revision" })
  )
  expect(
    await screen.findByRole("textbox", { name: "Document content" })
  ).toHaveValue("seed")
  expect(restore).toMatchObject({
    sourceRevision: "1",
    expectedContentRevision: "2",
    expectedAuthRevision: "1",
  })
})

it.each(["navigation", "logout"])(
  "ignores a rich save response after %s invalidates its scope",
  async (action) => {
    const document = importMarkdown("Private rich")
    const richRevision = {
      ...revision,
      format: "blocks",
      byteLength: String(
        new TextEncoder().encode(JSON.stringify(document)).length
      ),
    }
    const other = {
      ...resource,
      id: "44444444-4444-4444-8444-444444444444",
      title: "Other document",
    }
    let release!: (value: Response) => void
    const changed = vi.fn()
    mockNativeSession(undefined, {
      [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
      [`GET ${path}/content?revision=1`]: () =>
        json({ revision: richRevision, document, markdown: "Private rich\n" }),
      [`GET ${path}/revisions`]: () =>
        json({ items: [richRevision], nextAfter: null }),
      [`POST ${path}/revisions`]: () =>
        new Promise((resolve) => {
          release = resolve
        }),
      [`GET /api/neev/resources/${other.id}`]: () => json(other),
      [`GET /api/neev/documents/${other.id}/content?revision=1`]: () =>
        json({ revision, document: null, markdown: "safe" }),
      [`GET /api/neev/documents/${other.id}/revisions`]: () =>
        json({ items: [revision], nextAfter: null }),
      "POST /api/neev/logout": () => new Response(null, { status: 204 }),
    })
    const user = userEvent.setup()
    const renderDocument = (current: NativeResource) => (
      <NativeTestRoot>
        <LogoutControl />
        <NativeBoundary>
          <NativeDocument
            resource={current}
            onBack={() => {}}
            onChanged={changed}
            onDirtyChange={() => {}}
          />
        </NativeBoundary>
      </NativeTestRoot>
    )
    const view = render(renderDocument(resource))
    const richTextbox = await editRich("Unsaved secret")
    const retained = (richTextbox as HTMLElement & { editor: Editor }).editor
    await user.click(screen.getByRole("button", { name: "Save" }))
    await waitFor(() => expect(release).toBeDefined())
    if (action === "navigation") {
      view.rerender(renderDocument(other))
      expect(
        await screen.findByRole("textbox", { name: "Document content" })
      ).toHaveValue("safe")
    } else
      await user.click(
        screen.getByRole("button", { name: "Log out test session" })
      )
    changed.mockClear()
    await act(async () => {
      release(
        json({ ...richRevision, revision: "2", parentRevision: "1" }, 201)
      )
    })
    expect(changed).not.toHaveBeenCalled()
    expect(screen.queryByText("Unsaved secret")).toBeNull()
    expect(screen.queryByText("Saved")).toBeNull()
    await waitFor(() => expect(retained.state.doc.textContent).toBe(""))
    expect(retained.state.plugins).toHaveLength(0)
    expect(JSON.stringify(retained.options.content)).not.toContain(
      "Private rich"
    )
    expect(
      (window as Window & { ProseMirror?: Editor }).ProseMirror === retained
    ).toBe(false)
  }
)

it.each(["content", "audience"])(
  "pins dirty %s revisions through polling and retains a 409 draft for deliberate comparison",
  async (change) => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
    let latest = resource
    let body: unknown
    const mock = mockNativeSession(undefined, {
      [`GET /api/neev/resources/${resource.id}`]: () => json(latest),
      [`GET ${path}/content?revision=1`]: () =>
        json({ document: null, revision, markdown: "seed" }),
      [`GET ${path}/content?revision=2`]: () =>
        json({
          revision: { ...revision, revision: "2", byteLength: "6" },
          document: null,
          markdown: "latest",
        }),
      [`GET ${path}/revisions`]: () =>
        json({ items: [revision], nextAfter: null }),
      [`GET ${path}/revisions?after=1`]: () =>
        json({ items: [{ ...revision, revision: "2" }], nextAfter: null }),
      [`POST ${path}/revisions`]: (init) => {
        body = JSON.parse(init.body as string)
        return json({ code: "conflict" }, 409)
      },
    })
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime })
    mount()
    const editor = await screen.findByRole("textbox", {
      name: "Document content",
    })
    await user.clear(editor)
    await user.type(editor, "my unsaved changes")
    latest =
      change === "content"
        ? { ...resource, contentRevision: "2" }
        : { ...resource, authRevision: "2", visibility: "organization" }
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000)
    })
    expect(
      screen.getByRole("button", { name: "Compare with latest" })
    ).toBeVisible()
    await user.click(screen.getByRole("button", { name: "Save" }))
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "This document changed since you started editing. Your draft is preserved. Compare it with the latest version before saving again."
    )
    expect(screen.getByRole("alert")).not.toHaveTextContent(/sign-in method/i)
    expect(editor).toHaveValue("my unsaved changes")
    expect(body).toEqual({
      requestId: expect.any(String),
      expectedContentRevision: "1",
      expectedAuthRevision: "1",
      markdown: "my unsaved changes",
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000)
    })
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Your draft is preserved"
    )
    await user.click(
      screen.getByRole("button", { name: "Compare with latest" })
    )
    expect(await screen.findByRole("dialog")).toHaveTextContent(
      "my unsaved changes"
    )
    expect(screen.getByRole("dialog")).toHaveTextContent("Neev demo")
    expect(mock.unknown).toEqual([])
  }
)

it("uses the save shortcut and reuses the exact request after a lost save response", async () => {
  const bodies: Record<string, unknown>[] = []
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}/content?revision=1`]: () =>
      json({ document: null, revision, markdown: "seed" }),
    [`GET ${path}/revisions`]: () =>
      json({ items: [revision], nextAfter: null }),
    [`POST ${path}/revisions`]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      if (bodies.length === 1) throw new TypeError("lost response")
      return json(
        { ...revision, revision: "2", byteLength: "5", parentRevision: "1" },
        201
      )
    },
  })
  const user = userEvent.setup()
  mount()
  await user.type(
    await screen.findByRole("textbox", { name: "Document content" }),
    "!"
  )
  await user.keyboard("{Control>}s{/Control}")
  expect(await screen.findByRole("alert")).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Save" }))
  expect(await screen.findByText("Saved")).toBeVisible()
  expect(bodies).toHaveLength(2)
  expect(bodies[1]).toEqual(bodies[0])
  expect(mock.unknown).toEqual([])
})

it("lets the author accept a newer content/audience basis without losing their draft", async () => {
  let latest = resource
  const bodies: Record<string, unknown>[] = []
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(latest),
    [`GET ${path}/content?revision=1`]: () =>
      json({ document: null, revision, markdown: "seed" }),
    [`GET ${path}/content?revision=2`]: () =>
      json({
        revision: { ...revision, revision: "2", byteLength: "6" },
        document: null,
        markdown: "latest",
      }),
    [`GET ${path}/revisions`]: () =>
      json({ items: [revision], nextAfter: null }),
    [`POST ${path}/revisions`]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      return bodies.length === 1
        ? json({ code: "conflict" }, 409)
        : json(
            {
              ...revision,
              revision: "3",
              parentRevision: "2",
              byteLength: "5",
            },
            201
          )
    },
  })
  const user = userEvent.setup()
  mount()
  const editor = await screen.findByRole("textbox", {
    name: "Document content",
  })
  await user.type(editor, "!")
  latest = {
    ...resource,
    authRevision: "2",
    contentRevision: "2",
    visibility: "organization",
  }
  await user.click(screen.getByRole("button", { name: "Save" }))
  await user.click(
    await screen.findByRole("button", { name: "Compare with latest" })
  )
  await user.click(
    await screen.findByRole("button", {
      name: "Keep draft with latest revisions",
    })
  )
  expect(editor).toHaveValue("seed!")
  await user.click(screen.getByRole("button", { name: "Save" }))
  expect(await screen.findByText("Saved")).toBeVisible()
  expect(bodies[1]).toEqual({
    requestId: expect.any(String),
    markdown: "seed!",
    expectedContentRevision: "2",
    expectedAuthRevision: "2",
  })
  expect(bodies[1].requestId).not.toBe(bodies[0].requestId)
  expect(mock.unknown).toEqual([])
})

it("compares escaped historical text and restores it as a new revision with retry idempotency", async () => {
  const oldText = '<img src="https://example.com/track">'
  const old = {
    ...revision,
    revision: "2",
    byteLength: String(new TextEncoder().encode(oldText).length),
  }
  const bodies: Record<string, unknown>[] = []
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}/content?revision=1`]: () =>
      json({ document: null, revision, markdown: "seed" }),
    [`GET ${path}/content?revision=2`]: () =>
      json({ document: null, revision: old, markdown: oldText }),
    [`GET ${path}/revisions`]: () =>
      json({ items: [revision, old], nextAfter: null }),
    [`POST ${path}/restore`]: (init) => {
      bodies.push(JSON.parse(init.body as string))
      if (bodies.length === 1) throw new TypeError("lost response")
      return json(
        { ...old, revision: "3", parentRevision: "1", restoredFrom: "2" },
        201
      )
    },
  })
  const user = userEvent.setup()
  mount()
  await screen.findByRole("textbox", { name: "Document content" })
  await user.click(screen.getByRole("button", { name: "History" }))
  expect(document.querySelector("time")).toHaveAttribute(
    "datetime",
    "2023-11-14T22:13:20.000Z"
  )
  await user.click(screen.getByRole("button", { name: "Compare revision 2" }))
  expect(await screen.findByRole("dialog")).toHaveTextContent(oldText)
  expect(document.querySelector("img")).toBeNull()
  await user.click(
    screen.getByRole("button", { name: "Restore as new revision" })
  )
  expect(
    await within(screen.getByRole("dialog")).findByRole("alert")
  ).toBeVisible()
  await user.click(
    screen.getByRole("button", { name: "Restore as new revision" })
  )
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull())
  expect(screen.getByRole("textbox", { name: "Document content" })).toHaveValue(
    oldText
  )
  expect(screen.getByText(/Restored from 2/)).toBeVisible()
  expect(bodies[0]).toEqual({
    requestId: expect.any(String),
    sourceRevision: "2",
    expectedContentRevision: "1",
    expectedAuthRevision: "1",
  })
  expect(bodies[1]).toEqual(bodies[0])
  expect(mock.unknown).toEqual([])
})

it("returns focus to the revision trigger when Escape closes comparison", async () => {
  const oldText = "old text"
  const old = {
    ...revision,
    revision: "2",
    byteLength: String(new TextEncoder().encode(oldText).length),
  }
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}/content?revision=1`]: () =>
      json({ document: null, revision, markdown: "seed" }),
    [`GET ${path}/content?revision=2`]: () =>
      json({ document: null, revision: old, markdown: oldText }),
    [`GET ${path}/revisions`]: () =>
      json({ items: [revision, old], nextAfter: null }),
  })
  const user = userEvent.setup()
  mount()
  await screen.findByRole("textbox", { name: "Document content" })
  await user.click(screen.getByRole("button", { name: "History" }))
  const trigger = screen.getByRole("button", { name: "Compare revision 2" })
  trigger.focus()
  await user.keyboard("{Enter}")
  expect(await screen.findByRole("dialog")).toBeVisible()
  await user.keyboard("{Escape}")
  expect(screen.queryByRole("dialog")).toBeNull()
  expect(trigger).toHaveFocus()
  expect(mock.unknown).toEqual([])
})

it.each([
  [403, false],
  [404, false],
  [403, true],
  [404, true],
] as const)(
  "erases loaded document, draft, export, and history on %s (rich=%s) and stops polling",
  async (status, rich) => {
    const document = rich ? importMarkdown("seed") : null
    const bodyRevision = document
      ? {
          ...revision,
          format: "blocks",
          byteLength: String(
            new TextEncoder().encode(JSON.stringify(document)).length
          ),
        }
      : revision
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
    let revoked = false,
      calls = 0,
      bootstraps = 0
    const mock = mockNativeSession(undefined, {
      "GET /api/neev/session": () => {
        bootstraps += 1
        return status === 403 && revoked
          ? json({ code: "unauthorized" }, 401)
          : json(orgSession)
      },
      [`GET /api/neev/resources/${resource.id}`]: () => {
        calls++
        return revoked
          ? json({ code: status === 403 ? "forbidden" : "not_found" }, status)
          : json(resource)
      },
      [`GET ${path}/content?revision=1`]: () =>
        json({ document, revision: bodyRevision, markdown: "seed" }),
      [`GET ${path}/revisions`]: () =>
        json({ items: [revision], nextAfter: null }),
    })
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime })
    mount()
    if (rich) await editRich("seed secret draft")
    else
      await user.type(
        await screen.findByRole("textbox", { name: "Document content" }),
        " secret draft"
      )
    const retained = rich
      ? (
          screen.getByRole("textbox", {
            name: "Document content",
          }) as HTMLElement & { editor: Editor }
        ).editor
      : null
    await user.click(screen.getByRole("button", { name: "History" }))
    const comparisonTrigger = screen.getByRole("button", {
      name: "Compare revision 1",
    })
    comparisonTrigger.focus()
    await user.keyboard("{Enter}")
    expect(await screen.findByRole("dialog")).toBeVisible()
    revoked = true
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000)
    })
    expect(screen.queryByDisplayValue("seed secret draft")).toBeNull()
    expect(screen.queryByText("seed secret draft")).toBeNull()
    expect(
      screen.queryByRole("textbox", { name: "Document content" })
    ).toBeNull()
    expect(
      screen.queryByRole("button", { name: /Export saved Markdown/ })
    ).toBeNull()
    expect(screen.queryByText("Sagar")).toBeNull()
    if (retained) {
      await waitFor(() => expect(retained.state.doc.textContent).toBe(""))
      expect(retained.state.plugins).toHaveLength(0)
      expect(JSON.stringify(retained.options.content)).not.toContain("seed")
      expect(
        (window as Window & { ProseMirror?: Editor }).ProseMirror === retained
      ).toBe(false)
    }
    expect(comparisonTrigger).not.toHaveFocus()
    expect(bootstraps).toBe(status === 403 ? 2 : 1)
    const count = calls
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15000)
    })
    expect(calls).toBe(count)
    expect(mock.unknown).toEqual([])
  }
)

it("rejects a deferred body from a previous document after the new document has loaded", async () => {
  const other = {
    ...resource,
    id: "44444444-4444-4444-8444-444444444444",
    title: "Other document",
  }
  let release!: (value: Response) => void
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}/revisions`]: () =>
      json({ items: [revision], nextAfter: null }),
    [`GET ${path}/content?revision=1`]: () =>
      new Promise((resolve) => {
        release = resolve
      }),
    [`GET /api/neev/resources/${other.id}`]: () => json(other),
    [`GET /api/neev/documents/${other.id}/revisions`]: () =>
      json({ items: [revision], nextAfter: null }),
    [`GET /api/neev/documents/${other.id}/content?revision=1`]: () =>
      json({ document: null, revision, markdown: "safe" }),
  })
  const renderDoc = (value: NativeResource) => (
    <NativeTestRoot>
      <NativeBoundary>
        <NativeDocument
          resource={value}
          onBack={() => {}}
          onChanged={() => {}}
          onDirtyChange={() => {}}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
  const view = render(renderDoc(resource))
  await waitFor(() => expect(release).toBeDefined())
  view.rerender(renderDoc(other))
  expect(
    await screen.findByRole("textbox", { name: "Document content" })
  ).toHaveValue("safe")
  await act(async () => {
    release(
      json({
        revision: { ...revision, byteLength: "27" },
        document: null,
        markdown: "other organization's secret",
      })
    )
  })
  expect(screen.getByRole("textbox", { name: "Document content" })).toHaveValue(
    "safe"
  )
  expect(screen.queryByText("other organization's secret")).toBeNull()
  expect(mock.unknown).toEqual([])
})

it.each(["body", "history"])(
  "offers retry, not an empty editor or endless loading, when the existing document %s failed to load",
  async (failed) => {
    let first = true
    const mock = mockNativeSession(undefined, {
      [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
      [`GET ${path}/revisions`]: () => {
        if (first && failed === "history") {
          first = false
          throw new TypeError("offline")
        }
        return json({ items: [revision], nextAfter: null })
      },
      [`GET ${path}/content?revision=1`]: () => {
        if (first && failed === "body") {
          first = false
          throw new TypeError("offline")
        }
        return json({ document: null, revision, markdown: "seed" })
      },
    })
    const user = userEvent.setup()
    mount()
    expect(await screen.findByRole("alert")).toBeVisible()
    expect(
      screen.queryByRole("textbox", { name: "Document content" })
    ).toBeNull()
    await user.click(screen.getByRole("button", { name: "Try again" }))
    expect(
      await screen.findByRole("textbox", { name: "Document content" })
    ).toHaveValue("seed")
    expect(mock.unknown).toEqual([])
  }
)

it("preserves edits and their original preconditions when a changed-head poll body finishes afterward", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  let metadataReads = 0
  let release!: (value: Response) => void
  let submitted: unknown
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () =>
      json(
        ++metadataReads === 1 ? resource : { ...resource, contentRevision: "2" }
      ),
    [`GET ${path}/revisions`]: () =>
      json({ items: [revision], nextAfter: null }),
    [`GET ${path}/content?revision=1`]: () =>
      json({ document: null, revision, markdown: "seed" }),
    [`GET ${path}/content?revision=2`]: () =>
      new Promise((resolve) => {
        release = resolve
      }),
    [`GET ${path}/revisions?after=1`]: () =>
      json({ items: [{ ...revision, revision: "2" }], nextAfter: null }),
    [`POST ${path}/revisions`]: (init) => {
      submitted = JSON.parse(init.body as string)
      return json({ code: "conflict" }, 409)
    },
  })
  const user = userEvent.setup()
  mount()
  const editor = await screen.findByRole("textbox", {
    name: "Document content",
  })
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5000)
  })
  expect(release).toBeDefined()
  await user.type(editor, "!")
  await act(async () => {
    release(
      json({
        document: null,
        revision: { ...revision, revision: "2" },
        markdown: "peer",
      })
    )
  })
  expect(editor).toHaveValue("seed!")
  expect(screen.getByRole("button", { name: "Save" })).toBeEnabled()
  await user.click(screen.getByRole("button", { name: "Save" }))
  expect(submitted).toEqual({
    requestId: expect.any(String),
    markdown: "seed!",
    expectedContentRevision: "1",
    expectedAuthRevision: "1",
  })
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Your draft is preserved"
  )
  expect(mock.unknown).toEqual([])
})

it("reuses unchanged body and history pages while polling authority, then merges new revisions", async () => {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
  const history = Array.from({ length: 101 }, (_, i) => ({
    ...revision,
    revision: String(i + 1),
  }))
  let head = "101"
  let metadataReads = 0,
    bodyReads = 0,
    firstPages = 0,
    olderPages = 0,
    newPages = 0
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => {
      metadataReads++
      return json({ ...resource, contentRevision: head })
    },
    [`GET ${path}/content?revision=101`]: () => {
      bodyReads++
      return json({
        revision: { ...revision, revision: "101" },
        document: null,
        markdown: "seed",
      })
    },
    [`GET ${path}/content?revision=102`]: () => {
      bodyReads++
      return json({
        revision: { ...revision, revision: "102", byteLength: "3" },
        document: null,
        markdown: "new",
      })
    },
    [`GET ${path}/revisions`]: () => {
      firstPages++
      return json({ items: history.slice(0, 100), nextAfter: "100" })
    },
    [`GET ${path}/revisions?after=100`]: () => {
      olderPages++
      return json({ items: history.slice(100), nextAfter: null })
    },
    [`GET ${path}/revisions?after=101`]: () => {
      newPages++
      return json({
        items: [{ ...revision, revision: "102" }],
        nextAfter: null,
      })
    },
  })
  const user = userEvent.setup()
  mount()
  await screen.findByRole("textbox", { name: "Document content" })
  await user.click(screen.getByRole("button", { name: "History" }))
  await user.click(screen.getByRole("button", { name: "Load more" }))
  expect(
    await screen.findByRole("button", { name: "Compare revision 101" })
  ).toBeVisible()
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5000)
  })
  expect(
    screen.getByRole("button", { name: "Compare revision 1" })
  ).toBeVisible()
  expect(metadataReads).toBe(2)
  expect([bodyReads, firstPages, olderPages, newPages]).toEqual([1, 1, 1, 0])
  head = "102"
  await act(async () => {
    await vi.advanceTimersByTimeAsync(5000)
  })
  expect(
    screen.getByRole("button", { name: "Compare revision 102" })
  ).toBeVisible()
  expect(
    screen.getByRole("button", { name: "Compare revision 1" })
  ).toBeVisible()
  expect(screen.getByRole("textbox", { name: "Document content" })).toHaveValue(
    "new"
  )
  expect([bodyReads, firstPages, olderPages, newPages]).toEqual([2, 1, 1, 1])
  expect(mock.unknown).toEqual([])
})

it("keeps a dirty draft and recovery controls when foreground Compare has unavailable storage", async () => {
  let reads = 0
  const mock = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET ${path}/content?revision=1`]: () =>
      ++reads === 1
        ? json({ document: null, revision, markdown: "seed" })
        : json({ code: "storage_unavailable" }, 503),
    [`GET ${path}/revisions`]: () =>
      json({ items: [revision], nextAfter: null }),
    [`POST ${path}/revisions`]: () => json({ code: "conflict" }, 409),
  })
  const user = userEvent.setup()
  mount()
  const editor = await screen.findByRole("textbox", {
    name: "Document content",
  })
  await user.type(editor, " private draft")
  await user.click(screen.getByRole("button", { name: "Save" }))
  await user.click(
    await screen.findByRole("button", { name: "Compare with latest" })
  )
  expect(await screen.findByRole("alert")).toBeVisible()
  expect(editor).toHaveValue("seed private draft")
  for (const name of [
    "Save",
    "Copy draft",
    "Reload latest",
    "Compare with latest",
  ])
    expect(screen.getByRole("button", { name })).toBeEnabled()
  expect(screen.queryByRole("dialog")).toBeNull()
  expect(mock.unknown).toEqual([])
})
