import { render, waitFor } from "@testing-library/react"
import { beforeEach, expect, it, vi } from "vitest"
import { WorkspaceOpenFolderListener } from "./workspace-open-folder-listener"

const mocks = vi.hoisted(() => ({
  subscribe: vi.fn(),
  upsertFolder: vi.fn(),
  setBranch: vi.fn(),
  refreshConversations: vi.fn(),
  openConversations: vi.fn(),
  openNewConversationTab: vi.fn(),
  catalog: { enabled: true },
}))

vi.mock("@/lib/platform", () => ({ subscribe: mocks.subscribe }))
vi.mock("@/stores/app-workspace-store", () => ({
  useAppWorkspaceStore: {
    getState: () => ({
      upsertFolder: mocks.upsertFolder,
      setBranch: mocks.setBranch,
      refreshConversations: mocks.refreshConversations,
    }),
  },
}))
vi.mock("@/contexts/tab-context", () => ({
  useTabActions: () => ({
    openNewConversationTab: mocks.openNewConversationTab,
  }),
}))
vi.mock("@/contexts/workbench-route-context", () => ({
  useWorkbenchRoute: () => ({ openConversations: mocks.openConversations }),
}))
vi.mock("@/components/workspace/workspace-catalog-provider", () => ({
  useWorkspaceCatalogContext: () => mocks.catalog,
}))

beforeEach(() => {
  vi.clearAllMocks()
  mocks.catalog.enabled = true
  mocks.subscribe.mockImplementation(async (_event, handler) => {
    handler({
      id: 7,
      kind: "regular",
      name: "Legacy",
      path: "/legacy",
      git_branch: "main",
    })
    return vi.fn()
  })
})

it("ignores legacy open-folder events in catalog mode", async () => {
  render(<WorkspaceOpenFolderListener />)
  await waitFor(() => expect(mocks.subscribe).toHaveBeenCalledOnce())

  expect(mocks.upsertFolder).not.toHaveBeenCalled()
  expect(mocks.openNewConversationTab).not.toHaveBeenCalled()
  expect(mocks.refreshConversations).not.toHaveBeenCalled()
})
