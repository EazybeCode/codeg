import { getTransport } from "./transport"

export type WorkspaceNodeKind = "project" | "folder" | "working_directory"

export interface RepositoryDefault {
  repository_id: number
  starting_branch: string
}

export interface WorkspaceNode {
  id: number
  org_id: number
  parent_id: number | null
  kind: WorkspaceNodeKind
  name: string
  repository_default: RepositoryDefault | null
  revision: number
}

export interface WorkspaceRepository {
  id: number
  org_id: number
  github_repository_id: string
  full_name: string
}

export interface WorkspaceChatDraft {
  id: number
  org_id: number
  node_id: number
  creator_user_id: number
  repository_id: number
  github_repository_id: string
  full_name: string
  starting_branch: string
  title: string | null
  execution_state: "unprovisioned"
}

export type CreateNodeInput = {
  parentId: number | null
  kind: WorkspaceNodeKind
  name: string
  repositoryDefault: RepositoryDefault | null
}

export type UpdateNodeInput = {
  nodeId: number
  expectedRevision: number
  parentId: number | null
  name: string
  repositoryDefault: RepositoryDefault | null
}

export type CreateDraftInput = {
  nodeId: number
  repositoryOverride: RepositoryDefault | null
  title: string | null
  requestId: string
}

export function listWorkspaceNodes(): Promise<WorkspaceNode[]> {
  return getTransport().call("list_workspace_nodes", {})
}

export function createWorkspaceNode(
  input: CreateNodeInput
): Promise<WorkspaceNode> {
  return getTransport().call("create_workspace_node", input)
}

export function updateWorkspaceNode(
  input: UpdateNodeInput
): Promise<WorkspaceNode> {
  return getTransport().call("update_workspace_node", input)
}

export function connectWorkspaceRepository(
  fullName: string,
  startingBranch: string
): Promise<WorkspaceRepository> {
  return getTransport().call("connect_workspace_repository", {
    fullName,
    startingBranch,
  })
}

export function createWorkspaceChatDraft(
  input: CreateDraftInput
): Promise<WorkspaceChatDraft> {
  return getTransport().call("create_workspace_chat_draft", input)
}

export function listWorkspaceChatDrafts(
  nodeId: number
): Promise<WorkspaceChatDraft[]> {
  return getTransport().call("list_workspace_chat_drafts", { nodeId })
}
