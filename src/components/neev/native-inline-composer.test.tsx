import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import { useEffect, useRef, useState } from "react"
import { expect, it } from "vitest"
import { NextIntlClientProvider } from "next-intl"
import messages from "@/i18n/messages/en.json"
import { useAttachmentTray, type AttachmentTray } from "./native-attachments"
import {
  NativeInlineComposer,
  type InlineComposerHandle,
} from "./native-inline-composer"
import { typeInComposer } from "./test-helpers"
import {
  composeWirePrompt,
  parseSentPrompt,
  type PromptSegment,
} from "@/lib/neev-inline-prompt"

const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1])

function png(name: string): File {
  return new File([PNG.buffer.slice(0)], name, { type: "image/png" })
}

function stored(name: string, index: number) {
  return {
    key: `${String(index).repeat(8)}-aaaa-bbbb-cccc-dddddddddddd`,
    fileName: name,
    contentType: "image/png" as const,
    byteLength: PNG.length,
  }
}

let draft: PromptSegment[] = []
let names = new Map<string, string>()

function Harness({
  onComposer,
  onTray,
}: {
  onComposer: (handle: InlineComposerHandle) => void
  onTray: (tray: AttachmentTray) => void
}) {
  const tray = useAttachmentTray({
    upload: async (file) => stored(file.name, 1),
  })
  const [value, setValue] = useState<PromptSegment[]>([])
  const composer = useRef<InlineComposerHandle>(null)
  useEffect(() => {
    draft = value
    names = new Map(tray.items.map((item) => [item.id, item.name]))
    onTray(tray)
    if (composer.current) onComposer(composer.current)
  })
  return (
    <NextIntlClientProvider locale="en" messages={messages}>
      <NativeInlineComposer
        ref={composer}
        value={value}
        onChange={setValue}
        tray={tray}
        label="Message"
        placeholder="Ask anything"
        onSubmit={() => {}}
      />
    </NextIntlClientProvider>
  )
}

function mount() {
  let handle!: InlineComposerHandle
  let live!: AttachmentTray
  render(
    <Harness
      onComposer={(value) => {
        handle = value
      }}
      onTray={(value) => {
        live = value
      }}
    />
  )
  return {
    box: () => screen.findByRole("textbox", { name: "Message" }),
    tray: () => live,
    add: (files: File[]) => handle.addFiles(files),
  }
}

/**
 * Delete a reference the way a keystroke does: take its element out and tell
 * the editor its own DOM changed. jsdom has no caret to press Backspace with.
 */
async function deleteReference(box: HTMLElement) {
  const paragraph = box.querySelector("p")!
  // The editor's own element for the node, not React's content inside it.
  const chip = box.querySelector("span.node-neevAttachment")!
  await act(async () => {
    chip.remove()
    fireEvent.input(paragraph, { inputType: "deleteContentBackward" })
  })
}

async function undo(box: HTMLElement) {
  await act(async () => {
    fireEvent.keyDown(box, { key: "z", ctrlKey: true })
  })
}

it("keeps words before, between and after the files they are about", async () => {
  const { box, add } = mount()
  const editor = await box()
  await typeInComposer(editor, "compare ")
  await act(async () => {
    await add([png("before.png")])
  })
  await typeInComposer(editor, "with")
  await act(async () => {
    await add([png("after.png")])
  })
  await typeInComposer(editor, "please")
  await screen.findByRole("button", { name: "Remove after.png" })

  // Numbered in the order they appear in the sentence, and the ids come back in
  // that same order — which is the order the keys are sent and stored in.
  const { prompt, ids } = composeWirePrompt(draft, names)
  expect(prompt).toBe(
    "compare [attachment 1: before.png] with [attachment 2: after.png] please"
  )
  expect(ids).toHaveLength(2)
  expect(new Set(ids).size).toBe(2)

  // And the same sentence, read back from what the server stored, puts each
  // reference where it was written.
  const { parts, trailing } = parseSentPrompt(prompt, [
    stored("before.png", 1),
    stored("after.png", 2),
  ])
  expect(trailing).toEqual([])
  expect(parts).toEqual([
    { kind: "text", text: "compare " },
    { kind: "attachment", index: 0 },
    { kind: "text", text: " with " },
    { kind: "attachment", index: 1 },
    { kind: "text", text: " please" },
  ])
})

it("undoes a deleted reference back into the file it named", async () => {
  const { box, add } = mount()
  const editor = await box()
  await typeInComposer(editor, "look at ")
  await act(async () => {
    await add([png("shot.png")])
  })
  await screen.findByRole("button", { name: "Remove shot.png" })
  // Typed after the file, so deleting the file is its own step in the history
  // rather than part of the one that put it there.
  await typeInComposer(editor, "now")
  expect(composeWirePrompt(draft, names).prompt).toBe(
    "look at [attachment 1: shot.png] now"
  )

  // Deleting the reference releases the file: nothing staged is left behind for
  // a later message to carry.
  await deleteReference(editor)
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: "Remove shot.png" })).toBeNull()
  )
  expect(composeWirePrompt(draft, names).ids).toEqual([])

  // Undo restores the words AND the file. The reference comes back attached to
  // the bytes it named, so sending now sends the file rather than a mention of
  // one — which is the whole reason a deletion is undoable at all.
  await undo(editor)
  await screen.findByRole("button", { name: "Remove shot.png" })
  await waitFor(() =>
    expect(composeWirePrompt(draft, names).prompt).toBe(
      "look at [attachment 1: shot.png] now"
    )
  )
  expect(names.size).toBe(1)
})

it("forgets a released file once the tray is cleared", async () => {
  const { box, add, tray } = mount()
  const editor = await box()
  await act(async () => {
    await add([png("shot.png")])
  })
  await screen.findByRole("button", { name: "Remove shot.png" })
  await deleteReference(editor)
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: "Remove shot.png" })).toBeNull()
  )

  // Sending clears the tray: those files belong to the message now, and no undo
  // may drag one of them into the next one.
  await act(async () => {
    tray().clear()
  })
  await undo(editor)
  await waitFor(() =>
    expect(draft.some((segment) => segment.kind === "attachment")).toBe(false)
  )
  expect(screen.queryByRole("button", { name: "Remove shot.png" })).toBeNull()
})

it("pastes markup as the text it reads as, never as a file it does not have", async () => {
  const { box } = mount()
  const editor = await box()
  // Markup shaped exactly like this schema's attachment node, carrying an id
  // that belongs to no staged file.
  const html =
    '<p>see <span data-attachment-id="attachment-999">secret.png</span></p>'
  await act(async () => {
    fireEvent.paste(editor, {
      clipboardData: {
        types: ["text/plain", "text/html"],
        files: [],
        items: [],
        getData: (type: string) => (type === "text/html" ? html : "see"),
      },
    })
  })
  expect(draft.some((segment) => segment.kind === "attachment")).toBe(false)
  expect(composeWirePrompt(draft, names)).toEqual({ prompt: "see", ids: [] })
})
