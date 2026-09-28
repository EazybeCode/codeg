import { render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"
import { ChatFilesList } from "./native-chat-files"
import { listResourceChatFiles } from "@/lib/neev-session-files"
vi.mock("@/lib/neev-session-files", () => ({
  listResourceChatFiles: vi.fn(),
  listAgentSessionFiles: vi.fn(),
  formatFileSize: () => "80 B",
}))
const key = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
const file = {
  key,
  fileName: "result.html",
  contentType: "text/plain" as const,
  byteLength: 80,
  sequence: "1",
}
function view(running: boolean) {
  return (
    <NextIntlClientProvider locale="en" messages={messages}>
      <ChatFilesList
        id={key}
        kind="chat"
        scopeKey="org:user"
        isRunning={running}
      />
    </NextIntlClientProvider>
  )
}
it("keeps normal authenticated HTML links and refreshes when the run finishes without looping", async () => {
  vi.mocked(listResourceChatFiles).mockResolvedValue([file])
  const ui = render(view(true))
  expect(
    await screen.findByRole("link", { name: /result.html/ })
  ).toHaveAttribute("href", `/api/neev/attachments/${key}`)
  ui.rerender(view(false))
  await waitFor(() => expect(listResourceChatFiles).toHaveBeenCalledTimes(2))
  await screen.findByRole("link", { name: /result.html/ })
  expect(listResourceChatFiles).toHaveBeenCalledTimes(2)
})
it("aborts file loading when collapsed/unmounted", () => {
  vi.mocked(listResourceChatFiles).mockImplementation(
    () => new Promise(() => {})
  )
  const ui = render(view(false))
  const signal = vi.mocked(listResourceChatFiles).mock.calls[0][1]
  ui.unmount()
  expect(signal?.aborted).toBe(true)
})
