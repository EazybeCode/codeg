import { act, fireEvent, render, screen, waitFor } from "@testing-library/react"
import { BlockNoteEditor } from "@blocknote/core"
import type { Editor } from "@tiptap/core"
import { undoDepth } from "@tiptap/pm/history"
import { StrictMode } from "react"
import { expect, it, vi } from "vitest"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { ThemeProvider, useTheme } from "next-themes"
import messages from "@/i18n/messages/en.json"
import { NativeRichEditor, richSchema } from "./native-rich-editor"
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

it("visibly refuses a paste whose combined document would exceed stored byte limits", async () => {
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
})
