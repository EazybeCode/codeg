import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, beforeEach, expect, it, vi } from "vitest"
import { NativeChat } from "./native-chat"
import { NativeBoundary } from "./native-boundary"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  typeInComposer,
} from "./test-helpers"
import type { NativeResource } from "@/lib/neev-content-client"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

/**
 * Naming a coworker, from the composer to the transcript.
 *
 * The picker's own keyboard behaviour is proved in
 * `native-inline-composer-mentions.test.tsx`; what is proved here is the part
 * only a real conversation has — who may be offered at all, what actually goes
 * up the wire, and what a reader sees when it comes back.
 */
const shared: NativeResource = {
  id: "11111111-1111-4111-8111-111111111111",
  orgId: 3,
  nodeId: 4,
  kind: "chat",
  creatorId: 1,
  title: "Design chat",
  visibility: "organization",
  originChatId: null,
  authRevision: "1",
  contentRevision: "0",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
}
const secret: NativeResource = { ...shared, visibility: "private" }
const messagesPath = `/api/neev/chats/${shared.id}/messages`
const membersPath = "/api/neev/organization/members?limit=100&offset=0"

const directory = {
  members: [
    {
      id: 11,
      role: "member",
      user: { id: 1, displayName: "Sagar", avatarUrl: null },
    },
    {
      id: 12,
      role: "member",
      user: { id: 2, displayName: "Ana Duarte", avatarUrl: null },
    },
    {
      id: 13,
      role: "admin",
      user: { id: 3, displayName: "Andrés Gil", avatarUrl: null },
    },
  ],
}

beforeEach(() => localStorage.clear())
afterEach(() => vi.unstubAllGlobals())

function mount(resource: NativeResource) {
  render(
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

async function press(box: HTMLElement, key: string) {
  await act(async () => {
    fireEvent.keyDown(box, { key })
  })
}

it("sends who was named, and never the reader themself", async () => {
  const posted: Record<string, unknown>[] = []
  let listed = 0
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${shared.id}`]: () => json(shared),
    [`GET ${messagesPath}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${membersPath}`]: () => {
      listed += 1
      return json(directory)
    },
    [`POST ${messagesPath}`]: (init) => {
      posted.push(JSON.parse(init.body as string))
      return json(
        {
          id: "22222222-2222-4222-8222-222222222222",
          sequence: "1",
          authorId: 1,
          authorName: "Sagar",
          body: "ping @Ana Duarte",
          createdAt: 1_700_000_000,
          authorKind: "human",
          agentLabel: null,
          mentions: [{ userId: 2, start: 5, length: 11 }],
        },
        201
      )
    },
  })
  mount(shared)
  const box = await ready()
  await typeInComposer(box, "ping @")
  // The directory is asked for when the picker opens, not on mount.
  await waitFor(() => expect(listed).toBe(1))
  await typeInComposer(box, "An", "end")

  // Sagar is this reader; naming yourself notifies nobody, so nobody is
  // offered the chance to.
  await waitFor(() =>
    expect(screen.getAllByRole("option").map((o) => o.textContent)).toEqual([
      "Ana Duarte",
      "Andrés Gil",
    ])
  )
  await press(box, "Enter")

  await userEvent.setup().click(screen.getByRole("button", { name: "Send" }))
  await waitFor(() => expect(posted).toHaveLength(1))
  // The prompt keeps a plain readable name — this is what a provider and every
  // older client sees — and the identity travels beside it.
  expect(posted[0].body).toBe("ping @Ana Duarte")
  expect(posted[0].mentions).toEqual([{ userId: 2, start: 5, length: 11 }])

  // And what comes back is drawn from the stored span, not from the text.
  const chip = await screen.findByText("@Ana Duarte")
  expect(chip).toHaveAttribute("data-mention-user", "2")
})

it("offers nobody in a private conversation, and asks for no directory", async () => {
  const requested: string[] = []
  const { fetcher } = mockNativeSession(undefined, {
    [`GET /api/neev/resources/${secret.id}`]: () => json(secret),
    [`GET ${messagesPath}`]: () => json({ items: [], nextAfter: null }),
    [`GET ${membersPath}`]: () => {
      requested.push(membersPath)
      return json(directory)
    },
  })
  mount(secret)
  const box = await ready()
  await typeInComposer(box, "ping @An")

  // A private chat has an audience of one. Offering a list of people who
  // cannot read it would be an offer to share it.
  expect(screen.queryAllByRole("option")).toHaveLength(0)
  expect(box.getAttribute("aria-expanded")).toBeNull()
  expect(requested).toEqual([])
  expect(
    fetcher.mock.calls.filter((call) => String(call[0]).includes("/members"))
  ).toHaveLength(0)
})

it("draws a chip only where a stored span says one belongs", async () => {
  mockNativeSession(undefined, {
    [`GET /api/neev/resources/${shared.id}`]: () => json(shared),
    [`GET ${messagesPath}`]: () =>
      json({
        items: [
          {
            id: "22222222-2222-4222-8222-222222222222",
            sequence: "1",
            authorId: 1,
            authorName: "Sagar",
            body: "ping @Ana Duarte about `@Ana Duarte`",
            createdAt: 1_700_000_000,
            authorKind: "human",
            agentLabel: null,
            mentions: [{ userId: 2, start: 5, length: 11 }],
          },
          {
            id: "33333333-3333-4333-8333-333333333333",
            sequence: "2",
            authorId: 1,
            authorName: "Sagar",
            body: "I will ask @Ana Duarte to review it.",
            createdAt: 1_700_000_100,
            authorKind: "agent",
            agentLabel: "claude",
          },
        ],
        nextAfter: null,
      }),
  })
  mount(shared)
  await ready()

  await screen.findByText(/I will ask @Ana Duarte to review it\./)
  // One chip in the thread: the one the sender minted from the picker. The
  // second `@Ana Duarte` in their own sentence is the characters they typed,
  // and the agent's is the characters it wrote — an agent cannot ping anyone.
  const chips = document.querySelectorAll("[data-mention-user]")
  expect(chips).toHaveLength(1)
  expect(chips[0].getAttribute("data-mention-user")).toBe("2")
  expect(chips[0].textContent).toBe("@Ana Duarte")
})
