import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import messages from "@/i18n/messages/en.json"
import {
  TeamChatDialog,
  TeamNodeDialog,
  TeamShareDialog,
} from "./team-workspace-dialogs"

const project = {
  id: 1,
  parentId: null,
  kind: "project" as const,
  name: "Atlas",
  repositoryDefault: null,
  revision: "3",
}
const repository = {
  id: 2,
  githubRepositoryId: "8080",
  fullName: "atlas/repo",
}
const chat = {
  id: 9,
  nodeId: 1,
  title: "Plan",
  repositoryId: 2,
  githubRepositoryId: "8080",
  fullName: "atlas/repo",
  startingBranch: "main",
  visibility: "private" as const,
  accessRevision: "4",
  canShare: true,
  executionState: "unprovisioned" as const,
}

function show(child: React.ReactNode) {
  return render(
    <NextIntlClientProvider locale="en" messages={messages}>
      {child}
    </NextIntlClientProvider>
  )
}

beforeEach(() => vi.clearAllMocks())

describe("Team workspace dialogs", () => {
  it("requires a verified repository for a working directory and preserves failed input", async () => {
    const connectRepository = vi.fn().mockResolvedValue(repository)
    const createNode = vi.fn().mockRejectedValue(new Error("failed"))
    const user = userEvent.setup()
    show(
      <TeamNodeDialog
        mode="create"
        kind="working_directory"
        parentId={1}
        node={null}
        path="Atlas"
        connectRepository={connectRepository}
        createNode={createNode}
        updateNode={vi.fn()}
        onClose={vi.fn()}
      />
    )
    await user.type(screen.getByLabelText(/working directory name/i), "App")
    expect(
      screen.getByRole("button", { name: /create working directory/i })
    ).toBeDisabled()
    await user.type(screen.getByLabelText(/^repository$/i), "atlas/repo")
    await user.type(screen.getByLabelText(/starting branch/i), "main")
    await user.click(
      screen.getByRole("button", { name: /create working directory/i })
    )
    expect(connectRepository).toHaveBeenCalledWith("atlas/repo", "main")
    expect(createNode).toHaveBeenCalledWith({
      parentId: 1,
      kind: "working_directory",
      name: "App",
      repositoryDefault: { repositoryId: 2, startingBranch: "main" },
    })
    expect(await screen.findByRole("alert")).toBeVisible()
    expect(screen.getByLabelText(/working directory name/i)).toHaveValue("App")
  })

  it("edits with the exact revision and parent snapshot while retaining the draft on conflict", async () => {
    const updateNode = vi.fn().mockRejectedValue(new Error("conflict"))
    const user = userEvent.setup()
    show(
      <TeamNodeDialog
        mode="edit"
        kind="project"
        parentId={null}
        node={project}
        path="Atlas"
        connectRepository={vi.fn()}
        createNode={vi.fn()}
        updateNode={updateNode}
        onClose={vi.fn()}
      />
    )
    const name = screen.getByLabelText(/project name/i)
    await user.clear(name)
    await user.type(name, "Atlas renamed")
    await user.click(screen.getByRole("button", { name: /save details/i }))
    expect(updateNode).toHaveBeenCalledWith(project, {
      expectedRevision: "3",
      parentId: null,
      name: "Atlas renamed",
      repositoryDefault: null,
    })
    expect(await screen.findByRole("alert")).toBeVisible()
    expect(name).toHaveValue("Atlas renamed")
    expect(screen.getByText(/existing chats keep/i)).toBeVisible()
  })

  it("closes after a successful edit persists", async () => {
    const updateNode = vi
      .fn()
      .mockResolvedValue({ ...project, name: "Atlas renamed", revision: "4" })
    const onClose = vi.fn()
    const user = userEvent.setup()
    show(
      <TeamNodeDialog
        mode="edit"
        kind="project"
        parentId={null}
        node={project}
        path="Atlas"
        connectRepository={vi.fn()}
        createNode={vi.fn()}
        updateNode={updateNode}
        onClose={onClose}
      />
    )
    const name = screen.getByLabelText(/project name/i)
    await user.clear(name)
    await user.type(name, "Atlas renamed")
    await user.click(screen.getByRole("button", { name: /save details/i }))
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce())
    expect(updateNode).toHaveBeenCalledWith(
      project,
      expect.objectContaining({ name: "Atlas renamed" })
    )
  })

  it("shows private before chat creation and sends it explicitly", async () => {
    const createChat = vi.fn().mockResolvedValue(chat)
    const user = userEvent.setup()
    show(
      <TeamChatDialog
        node={project}
        path="Atlas"
        inheritedRepository={repository}
        inheritedBranch="main"
        createChat={createChat}
        connectRepository={vi.fn()}
        onClose={vi.fn()}
      />
    )
    expect(screen.getByText(/private to you/i)).toBeVisible()
    expect(screen.getByText(/nearest repository default/i)).toHaveTextContent(
      /atlas\/repo.*main/i
    )
    await user.type(screen.getByLabelText(/chat title/i), "Private plan")
    await user.click(screen.getByRole("button", { name: /create chat/i }))
    expect(createChat).toHaveBeenCalledWith(
      expect.objectContaining({
        nodeId: 1,
        title: "Private plan",
        visibility: "private",
        repositoryOverride: null,
      })
    )
  })

  it("discloses transcript sharing separately from GitHub access and keeps a failed choice", async () => {
    const share = vi.fn().mockRejectedValue(new Error("failed"))
    const user = userEvent.setup()
    show(<TeamShareDialog chat={chat} shareChat={share} onClose={vi.fn()} />)
    expect(screen.getByText(/entire conversation history/i)).toBeVisible()
    expect(
      screen.getByText(/github repository access is checked separately/i)
    ).toBeVisible()
    await user.click(screen.getByRole("radio", { name: /organization/i }))
    await user.click(screen.getByRole("button", { name: /^share$/i }))
    expect(share).toHaveBeenCalledWith(chat, "organization")
    expect(await screen.findByRole("alert")).toBeVisible()
    expect(screen.getByRole("radio", { name: /organization/i })).toBeChecked()
  })
})
