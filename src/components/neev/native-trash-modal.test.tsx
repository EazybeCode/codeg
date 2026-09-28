import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { expect, it, vi, beforeEach } from "vitest"
import messages from "@/i18n/messages/en.json"
import { AttachmentTrash } from "./native-trash-modal"
import {
  listOrgArchivedAttachments,
  archiveChatFile,
} from "@/lib/neev-file-actions"

vi.mock("./native-auth-provider", () => ({
  useOptionalNativeAuth: () => ({ session: { csrf: "test-csrf" } }),
}))
vi.mock("@/lib/neev-file-actions", async (original) => ({
  ...(await original<object>()),
  listOrgArchivedAttachments: vi.fn(),
  archiveChatFile: vi.fn(),
}))

beforeEach(() => {
  vi.clearAllMocks()
})

const archivedFile = {
  key: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  fileName: "deleted.pdf",
  contentType: "application/pdf" as const,
  byteLength: 1024,
  parentId: "chat-123",
  parentKind: "chat" as const,
}

it("lists org-wide archived attachments via GET endpoint", async () => {
  vi.mocked(listOrgArchivedAttachments).mockResolvedValue([archivedFile])

  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <AttachmentTrash />
    </NextIntlClientProvider>
  )

  expect(
    await screen.findByText("deleted.pdf", {}, { timeout: 10000 })
  ).toBeVisible()
  expect(listOrgArchivedAttachments).toHaveBeenCalledWith(expect.any(Object))
}, 15000)

it("restores file through PATCH with CSRF and removes from list", async () => {
  vi.mocked(listOrgArchivedAttachments)
    .mockResolvedValueOnce([archivedFile])
    .mockResolvedValueOnce([]) // After restore, list is empty
  vi.mocked(archiveChatFile).mockResolvedValue(undefined)

  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <AttachmentTrash />
    </NextIntlClientProvider>
  )

  expect(await screen.findByText("deleted.pdf")).toBeVisible()

  // Click restore button
  fireEvent.click(screen.getByRole("button", { name: "Restore" }))

  await waitFor(() =>
    expect(archiveChatFile).toHaveBeenCalledWith(
      archivedFile.key,
      false,
      "test-csrf"
    )
  )

  // Trigger refresh by dispatching file change event
  fireEvent(window, new CustomEvent("neev-files-changed"))

  // File should be removed from list after restore
  await waitFor(() => {
    expect(screen.queryByText("deleted.pdf")).not.toBeInTheDocument()
  })
})

it("displays empty state when no deleted files", async () => {
  vi.mocked(listOrgArchivedAttachments).mockResolvedValue([])

  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <AttachmentTrash />
    </NextIntlClientProvider>
  )

  expect(await screen.findByText("No deleted files")).toBeVisible()
})
