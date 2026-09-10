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
const folder = {
  ...project,
  id: 3,
  parentId: 1,
  kind: "folder" as const,
  name: "Planning",
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

  it("creates plain folders without repository controls or verification", async () => {
    const connectRepository = vi.fn()
    const createNode = vi.fn().mockResolvedValue(folder)
    const user = userEvent.setup()
    show(
      <TeamNodeDialog
        mode="create"
        kind="folder"
        parentId={1}
        node={null}
        path="Atlas"
        connectRepository={connectRepository}
        createNode={createNode}
        updateNode={vi.fn()}
        onClose={vi.fn()}
      />
    )

    expect(screen.queryByLabelText(/^repository$/i)).toBeNull()
    expect(screen.queryByRole("checkbox")).toBeNull()
    await user.type(screen.getByLabelText(/folder name/i), "Planning")
    await user.click(screen.getByRole("button", { name: /create folder/i }))

    expect(connectRepository).not.toHaveBeenCalled()
    expect(createNode).toHaveBeenCalledWith({
      parentId: 1,
      kind: "folder",
      name: "Planning",
      repositoryDefault: null,
    })
  })

  it("edits plain folders without repository controls or verification", async () => {
    const connectRepository = vi.fn()
    const updateNode = vi.fn().mockResolvedValue({
      ...folder,
      name: "Planning renamed",
      revision: "4",
    })
    const user = userEvent.setup()
    show(
      <TeamNodeDialog
        mode="edit"
        kind="folder"
        parentId={1}
        node={{
          ...folder,
          repositoryDefault: {
            repositoryId: repository.id,
            startingBranch: "main",
          },
        }}
        repository={repository}
        path="Atlas"
        connectRepository={connectRepository}
        createNode={vi.fn()}
        updateNode={updateNode}
        onClose={vi.fn()}
      />
    )

    expect(screen.queryByLabelText(/^repository$/i)).toBeNull()
    expect(screen.queryByRole("checkbox")).toBeNull()
    const name = screen.getByLabelText(/folder name/i)
    await user.clear(name)
    await user.type(name, "Planning renamed")
    await user.click(screen.getByRole("button", { name: /save details/i }))

    expect(connectRepository).not.toHaveBeenCalled()
    expect(updateNode).toHaveBeenCalledWith(
      expect.objectContaining({ id: 3 }),
      {
        expectedRevision: "3",
        parentId: 1,
        name: "Planning renamed",
        repositoryDefault: null,
      }
    )
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
        organization="Atlas Operations"
        inheritedRepository={repository}
        inheritedBranch="main"
        createChat={createChat}
        connectRepository={vi.fn()}
        onClose={vi.fn()}
      />
    )
    expect(
      screen.getByText(
        "Private is the default. Your selected audience applies when you create this chat.",
        { exact: true }
      )
    ).toBeVisible()
    expect(
      screen.getByRole("radio", { name: /^Private to you$/ })
    ).toBeChecked()
    expect(
      screen.getByRole("radio", {
        name: /^Everyone in Atlas Operations$/,
      })
    ).not.toBeChecked()
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

  it("creates for the selected exact organization with sharing disclosures", async () => {
    const created = { ...chat, visibility: "organization" as const }
    const createChat = vi.fn().mockResolvedValue(created)
    const user = userEvent.setup()
    show(
      <TeamChatDialog
        node={project}
        path="Atlas"
        organization="Atlas Operations"
        inheritedRepository={repository}
        inheritedBranch="main"
        createChat={createChat}
        connectRepository={vi.fn()}
        onClose={vi.fn()}
      />
    )

    await user.click(
      screen.getByRole("radio", {
        name: /^Everyone in Atlas Operations$/,
      })
    )
    expect(screen.getByText(/entire conversation history/i)).toBeVisible()
    expect(
      screen.getByText(/github repository access is checked separately/i)
    ).toBeVisible()
    await user.click(screen.getByRole("button", { name: /create chat/i }))

    expect(createChat).toHaveBeenCalledWith(
      expect.objectContaining({ visibility: "organization" })
    )
  })

  it("discloses transcript sharing separately from GitHub access and keeps a failed choice", async () => {
    const share = vi.fn().mockRejectedValue(new Error("failed"))
    const user = userEvent.setup()
    show(
      <TeamShareDialog
        chat={chat}
        organization="Atlas Operations"
        shareChat={share}
        onClose={vi.fn()}
      />
    )
    expect(screen.getByText(/entire conversation history/i)).toBeVisible()
    expect(
      screen.getByText(/github repository access is checked separately/i)
    ).toBeVisible()
    await user.click(
      screen.getByRole("radio", {
        name: /^Everyone in Atlas Operations$/,
      })
    )
    await user.click(screen.getByRole("button", { name: /^share$/i }))
    expect(share).toHaveBeenCalledWith(chat, "organization")
    expect(await screen.findByRole("alert")).toBeVisible()
    expect(
      screen.getByRole("radio", {
        name: /^Everyone in Atlas Operations$/,
      })
    ).toBeChecked()
  })
})
