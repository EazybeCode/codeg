import { render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativePeopleDirectory } from "./native-people"
import { NativeBoundary } from "./native-boundary"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  orgSession,
} from "./test-helpers"
import {
  ANA,
  BEN,
  DESIGN_TEAM,
  serveDirectory,
  type TestMember,
} from "./sharing-test-helpers"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

afterEach(() => vi.unstubAllGlobals())

const member = { ...orgSession, role: "member" as const }

function mount() {
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativePeopleDirectory />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

it("lets any member search the directory by name and load more, without contact details", async () => {
  const many: TestMember[] = Array.from({ length: 60 }, (_, i) => ({
    userId: 100 + i,
    displayName: `Person ${String(i).padStart(2, "0")}`,
    avatarUrl: null,
    role: "member",
  }))
  mockNativeSession(member, {})
  const queries = serveDirectory([ANA, BEN, ...many])
  const user = userEvent.setup()
  mount()
  const list = await screen.findByRole("list", { name: "People" })
  await waitFor(() =>
    expect(within(list).getAllByRole("listitem")).toHaveLength(50)
  )
  await user.click(screen.getByRole("button", { name: "Load more people" }))
  await waitFor(() =>
    expect(within(list).getAllByRole("listitem")).toHaveLength(62)
  )
  expect(screen.queryByRole("button", { name: "Load more people" })).toBeNull()
  await user.type(
    screen.getByRole("searchbox", { name: "Search people" }),
    "ana"
  )
  await waitFor(() =>
    expect(within(list).getAllByRole("listitem")).toHaveLength(1)
  )
  expect(within(list).getByText("Ana Lima")).toBeVisible()
  expect(within(list).getByText("Member")).toBeVisible()
  expect(queries[queries.length - 1]).toMatchObject({ q: "ana", after: null })
  expect(screen.queryByText(/@/)).toBeNull()
})

it("shows teams to a member read-only, with no management controls", async () => {
  mockNativeSession(member, {
    "GET /api/neev/teams": () =>
      json({ teams: [{ ...DESIGN_TEAM, canManage: false }] }),
    [`GET /api/neev/teams/${DESIGN_TEAM.id}/members`]: () =>
      json({
        teamId: DESIGN_TEAM.id,
        revision: "1",
        members: [{ userId: 2, displayName: "Ana Lima", avatarUrl: null }],
      }),
  })
  serveDirectory([ANA])
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("tab", { name: "Teams" }))
  expect(screen.queryByRole("textbox", { name: "New team name" })).toBeNull()
  await user.click(await screen.findByRole("button", { name: /Design/ }))
  expect(await screen.findByText("Ana Lima")).toBeVisible()
  expect(screen.queryByRole("button", { name: "Delete team" })).toBeNull()
  expect(screen.queryByRole("searchbox", { name: "Add people" })).toBeNull()
})

it("lets an admin create (same request id on retry), rename, add, remove and delete", async () => {
  const creates: Record<string, unknown>[] = []
  const writes: {
    method: string
    path: string
    body: Record<string, unknown>
  }[] = []
  let members = [{ userId: 2, displayName: "Ana Lima", avatarUrl: null }]
  let revision = 1
  const detail = () =>
    json({ teamId: DESIGN_TEAM.id, revision: String(revision), members })
  const record = (path: string) => (init: RequestInit) => {
    writes.push({
      method: init.method!,
      path,
      body: JSON.parse(init.body as string),
    })
    revision += 1
    return null
  }
  mockNativeSession(orgSession, {
    "GET /api/neev/teams": () => json({ teams: [] }),
    "POST /api/neev/teams": (init) => {
      creates.push(JSON.parse(init.body as string))
      return creates.length === 1
        ? json({ code: "team_name_taken" }, 409)
        : json(DESIGN_TEAM, 201)
    },
    [`GET /api/neev/teams/${DESIGN_TEAM.id}/members`]: detail,
    [`PATCH /api/neev/teams/${DESIGN_TEAM.id}`]: (init) => {
      record("rename")(init)
      return json({ ...DESIGN_TEAM, name: "Brand", revision: String(revision) })
    },
    [`POST /api/neev/teams/${DESIGN_TEAM.id}/members`]: (init) => {
      record("add")(init)
      members = [
        ...members,
        { userId: 3, displayName: "Ben Okafor", avatarUrl: null },
      ]
      return detail()
    },
    [`DELETE /api/neev/teams/${DESIGN_TEAM.id}/members/2`]: (init) => {
      record("remove")(init)
      members = members.filter((m) => m.userId !== 2)
      return detail()
    },
    [`DELETE /api/neev/teams/${DESIGN_TEAM.id}`]: (init) => {
      record("delete")(init)
      return json({ ok: true })
    },
  })
  serveDirectory([ANA, BEN])
  const user = userEvent.setup()
  mount()
  await user.click(await screen.findByRole("tab", { name: "Teams" }))
  const name = await screen.findByRole("textbox", { name: "New team name" })
  await user.type(name, "Design")
  await user.click(screen.getByRole("button", { name: "Create team" }))
  expect(
    await screen.findByText("A team with that name already exists.")
  ).toBeVisible()
  await user.click(screen.getByRole("button", { name: "Create team" }))
  await waitFor(() => expect(creates).toHaveLength(2))
  expect(creates[1].requestId).toBe(creates[0].requestId)
  // Opened on creation.
  const nameField = await screen.findByRole("textbox", { name: "Team name" })
  await user.clear(nameField)
  await user.type(nameField, "Brand")
  await user.click(screen.getByRole("button", { name: "Rename" }))
  await waitFor(() =>
    expect(writes[0]).toMatchObject({
      path: "rename",
      body: { name: "Brand", expectedRevision: "1" },
    })
  )
  await user.type(screen.getByRole("searchbox", { name: "Add people" }), "ben")
  await user.click(
    await screen.findByRole("button", { name: "Add Ben Okafor" })
  )
  await waitFor(() =>
    expect(writes[1]).toMatchObject({ path: "add", body: { userId: 3 } })
  )
  await user.click(
    await screen.findByRole("button", { name: "Remove Ana Lima from the team" })
  )
  await waitFor(() => expect(writes[2]).toMatchObject({ path: "remove" }))
  await user.click(screen.getByRole("button", { name: "Delete team" }))
  const confirm = await screen.findByRole("dialog")
  expect(confirm).toHaveTextContent("stops being shared")
  await user.click(within(confirm).getByRole("button", { name: "Delete team" }))
  await waitFor(() =>
    expect(writes[3]).toMatchObject({ method: "DELETE", path: "delete" })
  )
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: /Brand|Design/ })).toBeNull()
  )
})
