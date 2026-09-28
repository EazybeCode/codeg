import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"
import { FileActions } from "./native-file-actions"
import { archiveChatFile, readTextPreview } from "@/lib/neev-file-actions"
vi.mock("./native-auth-provider", () => ({
  useOptionalNativeAuth: () => ({ session: { csrf: "csrf" } }),
}))
vi.mock("@/lib/neev-file-actions", async (original) => ({
  ...(await original<object>()),
  archiveChatFile: vi.fn(),
  readTextPreview: vi.fn(),
}))
const file = {
  key: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  fileName: "notes.txt",
  contentType: "text/plain" as const,
  byteLength: 20,
}
it("opens text in a dialog instead of starting a download", async () => {
  vi.mocked(readTextPreview).mockResolvedValue({
    text: "Review these notes",
    truncated: false,
  })
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <FileActions file={file}>notes.txt</FileActions>
    </NextIntlClientProvider>
  )
  fireEvent.click(screen.getByRole("link", { name: "notes.txt" }))
  expect(await screen.findByText("Review these notes")).toBeVisible()
  expect(screen.getByRole("dialog")).toBeVisible()
  expect(screen.getByRole("link", { name: "Download" })).toHaveAttribute(
    "download",
    "notes.txt"
  )
})
it("trash can restore a file without fetching its bytes", async () => {
  vi.mocked(archiveChatFile).mockResolvedValue(undefined)
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <FileActions file={file} archived>
        notes.txt
      </FileActions>
    </NextIntlClientProvider>
  )
  fireEvent.click(screen.getByRole("button", { name: "Restore" }))
  await waitFor(() =>
    expect(archiveChatFile).toHaveBeenCalledWith(file.key, false, "csrf")
  )
  expect(readTextPreview).not.toHaveBeenCalled()
})

it("deletes from the open preview without downloading", async () => {
  vi.mocked(readTextPreview).mockResolvedValue({
    text: "Disposable note",
    truncated: false,
  })
  vi.mocked(archiveChatFile).mockResolvedValue(undefined)
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <FileActions file={file}>notes.txt</FileActions>
    </NextIntlClientProvider>
  )
  fireEvent.click(screen.getByRole("link", { name: "notes.txt" }))
  await screen.findByText("Disposable note")
  fireEvent.click(screen.getByRole("button", { name: "Delete" }))
  await waitFor(() =>
    expect(archiveChatFile).toHaveBeenCalledWith(file.key, true, "csrf")
  )
  await waitFor(() =>
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument()
  )
  expect(screen.getByRole("button", { name: "Restore" })).toBeVisible()
})
