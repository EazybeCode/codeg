import { act, render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { useEffect, useRef, useState } from "react"
import { expect, it, vi } from "vitest"
import { NextIntlClientProvider } from "next-intl"
import messages from "@/i18n/messages/en.json"
import {
  SentAttachments,
  useAttachmentTray,
  type AttachmentTray,
  type AttachmentUploader,
} from "./native-attachments"
import {
  NativeInlineComposer,
  type InlineComposerHandle,
} from "./native-inline-composer"
import type { PromptSegment } from "@/lib/neev-inline-prompt"
import { NEEV_MAX_PREVIEW_BYTES } from "@/lib/neev-attachments"
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

/**
 * The real tray, bound to the real editor, with the test holding both handles.
 *
 * Staging goes through the composer rather than the tray directly, because
 * that is the only path a person has: a file is admitted AND a node for it is
 * put in the sentence, or neither happens.
 */
function Harness({
  upload,
  onTray,
  onComposer,
}: {
  upload: AttachmentUploader | null
  onTray: (tray: AttachmentTray) => void
  onComposer: (handle: InlineComposerHandle | null) => void
}) {
  const tray = useAttachmentTray({ upload })
  const [draft, setDraft] = useState<PromptSegment[]>([])
  const composer = useRef<InlineComposerHandle>(null)
  useEffect(() => {
    onTray(tray)
    onComposer(composer.current)
  })
  return (
    <NextIntlClientProvider locale="en" messages={messages}>
      <NativeInlineComposer
        ref={composer}
        value={draft}
        onChange={setDraft}
        tray={tray}
        label="Message"
        placeholder="Message"
        onSubmit={() => {}}
      />
    </NextIntlClientProvider>
  )
}

function mount(upload: AttachmentUploader | null) {
  let tray!: AttachmentTray
  let composer: InlineComposerHandle | null = null
  const view = render(
    <Harness
      upload={upload}
      onTray={(value) => {
        tray = value
      }}
      onComposer={(value) => {
        if (value) composer = value
      }}
    />
  )
  return {
    view,
    tray: () => tray,
    add: (files: readonly File[]) =>
      (composer as InlineComposerHandle | null)?.addFiles(files) ??
      Promise.resolve(),
  }
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
  const { tray, add } = mount(upload)
  await act(async () => {
    await add([png("shot.png")])
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
  const { tray, add } = mount(upload)
  let staged!: Promise<void>
  act(() => {
    staged = add([png("shot.png")])
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
  const { view, add } = mount(upload)
  act(() => void add([png("shot.png")]))
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
  const { tray, add } = mount(upload)
  // Two drops landing together: the second must see what the first staged.
  await act(async () => {
    const first = add([png("a.png"), png("b.png"), png("c.png")])
    const second = add([png("d.png"), png("e.png"), png("f.png")])
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
  const { add } = mount(upload)
  await act(async () => {
    await add([new File([new Uint8Array([0, 1, 2])], "a.bin")])
  })
  expect(upload).not.toHaveBeenCalled()
  const alert = await screen.findByRole("alert")
  expect(alert).toHaveTextContent("a.bin isn't a file an agent can open")
})

it("keeps a failed upload visible, retries it, and blocks sending meanwhile", async () => {
  let attempt = 0
  const upload = vi.fn(async (file: File) => {
    attempt += 1
    if (attempt === 1) throw new Error("network")
    return stored(file.name, 2)
  })
  const { tray, add } = mount(upload)
  await act(async () => {
    await add([png("shot.png")])
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
  const { tray, add } = mount(null)
  await act(async () => {
    await add([png("shot.png")])
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
  const { tray, add } = mount(upload)
  await act(async () => {
    await add([png("shot.png")])
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
  const { tray, add } = mount(upload)
  let first!: Promise<void>
  let second!: Promise<void>
  act(() => {
    first = add([png("a.png")])
    second = add([png("b.png")])
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
    await add([png("c.png")])
  })
  expect(tray().items.map((item) => item.name)).toEqual(["c.png"])
})

it("says the organization is out of storage rather than blaming the message", async () => {
  const upload = vi.fn(async () => {
    throw new NativeClientError("limited", 429, "content_quota")
  })
  const { tray, add } = mount(upload)
  await act(async () => {
    await add([png("shot.png")])
  })
  await waitFor(() => expect(tray().items[0].reason).toBe("quota"))
  expect(screen.getByText(/out of attachment storage/)).toBeVisible()
  expect(screen.queryByText(/25 MB/)).toBeNull()
})

it("caps the refusal list and counts the rest", async () => {
  const { add } = mount(null)
  const junk = Array.from(
    { length: 8 },
    (_, index) => new File([new Uint8Array([0, 1, 2])], `bin-${index}.bin`)
  )
  await act(async () => {
    await add(junk)
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
  // Every attachment is the same compact reference now, image or not: named,
  // downloadable, and addressed by the key the server gave it.
  const image = screen.getByRole("link", { name: /diagram\.png/ })
  expect(image).toHaveAttribute("href", `/api/neev/attachments/${key(1)}`)
  expect(image).toHaveAttribute("download", "diagram.png")
  // The thumbnail rides inside that reference and is decorative — the name is
  // already on the row, so announcing the file twice would only make it longer
  // to hear.
  expect(
    within(image).getByRole("presentation", { hidden: true })
  ).toHaveAttribute("src", `/api/neev/attachments/${key(1)}`)
  const download = screen.getByRole("link", { name: /notes\.txt/ })
  expect(download).toHaveAttribute("href", `/api/neev/attachments/${key(2)}`)
  expect(download).toHaveAttribute("download", "notes.txt")
})

it("previews a staged image as data:, never as a blob: the CSP blocks", async () => {
  // The regression this exists for: previews were object URLs, and the app is
  // served under `img-src 'self' data:` (neev/http/static_files.rs), so every
  // one of them was blocked. The element still "loaded" — complete, and
  // naturalWidth 0 — so it failed as a broken image with nothing on screen
  // saying why. A data: URL is inside the policy.
  const created = vi.spyOn(URL, "createObjectURL")
  const { add } = mount(async (file) => stored(file.name, 1))
  await act(async () => {
    await add([png("shot.png")])
  })
  const thumb = await waitFor(() => {
    const found = document.querySelector("img")
    expect(found?.getAttribute("src")).toMatch(/^data:image\/png;base64,/)
    return found!
  })
  expect(thumb.getAttribute("src")).not.toMatch(/^blob:/)
  expect(created).not.toHaveBeenCalled()
  created.mockRestore()
})

it("stages a large image without a preview, and still sends it", async () => {
  // Over the preview bound, so no data: URL is built — encoding megabytes on
  // the main thread would cost more than the thumbnail is worth. The file is
  // unaffected: staged, uploaded, and sent exactly as a small one is.
  // The sniffed type has to stay honest: the first bytes decide, not the name.
  const bytes = new Uint8Array(NEEV_MAX_PREVIEW_BYTES + 1024)
  bytes.set(PNG, 0)
  const file = new File([bytes.buffer], "huge.png", { type: "image/png" })
  const upload = vi.fn(async (f: File) => stored(f.name, 1))
  const { tray, add } = mount(upload)
  await act(async () => {
    await add([file])
  })
  await waitFor(() => expect(tray().uploading).toBe(false))
  expect(tray().items[0].previewUrl).toBeNull()
  expect(document.querySelector("img")).toBeNull()
  expect(screen.getByText("huge.png")).toBeInTheDocument()
  expect(await tray().takeKeys()).toEqual([key(1)])
})

it("says a preview failed instead of quietly drawing a file icon", async () => {
  // A stored image whose bytes will not decode. Falling back to the plain file
  // icon here would make a real failure look like an ordinary attachment with
  // no thumbnail — which is exactly how the CSP breakage stayed invisible.
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <SentAttachments attachments={[stored("broken.png", 1)]} />
    </NextIntlClientProvider>
  )
  const thumb = document.querySelector("img")!
  await act(async () => {
    thumb.dispatchEvent(new Event("error"))
  })
  expect(
    await screen.findByText("broken.png could not be previewed.")
  ).toBeInTheDocument()
  // Still named, still a working link: the file WAS sent and the agent read it.
  expect(screen.getByRole("link", { name: /broken\.png/ })).toHaveAttribute(
    "href",
    `/api/neev/attachments/${key(1)}`
  )
})
