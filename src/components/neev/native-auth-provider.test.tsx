import { act, render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeBoundary } from "./native-boundary"
import { useNativeAuth } from "./native-auth-provider"
import { NativeWorkspace } from "./native-workspace"
import {
  json,
  mockNativeSession,
  nativeCapabilities,
  NativeTestRoot,
  orgSession,
  composerText,
  typeInComposer,
} from "./test-helpers"
const router = vi.hoisted(() => ({ replace: vi.fn(), push: vi.fn() }))
vi.mock("next/navigation", () => ({ useRouter: () => router }))
afterEach(() => {
  vi.unstubAllGlobals()
  vi.clearAllMocks()
})
function StatusProbe() {
  const auth = useNativeAuth()
  return (
    <>
      <span data-testid="auth-status">{auth.status}</span>
      <span data-testid="auth-providers">
        {auth.capabilities
          ? Object.entries(auth.capabilities.providers)
              .filter(([, on]) => on)
              .map(([name]) => name)
              .join(",")
          : "none"}
      </span>
      <span data-testid="auth-session">{auth.session ? "held" : "none"}</span>
    </>
  )
}
function Controls() {
  const auth = useNativeAuth()
  return (
    <>
      <button onClick={() => void auth.refresh()}>Refresh fixture</button>
      <NativeBoundary>
        <p>{auth.session?.organization?.name}</p>
        <button onClick={() => void auth.logout()}>Logout fixture</button>
      </NativeBoundary>
    </>
  )
}
it("hides the old organization while refreshing and rejects an aborted old response", async () => {
  let request = 0,
    oldReply: ((response: Response) => void) | undefined,
    oldSignal: AbortSignal | null | undefined
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/session": (init) => {
      request += 1
      if (request === 2) {
        oldSignal = init.signal
        return new Promise((resolve) => {
          oldReply = resolve
        })
      }
      return json(
        request === 1
          ? orgSession
          : {
              ...orgSession,
              organization: { id: 4, name: "Second company", avatarUrl: null },
            }
      )
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <Controls />
    </NativeTestRoot>
  )
  await screen.findByText("Neev demo")
  await user.click(screen.getByText("Refresh fixture"))
  expect(screen.queryByText("Neev demo")).toBeNull()
  await user.click(screen.getByText("Refresh fixture"))
  await screen.findByText("Second company")
  expect(oldSignal?.aborted).toBe(true)
  await act(async () => oldReply?.(json(orgSession)))
  expect(screen.queryByText("Neev demo")).toBeNull()
  expect(mock.unknown).toEqual([])
})
it("clears scoped workspace immediately even if logout fails, then permits retry", async () => {
  let calls = 0
  const mock = mockNativeSession(orgSession, {
    "POST /api/neev/logout": () =>
      ++calls === 1
        ? json({ code: "unavailable" }, 503)
        : new Response(null, { status: 204 }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <Controls />
    </NativeTestRoot>
  )
  await user.click(await screen.findByText("Logout fixture"))
  expect(await screen.findByRole("alert")).toHaveTextContent(
    /sign-out could not/i
  )
  expect(screen.queryByText("Neev demo")).toBeNull()
  await act(async () => window.dispatchEvent(new Event("focus")))
  expect(screen.queryByText("Neev demo")).toBeNull()
  expect(screen.getByRole("button", { name: "Retry sign-out" })).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Retry sign-out" }))
  await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
  expect(mock.unknown).toEqual([])
})
it("invalidates another tab's scope and redirects a lost session", async () => {
  let valid = true,
    receive: ((event: MessageEvent) => void) | undefined
  class Channel {
    addEventListener(_name: string, callback: (event: MessageEvent) => void) {
      receive = callback
    }
    removeEventListener() {}
    close() {}
    postMessage() {}
  }
  vi.stubGlobal("BroadcastChannel", Channel)
  mockNativeSession(orgSession, {
    "GET /api/neev/session": () =>
      json(valid ? orgSession : { code: "unauthorized" }, valid ? 200 : 401),
  })
  render(
    <NativeTestRoot>
      <Controls />
    </NativeTestRoot>
  )
  await screen.findByText("Neev demo")
  valid = false
  await act(async () =>
    receive?.({ data: { type: "invalidate" } } as MessageEvent)
  )
  await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
  expect(screen.queryByText("Neev demo")).toBeNull()
})

it("hides the current scope on visibility return and waits for fresh authority", async () => {
  let request = 0,
    nextReply: ((response: Response) => void) | undefined
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/session": () =>
      ++request === 1
        ? json(orgSession)
        : new Promise((resolve) => {
            nextReply = resolve
          }),
  })
  render(
    <NativeTestRoot>
      <Controls />
    </NativeTestRoot>
  )
  await screen.findByText("Neev demo")
  const visibility = vi
    .spyOn(document, "visibilityState", "get")
    .mockReturnValue("visible")
  await act(async () => document.dispatchEvent(new Event("visibilitychange")))
  expect(screen.getByText("Neev demo")).not.toBeVisible()
  expect(screen.getByText("Neev demo").closest("[inert]")).not.toBeNull()
  await act(async () => nextReply?.(json({ code: "unauthorized" }, 401)))
  await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
  visibility.mockRestore()
  expect(mock.unknown).toEqual([])
})

it("does not claim logout when401 is caused by a still-valid rotated session", async () => {
  let bootstraps = 0,
    logouts = 0
  const headers: unknown[] = []
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/session": () =>
      json({
        ...orgSession,
        csrf: ++bootstraps === 1 ? "synthetic-csrf" : "rotated-csrf",
      }),
    "POST /api/neev/logout": (init) => {
      headers.push((init.headers as Record<string, string>)["x-neev-csrf"])
      return ++logouts === 1
        ? json({ code: "unauthorized" }, 401)
        : new Response(null, { status: 204 })
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <Controls />
    </NativeTestRoot>
  )
  await user.click(await screen.findByText("Logout fixture"))
  expect(
    await screen.findByRole("button", { name: "Retry sign-out" })
  ).toBeVisible()
  expect(router.replace).not.toHaveBeenCalled()
  expect(screen.queryByText("Neev demo")).toBeNull()
  await user.click(screen.getByRole("button", { name: "Retry sign-out" }))
  await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
  expect(headers).toEqual(["synthetic-csrf", "rotated-csrf"])
  expect(mock.unknown).toEqual([])
})

it("confirms an already-ended session after logout401 using a fresh bootstrap", async () => {
  let bootstraps = 0
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/session": () =>
      ++bootstraps === 1
        ? json(orgSession)
        : json({ code: "unauthorized" }, 401),
    "POST /api/neev/logout": () => json({ code: "unauthorized" }, 401),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <Controls />
    </NativeTestRoot>
  )
  await user.click(await screen.findByText("Logout fixture"))
  await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
  expect(bootstraps).toBe(2)
  expect(mock.unknown).toEqual([])
})
it("never paints an old catalog response after a tab changes organization", async () => {
  let receive: ((event: MessageEvent) => void) | undefined,
    oldReply: ((response: Response) => void) | undefined,
    currentOrg = 3,
    nodeSignal: AbortSignal | null | undefined
  class Channel {
    addEventListener(_name: string, callback: (event: MessageEvent) => void) {
      receive = callback
    }
    removeEventListener() {}
    close() {}
    postMessage() {}
  }
  vi.stubGlobal("BroadcastChannel", Channel)
  const mock = mockNativeSession(orgSession, {
    // The workspace reads the caller's agent chats alongside the tree.
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/session": () =>
      json({
        ...orgSession,
        organization: {
          id: currentOrg,
          name: currentOrg === 3 ? "Old company" : "New company",
          avatarUrl: null,
        },
      }),
    "GET /api/neev/nodes": (init) =>
      currentOrg === 3
        ? new Promise((resolve) => {
            oldReply = resolve
            nodeSignal = init.signal
          })
        : json({ nodes: [], nextAfter: null }),
  })
  render(
    <NativeTestRoot>
      <NativeWorkspace />
    </NativeTestRoot>
  )
  await screen.findByText("Old company")
  currentOrg = 4
  await act(async () =>
    receive?.({ data: { type: "invalidate" } } as MessageEvent)
  )
  await screen.findByText("New company")
  expect(nodeSignal?.aborted).toBe(true)
  await act(async () =>
    oldReply?.(
      json({
        nodes: [
          {
            id: 7,
            orgId: 3,
            parentId: null,
            kind: "project",
            name: "Private old project",
            revision: "1",
          },
        ],
        nextAfter: null,
      })
    )
  )
  expect(screen.queryByText("Private old project")).toBeNull()
  expect(mock.unknown).toEqual([])
})

/** A workspace with one document open and something typed in it. */
const DRAFT_DOC = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
const DRAFT_NODE = {
  id: 4,
  orgId: 3,
  kind: "project" as const,
  parentId: null,
  name: "Roadmap",
  revision: "1",
}
const DRAFT_RESOURCE = {
  id: DRAFT_DOC,
  orgId: 3,
  nodeId: 4,
  kind: "chat",
  creatorId: 1,
  title: "Launch plan",
  visibility: "private",
  originChatId: null,
  authRevision: "1",
  contentRevision: "0",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
}

function draftRoutes(session: () => Response) {
  return {
    "GET /api/neev/session": session,
    "GET /api/neev/agent/sessions": () => json({ sessions: [] }),
    "GET /api/neev/nodes": () => json({ nodes: [DRAFT_NODE], nextAfter: null }),
    "GET /api/neev/nodes/4/documents": () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/nodes/4/chats": () =>
      json({ items: [DRAFT_RESOURCE], nextAfter: null }),
    [`GET /api/neev/resources/${DRAFT_DOC}`]: () => json(DRAFT_RESOURCE),
    [`GET /api/neev/chats/${DRAFT_DOC}/messages`]: () =>
      json({ items: [], nextAfter: null }),
  }
}

async function openDraft(user: ReturnType<typeof userEvent.setup>) {
  await user.click(
    (await screen.findAllByRole("button", { name: "Roadmap" }))[0]
  )
  const tree = screen.getByRole("navigation", { name: "Projects" })
  await user.click(
    await within(tree).findByRole(
      "button",
      { name: "Launch plan" },
      { timeout: 5000 }
    )
  )
  const composer = await screen.findByRole(
    "textbox",
    { name: "Message" },
    { timeout: 5000 }
  )
  await typeInComposer(composer, "unsaved sentence")
  return composer
}

it(
  "keeps an unsaved draft through a revalidation that could not be answered",
  { timeout: 30000 },
  async () => {
    // Silence is not a refusal. Treating an unreachable server as one unmounted
    // the whole workspace, and a moment of bad wifi took the writing with it.
    let reachable = true
    mockNativeSession(
      undefined,
      draftRoutes(() =>
        reachable ? json(orgSession) : json({ code: "unavailable" }, 503)
      )
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <StatusProbe />
        <NativeBoundary>
          <NativeWorkspace />
        </NativeBoundary>
      </NativeTestRoot>
    )
    const composer = await openDraft(user)
    expect(composerText(composer)).toBe("unsaved sentence")

    reachable = false
    await act(async () => document.dispatchEvent(new Event("visibilitychange")))
    // Held, not taken: the draft is still mounted, and nothing under here can be
    // reached or written until the session is proved again.
    await screen.findByRole("alert")
    // The session is held, not in force — the surface it belongs to is hidden
    // and inert for as long as that lasts (covered where the same `held` path is
    // exercised by a revalidation in flight), and the draft is still in it.
    expect(screen.getByTestId("auth-status")).toHaveTextContent("unverified")
    // The composer's element goes with the surface that is hidden; the editor
    // still holds every word of the draft, and puts it back on the way in.
    expect(composerText(composer)).toBe("unsaved sentence")
    expect(router.replace).not.toHaveBeenCalledWith("/login")

    // A retry that fails again changes nothing: still held, still holding the
    // draft. Retrying is not a second chance to lose the work.
    await user.click(screen.getByRole("button", { name: "Try again" }))
    await waitFor(() =>
      expect(screen.getByTestId("auth-status")).toHaveTextContent("unverified")
    )
    expect(composerText(composer)).toBe("unsaved sentence")

    reachable = true
    await user.click(screen.getByRole("button", { name: "Try again" }))
    // The same element, still holding the same words, reachable again.
    await waitFor(() =>
      expect(screen.getByTestId("auth-status")).toHaveTextContent(
        "authenticated"
      )
    )
    // The same element, still holding the same words.
    expect(composerText(composer)).toBe("unsaved sentence")
  }
)

it(
  "clears an unsaved draft when the revalidation is an actual refusal",
  { timeout: 30000 },
  async () => {
    // The other half of the same rule: an answer that says no is obeyed, and
    // everything scoped to that session goes, drafts included.
    let allowed = true
    mockNativeSession(
      undefined,
      draftRoutes(() =>
        allowed ? json(orgSession) : json({ code: "unauthorized" }, 401)
      )
    )
    const user = userEvent.setup()
    render(
      <NativeTestRoot>
        <NativeBoundary>
          <NativeWorkspace />
        </NativeBoundary>
      </NativeTestRoot>
    )
    await openDraft(user)

    allowed = false
    await act(async () => document.dispatchEvent(new Event("visibilitychange")))
    await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
    expect(screen.queryByText("unsaved sentence")).toBeNull()
    expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull()
  }
)

/** Bootstrap with the public answer and the private one under test. */
function bootstrap(routes: Record<string, () => Response | Promise<Response>>) {
  mockNativeSession(undefined, routes)
  return render(
    <NativeTestRoot>
      <StatusProbe />
    </NativeTestRoot>
  )
}

it.each([
  ["the refusal arrives first", true],
  ["the capabilities arrive first", false],
])(
  "publishes the public capabilities to a signed-out visitor when %s",
  async (_label, refusalFirst) => {
    // The two answers race, and the order they land in must not decide whether
    // the sign-in page knows how to sign anyone in.
    let releaseCaps!: (value: Response) => void
    const held = new Promise<Response>((resolve) => {
      releaseCaps = resolve
    })
    const caps = json({
      ...nativeCapabilities,
      providers: { google: true, email: true, github: false },
    })
    bootstrap({
      "GET /api/workos/capabilities": () => (refusalFirst ? held : caps),
      "GET /api/neev/session": () => json({ code: "unauthorized" }, 401),
    })
    if (refusalFirst) {
      // Let the 401 land alone first, then answer the public question.
      await waitFor(() =>
        expect(screen.getByTestId("auth-status")).toHaveTextContent("loading")
      )
      await act(async () => releaseCaps(caps))
    }
    await waitFor(() =>
      expect(screen.getByTestId("auth-status")).toHaveTextContent(
        "unauthenticated"
      )
    )
    expect(screen.getByTestId("auth-providers")).toHaveTextContent(
      "google,email"
    )
    // Signed out is signed out: nothing put a private session back.
    expect(screen.getByTestId("auth-session")).toHaveTextContent("none")
  }
)

it.each([
  ["they cannot be read", () => json({ code: "unavailable" }, 503)],
  [
    "they come from something that is not this server",
    () => json({ ...nativeCapabilities, identityMode: "workos" }),
  ],
  ["they are malformed", () => json({ nonsense: true })],
])(
  "publishes nothing and fails closed when %s",
  async (_label, capabilities) => {
    // A server whose capabilities cannot be trusted cannot be signed in to, and
    // the page says so rather than guessing at a method.
    bootstrap({
      "GET /api/workos/capabilities": capabilities,
      "GET /api/neev/session": () => json(orgSession),
    })
    await waitFor(() =>
      expect(screen.getByTestId("auth-status")).toHaveTextContent("unavailable")
    )
    expect(screen.getByTestId("auth-providers")).toHaveTextContent("none")
    // Not even a session that answered perfectly well beside it.
    expect(screen.getByTestId("auth-session")).toHaveTextContent("none")
  }
)
