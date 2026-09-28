import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"
import { FileActions } from "./native-file-actions"
import {
  archiveChatFile,
  readTextPreview,
  readHtmlPreview,
} from "@/lib/neev-file-actions"
vi.mock("./native-auth-provider", () => ({
  useOptionalNativeAuth: () => ({ session: { csrf: "csrf" } }),
}))
vi.mock("@/lib/neev-file-actions", async (original) => ({
  ...(await original<object>()),
  archiveChatFile: vi.fn(),
  readTextPreview: vi.fn(),
  readHtmlPreview: vi.fn(),
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
  expect(
    await screen.findByText("Review these notes", {}, { timeout: 10000 })
  ).toBeVisible()
  expect(screen.getByRole("dialog")).toBeVisible()
  expect(screen.getByRole("link", { name: "Download" })).toHaveAttribute(
    "download",
    "notes.txt"
  )
}, 15000)
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
  expect(
    screen.queryByRole("link", { name: "notes.txt" })
  ).not.toBeInTheDocument()
  expect(
    screen.queryByRole("button", { name: "Restore" })
  ).not.toBeInTheDocument()
})
it("deleted chip hidden in chat context without archived prop", async () => {
  vi.mocked(archiveChatFile).mockResolvedValue(undefined)
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <FileActions file={file} archived={false}>
        deleted.txt
      </FileActions>
    </NextIntlClientProvider>
  )

  // Initially visible (not deleted)
  expect(screen.getByRole("link", { name: "deleted.txt" })).toBeVisible()

  // Simulate deletion via external event
  fireEvent(
    window,
    new CustomEvent("neev-files-changed", {
      detail: { key: file.key, archived: true },
    })
  )

  // After deletion, chip disappears (returns null) in chat context
  await waitFor(() => {
    expect(
      screen.queryByRole("link", { name: "deleted.txt" })
    ).not.toBeInTheDocument()
  })
})

it("renders HTML preview in sandboxed iframe with empty sandbox", async () => {
  const htmlFile = { ...file, fileName: "page.html" }
  vi.mocked(readHtmlPreview).mockResolvedValue({
    html: "<html><body>Test content</body></html>",
    truncated: false,
  })
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <FileActions file={htmlFile}>page.html</FileActions>
    </NextIntlClientProvider>
  )
  fireEvent.click(screen.getByRole("link", { name: "page.html" }))

  // Wait for dialog to open
  expect(await screen.findByRole("dialog")).toBeVisible()

  // Check for static preview message
  expect(screen.getByText(/Static preview/i)).toBeVisible()

  // Find iframe by tag since title may be set differently
  const iframe = screen.getByRole("dialog").querySelector("iframe")
  expect(iframe).toBeTruthy()
  expect(iframe).toHaveAttribute("sandbox", "")
  expect(iframe).toHaveAttribute("srcdoc")
}, 10000)

it("keeps an in-flight preview across chat refreshes", async () => {
  const htmlFile = { ...file, fileName: "page.html" }
  let finish!: (value: { html: string; truncated: boolean }) => void
  vi.mocked(readHtmlPreview).mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve
      })
  )
  const view = (attachment: typeof htmlFile) => (
    <NextIntlClientProvider locale="en" messages={messages}>
      <FileActions file={attachment}>page.html</FileActions>
    </NextIntlClientProvider>
  )
  const { rerender } = render(view(htmlFile))
  fireEvent.click(screen.getByRole("link", { name: "page.html" }))
  await screen.findByRole("dialog")
  const signal = vi.mocked(readHtmlPreview).mock.calls[0][1]
  rerender(view({ ...htmlFile }))
  expect(signal?.aborted).toBe(false)
  expect(readHtmlPreview).toHaveBeenCalledTimes(1)
  finish({ html: "<p>Ready</p>", truncated: false })
  await waitFor(() =>
    expect(screen.getByRole("dialog").querySelector("iframe")).toHaveAttribute(
      "srcdoc",
      "<p>Ready</p>"
    )
  )
})

it("offers retry after a stalled HTML preview fails", async () => {
  vi.mocked(readHtmlPreview)
    .mockRejectedValueOnce(new Error("Stalled"))
    .mockResolvedValueOnce({ html: "<p>Recovered</p>", truncated: false })
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <FileActions file={{ ...file, fileName: "page.html" }}>
        page.html
      </FileActions>
    </NextIntlClientProvider>
  )
  fireEvent.click(screen.getByRole("link", { name: "page.html" }))
  await screen.findByRole("alert")
  fireEvent.click(screen.getByRole("button", { name: "Try again" }))
  await waitFor(() =>
    expect(screen.getByRole("dialog").querySelector("iframe")).toHaveAttribute(
      "srcdoc",
      "<p>Recovered</p>"
    )
  )
})

it("shows received bytes while a large HTML download is pending", async () => {
  vi.mocked(readHtmlPreview).mockImplementation((_file, _signal, progress) => {
    progress?.(1024 * 1024)
    return new Promise(() => {})
  })
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <FileActions
        file={{ ...file, fileName: "page.html", byteLength: 2 * 1024 * 1024 }}
      >
        page.html
      </FileActions>
    </NextIntlClientProvider>
  )
  fireEvent.click(screen.getByRole("link", { name: "page.html" }))
  expect(await screen.findByRole("status")).toHaveTextContent("1.0 / 2.0 MB")
  expect(screen.getByRole("progressbar")).toHaveAttribute("value", "1048576")
})
