import { act, render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { expect, it, vi } from "vitest"
import { NextIntlClientProvider } from "next-intl"
import messages from "@/i18n/messages/en.json"
import {
  AttachmentTrayView,
  SentAttachments,
  useAttachmentTray,
  type AttachmentTray,
  type AttachmentUploader,
} from "./native-attachments"
import { NEEV_MAX_ATTACHMENT_BYTES } from "@/lib/neev-attachments"
import { NativeClientError } from "@/lib/neev-client"

const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1])

function png(name: string): File {
  return new File([PNG.buffer.slice(0)], name, { type: "image/png" })
}

function key(index: number): string {
  return `${String(index).repeat(8)}-aaaa-bbbb-cccc-dddddddddddd`
}

function stored(name: string, index: number) {
  return {
    key: key(index),
    fileName: name,
    contentType: "image/png" as const,
    byteLength: PNG.length,
  }
}

/** Renders the real tray and hands the test its live handle. */
function Harness({
  upload,
  onTray,
}: {
  upload: AttachmentUploader | null
  onTray: (tray: AttachmentTray) => void
}) {
  const tray = useAttachmentTray({ upload })
  onTray(tray)
  return (
    <NextIntlClientProvider locale="en" messages={messages}>
      <AttachmentTrayView tray={tray} />
    </NextIntlClientProvider>
  )
}

function mount(upload: AttachmentUploader | null) {
  let tray!: AttachmentTray
  const view = render(
    <Harness
      upload={upload}
      onTray={(value) => {
        tray = value
      }}
    />
  )
  return { view, tray: () => tray }
}

/** An uploader the test resolves by hand, one call at a time. */
function deferred() {
  const calls: {
    file: File
    signal?: AbortSignal
    resolve: (index: number) => void
    reject: (cause: unknown) => void
  }[] = []
  const upload: AttachmentUploader = (file, signal) =>
    new Promise((resolve, reject) => {
      calls.push({
        file,
        signal,
        resolve: (index) => resolve(stored(file.name, index)),
        reject,
      })
    })
  return { calls, upload }
}

it("uploads a staged file once and hands back its key", async () => {
  const upload = vi.fn(async (file: File) => stored(file.name, 1))
  const { tray } = mount(upload)
  await act(async () => {
    await tray().add([png("shot.png")])
  })
  expect(upload).toHaveBeenCalledTimes(1)
  await waitFor(() => expect(tray().uploading).toBe(false))
  expect(await tray().takeKeys()).toEqual([key(1)])
  // A second send of the same staged file must not upload the bytes again.
  expect(await tray().takeKeys()).toEqual([key(1)])
  expect(upload).toHaveBeenCalledTimes(1)
})

it("never turns an upload that finished after removal into a key", async () => {
  const { calls, upload } = deferred()
  const { tray } = mount(upload)
  let staged!: Promise<void>
  act(() => {
    staged = tray().add([png("shot.png")])
  })
  await waitFor(() => expect(calls).toHaveLength(1))
  const id = tray().items[0].id
  act(() => tray().remove(id))
  // The server answers anyway — the request was already on its way.
  await act(async () => {
    calls[0].resolve(1)
    await staged
  })
  expect(calls[0].signal?.aborted).toBe(true)
  expect(tray().items).toHaveLength(0)
  expect(await tray().takeKeys()).toEqual([])
})

it("abandons an upload when the composer goes away", async () => {
  const { calls, upload } = deferred()
  const { view, tray } = mount(upload)
  act(() => void tray().add([png("shot.png")]))
  await waitFor(() => expect(calls).toHaveLength(1))
  view.unmount()
  await act(async () => {
    calls[0].resolve(1)
  })
  // Aborted, so nothing can carry this key into whatever is open now.
  expect(calls[0].signal?.aborted).toBe(true)
})

it("counts overlapping drops against one shared limit", async () => {
  const upload = vi.fn(async (file: File) => stored(file.name, 1))
  const { tray } = mount(upload)
  // Two drops landing together: the second must see what the first staged.
  await act(async () => {
    const first = tray().add([png("a.png"), png("b.png"), png("c.png")])
    const second = tray().add([png("d.png"), png("e.png"), png("f.png")])
    await Promise.all([first, second])
  })
  expect(tray().items).toHaveLength(5)
  expect(tray().rejected).toEqual([{ name: "f.png", reason: "tooMany" }])
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "f.png was not attached: five files at most."
  )
})

it("says why a file was refused, naming it, and uploads nothing", async () => {
  const upload = vi.fn(async (file: File) => stored(file.name, 1))
  const { tray } = mount(upload)
  const big = png("huge.png")
  Object.defineProperty(big, "size", { value: NEEV_MAX_ATTACHMENT_BYTES + 1 })
  await act(async () => {
    await tray().add([big, new File([new Uint8Array([0, 1, 2])], "a.bin")])
  })
  expect(upload).not.toHaveBeenCalled()
  const alert = await screen.findByRole("alert")
  expect(alert).toHaveTextContent("huge.png is larger than 10 MB.")
  expect(alert).toHaveTextContent("a.bin isn't a file an agent can open")
})

it("keeps a failed upload visible, retries it, and blocks sending meanwhile", async () => {
  let attempt = 0
  const upload = vi.fn(async (file: File) => {
    attempt += 1
    if (attempt === 1) throw new Error("network")
    return stored(file.name, 2)
  })
  const { tray } = mount(upload)
  await act(async () => {
    await tray().add([png("shot.png")])
  })
  await waitFor(() => expect(tray().items[0].status).toBe("failed"))
  // Still staged, so the composer knows a send would drop it.
  expect(tray().blocked).toBe(true)
  const user = userEvent.setup()
  await user.click(screen.getByRole("button", { name: "Try again" }))
  await waitFor(() => expect(tray().items[0].status).toBe("uploaded"))
  expect(await tray().takeKeys()).toEqual([key(2)])
  expect(upload).toHaveBeenCalledTimes(2)
})

it("uploads at send time when there was nowhere to upload to", async () => {
  const { tray } = mount(null)
  await act(async () => {
    await tray().add([png("shot.png")])
  })
  expect(tray().items[0].status).toBe("queued")
  const late = vi.fn(async (file: File) => stored(file.name, 3))
  await act(async () => {
    expect(await tray().takeKeys(late)).toEqual([key(3)])
  })
  expect(late).toHaveBeenCalledTimes(1)
})

it("removes a staged file before it is sent", async () => {
  const upload = vi.fn(async (file: File) => stored(file.name, 1))
  const { tray } = mount(upload)
  await act(async () => {
    await tray().add([png("shot.png")])
  })
  const user = userEvent.setup()
  await user.click(screen.getByRole("button", { name: "Remove shot.png" }))
  expect(tray().items).toHaveLength(0)
  expect(await tray().takeKeys()).toEqual([])
})

it("drops a classification that finished after the chat changed", async () => {
  // The saved agent chat view is re-rendered rather than remounted, so `clear`
  // is what marks "different conversation now" — including for a drop still
  // being classified, and for one queued behind it.
  const upload = vi.fn(async (file: File) => stored(file.name, 1))
  const { tray } = mount(upload)
  let first!: Promise<void>
  let second!: Promise<void>
  act(() => {
    first = tray().add([png("a.png")])
    second = tray().add([png("b.png")])
    // Same tick as the drops: both are still inside admission.
    tray().clear()
  })
  await act(async () => {
    await Promise.all([first, second])
  })
  expect(tray().items).toEqual([])
  expect(tray().rejected).toEqual([])
  expect(upload).not.toHaveBeenCalled()
  // The tray still works for the conversation that is open now.
  await act(async () => {
    await tray().add([png("c.png")])
  })
  expect(tray().items.map((item) => item.name)).toEqual(["c.png"])
})

it("says the organization is out of storage rather than blaming the message", async () => {
  const upload = vi.fn(async () => {
    throw new NativeClientError("conflict", 409)
  })
  const { tray } = mount(upload)
  await act(async () => {
    await tray().add([png("shot.png")])
  })
  await waitFor(() => expect(tray().items[0].reason).toBe("quota"))
  expect(screen.getByText(/out of attachment storage/)).toBeVisible()
  expect(screen.queryByText(/25 MB/)).toBeNull()
})

it("caps the refusal list and counts the rest", async () => {
  const { tray } = mount(null)
  const junk = Array.from(
    { length: 8 },
    (_, index) => new File([new Uint8Array([0, 1, 2])], `bin-${index}.bin`)
  )
  await act(async () => {
    await tray().add(junk)
  })
  const alert = await screen.findByRole("alert")
  expect(within(alert).getAllByRole("listitem")).toHaveLength(6)
  expect(alert).toHaveTextContent("And 3 more.")
})

it("renders what was actually stored, by its key", () => {
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <SentAttachments
        attachments={[
          stored("diagram.png", 1),
          {
            key: key(2),
            fileName: "notes.txt",
            contentType: "text/plain",
            byteLength: 2048,
          },
        ]}
      />
    </NextIntlClientProvider>
  )
  expect(screen.getByRole("img", { name: "diagram.png" })).toHaveAttribute(
    "src",
    `/api/neev/attachments/${key(1)}`
  )
  const download = screen.getByRole("link", { name: /notes\.txt/ })
  expect(download).toHaveAttribute("href", `/api/neev/attachments/${key(2)}`)
  expect(download).toHaveAttribute("download", "notes.txt")
})
