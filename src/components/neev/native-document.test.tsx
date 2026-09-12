import { act, render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeDocument } from "./native-document"
import { NativeBoundary } from "./native-boundary"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  orgSession,
} from "./test-helpers"
import type { NativeResource } from "@/lib/neev-content-client"
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
function mount() {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeDocument
          resource={resource}
          onBack={() => {}}
          onChanged={() => {}}
          onDirtyChange={() => {}}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

it.each(["content", "audience"])(
  "pins dirty %s revisions through polling and retains a 409 draft for deliberate comparison",
  async (change) => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
    let latest = resource
    let body: unknown
    const mock = mockNativeSession(undefined, {
      [`GET /api/neev/resources/${resource.id}`]: () => json(latest),
      [`GET ${path}/content?revision=1`]: () =>
        json({ revision, markdown: "seed" }),
      [`GET ${path}/content?revision=2`]: () =>
        json({
          revision: { ...revision, revision: "2", byteLength: "6" },
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
      json({ revision, markdown: "seed" }),
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
      json({ revision, markdown: "seed" }),
    [`GET ${path}/content?revision=2`]: () =>
      json({
        revision: { ...revision, revision: "2", byteLength: "6" },
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
      json({ revision, markdown: "seed" }),
    [`GET ${path}/content?revision=2`]: () =>
      json({ revision: old, markdown: oldText }),
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
      json({ revision, markdown: "seed" }),
    [`GET ${path}/content?revision=2`]: () =>
      json({ revision: old, markdown: oldText }),
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

it.each([403, 404])(
  "erases loaded document, draft, and history on %s and stops polling",
  async (status) => {
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
        json({ revision, markdown: "seed" }),
      [`GET ${path}/revisions`]: () =>
        json({ items: [revision], nextAfter: null }),
    })
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime })
    mount()
    await user.type(
      await screen.findByRole("textbox", { name: "Document content" }),
      " secret draft"
    )
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
    expect(screen.queryByText("Sagar")).toBeNull()
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
      json({ revision, markdown: "safe" }),
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
        return json({ revision, markdown: "seed" })
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
      json({ revision, markdown: "seed" }),
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
      json({ revision: { ...revision, revision: "2" }, markdown: "peer" })
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
        markdown: "seed",
      })
    },
    [`GET ${path}/content?revision=102`]: () => {
      bodyReads++
      return json({
        revision: { ...revision, revision: "102", byteLength: "3" },
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
        ? json({ revision, markdown: "seed" })
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
