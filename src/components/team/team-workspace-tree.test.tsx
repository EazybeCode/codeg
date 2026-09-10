import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { expect, it, vi } from "vitest"

import messages from "@/i18n/messages/en.json"
import { TeamWorkspaceTree } from "./team-workspace-tree"

const nodes = [
  {
    id: 1,
    parentId: null,
    kind: "project" as const,
    name: "Atlas",
    repositoryDefault: null,
    revision: "0",
  },
  {
    id: 2,
    parentId: 1,
    kind: "folder" as const,
    name: "Planning",
    repositoryDefault: null,
    revision: "0",
  },
]

it("provides keyboard-operable nested actions without another project action", async () => {
  const onCreate = vi.fn()
  const user = userEvent.setup()
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <TeamWorkspaceTree
        nodes={nodes}
        chats={[]}
        expanded={new Set([1])}
        selectedNodeId={null}
        selectedChatId={null}
        onToggle={vi.fn()}
        onSelectNode={vi.fn()}
        onSelectChat={vi.fn()}
        onCreate={onCreate}
        onEdit={vi.fn()}
        onLoadMore={vi.fn()}
        hasMore={false}
        loadingMore={false}
      />
    </NextIntlClientProvider>
  )
  const actions = screen.getByRole("button", { name: /actions for atlas/i })
  actions.focus()
  await user.keyboard("{Enter}")
  expect(screen.getByRole("menuitem", { name: /new folder/i })).toBeVisible()
  expect(
    screen.getByRole("menuitem", { name: /new working directory/i })
  ).toBeVisible()
  expect(screen.getByRole("menuitem", { name: /new chat/i })).toBeVisible()
  expect(screen.queryByRole("menuitem", { name: /new project/i })).toBeNull()
  await user.click(screen.getByRole("menuitem", { name: /new folder/i }))
  expect(onCreate).toHaveBeenCalledWith("folder", 1)
})
