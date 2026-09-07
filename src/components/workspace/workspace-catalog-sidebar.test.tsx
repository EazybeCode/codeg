import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import en from "@/i18n/messages/en.json"
import { WorkspaceCatalogProvider } from "./workspace-catalog-provider"
import { WorkspaceCatalogSidebar } from "./workspace-catalog-sidebar"
import type { WorkspaceNode, WorkspaceChatDraft } from "@/lib/workspace-catalog"

const { call } = vi.hoisted(() => ({ call: vi.fn() }))
vi.mock("@/lib/transport", () => ({
  getTransport: () => ({ call }),
  isDesktop: () => false,
}))
vi.mock("@/lib/platform", () => ({ isDesktop: () => false }))
let nodes: WorkspaceNode[]
const project: WorkspaceNode = {
  id: 1,
  org_id: 4,
  parent_id: null,
  kind: "project",
  name: "Marketing",
  repository_default: null,
  revision: 1,
}
const draft: WorkspaceChatDraft = {
  id: 20,
  org_id: 4,
  node_id: 1,
  creator_user_id: 7,
  repository_id: 8,
  github_repository_id: "900",
  full_name: "acme/site",
  starting_branch: "main",
  title: "Launch",
  execution_state: "unprovisioned",
}
function mount() {
  return render(
    <NextIntlClientProvider locale="en" messages={en}>
      <WorkspaceCatalogProvider>
        <WorkspaceCatalogSidebar />
      </WorkspaceCatalogProvider>
    </NextIntlClientProvider>
  )
}
beforeEach(() => {
  nodes = []
  call.mockReset().mockImplementation(async (command, body) => {
    if (command === "list_workspace_nodes") return [...nodes]
    if (command === "list_workspace_repositories")
      return [
        {
          id: 8,
          org_id: 4,
          github_repository_id: "900",
          full_name: "acme/site",
        },
      ]
    if (command === "list_workspace_chat_drafts") return []
    if (command === "create_workspace_node") {
      const node = {
        ...project,
        id: nodes.length + 1,
        parent_id: body.parentId,
        name: body.name,
        kind: body.kind,
        repository_default: body.repositoryDefault,
      }
      nodes.push(node)
      return node
    }
    if (command === "connect_workspace_repository")
      return {
        id: 8,
        org_id: 4,
        github_repository_id: "900",
        full_name: body.fullName,
      }
    if (command === "create_workspace_chat_draft")
      return { ...draft, title: body.title }
    throw new Error(`Unexpected command ${command}`)
  })
  vi.stubGlobal(
    "fetch",
    vi.fn(
      async (url) =>
        new Response(
          JSON.stringify(
            url === "/auth/me"
              ? {
                  user: {
                    id: 7,
                    login: "maya",
                    name: "Maya Rao",
                    role: "member",
                  },
                  organization: { id: 4, login: "acme", name: "Acme Product" },
                }
              : [
                  {
                    full_name: "acme/site",
                    name: "site",
                    owner: "acme",
                    private: true,
                    default_branch: "main",
                  },
                ]
          )
        )
    )
  )
  window.history.replaceState({}, "", "/workspace")
})
afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})
async function action(name: string) {
  await userEvent.click(
    await screen.findByRole("button", { name: "Actions for Marketing" })
  )
  await userEvent.click(await screen.findByRole("menuitem", { name }))
}

describe("organization catalog creation", () => {
  it("creates a repo-free project in an empty organization", async () => {
    mount()
    await userEvent.click(
      await screen.findByRole("button", { name: "New project" })
    )
    await userEvent.type(screen.getByLabelText("Project name"), "Marketing")
    await userEvent.click(
      screen.getByRole("button", { name: "Create project" })
    )
    expect(
      await screen.findByRole("button", { name: "Marketing" })
    ).toBeVisible()
    expect(
      screen.queryByRole("button", { name: "New page" })
    ).not.toBeInTheDocument()
    expect(
      call.mock.calls.some(([cmd]) => cmd === "connect_workspace_repository")
    ).toBe(false)
  })
  it("loads drafts only on expansion and preserves nested folder expansion", async () => {
    nodes = [
      project,
      { ...project, id: 2, parent_id: 1, kind: "folder", name: "Campaigns" },
      { ...project, id: 3, parent_id: 2, kind: "folder", name: "September" },
    ]
    mount()
    await screen.findByRole("button", { name: "Marketing" })
    expect(
      call.mock.calls.filter(([cmd]) => cmd === "list_workspace_chat_drafts")
    ).toHaveLength(0)
    await userEvent.click(
      screen.getByRole("button", { name: "Expand Marketing" })
    )
    await userEvent.click(
      await screen.findByRole("button", { name: "Expand Campaigns" })
    )
    expect(
      await screen.findByRole("button", { name: "September" })
    ).toBeVisible()
    await userEvent.click(
      screen.getByRole("button", { name: "Collapse Campaigns" })
    )
    expect(
      screen.queryByRole("button", { name: "September" })
    ).not.toBeInTheDocument()
    expect(
      call.mock.calls
        .filter(([cmd]) => cmd === "list_workspace_chat_drafts")
        .map(([, body]) => body.nodeId)
    ).toEqual([1, 2])
  })
  it("requires a repository for unbound chats and records canonical chat links", async () => {
    nodes = [project]
    mount()
    await action("New chat")
    await userEvent.type(screen.getByLabelText("Chat title"), "Launch")
    expect(screen.getByRole("button", { name: "Create chat" })).toBeDisabled()
    await userEvent.type(screen.getByLabelText("Repository"), "acme/site")
    await userEvent.type(screen.getByLabelText("Starting branch"), "main")
    await userEvent.click(screen.getByRole("button", { name: "Create chat" }))
    await waitFor(() => expect(window.location.search).toBe("?chatId=20"))
    expect(call).toHaveBeenCalledWith(
      "create_workspace_chat_draft",
      expect.objectContaining({
        nodeId: 1,
        title: "Launch",
        repositoryOverride: { repository_id: 8, starting_branch: "main" },
      })
    )
  })
  it("shows reloaded inherited names and sends an inherited default without an override", async () => {
    nodes = [
      {
        ...project,
        repository_default: { repository_id: 8, starting_branch: "release" },
      },
    ]
    mount()
    await action("New chat")
    expect(screen.getByText(/acme\/site.*release/)).toBeVisible()
    await userEvent.click(screen.getByRole("button", { name: "Create chat" }))
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith(
        "create_workspace_chat_draft",
        expect.objectContaining({ repositoryOverride: null })
      )
    )
  })
  it("retains input and UUID for unchanged retries, changes UUID after edits", async () => {
    nodes = [
      {
        ...project,
        repository_default: { repository_id: 8, starting_branch: "main" },
      },
    ]
    const normal = call.getMockImplementation()!
    call.mockImplementation(async (command, body) => {
      if (command === "create_workspace_chat_draft")
        throw { error: "upstream_unavailable" }
      return normal(command, body)
    })
    mount()
    await action("New chat")
    await userEvent.type(screen.getByLabelText("Chat title"), "Launch")
    await userEvent.click(screen.getByRole("button", { name: "Create chat" }))
    await screen.findByRole("alert")
    await userEvent.click(screen.getByRole("button", { name: "Create chat" }))
    await waitFor(() =>
      expect(
        call.mock.calls.filter(([cmd]) => cmd === "create_workspace_chat_draft")
      ).toHaveLength(2)
    )
    await userEvent.type(screen.getByLabelText("Chat title"), " revised")
    await userEvent.click(screen.getByRole("button", { name: "Create chat" }))
    await waitFor(() =>
      expect(
        call.mock.calls.filter(([cmd]) => cmd === "create_workspace_chat_draft")
      ).toHaveLength(3)
    )
    const attempts = call.mock.calls
      .filter(([cmd]) => cmd === "create_workspace_chat_draft")
      .map(([, body]) => body)
    expect(attempts[0].requestId).toBe(attempts[1].requestId)
    expect(attempts[2].requestId).not.toBe(attempts[1].requestId)
    expect(screen.getByLabelText("Chat title")).toHaveValue("Launch revised")
  })
  it("verifies an explicit chat override without renaming or changing the parent default", async () => {
    nodes = [
      {
        ...project,
        repository_default: { repository_id: 8, starting_branch: "main" },
      },
    ]
    mount()
    await action("New chat")
    await userEvent.click(screen.getByRole("checkbox"))
    expect(screen.getByRole("button", { name: "Create chat" })).toBeDisabled()
    await userEvent.type(screen.getByLabelText("Repository"), "acme/other")
    await userEvent.type(screen.getByLabelText("Starting branch"), "release")
    await userEvent.click(screen.getByRole("button", { name: "Create chat" }))
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith(
        "create_workspace_chat_draft",
        expect.objectContaining({
          repositoryOverride: { repository_id: 8, starting_branch: "release" },
        })
      )
    )
    expect(call).toHaveBeenCalledWith("connect_workspace_repository", {
      fullName: "acme/other",
      startingBranch: "release",
    })
    expect(nodes[0]).toMatchObject({
      name: "Marketing",
      repository_default: { repository_id: 8, starting_branch: "main" },
    })
  })
  it("creates a working directory with a separately named repository default", async () => {
    nodes = [project]
    mount()
    await action("New working directory")
    await userEvent.type(
      screen.getByLabelText("Working directory name"),
      "Website"
    )
    expect(
      screen.getByRole("button", { name: "Create working directory" })
    ).toBeDisabled()
    await userEvent.type(screen.getByLabelText("Repository"), "acme/site")
    await userEvent.type(screen.getByLabelText("Starting branch"), "main")
    await userEvent.click(
      screen.getByRole("button", { name: "Create working directory" })
    )
    expect(await screen.findByRole("button", { name: "Website" })).toBeVisible()
    expect(call).toHaveBeenCalledWith("create_workspace_node", {
      parentId: 1,
      kind: "working_directory",
      name: "Website",
      repositoryDefault: { repository_id: 8, starting_branch: "main" },
    })
  })
  it("offers reconnect for parsed authentication errors and retains form fields", async () => {
    nodes = [project]
    const normal = call.getMockImplementation()!
    call.mockImplementation(async (command, body) => {
      if (command === "create_workspace_node")
        throw { error: "reauth_required" }
      return normal(command, body)
    })
    mount()
    await action("New folder")
    await userEvent.type(screen.getByLabelText("Folder name"), "Campaigns")
    await userEvent.click(screen.getByRole("button", { name: "Create folder" }))
    expect(
      await within(screen.getByRole("dialog")).findByRole("link", {
        name: "Reconnect GitHub",
      })
    ).toHaveAttribute("href", "/auth/github/login")
    expect(screen.getByLabelText("Folder name")).toHaveValue("Campaigns")
  })
})
