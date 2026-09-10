import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import messages from "@/i18n/messages/en.json"

const catalog = vi.hoisted(() => ({ value: {} as Record<string, unknown> }))
vi.mock("./use-team-catalog", () => ({ useTeamCatalog: () => catalog.value }))

import { TeamWorkspace } from "./team-workspace"

const session = {
  user: { id: 11, login: "alice", name: "Alice Atlas", avatarUrl: null },
  organization: {
    id: 21,
    login: "atlas-labs",
    name: "Atlas Operations",
    avatarUrl: null,
  },
  role: "admin" as const,
  scope: "scope-a",
  csrf: "csrf-a",
}
const project = {
  id: 1,
  parentId: null,
  kind: "project" as const,
  name: "Atlas",
  repositoryDefault: null,
  revision: "0",
}
const chat = {
  id: 9,
  nodeId: 1,
  title: "Shared plan",
  repositoryId: 2,
  githubRepositoryId: "8080",
  fullName: "atlas/repo",
  startingBranch: "main",
  visibility: "organization" as const,
  accessRevision: "0",
  canShare: false,
  executionState: "unprovisioned",
}

function show(viewport = 1440) {
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    value: viewport,
  })
  return render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <TeamWorkspace session={session} logout={vi.fn()} logoutError={false} />
    </NextIntlClientProvider>
  )
}

beforeEach(() => {
  vi.clearAllMocks()
  catalog.value = {
    nodes: [],
    repositories: [],
    chats: [],
    selectedNodeId: null,
    selectedChatId: null,
    selectedChat: null,
    expanded: new Set<number>(),
    loading: false,
    error: null,
    loadingMoreChats: false,
    hasMoreChats: false,
    refresh: vi.fn(),
    loadMoreChats: vi.fn(),
    toggleNode: vi.fn(),
    selectNode: vi.fn(),
    selectChat: vi.fn(),
    createNode: vi.fn(),
    updateNode: vi.fn(),
    connectRepository: vi.fn(),
    createChat: vi.fn(),
    shareChat: vi.fn(),
    pathFor: vi.fn(() => []),
  }
})

describe("TeamWorkspace", () => {
  it("offers only project creation globally and labels person and organization accurately", () => {
    show()
    expect(screen.getByText("Alice Atlas")).toBeVisible()
    expect(screen.getByText("Atlas Operations")).toBeVisible()
    expect(
      screen.getAllByRole("button", { name: /new project/i }).length
    ).toBeGreaterThan(0)
    expect(screen.queryByRole("button", { name: /new chat/i })).toBeNull()
  })

  it("shows loading, retryable error and clean empty states", async () => {
    catalog.value = { ...catalog.value, loading: true }
    const view = show()
    expect(screen.getByRole("status")).toHaveTextContent(/loading workspace/i)

    catalog.value = { ...catalog.value, loading: false, error: new Error("no") }
    view.rerender(
      <NextIntlClientProvider locale="en" messages={messages}>
        <TeamWorkspace session={session} logout={vi.fn()} logoutError={false} />
      </NextIntlClientProvider>
    )
    expect(screen.getByRole("alert")).toBeVisible()
    await userEvent.click(screen.getByRole("button", { name: /retry/i }))
    expect(catalog.value.refresh).toHaveBeenCalledOnce()
  })

  it("renders selected chat metadata, no send control, and Share only for creators", () => {
    catalog.value = {
      ...catalog.value,
      nodes: [project],
      chats: [chat],
      selectedNodeId: 1,
      selectedChatId: 9,
      selectedChat: chat,
      pathFor: vi.fn(() => [project]),
    }
    const view = show()
    expect(screen.getByRole("heading", { name: "Shared plan" })).toBeVisible()
    expect(screen.getByText("atlas/repo")).toBeVisible()
    expect(screen.getByText("main")).toBeVisible()
    expect(screen.getByText(/runtime is disabled/i)).toBeVisible()
    expect(screen.queryByRole("textbox", { name: /message/i })).toBeNull()
    expect(screen.queryByRole("button", { name: /^share$/i })).toBeNull()

    catalog.value = {
      ...catalog.value,
      selectedChat: { ...chat, canShare: true },
    }
    view.rerender(
      <NextIntlClientProvider locale="en" messages={messages}>
        <TeamWorkspace session={session} logout={vi.fn()} logoutError={false} />
      </NextIntlClientProvider>
    )
    expect(screen.getByRole("button", { name: /^share$/i })).toBeVisible()
  })

  it("opens Share from an authoritative deep-linked detail outside page one", async () => {
    catalog.value = {
      ...catalog.value,
      nodes: [project],
      chats: [],
      selectedNodeId: 1,
      selectedChatId: 9,
      selectedChat: { ...chat, canShare: true },
      pathFor: vi.fn(() => [project]),
    }
    show()

    await userEvent.click(screen.getByRole("button", { name: /^share$/i }))

    expect(screen.getByRole("dialog")).toBeVisible()
    expect(screen.getByRole("heading", { name: /^share$/i })).toBeVisible()
  })

  it("provides a labeled mobile navigation toggle", async () => {
    show(375)
    const toggle = screen.getByRole("button", {
      name: /open workspace navigation/i,
    })
    await userEvent.click(toggle)
    expect(
      screen.getByRole("navigation", { name: /workspace navigation/i })
    ).toBeVisible()
  })
})
