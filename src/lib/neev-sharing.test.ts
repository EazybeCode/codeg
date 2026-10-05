import { afterEach, expect, it, vi } from "vitest"
import { NativeClientError } from "./neev-client"
import {
  addTeamMember,
  createTeam,
  deleteTeam,
  listSharedSessions,
  listTeams,
  loadNodeAccess,
  loadResourceAccess,
  loadSessionAccess,
  loadTeamMembers,
  removeNodeGrant,
  removeResourceGrant,
  removeSessionGrant,
  removeTeamMember,
  renameTeam,
  searchDirectory,
  setNodeGrant,
  setResourceGrant,
  setSessionGrant,
  listSharedResources,
  setNodeRestriction,
  sharingRefusal,
} from "./neev-sharing"

const A = "11111111-1111-4111-8111-111111111111"
const T = "22222222-2222-4222-8222-222222222222"
const S = "33333333-3333-4333-8333-333333333333"
const R = "44444444-4444-4444-8444-444444444444"

const user = { kind: "user", userId: 5, displayName: "Ana", avatarUrl: null }
const team = { kind: "team", teamId: T, name: "Design", memberCount: 3 }
const teamRow = {
  id: T,
  name: "Design",
  memberCount: 3,
  revision: "2",
  canManage: true,
}
const members = {
  teamId: T,
  revision: "3",
  members: [{ userId: 5, displayName: "Ana", avatarUrl: null }],
}
const resourceAccess = {
  resourceId: A,
  visibility: "shared",
  authRevision: "4",
  level: "owner",
  canManage: true,
  boundTo: null,
  direct: [{ principal: user, level: "view" }],
  inherited: [{ nodeId: 7, nodeName: "Plans", principal: team, level: "edit" }],
}
const nodeAccess = {
  nodeId: 7,
  revision: "9",
  canManage: false,
  canRestrict: false,
  restricted: false,
  level: "edit",
  direct: [{ principal: team, level: "edit" }],
  inherited: [],
}
const sessionAccess = {
  sessionId: S,
  revision: "2",
  level: "owner",
  canManage: true,
  direct: [{ principal: user, level: "view" }],
}
const session = {
  id: S,
  orgId: 3,
  ownerId: 1,
  title: "Refactor the parser",
  provider: "claude",
  model: "default",
  boundChatId: null,
  archived: false,
  revision: "1",
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
}

function respond(status: number, body: unknown) {
  const fetcher = vi.fn().mockResolvedValue(
    new Response(JSON.stringify(body), {
      status,
      headers: { "Content-Type": "application/json" },
    })
  )
  vi.stubGlobal("fetch", fetcher)
  return fetcher
}

afterEach(() => vi.unstubAllGlobals())

it("searches the directory with a bounded query and parses the page", async () => {
  const page = {
    members: [
      { userId: 5, displayName: "Ana", avatarUrl: null, role: "member" },
    ],
    nextAfter: "abc",
  }
  const fetcher = respond(200, page)
  await expect(
    searchDirectory({ q: "an", limit: 20, after: "xyz" })
  ).resolves.toEqual(page)
  expect(fetcher.mock.calls[0][0]).toBe(
    "/api/neev/organization/directory?q=an&limit=20&after=xyz"
  )
  respond(200, { members: [], nextAfter: null })
  await searchDirectory({})
  expect(vi.mocked(fetch).mock.calls[0][0]).toBe(
    "/api/neev/organization/directory"
  )
})

it("refuses a directory row that carries more than the contract (no email)", async () => {
  respond(200, {
    members: [
      {
        userId: 5,
        displayName: "Ana",
        avatarUrl: null,
        role: "member",
        email: "a@x.test",
      },
    ],
    nextAfter: null,
  })
  await expect(searchDirectory({})).rejects.toMatchObject({
    kind: "invalid_response",
  })
})

it("manages teams with exact bodies and methods", async () => {
  let fetcher = respond(200, { teams: [teamRow] })
  await expect(listTeams()).resolves.toEqual([teamRow])
  expect(fetcher.mock.calls[0][0]).toBe("/api/neev/teams")

  fetcher = respond(201, teamRow)
  await createTeam("Design", R, "csrf")
  expect(fetcher.mock.calls[0][1].method).toBe("POST")
  expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
    name: "Design",
    requestId: R,
  })

  fetcher = respond(200, teamRow)
  await renameTeam(T, "Design", "2", "csrf")
  expect(fetcher.mock.calls[0][0]).toBe(`/api/neev/teams/${T}`)
  expect(fetcher.mock.calls[0][1].method).toBe("PATCH")
  expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
    name: "Design",
    expectedRevision: "2",
  })

  fetcher = respond(200, { ok: true })
  await expect(deleteTeam(T, "2", "csrf")).resolves.toBeUndefined()
  expect(fetcher.mock.calls[0][1].method).toBe("DELETE")
  expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
    expectedRevision: "2",
  })

  fetcher = respond(200, members)
  await expect(loadTeamMembers(T)).resolves.toEqual(members)
  expect(fetcher.mock.calls[0][0]).toBe(`/api/neev/teams/${T}/members`)

  fetcher = respond(200, members)
  await addTeamMember(T, 5, "2", "csrf")
  expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
    userId: 5,
    expectedRevision: "2",
  })

  fetcher = respond(200, members)
  await removeTeamMember(T, 5, "3", "csrf")
  expect(fetcher.mock.calls[0][0]).toBe(`/api/neev/teams/${T}/members/5`)
  expect(fetcher.mock.calls[0][1].method).toBe("DELETE")
})

it("names the sharing refusals and nothing else", async () => {
  respond(409, { code: "team_name_taken" })
  const taken = await createTeam("Design", R, "csrf").catch((error) => error)
  expect(sharingRefusal(taken)).toBe("team_name_taken")

  respond(409, { code: "resource_not_shared" })
  const unshared = await setResourceGrant(
    A,
    {
      expectedAuthRevision: "4",
      principal: { kind: "user", userId: 5 },
      level: "view",
    },
    "csrf"
  ).catch((error) => error)
  expect(sharingRefusal(unshared)).toBe("resource_not_shared")

  respond(409, { code: "conflict" })
  const stale = await renameTeam(T, "X", "2", "csrf").catch((error) => error)
  expect(sharingRefusal(stale)).toBeNull()
  expect(sharingRefusal(new Error("x"))).toBeNull()
  expect(sharingRefusal(new NativeClientError("forbidden"))).toBeNull()
})

it("reads and changes resource grants", async () => {
  let fetcher = respond(200, resourceAccess)
  await expect(loadResourceAccess(A)).resolves.toEqual(resourceAccess)
  expect(fetcher.mock.calls[0][0]).toBe(`/api/neev/resources/${A}/access`)

  fetcher = respond(200, resourceAccess)
  await setResourceGrant(
    A,
    {
      expectedAuthRevision: "4",
      principal: { kind: "team", teamId: T },
      level: "edit",
    },
    "csrf"
  )
  expect(fetcher.mock.calls[0][0]).toBe(`/api/neev/resources/${A}/grants`)
  expect(fetcher.mock.calls[0][1].method).toBe("PUT")
  expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
    expectedAuthRevision: "4",
    principal: { kind: "team", teamId: T },
    level: "edit",
  })

  fetcher = respond(200, resourceAccess)
  await removeResourceGrant(
    A,
    { expectedAuthRevision: "5", principal: { kind: "user", userId: 5 } },
    "csrf"
  )
  expect(fetcher.mock.calls[0][1].method).toBe("DELETE")
  expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
    expectedAuthRevision: "5",
    principal: { kind: "user", userId: 5 },
  })
})

it("refuses an incoherent resource access answer", async () => {
  for (const bad of [
    { ...resourceAccess, resourceId: S },
    { ...resourceAccess, level: "viewer" },
    // Only the owner manages.
    { ...resourceAccess, level: "edit" },
    // Direct grants only exist on a shared item.
    { ...resourceAccess, visibility: "private" },
    // A bound document is managed through its chat.
    { ...resourceAccess, boundTo: S },
    { ...resourceAccess, direct: [{ principal: user, level: "owner" }] },
    {
      ...resourceAccess,
      direct: [{ principal: { ...user, email: "x" }, level: "view" }],
    },
    { ...resourceAccess, extra: true },
  ]) {
    respond(200, bad)
    await expect(loadResourceAccess(A)).rejects.toMatchObject({
      kind: "invalid_response",
    })
  }
})

it("reads and changes node grants", async () => {
  let fetcher = respond(200, nodeAccess)
  await expect(loadNodeAccess(7)).resolves.toEqual(nodeAccess)
  expect(fetcher.mock.calls[0][0]).toBe("/api/neev/nodes/7/access")
  fetcher = respond(200, nodeAccess)
  await setNodeGrant(
    7,
    {
      expectedRevision: "9",
      principal: { kind: "user", userId: 5 },
      level: "view",
    },
    "csrf"
  )
  expect(fetcher.mock.calls[0][0]).toBe("/api/neev/nodes/7/grants")
  expect(fetcher.mock.calls[0][1].method).toBe("PUT")
  fetcher = respond(200, nodeAccess)
  await removeNodeGrant(
    7,
    { expectedRevision: "10", principal: { kind: "team", teamId: T } },
    "csrf"
  )
  expect(fetcher.mock.calls[0][1].method).toBe("DELETE")
  respond(200, { ...nodeAccess, nodeId: 8 })
  await expect(loadNodeAccess(7)).rejects.toMatchObject({
    kind: "invalid_response",
  })
})

it("shares an Agent Chat for viewing only", async () => {
  let fetcher = respond(200, sessionAccess)
  await expect(loadSessionAccess(S)).resolves.toEqual(sessionAccess)
  expect(fetcher.mock.calls[0][0]).toBe(`/api/neev/agent/sessions/${S}/access`)
  fetcher = respond(200, sessionAccess)
  await setSessionGrant(
    S,
    {
      expectedRevision: "2",
      principal: { kind: "user", userId: 5 },
      level: "view",
    },
    "csrf"
  )
  expect(fetcher.mock.calls[0][1].method).toBe("PUT")
  expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
    expectedRevision: "2",
    principal: { kind: "user", userId: 5 },
    level: "view",
  })
  fetcher = respond(200, sessionAccess)
  await removeSessionGrant(
    S,
    { expectedRevision: "3", principal: { kind: "user", userId: 5 } },
    "csrf"
  )
  expect(fetcher.mock.calls[0][1].method).toBe("DELETE")

  // "edit" never leaves the client, and never parses.
  fetcher = respond(200, sessionAccess)
  await expect(
    setSessionGrant(
      S,
      {
        expectedRevision: "2",
        principal: { kind: "user", userId: 5 },
        level: "edit" as "view",
      },
      "csrf"
    )
  ).rejects.toMatchObject({ kind: "invalid_request" })
  expect(fetcher).not.toHaveBeenCalled()
  for (const bad of [
    { ...sessionAccess, direct: [{ principal: user, level: "edit" }] },
    { ...sessionAccess, level: "view" },
    { ...sessionAccess, level: "edit", canManage: false },
  ]) {
    respond(200, bad)
    await expect(loadSessionAccess(S)).rejects.toMatchObject({
      kind: "invalid_response",
    })
  }
})

it("lists Agent Chats shared with the caller, with their owners", async () => {
  const shared = {
    sessions: [
      {
        session,
        owner: { userId: 1, displayName: "Owner", avatarUrl: null },
      },
    ],
  }
  const fetcher = respond(200, shared)
  const result = await listSharedSessions()
  expect(fetcher.mock.calls[0][0]).toBe("/api/neev/agent/shared-sessions")
  expect(result).toHaveLength(1)
  expect(result[0].session.id).toBe(S)
  expect(result[0].owner).toEqual({
    userId: 1,
    displayName: "Owner",
    avatarUrl: null,
  })
  // The named owner must be the session's owner.
  respond(200, {
    sessions: [
      { session, owner: { userId: 2, displayName: "X", avatarUrl: null } },
    ],
  })
  await expect(listSharedSessions()).rejects.toMatchObject({
    kind: "invalid_response",
  })
})

it("refuses malformed arguments before making any request", async () => {
  const fetcher = respond(200, {})
  for (const call of [
    () => loadResourceAccess("../x"),
    () => loadNodeAccess(0),
    () => loadSessionAccess("nope"),
    () => loadTeamMembers("nope"),
    () => createTeam("Design", "not-a-uuid", "csrf"),
    () => renameTeam(T, "Design", "0", "csrf"),
    () => addTeamMember(T, -1, "2", "csrf"),
    () => searchDirectory({ limit: 101 }),
    () => searchDirectory({ q: "x".repeat(101) }),
    () =>
      setResourceGrant(
        A,
        {
          expectedAuthRevision: "4",
          principal: { kind: "team", teamId: "x" },
          level: "view",
        },
        "csrf"
      ),
    () =>
      setNodeGrant(
        7,
        {
          expectedRevision: "9",
          principal: { kind: "user", userId: 5 },
          level: "owner" as "view",
        },
        "csrf"
      ),
    () =>
      setResourceGrant(
        A,
        {
          expectedAuthRevision: "4",
          principal: { kind: "user", userId: 5 },
          level: "view",
        },
        ""
      ),
  ])
    await expect(call()).rejects.toMatchObject({ kind: "invalid_request" })
  expect(fetcher).not.toHaveBeenCalled()
})

it("restricts a folder, and refuses an answer that did not", async () => {
  const restricted = {
    ...nodeAccess,
    restricted: true,
    level: "owner",
    canManage: true,
    canRestrict: true,
  }
  const fetcher = respond(200, restricted)
  await expect(
    setNodeRestriction(7, { expectedRevision: "9", restricted: true }, "csrf")
  ).resolves.toEqual(restricted)
  expect(fetcher.mock.calls[0][0]).toBe("/api/neev/nodes/7/restriction")
  expect(fetcher.mock.calls[0][1].method).toBe("PUT")
  expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
    expectedRevision: "9",
    restricted: true,
  })
  respond(200, nodeAccess)
  await expect(
    setNodeRestriction(7, { expectedRevision: "9", restricted: true }, "csrf")
  ).rejects.toMatchObject({ kind: "invalid_response" })
  for (const bad of [
    { ...nodeAccess, level: "admin" },
    { ...nodeAccess, canManage: true, level: "view" },
    { ...nodeAccess, canRestrict: true, level: "view" },
    { ...nodeAccess, canRestrict: undefined },
    { ...nodeAccess, restricted: "yes" },
  ]) {
    respond(200, bad)
    await expect(loadNodeAccess(7)).rejects.toMatchObject({
      kind: "invalid_response",
    })
  }
})

it("lists resources in folders hidden from the caller", async () => {
  const resource = {
    id: A,
    orgId: 3,
    nodeId: 7,
    kind: "chat",
    creatorId: 1,
    title: "Plan",
    visibility: "shared",
    originChatId: null,
    authRevision: "2",
    contentRevision: "0",
    archived: false,
    createdAt: 1_700_000_000,
    updatedAt: 1_700_000_000,
  }
  const fetcher = respond(200, { resources: [resource] })
  const result = await listSharedResources()
  expect(fetcher.mock.calls[0][0]).toBe("/api/neev/shared/resources")
  expect(result.map((r) => r.id)).toEqual([A])
  respond(200, { resources: [resource], folders: [] })
  await expect(listSharedResources()).rejects.toMatchObject({
    kind: "invalid_response",
  })
})
