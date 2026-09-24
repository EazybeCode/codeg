import { act, fireEvent, render, screen } from "@testing-library/react"
import { useEffect, useState } from "react"
import { expect, it, vi } from "vitest"
import { NextIntlClientProvider } from "next-intl"
import messages from "@/i18n/messages/en.json"
import { useAttachmentTray, type AttachmentTray } from "./native-attachments"
import { NativeInlineComposer } from "./native-inline-composer"
import { typeInComposer } from "./test-helpers"
import {
  composeWirePrompt,
  promptText,
  type PromptSegment,
} from "@/lib/neev-inline-prompt"
import { matchCandidates } from "@/lib/neev-mentions"
import type { NativeMember } from "@/lib/neev-client"

function member(id: number, displayName: string): NativeMember {
  return { id, role: "member", user: { id, displayName, avatarUrl: null } }
}

const DIRECTORY = [
  member(2, "Ana Duarte"),
  member(3, "Andrés Gil"),
  member(4, "Susan Ang"),
]

let draft: PromptSegment[] = []

function Harness({
  onSubmit,
  directory = DIRECTORY,
  offerNobody = false,
}: {
  onSubmit: () => void
  directory?: readonly NativeMember[]
  offerNobody?: boolean
}) {
  const tray: AttachmentTray = useAttachmentTray({
    upload: async () => {
      throw new Error("no uploads in this test")
    },
  })
  const [value, setValue] = useState<PromptSegment[]>([])
  useEffect(() => {
    draft = value
  })
  return (
    <NextIntlClientProvider locale="en" messages={messages}>
      <NativeInlineComposer
        value={value}
        onChange={setValue}
        tray={tray}
        label="Message"
        placeholder="Ask anything"
        onSubmit={onSubmit}
        mentions={
          offerNobody
            ? undefined
            : {
                candidates: (query) => matchCandidates(directory, query, 1),
              }
        }
      />
    </NextIntlClientProvider>
  )
}

function mount(options: Parameters<typeof Harness>[0]) {
  render(<Harness {...options} />)
  return screen.findByRole("textbox", { name: "Message" })
}

async function press(box: HTMLElement, key: string) {
  await act(async () => {
    fireEvent.keyDown(box, { key })
  })
}

it("offers coworkers for a query and names the one chosen with the keyboard", async () => {
  const submit = vi.fn()
  const box = await mount({ onSubmit: submit })
  await typeInComposer(box, "ping @an")

  const options = screen.getAllByRole("option")
  // Name prefix before a word start: Ana and Andrés before Susan Ang.
  expect(options.map((option) => option.textContent)).toEqual([
    "Ana Duarte",
    "Andrés Gil",
    "Susan Ang",
  ])
  expect(box.getAttribute("aria-expanded")).toBe("true")

  await press(box, "ArrowDown")
  expect(screen.getAllByRole("option")[1].getAttribute("aria-selected")).toBe(
    "true"
  )

  // Enter CHOOSES. The one thing this must never do is send a half-typed name.
  await press(box, "Enter")
  expect(submit).not.toHaveBeenCalled()
  expect(screen.queryAllByRole("option")).toHaveLength(0)

  expect(draft).toEqual([
    { kind: "text", text: "ping " },
    { kind: "mention", userId: 3, name: "Andrés Gil" },
    { kind: "text", text: " " },
  ])
  // The span says which characters are the person, counted the way the server
  // counts them — "Andrés" has a multi-byte character in it on purpose.
  expect(composeWirePrompt(draft, new Map())).toEqual({
    prompt: "ping @Andrés Gil",
    ids: [],
    mentions: [{ userId: 3, start: 5, length: 11 }],
  })
  const { prompt, mentions } = composeWirePrompt(draft, new Map())
  expect(
    [...prompt].slice(mentions[0].start, mentions[0].start + mentions[0].length).join("")
  ).toBe("@Andrés Gil")
})

it("gives Enter back to Send as soon as the picker is dismissed", async () => {
  const submit = vi.fn()
  const box = await mount({ onSubmit: submit })
  await typeInComposer(box, "hello @an")
  expect(screen.getAllByRole("option").length).toBeGreaterThan(0)

  await press(box, "Escape")
  expect(screen.queryAllByRole("option")).toHaveLength(0)
  expect(submit).not.toHaveBeenCalled()

  await press(box, "Enter")
  expect(submit).toHaveBeenCalledTimes(1)
  // And the text stays exactly what was typed: nothing was turned into anybody.
  expect(promptText(draft)).toBe("hello @an")
  expect(draft.some((segment) => segment.kind === "mention")).toBe(false)
})

it("leaves an @ that is not asking for anyone alone", async () => {
  const box = await mount({ onSubmit: () => {} })
  // Mid-word: an address, a handle being quoted, a path — not a question.
  await typeInComposer(box, "write to ana@example.com")
  expect(screen.queryAllByRole("option")).toHaveLength(0)
  expect(box.getAttribute("aria-expanded")).toBe("false")
  expect(promptText(draft)).toBe("write to ana@example.com")
})

it("never opens a picker where there is nobody to name", async () => {
  const submit = vi.fn()
  const box = await mount({ onSubmit: submit, offerNobody: true })
  await typeInComposer(box, "note @an")

  expect(screen.queryAllByRole("option")).toHaveLength(0)
  // Not a combobox at all: a private conversation has no directory to claim.
  expect(box.getAttribute("aria-expanded")).toBeNull()
  await press(box, "Enter")
  expect(submit).toHaveBeenCalledTimes(1)
  expect(composeWirePrompt(draft, new Map()).mentions).toEqual([])
})

it("keeps an IME's keys for the IME", async () => {
  const submit = vi.fn()
  const box = await mount({ onSubmit: submit })
  await typeInComposer(box, "hi @an")
  expect(screen.getAllByRole("option").length).toBeGreaterThan(0)

  // A candidate window is driven by exactly these keys; taking Enter here
  // would choose a coworker while somebody is choosing a character.
  await act(async () => {
    fireEvent.keyDown(box, { key: "Enter", isComposing: true, keyCode: 229 })
  })
  // Neither outcome Enter normally has: nothing was sent, and nobody was
  // named. The keystroke goes back to the editor, which does what it does
  // during composition today — it makes a line.
  expect(submit).not.toHaveBeenCalled()
  expect(draft.some((segment) => segment.kind === "mention")).toBe(false)
})
