import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import { BlockNoteEditor } from "@blocknote/core"
import type { Editor } from "@tiptap/core"
import { undoDepth } from "@tiptap/pm/history"
import { StrictMode } from "react"
import { describe, expect, it, vi } from "vitest"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { ThemeProvider, useTheme } from "next-themes"
import messages from "@/i18n/messages/en.json"
import {
  choosePaste,
  NativeRichEditor,
  refusesLineBreak,
  richSchema,
} from "./native-rich-editor"
import { NativeRichView } from "./native-rich-view"
import { NativeSuspendedSurface } from "./native-suspended-surface"
import { fromNeev, importMarkdown, toNeev } from "@/lib/neev-rich-adapter"
import { parseRichDocument } from "@/lib/neev-rich-document"
document.elementsFromPoint ??= () => []

function NativeTestRoot({ children }: { children: React.ReactNode }) {
  return (
    <NextIntlClientProvider locale="en" messages={messages}>
      {children}
    </NextIntlClientProvider>
  )
}

function ThemeControl() {
  const { setTheme } = useTheme()
  return <button onClick={() => setTheme("light")}>Use light theme</button>
}

it("does not publish a private debug handle or clobber another editor's global", async () => {
  const other = BlockNoteEditor.create({
    initialContent: [{ type: "paragraph", content: "Other editor" }],
  })
  const browser = window as Window & { ProseMirror?: Editor }
  const previous = browser.ProseMirror
  browser.ProseMirror = other._tiptapEditor
  const mounted = render(
    <NativeTestRoot>
      <NativeRichEditor
        document={importMarkdown("Private draft")}
        readOnly={false}
        onChange={() => {}}
      />
    </NativeTestRoot>
  )
  try {
    await screen.findByRole("textbox", { name: "Document content" })
    expect(browser.ProseMirror === other._tiptapEditor).toBe(true)
    mounted.unmount()
    await act(async () => {})
    expect(browser.ProseMirror === other._tiptapEditor).toBe(true)
    expect(other._tiptapEditor.state.doc.textContent).toBe("Other editor")
  } finally {
    mounted.unmount()
    if (previous) browser.ProseMirror = previous
    else delete browser.ProseMirror
    other._tiptapEditor.destroy()
  }
})

it.each([false, true])(
  "releases retained private state after real removal, but retains Activity drafts (StrictMode=%s)",
  async (strict) => {
    const changed = vi.fn()
    const content = (hidden: boolean) => {
      const surface = (
        <NativeTestRoot>
          <NativeSuspendedSurface hidden={hidden}>
            <NativeRichEditor
              document={importMarkdown("Initial private body")}
              readOnly={false}
              onChange={changed}
            />
          </NativeSuspendedSurface>
        </NativeTestRoot>
      )
      return strict ? <StrictMode>{surface}</StrictMode> : surface
    }
    const mounted = render(content(false))
    const textbox = await screen.findByRole("textbox", {
      name: "Document content",
    })
    const retained = (textbox as HTMLElement & { editor: Editor }).editor
    expect(retained.state.doc.textContent).toBe("Initial private body")
    await act(async () => {
      const paragraph = textbox.querySelector("p")!
      paragraph.textContent = "Unsaved private draft"
      fireEvent.input(paragraph, {
        inputType: "insertText",
        data: "Unsaved private draft",
      })
    })
    expect(retained.state.doc.textContent).toBe("Unsaved private draft")
    expect(undoDepth(retained.state)).toBeGreaterThan(0)
    await act(async () => mounted.rerender(content(true)))
    expect(textbox).not.toBeVisible()
    expect(retained.state.doc.textContent).toBe("Unsaved private draft")
    await act(async () => mounted.rerender(content(false)))
    const resumed = await screen.findByRole("textbox", {
      name: "Document content",
    })
    expect(
      (resumed as HTMLElement & { editor: Editor }).editor === retained
    ).toBe(true)
    expect(resumed).toHaveTextContent("Unsaved private draft")
    await act(async () => mounted.rerender(content(true)))
    changed.mockClear()
    mounted.unmount()
    await waitFor(() => expect(retained.state.doc.textContent).toBe(""))
    expect(retained.state.plugins).toHaveLength(0)
    expect(undoDepth(retained.state)).toBe(0)
    const retainedBlockNote = retained.state.schema.cached.blockNoteEditor
    expect(JSON.stringify(retainedBlockNote.document)).not.toContain("private")
    expect(
      String(JSON.stringify(retainedBlockNote.options.initialContent))
    ).not.toContain("private")
    expect(JSON.stringify(retained.options.content)).not.toContain(
      "Initial private body"
    )
    expect(JSON.stringify(retained.options.content)).not.toContain(
      "Unsaved private draft"
    )
    expect(
      (window as Window & { ProseMirror?: Editor }).ProseMirror === retained
    ).toBe(false)
    expect(changed).not.toHaveBeenCalled()
  }
)

it("matches the shell's resolved theme and keeps the same draft when theme changes", async () => {
  const onChange = vi.fn()
  const originalClass = document.documentElement.className
  const mounted = render(
    <ThemeProvider
      attribute="class"
      defaultTheme="dark"
      enableSystem={false}
      storageKey="neev-rich-theme-test"
    >
      <NativeTestRoot>
        <ThemeControl />
        <NativeRichEditor
          document={importMarkdown("Private draft")}
          readOnly={false}
          onChange={onChange}
        />
      </NativeTestRoot>
    </ThemeProvider>
  )
  try {
    const editor = await screen.findByRole("textbox", {
      name: "Document content",
    })
    expect(document.documentElement).toHaveClass("dark")
    expect(editor.closest(".bn-container")).toHaveAttribute(
      "data-color-scheme",
      "dark"
    )
    fireEvent.click(screen.getByRole("button", { name: "Use light theme" }))
    await waitFor(() =>
      expect(editor.closest(".bn-container")).toHaveAttribute(
        "data-color-scheme",
        "light"
      )
    )
    expect(screen.getByRole("textbox", { name: "Document content" })).toBe(
      editor
    )
    expect(editor).toHaveTextContent("Private draft")
    expect(onChange).not.toHaveBeenCalled()
  } finally {
    mounted.unmount()
    localStorage.removeItem("neev-rich-theme-test")
    document.documentElement.className = originalClass
  }
})

it("round-trips canonical content through the actual pinned editor schema", () => {
  const original = importMarkdown(
    "# Heading\n\n**Bold** and [link](https://example.test)\n\n- item\n\n```js\nx()\n```"
  )
  const editor = BlockNoteEditor.create({
    schema: richSchema,
    initialContent: fromNeev(original) as never,
    trailingBlock: false,
  })
  expect(toNeev(editor.document)).toEqual(original)
  // The schema stays pinned: a BlockNote default we deliberately exclude (audio)
  // is still rejected.
  expect(() =>
    editor.updateBlock(editor.document[0], { type: "audio" } as never)
  ).toThrow()
})

it("round-trips divider and internal-image blocks through the pinned schema", () => {
  const url = "/api/neev/images/11111111-1111-4111-8111-111111111111"
  const original = parseRichDocument({
    version: 1,
    blocks: [
      {
        type: "paragraph",
        id: "22222222-2222-4222-8222-222222222222",
        content: [{ text: "Above", styles: [], href: null }],
        children: [],
      },
      {
        type: "divider",
        id: "33333333-3333-4333-8333-333333333333",
        children: [],
      },
      {
        type: "image",
        id: "44444444-4444-4444-8444-444444444444",
        url,
        caption: "A chart",
        children: [],
      },
    ],
  })
  const editor = BlockNoteEditor.create({
    schema: richSchema,
    initialContent: fromNeev(original) as never,
    trailingBlock: false,
  })
  expect(toNeev(editor.document)).toEqual(original)
})

it("keeps a controlled-lifetime draft across prop updates and emits actual text edits", async () => {
  const changes = vi.fn()
  const original = importMarkdown("Seed")
  const mounted = render(
    <NativeTestRoot>
      <NativeRichEditor
        document={original}
        readOnly={false}
        onChange={changes}
      />
    </NativeTestRoot>
  )
  const editor = await screen.findByRole("textbox", {
    name: "Document content",
  })
  const paragraph = editor.querySelector("p")!
  await act(async () => {
    paragraph.textContent = "Draft"
    fireEvent.input(paragraph, { inputType: "insertText", data: "Draft" })
  })
  await waitFor(() => expect(changes).toHaveBeenCalled())
  expect(changes.mock.lastCall?.[0].blocks[0].content[0].text).toBe("Draft")
  mounted.rerender(
    <NativeTestRoot>
      <NativeRichEditor
        document={importMarkdown("External")}
        readOnly={false}
        onChange={changes}
      />
    </NativeTestRoot>
  )
  expect(editor).toHaveTextContent("Draft")
  mounted.rerender(
    <NativeTestRoot>
      <NativeRichEditor
        document={original}
        readOnly={true}
        onChange={changes}
      />
    </NativeTestRoot>
  )
  await waitFor(() =>
    expect(editor).toHaveAttribute("contenteditable", "false")
  )
})

it("falls back to plain text when HTML paste is unsupported, with a truthful notice", async () => {
  render(
    <NativeTestRoot>
      <NativeRichEditor
        document={importMarkdown("Keep me")}
        readOnly={false}
        onChange={() => {}}
      />
    </NativeTestRoot>
  )
  const editor = await screen.findByRole("textbox", {
    name: "Document content",
  })
  // HTML clipboard with a plain-text alternative: the HTML is dropped and the
  // plain text is inserted (formatting removed) — NOT silently refused.
  fireEvent.paste(editor, {
    clipboardData: {
      files: [],
      types: ["text/html", "text/plain"],
      getData: (type: string) =>
        type === "text/html"
          ? '<img src="https://example.test/a">'
          : "plain fallback text",
    },
  })
  // Truthful, non-error notice; the plain text is actually inserted.
  expect(await screen.findByRole("status")).toHaveTextContent(/plain text/i)
  expect(editor).toHaveTextContent("Keep me")
  expect(editor).toHaveTextContent("plain fallback text")
})

it("refuses a files-only paste with no usable text and keeps the draft", async () => {
  render(
    <NativeTestRoot>
      <NativeRichEditor
        document={importMarkdown("Keep me")}
        readOnly={false}
        onChange={() => {}}
      />
    </NativeTestRoot>
  )
  const editor = await screen.findByRole("textbox", {
    name: "Document content",
  })
  // A files-only clipboard has no plain-text alternative: insert nothing and say
  // so honestly (never claim text was kept).
  fireEvent.paste(editor, {
    clipboardData: {
      files: [{ name: "a.png" } as unknown as File],
      types: ["Files"],
      getData: () => "",
    },
  })
  expect(await screen.findByRole("alert")).toHaveTextContent(/paste/i)
  expect(editor).toHaveTextContent("Keep me")
})

it("renders rich history as React content without an editor or injected HTML", () => {
  const document = importMarkdown(
    "# Safe\n\n`<img src=x onerror=alert(1)>` [link](https://example.test)"
  )
  const { container } = render(<NativeRichView document={document} />)
  expect(screen.getByRole("heading", { name: "Safe" })).toBeVisible()
  expect(screen.getByRole("link")).toHaveAttribute("rel", "noreferrer")
  expect(container.querySelector("img")).toBeNull()
  expect(container.querySelector("[contenteditable]")).toBeNull()
})

it("refuses a resized/aligned image so formatting is never silently dropped", () => {
  const image = (extra: Record<string, unknown>) => [
    {
      id: "11111111-1111-4111-8111-111111111111",
      type: "image",
      props: {
        textColor: "default",
        backgroundColor: "default",
        textAlignment: "left",
        name: "",
        url: "/api/neev/images/22222222-2222-4222-8222-222222222222",
        caption: "C",
        showPreview: true,
        previewWidth: undefined,
        ...extra,
      },
      content: undefined,
      children: [],
    },
  ]
  // Default (un-resized) image round-trips.
  expect(toNeev(image({}))).toBeTruthy()
  // A user resize or alignment is a visible refusal, not a silent reset.
  expect(() => toNeev(image({ previewWidth: 200 }))).toThrow()
  expect(() => toNeev(image({ textAlignment: "center" }))).toThrow()
})

it("shows an image caption as a visible figcaption in read mode", () => {
  const document = parseRichDocument({
    version: 1,
    blocks: [
      {
        type: "image",
        id: "11111111-1111-4111-8111-111111111111",
        url: "/api/neev/images/22222222-2222-4222-8222-222222222222",
        caption: "My chart",
        children: [],
      },
    ],
  })
  render(<NativeRichView document={document} />)
  const caption = screen.getByText("My chart")
  expect(caption.tagName).toBe("FIGCAPTION")
  expect(screen.getByRole("img")).toHaveAttribute("alt", "My chart")
})

it("refuses externally supplied blocknote HTML drops before parsing attachments", async () => {
  render(
    <NativeTestRoot>
      <NativeRichEditor
        document={importMarkdown("Keep me")}
        readOnly={false}
        onChange={() => {}}
      />
    </NativeTestRoot>
  )
  const editor = await screen.findByRole("textbox", {
    name: "Document content",
  })
  const getData = vi.fn(() => '<img src="https://example.test/a">')
  fireEvent.drop(editor, {
    dataTransfer: { files: [], types: ["blocknote/html"], getData },
  })
  expect(await screen.findByRole("alert")).toHaveTextContent(/refused/i)
  expect(getData).not.toHaveBeenCalled()
  expect(editor).toHaveTextContent("Keep me")
})

it("renders consecutive numbered items as one semantic ordered list", () => {
  render(<NativeRichView document={importMarkdown("1. First\n2. Second")} />)
  expect(screen.getAllByRole("list")).toHaveLength(1)
  expect(screen.getAllByRole("listitem")).toHaveLength(2)
})

it("keeps rich tables and checklist state in an actual editor snapshot", () => {
  const document = {
    version: 1 as const,
    blocks: [
      {
        type: "table" as const,
        id: crypto.randomUUID(),
        children: [],
        rows: [
          [[{ text: "cell", styles: [], href: null }], []],
          [[], []],
        ],
      },
      {
        type: "checkListItem" as const,
        id: crypto.randomUUID(),
        children: [],
        checked: true,
        content: [{ text: "done", styles: [], href: null }],
      },
    ],
  }
  const editor = BlockNoteEditor.create({
    schema: richSchema,
    initialContent: fromNeev(document) as never,
    trailingBlock: false,
  })
  expect(toNeev(editor.document)).toEqual(document)
})

it("allows Enter to create a new canonical paragraph with its own ID", async () => {
  const changes = vi.fn()
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeRichEditor
        document={importMarkdown("Seed")}
        readOnly={false}
        onChange={changes}
      />
    </NativeTestRoot>
  )
  const editor = await screen.findByRole("textbox", {
    name: "Document content",
  })
  await user.click(editor.querySelector("p")!)
  const range = document.createRange()
  range.selectNodeContents(editor.querySelector("p")!)
  range.collapse(false)
  document.getSelection()!.removeAllRanges()
  document.getSelection()!.addRange(range)
  fireEvent.keyDown(editor, { key: "Enter", code: "Enter", keyCode: 13 })
  await waitFor(() => expect(changes.mock.lastCall?.[0].blocks).toHaveLength(2))
  expect(changes.mock.lastCall?.[0].blocks[1]).toMatchObject({
    type: "paragraph",
    id: expect.stringMatching(/^[0-9a-f-]{36}$/),
  })
  expect(screen.queryByRole("alert")).toBeNull()
})

it(
  "visibly refuses a paste whose combined document would exceed stored byte limits",
  { timeout: 30000 },
  async () => {
    const change = vi.fn()
    render(
      <NativeTestRoot>
        <NativeRichEditor
          document={importMarkdown("a".repeat(600_000))}
          readOnly={false}
          onChange={change}
        />
      </NativeTestRoot>
    )
    const editor = await screen.findByRole("textbox", {
      name: "Document content",
    })
    fireEvent.paste(editor, {
      clipboardData: {
        files: [],
        types: ["text/plain"],
        getData: () => "b".repeat(600_000),
      },
    })
    expect(editor.textContent?.length).toBe(600_000)
    expect(change).not.toHaveBeenCalled()
    expect(screen.queryByRole("alert")).not.toBeNull()
  }
)

it("clears a stale refusal once the next edit is accepted", async () => {
  // The notice describes the last action. Latching it meant one refused paste
  // left a red "editing refused" banner over a document that was taking every
  // keystroke perfectly well.
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeRichEditor
        document={importMarkdown("start")}
        readOnly={false}
        onChange={() => {}}
      />
    </NativeTestRoot>
  )
  const editor = await screen.findByRole("textbox", {
    name: "Document content",
  })
  // A files-only clipboard has nothing to insert, so it is refused.
  await act(async () => {
    fireEvent.paste(editor, {
      clipboardData: {
        files: [new File(["x"], "x.png", { type: "image/png" })],
        types: ["Files"],
        getData: () => "",
      },
    })
  })
  expect(await screen.findByRole("alert")).toHaveTextContent(/refused/i)
  await user.click(editor)
  await user.keyboard("ok")
  await waitFor(() => expect(screen.queryByRole("alert")).toBeNull())
})

describe("where a paste goes", () => {
  const cell = { plain: "", onlyPlain: true, where: "cell" as const }
  const body = { plain: "", onlyPlain: true, where: "body" as const }

  it("puts text in the cell the cursor is in, as inline content", () => {
    // A cursor inside a cell reports the TABLE as its block, so a block
    // inserted "after" it landed below the whole table. Inline content goes
    // where the cursor actually is, and is all a cell can hold.
    const action = choosePaste({ ...cell, plain: "PASTED" })
    expect(action.kind).toBe("inline")
    expect(action.notice).toBe("none")
    if (action.kind !== "inline") throw new Error("unreachable")
    // Inline runs rather than a bare string, so bold and links survive.
    expect(
      (action.content as { text: string }[]).map((run) => run.text).join("")
    ).toBe("PASTED")
  })

  it("flattens multi-line text into a cell and says the breaks were removed", () => {
    // A table run may not hold a line break (neev-rich-document forbids it, and
    // the stored model agrees), so refusing would lose the text and flattening
    // silently would change it without saying.
    expect(choosePaste({ ...cell, plain: "one\ntwo\nthree" })).toEqual({
      kind: "inline",
      content: "one two three",
      notice: "celllines",
    })
  })

  it("flattens a soft line break in a cell, which one Markdown paragraph can hold", () => {
    // "a\nb" is ONE paragraph with a break inside it, not two paragraphs, so
    // the single-paragraph path would have carried the newline into a cell that
    // cannot hold it and the guard would have refused the whole paste.
    expect(choosePaste({ ...cell, plain: "a\nb" })).toEqual({
      kind: "inline",
      content: "a b",
      notice: "celllines",
    })
    // A blank line makes them two paragraphs, which a cell also cannot hold.
    expect(choosePaste({ ...cell, plain: "a\n\nb" })).toEqual({
      kind: "inline",
      content: "a  b",
      notice: "celllines",
    })
    // Formatting on a single line still survives into the cell.
    const styled = choosePaste({ ...cell, plain: "**bold** text" })
    expect(styled.kind).toBe("inline")
    if (styled.kind !== "inline") throw new Error("unreachable")
    expect(
      (styled.content as { text: string; styles: Record<string, unknown> }[])[0]
    ).toMatchObject({ text: "bold", styles: { bold: true } })
  })

  it("keeps structure out of a cell rather than breaking out of the table", () => {
    const action = choosePaste({ ...cell, plain: "# Title\n\n- a\n- b" })
    expect(action.kind).toBe("inline")
    expect(action.notice).toBe("celllines")
  })

  it("keeps pasted structure when it lands somewhere that can hold it", () => {
    const action = choosePaste({ ...body, plain: "# Title\n\n- a\n- b" })
    expect(action.kind).toBe("blocks")
    expect(action.notice).toBe("none")
    if (action.kind !== "blocks") throw new Error("unreachable")
    expect(
      action.blocks.map((block) => (block as { type: string }).type)
    ).toEqual(["heading", "bulletListItem", "bulletListItem"])
  })

  it("says so when formatting was on offer and was not taken", () => {
    // text/plain taken from a clipboard that also held HTML: the text is kept,
    // its formatting is not, and the notice must not pretend otherwise.
    expect(
      choosePaste({ plain: "one line", onlyPlain: false, where: "body" }).notice
    ).toBe("plaintext")
    expect(
      choosePaste({ plain: "one line", onlyPlain: false, where: "cell" }).notice
    ).toBe("plaintext")
    // Nothing was dropped when plain text is all there was.
    expect(choosePaste({ ...body, plain: "one line" }).notice).toBe("none")
  })

  it("names the limit it hit instead of blaming the formatting", () => {
    expect(choosePaste({ ...body, plain: "a".repeat(1_048_577) })).toEqual({
      kind: "refuse",
      notice: "toolarge",
    })
  })

  it("refuses a clipboard with no text to take", () => {
    expect(choosePaste({ plain: "", onlyPlain: false, where: "body" })).toEqual(
      {
        kind: "refuse",
        notice: "unsupported",
      }
    )
  })
})

describe("where a paste must not break what it lands in", () => {
  const cell = { plain: "", onlyPlain: true, where: "cell" as const }
  const code = { plain: "", onlyPlain: true, where: "code" as const }
  const body = { plain: "", onlyPlain: true, where: "body" as const }

  it("sees a line break hiding inside a link, and flattens rather than refusing", () => {
    // A link is emitted as { type: "link", content: [text] }, so probing the
    // run's own `text` reads nothing and the break slips through into a cell
    // that cannot hold one — the guard then refuses the whole paste, which is
    // the outcome the check exists to prevent.
    const action = choosePaste({
      ...cell,
      plain: "[hello\nworld](https://example.com)",
    })
    expect(action).toEqual({
      kind: "inline",
      content: "[hello world](https://example.com)",
      notice: "celllines",
    })
  })

  it("flattens every separator a cell rejects, not only the familiar two", () => {
    // The model rejects U+2028 and U+2029 in a table run exactly as it rejects
    // LF and CR, so splitting on the familiar two alone left the paste to be
    // refused by the guard the flattening is meant to satisfy.
    expect(choosePaste({ ...cell, plain: "one\u2028two\u2029three" })).toEqual({
      kind: "inline",
      content: "one two three",
      notice: "celllines",
    })
    expect(choosePaste({ ...cell, plain: "a\r\nb\u2028c" })).toEqual({
      kind: "inline",
      content: "a b c",
      notice: "celllines",
    })
    // Outside a cell they are ordinary text and are left alone.
    expect(choosePaste({ ...code, plain: "one\u2028two" })).toEqual({
      kind: "inline",
      content: "one\u2028two",
      notice: "none",
    })
  })

  it("keeps multi-line text inside the code block it was pasted into", () => {
    // A code block is plain text and is NOT isolating, so a paragraph slice
    // splits out of it: the paste would land as paragraphs after the block and
    // quietly mangle the code. It holds its own line breaks, so the text goes
    // in as it came.
    expect(choosePaste({ ...code, plain: "one\ntwo\nthree" })).toEqual({
      kind: "inline",
      content: "one\ntwo\nthree",
      notice: "none",
    })
    // Structure is not interpreted inside code either.
    expect(
      choosePaste({ ...code, plain: "# not a heading\n- not a list" })
    ).toEqual({
      kind: "inline",
      content: "# not a heading\n- not a list",
      notice: "none",
    })
  })

  it("never replaces a selection with nothing", () => {
    // Markdown makes an empty paragraph of whitespace, and inserting empty
    // content deletes the selected text and puts nothing in its place.
    for (const where of ["body", "cell", "code"] as const) {
      const action = choosePaste({ plain: "   ", onlyPlain: true, where })
      expect(action.kind).toBe("inline")
      if (action.kind !== "inline") throw new Error("unreachable")
      expect(action.content).toBe("   ")
    }
    // A bare newline pastes a line break — which is what it is — rather than
    // reducing to empty content that would leave the selection deleted.
    const broken = choosePaste({ ...body, plain: "\n" })
    expect(broken.kind).toBe("blocks")
    if (broken.kind !== "blocks") throw new Error("unreachable")
    expect(broken.blocks).toHaveLength(2)
  })
})

describe("a line break where one cannot go", () => {
  it("refuses Shift+Enter in a table cell, and only there", () => {
    // The shortest path to the refusal people actually hit: a new table, a few
    // words in a cell, Shift+Enter. The stored model rejects the break, and
    // what they saw was a banner about unsupported formatting, files and HTML
    // — none of which they had touched.
    expect(
      refusesLineBreak({ key: "Enter", shiftKey: true, where: "cell" })
    ).toBe(true)
    // Everywhere else a soft break is ordinary.
    expect(
      refusesLineBreak({ key: "Enter", shiftKey: true, where: "body" })
    ).toBe(false)
    expect(
      refusesLineBreak({ key: "Enter", shiftKey: true, where: "code" })
    ).toBe(false)
    // A plain Enter in a cell moves between cells; it is not a line break.
    expect(
      refusesLineBreak({ key: "Enter", shiftKey: false, where: "cell" })
    ).toBe(false)
    expect(refusesLineBreak({ key: "a", shiftKey: true, where: "cell" })).toBe(
      false
    )
  })
})
