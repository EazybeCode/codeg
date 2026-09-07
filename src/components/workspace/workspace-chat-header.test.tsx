import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import en from "@/i18n/messages/en.json"
import { WorkspaceChatHeader } from "./workspace-chat-header"
import type { WorkspaceChatRuntime } from "@/lib/workspace-execution"

vi.mock("./workspace-catalog-provider", () => ({
  useWorkspaceCatalogContext: () => ({
    pathFor: () => [
      { name: "Website refresh" },
      { name: "Launch planning" },
      { name: "Campaign site" },
    ],
  }),
}))
vi.mock("@/contexts/aux-panel-context", () => ({
  useAuxPanelContext: () => ({ revealInFileTree: vi.fn() }),
}))

it("exposes the complete logical path when the Files pane leaves a narrow chat header", () => {
  const runtime = {
    draft: { node_id: 3, full_name: "fixture-org/docs" },
    branch_name: "workos/chat/2",
    folder: null,
  } as WorkspaceChatRuntime
  render(
    <NextIntlClientProvider locale="en" messages={en}>
      <WorkspaceChatHeader runtime={runtime} />
    </NextIntlClientProvider>
  )
  const path = screen.getByRole("navigation", { name: "Projects" })
  expect(path).toHaveAttribute(
    "title",
    "Website refresh / Launch planning / Campaign site"
  )
})
