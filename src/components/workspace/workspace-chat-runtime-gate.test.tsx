import {
  cleanup,
  render as rtlRender,
  screen,
  waitFor,
  fireEvent,
} from "@testing-library/react"
import { afterEach, expect, it, vi } from "vitest"
import { NextIntlClientProvider } from "next-intl"
import enMessages from "@/i18n/messages/en.json"
import type { ReactNode } from "react"
const render = (ui: ReactNode) =>
  rtlRender(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      {ui}
    </NextIntlClientProvider>
  )
import { WorkspaceChatRuntimeGate } from "./workspace-chat-runtime-gate"
const call = vi.hoisted(() => vi.fn())
vi.mock("@/lib/transport", () => ({ getTransport: () => ({ call }) }))
afterEach(() => {
  cleanup()
  call.mockReset()
})

it("hydrates two saved tabs from legacy IDs before mounting managed ACP", async () => {
  const pending = new Map<number, (value: unknown) => void>()
  call.mockImplementation(
    (_method, { conversationId }) =>
      new Promise((resolve) => pending.set(conversationId, resolve))
  )
  render(
    <>
      <WorkspaceChatRuntimeGate conversationId={17} folderId={8}>
        {(id) => <p>Agent chat {id}</p>}
      </WorkspaceChatRuntimeGate>
      <WorkspaceChatRuntimeGate conversationId={18} folderId={9}>
        {(id) => <p>Agent chat {id}</p>}
      </WorkspaceChatRuntimeGate>
    </>
  )
  expect(screen.queryAllByText(/Agent chat/)).toHaveLength(0)
  await waitFor(() => expect(call).toHaveBeenCalledTimes(2))
  pending.get(17)!({
    state: "ready",
    draft: { id: 42 },
    folder: { id: 8 },
    conversation: { id: 17 },
  })
  pending.get(18)!({
    state: "ready",
    draft: { id: 71 },
    folder: { id: 9 },
    conversation: { id: 18 },
  })
  expect(await screen.findByText("Agent chat 42")).toBeInTheDocument()
  expect(await screen.findByText("Agent chat 71")).toBeInTheDocument()
  expect(call).toHaveBeenCalledWith(
    "get_workspace_chat_runtime_for_conversation",
    { conversationId: 18 }
  )
})

it("keeps managed ACP unmounted on mapping failure and offers retry", async () => {
  call.mockRejectedValueOnce({ error: "unauthorized" }).mockResolvedValueOnce({
    state: "ready",
    draft: { id: 42 },
    folder: { id: 8 },
    conversation: { id: 17 },
  })
  render(
    <WorkspaceChatRuntimeGate conversationId={17} folderId={8}>
      {(id) => <p>Agent chat {id}</p>}
    </WorkspaceChatRuntimeGate>
  )
  fireEvent.click(await screen.findByRole("button", { name: "Retry" }))
  expect(screen.queryByText("Agent chat 0")).toBeNull()
  expect(await screen.findByText("Agent chat 42")).toBeInTheDocument()
})
