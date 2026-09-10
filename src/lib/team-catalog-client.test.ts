import { beforeEach, describe, expect, it, vi } from "vitest"

import {
  changeTeamChatVisibility,
  connectTeamRepository,
  createTeamChat,
  createTeamNode,
  listTeamChats,
  listTeamNodes,
  parseTeamChat,
  parseTeamNode,
  updateTeamNode,
} from "./team-catalog-client"

const fetchMock = vi.fn()

const node = {
  id: 7,
  parentId: null,
  kind: "project" as const,
  name: "Atlas",
  repositoryDefault: null,
  revision: "0",
}

const chat = {
  id: 11,
  nodeId: 7,
  title: "Private plan",
  repositoryId: 3,
  githubRepositoryId: "8080",
  fullName: "atlas/repo",
  startingBranch: "main",
  visibility: "private" as const,
  accessRevision: "4",
  canShare: true,
  executionState: "unprovisioned",
}

beforeEach(() => {
  fetchMock.mockReset()
  vi.stubGlobal("fetch", fetchMock)
})

describe("team catalog DTO validation", () => {
  it("accepts the exact node and chat projections", () => {
    expect(parseTeamNode(node)).toEqual(node)
    expect(parseTeamChat(chat)).toEqual(chat)
  })

  it.each([
    { ...node, id: 0 },
    { ...node, revision: "01" },
    { ...node, revision: "9223372036854775808" },
    { ...node, organizationId: 2 },
    {
      ...node,
      repositoryDefault: { repositoryId: -1, startingBranch: "main" },
    },
  ])("rejects malformed node DTO %#", (value) => {
    try {
      parseTeamNode(value)
      throw new Error("expected invalid node rejection")
    } catch (error) {
      expect(error).toMatchObject({ kind: "invalid_response" })
    }
  })

  it.each([
    { ...chat, accessRevision: "-1" },
    { ...chat, githubRepositoryId: "0" },
    { ...chat, visibility: "team" },
    { ...chat, canShare: "yes" },
    { ...chat, creatorUserId: 99 },
  ])("rejects malformed chat DTO %#", (value) => {
    try {
      parseTeamChat(value)
      throw new Error("expected invalid chat rejection")
    } catch (error) {
      expect(error).toMatchObject({ kind: "invalid_response" })
    }
  })
})

describe("team catalog HTTP client", () => {
  it("loads a validated no-store node list", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ items: [node] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      })
    )
    await expect(listTeamNodes()).resolves.toEqual([node])
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/team/catalog/nodes",
      expect.objectContaining({ credentials: "same-origin", cache: "no-store" })
    )
  })

  it("sends exact node revisions and parent snapshots without authority fields", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ ...node, name: "Renamed", revision: "1" }))
    )
    await updateTeamNode(
      7,
      {
        expectedRevision: "0",
        parentId: null,
        name: "Renamed",
        repositoryDefault: null,
      },
      "csrf"
    )
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({
      expectedRevision: "0",
      parentId: null,
      name: "Renamed",
      repositoryDefault: null,
    })
  })

  it.each([
    { userId: 9 },
    { organizationId: 8 },
    { orgId: 8 },
    { role: "admin" },
  ])("rejects accidental authority input %# before fetch", async (extra) => {
    await expect(
      createTeamNode(
        {
          parentId: null,
          kind: "project",
          name: "Atlas",
          repositoryDefault: null,
          ...extra,
        } as never,
        "csrf"
      )
    ).rejects.toMatchObject({ kind: "invalid_request" })
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it("connects a typed repository before catalog mutations", async () => {
    const repository = {
      id: 3,
      githubRepositoryId: "8080",
      fullName: "atlas/repo",
    }
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(repository)))
    await expect(
      connectTeamRepository(
        { fullName: "atlas/repo", startingBranch: "main" },
        "csrf"
      )
    ).resolves.toEqual(repository)
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({
      fullName: "atlas/repo",
      startingBranch: "main",
    })
  })

  it("creates private chats explicitly and paginates after the last visible id", async () => {
    fetchMock
      .mockResolvedValueOnce(new Response(JSON.stringify(chat)))
      .mockResolvedValueOnce(new Response(JSON.stringify({ items: [chat] })))
    await createTeamChat(
      {
        nodeId: 7,
        repositoryOverride: null,
        title: "Private plan",
        requestId: "123e4567-e89b-12d3-a456-426614174000",
        visibility: "private",
      },
      "csrf"
    )
    expect(JSON.parse(fetchMock.mock.calls[0][1].body).visibility).toBe(
      "private"
    )
    await listTeamChats({ nodeId: 7, afterId: 200 })
    expect(fetchMock.mock.calls[1][0]).toBe(
      "/api/team/chats?nodeId=7&afterId=200"
    )
  })

  it("changes visibility with the current access revision", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          ...chat,
          visibility: "organization",
          accessRevision: "5",
        })
      )
    )
    await changeTeamChatVisibility(
      11,
      { expectedRevision: "4", visibility: "organization" },
      "csrf"
    )
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({
      expectedRevision: "4",
      visibility: "organization",
    })
  })
})
