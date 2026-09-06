import { beforeEach, describe, expect, it, vi } from "vitest"

const { call } = vi.hoisted(() => ({
  call: vi.fn().mockResolvedValue({}),
}))

vi.mock("./transport", () => ({ getTransport: () => ({ call }) }))

import {
  connectWorkspaceRepository,
  createWorkspaceChatDraft,
  createWorkspaceNode,
  listWorkspaceChatDrafts,
  listWorkspaceNodes,
  updateWorkspaceNode,
  type CreateNodeInput,
} from "./workspace-catalog"

describe("workspace catalog transport", () => {
  beforeEach(() => {
    call.mockReset()
    call.mockResolvedValue({})
  })

  it("passes create node input unchanged", async () => {
    const input: CreateNodeInput = {
      parentId: null,
      kind: "project",
      name: "Marketing",
      repositoryDefault: null,
    }

    await createWorkspaceNode(input)

    expect(call).toHaveBeenCalledWith("create_workspace_node", input)
  })

  it("uses the exact endpoint parameters for every catalog operation", async () => {
    const update = {
      nodeId: 3,
      expectedRevision: 2,
      parentId: null,
      name: "Marketing renamed",
      repositoryDefault: null,
    } as const
    const draft = {
      nodeId: 3,
      repositoryOverride: null,
      title: "Campaign",
      requestId: "0c58d910-3e1a-43c9-9462-785335828829",
    }

    await listWorkspaceNodes()
    await updateWorkspaceNode(update)
    await connectWorkspaceRepository("acme/docs", "main")
    await createWorkspaceChatDraft(draft)
    await listWorkspaceChatDrafts(3)

    expect(call.mock.calls).toEqual([
      ["list_workspace_nodes", {}],
      ["update_workspace_node", update],
      [
        "connect_workspace_repository",
        { fullName: "acme/docs", startingBranch: "main" },
      ],
      ["create_workspace_chat_draft", draft],
      ["list_workspace_chat_drafts", { nodeId: 3 }],
    ])
  })

  it("propagates rejected transport requests", async () => {
    const rejection = { error: "repository_required" }
    call.mockRejectedValueOnce(rejection)

    await expect(
      createWorkspaceChatDraft({
        nodeId: 4,
        repositoryOverride: null,
        title: null,
        requestId: "ce61a385-f70c-4d6d-b96e-f58c9c86cc3c",
      })
    ).rejects.toBe(rejection)
  })
})
