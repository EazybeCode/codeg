"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { useTheme } from "next-themes"
import {
  BlockNoteSchema,
  BlockNoteEditor,
  createCodeBlockSpec,
  createHeadingBlockSpec,
  defaultBlockSpecs,
  defaultInlineContentSpecs,
  defaultStyleSpecs,
  docToBlocks,
  type PartialBlock,
} from "@blocknote/core"
import {
  FormattingToolbar,
  FormattingToolbarController,
  BasicTextStyleButton,
  CreateLinkButton,
  SuggestionMenuController,
  SideMenuController,
  SideMenu,
  DragHandleButton,
  DragHandleMenu,
  RemoveBlockItem,
} from "@blocknote/react"
import { BlockNoteView } from "@blocknote/shadcn"
import { EditorState } from "@tiptap/pm/state"
import "@blocknote/shadcn/style.css"
import "./native-rich-editor.css"
import { fromNeev, importMarkdown, toNeev } from "@/lib/neev-rich-adapter"
import type { RichDocument } from "@/lib/neev-rich-document"

export const richSchema = BlockNoteSchema.create({
  blockSpecs: {
    paragraph: defaultBlockSpecs.paragraph,
    heading: createHeadingBlockSpec({
      levels: [1, 2, 3],
      allowToggleHeadings: false,
    }),
    bulletListItem: defaultBlockSpecs.bulletListItem,
    numberedListItem: defaultBlockSpecs.numberedListItem,
    checkListItem: defaultBlockSpecs.checkListItem,
    quote: defaultBlockSpecs.quote,
    codeBlock: createCodeBlockSpec({ defaultLanguage: "" }),
    table: defaultBlockSpecs.table,
    divider: defaultBlockSpecs.divider,
    image: defaultBlockSpecs.image,
  },
  inlineContentSpecs: {
    text: defaultInlineContentSpecs.text,
    link: defaultInlineContentSpecs.link,
  },
  styleSpecs: {
    bold: defaultStyleSpecs.bold,
    italic: defaultStyleSpecs.italic,
    strike: defaultStyleSpecs.strike,
    code: defaultStyleSpecs.code,
  },
})
export type NativeRichEditorProps = {
  document: RichDocument
  readOnly: boolean
  /**
   * Put the cursor here once, when the editor is ready.
   *
   * For a document that was just created: the writer asked for a document, so
   * they should be able to type. Only once, and only when asked — stealing
   * focus on every render, or in a tab that is not in front, would fight the
   * person using it.
   */
  autoFocus?: boolean
  onChange: (next: RichDocument) => void
  /** Upload an image bound to this document; resolves to its internal serve URL.
   *  Absent in read-only mode. */
  uploadImage?: (file: File) => Promise<string>
}

/** The parent owns load generations. Incoming props never overwrite this instance. */
export function NativeRichEditor({
  document,
  readOnly,
  autoFocus = false,
  onChange,
  uploadImage,
}: NativeRichEditorProps) {
  const t = useTranslations("NativeContent")
  const { resolvedTheme } = useTheme()
  // "unsupported" = paste/edit refused (nothing inserted); "plaintext" = rich
  // content fell back to a plain-text paste (formatting removed). Truthful copy
  // per state — never claim text was kept when nothing was inserted.
  const [notice, setNotice] = useState<"none" | "unsupported" | "plaintext">(
    "none"
  )
  const localDrag = useRef(false)
  const callback = useRef(onChange)
  const fileInput = useRef<HTMLInputElement>(null)
  // Keep the latest onChange without re-creating the editor; sync after render
  // (a ref must not be written during render).
  useEffect(() => {
    callback.current = onChange
  })
  const [editor] = useState(() => {
    // The upstream React creation hook publishes window.ProseMirror. Own creation
    // directly, and keep private initial content out of retained constructor options.
    const editor = BlockNoteEditor.create({
      schema: richSchema,
      trailingBlock: false,
      tables: {
        splitCells: false,
        cellBackgroundColor: false,
        cellTextColor: false,
        headers: false,
      },
      domAttributes: {
        editor: {
          role: "textbox",
          "aria-label": t("documentContent"),
          "aria-multiline": "true",
        },
      },
      pasteHandler: ({ event, editor }) => {
        event.preventDefault()
        const data = event.clipboardData
        const plain = data ? data.getData("text/plain") : ""
        const richOnly =
          !!data &&
          !data.files.length &&
          Array.from(data.types).every((type) => type === "text/plain")
        // First choice: parse text/plain as our restricted Markdown so pasted
        // structure (lists, headings) is preserved when it is representable.
        if (richOnly && plain) {
          try {
            const candidate = importMarkdown(plain)
            setNotice("none")
            editor.insertBlocks(
              fromNeev(candidate) as Parameters<typeof editor.insertBlocks>[0],
              editor.getTextCursorPosition().block,
              "after"
            )
            return true
          } catch {
            // Not representable as our Markdown — fall through to plain text.
          }
        }
        // Fallback: if there is ANY plain text, paste it as plain paragraphs
        // (formatting removed). Set the notice before inserting so the edit guard
        // can override to "unsupported" if the result would exceed doc limits.
        if (plain) {
          try {
            const paragraphs = plain
              .replace(/\r\n?/g, "\n")
              .split("\n")
              .map((line) => ({ type: "paragraph" as const, content: line }))
            setNotice("plaintext")
            editor.insertBlocks(
              paragraphs as Parameters<typeof editor.insertBlocks>[0],
              editor.getTextCursorPosition().block,
              "after"
            )
            return true
          } catch {
            setNotice("unsupported")
            return false
          }
        }
        // No usable text (files-only / empty clipboard): refuse, insert nothing.
        setNotice("unsupported")
        return false
      },
    })
    editor.replaceBlocks(
      editor.document,
      fromNeev(document) as Parameters<typeof editor.replaceBlocks>[1]
    )
    return editor
  })
  const lastEmitted = useRef(JSON.stringify(toNeev(editor.document)))
  // Mutable across renders by design → refs (not render-scoped `let`s, which the
  // React Compiler rule forbids reassigning after render). `observerRef` is the
  // single live MutationObserver; `releasedRef` latches the one-shot teardown.
  const observerRef = useRef<MutationObserver | null>(null)
  const releasedRef = useRef(false)
  const ownHost = useCallback(
    (host: HTMLDivElement | null) => {
      if (!host || observerRef.current || releasedRef.current) return
      // Activity/StrictMode detach refs and effects without removing the host.
      // Observe real removal, including an unmount while Activity is already hidden.
      const observer = new MutationObserver(() => {
        if (host.isConnected) return
        releasedRef.current = true
        observer.disconnect()
        observerRef.current = null
        const tiptap = editor._tiptapEditor
        const empty = EditorState.create({ schema: tiptap.state.schema })
        editor.unmount()
        // Destroy alone retains Tiptap's document and undo state. Replace both
        // without transactions/callbacks before releasing the owned editor.
        tiptap.view.updateState(empty)
        tiptap.setOptions({ content: empty.doc.toJSON() })
        editor.blockCache = new WeakMap()
        editor.elementRenderer = null
        tiptap.contentComponent = undefined
        tiptap.destroy()
        callback.current = () => {}
        lastEmitted.current = ""
      })
      observerRef.current = observer
      observer.observe(host.ownerDocument, { childList: true, subtree: true })
    },
    [editor]
  )
  useEffect(
    () =>
      editor.onBeforeChange(({ tr }) => {
        if (!tr.docChanged) return
        try {
          toNeev(docToBlocks(tr.doc))
        } catch {
          setNotice("unsupported")
          return false
        }
      }),
    [editor]
  )
  const focused = useRef(false)
  useEffect(() => {
    if (!autoFocus || readOnly || focused.current) return
    focused.current = true
    // After the editor has mounted its view; focusing before that is a no-op.
    const timer = window.setTimeout(() => {
      try {
        editor.focus()
      } catch {
        // A view that went away before this ran needs no cursor.
      }
    }, 0)
    return () => window.clearTimeout(timer)
  }, [autoFocus, readOnly, editor])

  const insertImage = async (file: File) => {
    if (!uploadImage) return
    try {
      const url = await uploadImage(file)
      editor.insertBlocks(
        [{ type: "image", props: { url } }],
        editor.getTextCursorPosition().block,
        "after"
      )
    } catch {
      setNotice("unsupported")
    }
  }
  const choices: {
    title: string
    block?: PartialBlock
    action?: () => void
  }[] = [
    { title: t("paragraphBlock"), block: { type: "paragraph" } },
    ...([1, 2, 3] as const).map((level) => ({
      title: t("headingBlock", { level }),
      block: { type: "heading" as const, props: { level } },
    })),
    { title: t("bulletBlock"), block: { type: "bulletListItem" } },
    { title: t("numberedBlock"), block: { type: "numberedListItem" } },
    { title: t("checkBlock"), block: { type: "checkListItem" } },
    { title: t("quoteBlock"), block: { type: "quote" } },
    { title: t("codeBlock"), block: { type: "codeBlock" } },
    {
      title: t("tableBlock"),
      block: {
        type: "table",
        content: {
          type: "tableContent",
          rows: Array.from({ length: 3 }, () => ({ cells: [[], [], []] })),
        },
      },
    },
    { title: t("dividerBlock"), block: { type: "divider" } },
    ...(uploadImage
      ? [
          {
            title: t("imageBlock"),
            action: () => fileInput.current?.click(),
          },
        ]
      : []),
  ]
  return (
    <div
      ref={ownHost}
      className="neev-writing-canvas min-w-0 bg-background py-4"
    >
      {notice === "unsupported" && (
        <p role="alert" className="px-4 pb-3 text-sm text-destructive">
          {t("richUnsupported")}
        </p>
      )}
      {notice === "plaintext" && (
        <p role="status" className="px-4 pb-3 text-sm text-muted-foreground">
          {t("pastedPlainText")}
        </p>
      )}
      <input
        ref={fileInput}
        type="file"
        accept="image/png,image/jpeg,image/webp,image/gif"
        className="hidden"
        aria-hidden="true"
        tabIndex={-1}
        onChange={(event) => {
          const file = event.target.files?.[0]
          event.target.value = ""
          if (file) void insertImage(file)
        }}
      />
      <BlockNoteView
        editor={editor}
        theme={resolvedTheme === "dark" ? "dark" : "light"}
        editable={!readOnly}
        formattingToolbar={false}
        linkToolbar={false}
        slashMenu={false}
        sideMenu={false}
        filePanel={false}
        tableHandles={false}
        emojiPicker={false}
        comments={false}
        onDragStartCapture={() => {
          localDrag.current = true
        }}
        onDragEndCapture={() => {
          localDrag.current = false
        }}
        onDropCapture={(event) => {
          const internal = localDrag.current
          localDrag.current = false
          if (
            !internal ||
            event.dataTransfer.files.length ||
            !event.dataTransfer.types.includes("blocknote/html")
          ) {
            event.preventDefault()
            event.stopPropagation()
            setNotice("unsupported")
          }
        }}
        onChange={() => {
          try {
            const next = toNeev(editor.document)
            const serialized = JSON.stringify(next)
            if (serialized !== lastEmitted.current) {
              lastEmitted.current = serialized
              callback.current(next)
            }
          } catch {
            setNotice("unsupported")
          }
        }}
      >
        <FormattingToolbarController
          formattingToolbar={() => (
            <FormattingToolbar>
              <BasicTextStyleButton basicTextStyle="bold" />
              <BasicTextStyleButton basicTextStyle="italic" />
              <BasicTextStyleButton basicTextStyle="strike" />
              <BasicTextStyleButton basicTextStyle="code" />
              <CreateLinkButton />
            </FormattingToolbar>
          )}
        />
        <SuggestionMenuController
          triggerCharacter="/"
          getItems={async (query) =>
            choices
              .filter((choice) =>
                choice.title
                  .toLocaleLowerCase()
                  .includes(query.toLocaleLowerCase())
              )
              .map((choice) => ({
                title: choice.title,
                onItemClick: () =>
                  choice.action
                    ? choice.action()
                    : editor.updateBlock(
                        editor.getTextCursorPosition().block,
                        choice.block as Parameters<typeof editor.updateBlock>[1]
                      ),
              }))
          }
        />
        <SideMenuController
          sideMenu={(props) => (
            <SideMenu {...props}>
              <DragHandleButton {...props}>
                <DragHandleMenu>
                  <RemoveBlockItem>{t("removeBlock")}</RemoveBlockItem>
                </DragHandleMenu>
              </DragHandleButton>
            </SideMenu>
          )}
        />
      </BlockNoteView>
    </div>
  )
}
