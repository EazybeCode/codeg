"use client"

import { useEffect, useMemo, useRef, useState } from "react"
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
  onChange: (next: RichDocument) => void
}

/** The parent owns load generations. Incoming props never overwrite this instance. */
export function NativeRichEditor({
  document,
  readOnly,
  onChange,
}: NativeRichEditorProps) {
  const t = useTranslations("NativeContent")
  const { resolvedTheme } = useTheme()
  const [notice, setNotice] = useState(false)
  const localDrag = useRef(false)
  const callback = useRef(onChange)
  callback.current = onChange
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
        try {
          const data = event.clipboardData
          if (
            !data ||
            data.files.length ||
            Array.from(data.types).some((type) => type !== "text/plain")
          )
            throw new Error("Unsupported paste")
          const candidate = importMarkdown(data.getData("text/plain"))
          // Parse the entire candidate before the editor can normalize clipboard data.
          setNotice(false)
          editor.insertBlocks(
            fromNeev(candidate) as Parameters<typeof editor.insertBlocks>[0],
            editor.getTextCursorPosition().block,
            "after"
          )
          return true
        } catch {
          setNotice(true)
          return false
        }
      },
    })
    editor.replaceBlocks(
      editor.document,
      fromNeev(document) as Parameters<typeof editor.replaceBlocks>[1]
    )
    return editor
  })
  const lastEmitted = useRef(JSON.stringify(toNeev(editor.document)))
  const ownHost = useMemo(() => {
    let observer: MutationObserver | null = null
    let released = false
    return (host: HTMLDivElement | null) => {
      if (!host || observer || released) return
      // Activity/StrictMode detach refs and effects without removing the host.
      // Observe real removal, including an unmount while Activity is already hidden.
      observer = new MutationObserver(() => {
        if (host.isConnected) return
        released = true
        observer?.disconnect()
        observer = null
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
      observer.observe(host.ownerDocument, { childList: true, subtree: true })
    }
  }, [editor])
  useEffect(
    () =>
      editor.onBeforeChange(({ tr }) => {
        if (!tr.docChanged) return
        try {
          toNeev(docToBlocks(tr.doc))
        } catch {
          setNotice(true)
          return false
        }
      }),
    [editor]
  )
  const choices: { title: string; block: PartialBlock }[] = [
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
  ]
  return (
    <div
      ref={ownHost}
      className="neev-writing-canvas min-w-0 bg-background py-4"
    >
      {notice && (
        <p role="alert" className="px-4 pb-3 text-sm text-destructive">
          {t("richUnsupported")}
        </p>
      )}
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
            setNotice(true)
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
            setNotice(true)
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
                  editor.updateBlock(
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
