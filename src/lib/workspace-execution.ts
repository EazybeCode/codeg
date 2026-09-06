import type { DbConversationSummary, FolderDetail } from "./types"
import type { WorkspaceChatDraft } from "./workspace-catalog"

export type WorkspaceExecutionState =
  | "unprovisioned"
  | "provisioning"
  | "ready"
  | "failed"

export interface WorkspaceChatRuntime {
  draft: WorkspaceChatDraft
  state: WorkspaceExecutionState
  folder: FolderDetail | null
  conversation: DbConversationSummary | null
  branch_name: string | null
  starting_commit: string | null
  error_code: string | null
}
