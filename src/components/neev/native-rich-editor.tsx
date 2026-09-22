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
import {
  fromNeev,
  importMarkdown,
  RichTooLargeError,
  toNeev,
} from "@/lib/neev-rich-adapter"
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
/**
 * What the last action did.
 *
 * `unsupported` and `toolarge` mean nothing was inserted; `plaintext` and
 * `celllines` mean it was, with something named lost on the way in.
 */
type Notice =
  | "none"
  | "unsupported"
  | "toolarge"
  | "plaintext"
  | "celllines"
  | "cellbreak"

/** A run as the adapter emits it: text, or a link wrapping text. */
type InlineRun = { text?: string; content?: InlineRun[] }

/** Whether any run carries a line break, link content included. */
function runsBreak(runs: InlineRun[] | undefined): boolean {
  return (runs ?? []).some((run) =>
    run.content
      ? runsBreak(run.content)
      : /[\n\r\u2028\u2029]/u.test(run.text ?? "")
  )
}

/** What a paste should do, decided before anything touches the document. */
export type PasteAction =
  | { kind: "inline"; content: unknown; notice: Notice }
  /** Literal text, inserted the way the editor pastes text. */
  | { kind: "text"; text: string; notice: Notice }
  | { kind: "blocks"; blocks: unknown[]; notice: Notice }
  | { kind: "refuse"; notice: "unsupported" | "toolarge" }

/**
 * Where a paste goes, and what it costs to put it there.
 *
 * Pure, because the interesting part is the decision rather than the insertion:
 * whether the text can keep its structure, whether it is going somewhere that
 * can hold structure at all, and what was lost on the way in.
 */
export function choosePaste({
  plain,
  onlyPlain,
  where,
}: {
  plain: string
  onlyPlain: boolean
  /**
   * What the cursor is inside.
   *
   * `cell` and `code` cannot hold blocks, and a cursor in either reports its
   * CONTAINER as the block — so inserting "after" it lands outside the thing
   * the reader was typing in. They differ in what they can hold: a code block
   * keeps line breaks, a table run may not contain one at all.
   */
  where: "body" | "cell" | "code"
}): PasteAction {
  // Formatting was on offer and we are not taking it.
  const dropped = !onlyPlain
  if (!plain) return { kind: "refuse", notice: "unsupported" }
  const lines = plain.replace(/\r\n?/g, "\n").split("\n")
  // A code block takes text literally — it holds its own line breaks, and
  // Markdown read inside code would be the one place it means nothing.
  //
  // Through the editor's own text paste, which is what upstream does for a code
  // selection. Inserting the same string as inline content dropped it entirely:
  // a line break there is a hard break, a code block holds plain text and
  // nothing else, and the result was an empty block.
  if (where === "code")
    return {
      kind: "text",
      text: lines.join("\n"),
      notice: dropped ? "plaintext" : "none",
    }
  // Plain text alone is read as our restricted Markdown, so pasted structure
  // survives when it is representable.
  if (onlyPlain) {
    try {
      const parsed = fromNeev(importMarkdown(plain))
      const [only] = parsed
      const content = only?.content as InlineRun[] | undefined
      const flat =
        parsed.length === 1 &&
        only.type === "paragraph" &&
        !only.children?.length
      // A line break can hide inside a link's own content, where a probe of
      // the top-level run reads nothing at all — and a table run may not hold
      // one, so missing it means the guard refuses the paste we were trying to
      // keep.
      const unbroken = where !== "cell" || !runsBreak(content)
      // An empty paragraph is what Markdown makes of whitespace. Both paths
      // below would replace the selection with nothing, so the literal text
      // takes the plain path instead.
      const empty = flat && !content?.length
      if (flat && unbroken && !empty)
        return { kind: "inline", content, notice: "none" }
      if (where === "body" && !empty)
        return { kind: "blocks", blocks: parsed, notice: "none" }
    } catch (cause) {
      // The plain-text path below carries the same bytes, so name the limit
      // that was hit instead of blaming the formatting.
      if (cause instanceof RichTooLargeError)
        return { kind: "refuse", notice: "toolarge" }
    }
  }
  // A table run may not hold a line break — the stored model forbids it — so
  // the text goes in as one line and the notice says so. Refusing would lose
  // it; flattening quietly would change it without saying.
  //
  // All four separators, not just LF and CR: the model rejects U+2028 and
  // U+2029 in a table run too, so leaving them in would refuse the paste that
  // this flattening exists to keep. Elsewhere they are ordinary text and stay.
  if (where === "cell") {
    const pieces = plain.split(/\r\n?|[\n\u2028\u2029]/u)
    return {
      kind: "inline",
      content: pieces.join(" "),
      notice: pieces.length > 1 ? "celllines" : dropped ? "plaintext" : "none",
    }
  }
  if (lines.length === 1)
    return {
      kind: "inline",
      content: lines[0],
      notice: dropped ? "plaintext" : "none",
    }
  return {
    kind: "blocks",
    blocks: lines.map((line) => ({
      type: "paragraph" as const,
      content: line,
    })),
    notice: "plaintext",
  }
}

/**
 * Whether this keystroke would put a line break where one cannot go.
 *
 * Shift+Enter in a table cell is the shortest path to the refusal people
 * actually hit: the break is rejected by the stored model, and what they saw
 * was a banner about unsupported formatting, files and HTML — none of which
 * they had used. Catching the keystroke says the true and much smaller thing.
 */
export function refusesLineBreak({
  key,
  shiftKey,
  where,
}: {
  key: string
  shiftKey: boolean
  where: "body" | "cell" | "code"
}): boolean {
  return key === "Enter" && shiftKey && where === "cell"
}

/** Which of the three kinds of place the cursor is in. */
function placeOf(type: string): "body" | "cell" | "code" {
  if (type === "table") return "cell"
  if (type === "codeBlock") return "code"
  return "body"
}

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
  // What the LAST action did, and nothing older than that. "unsupported" =
  // refused, nothing inserted; "toolarge" = refused for size rather than shape,
  // which is a different thing to tell someone; "plaintext" = inserted, but
  // formatting was dropped on the way in. Truthful copy per state — never claim
  // text was kept when nothing was inserted.
  //
  // It clears when the next edit succeeds. Latching it forever meant one
  // refused paste left a red "editing refused" banner over a document that was
  // taking every keystroke perfectly well, which is how a stale notice becomes
  // a bug report about the editor being broken.
  const [notice, setNotice] = useState<Notice>("none")
  /**
   * Whether the edit guard refused the transaction we just asked for.
   *
   * Set inside `onBeforeChange`, which ProseMirror runs synchronously while the
   * insert call below is still on the stack — so the paste can report what
   * actually happened rather than what it intended. A held object rather than a
   * ref, because the paste handler is built during render and reading a ref
   * there is exactly what the refs rule forbids.
   */
  const [edits] = useState(() => ({ refused: false }))
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
        const action = choosePaste({
          plain: data ? data.getData("text/plain") : "",
          onlyPlain:
            !!data &&
            !data.files.length &&
            Array.from(data.types).every((type) => type === "text/plain"),
          where: placeOf(editor.getTextCursorPosition().block.type),
        })
        if (action.kind === "refuse") {
          setNotice(action.notice)
          return false
        }
        edits.refused = false
        try {
          if (action.kind === "inline")
            editor.insertInlineContent(
              action.content as Parameters<typeof editor.insertInlineContent>[0]
            )
          else if (action.kind === "text") editor.pasteText(action.text)
          else
            // Through the editor's own serializer and paste pipeline: one
            // transaction that replaces the selection and splits the block at
            // the cursor, rather than an insert bolted on after it.
            // `pasteHTML` calls the ProseMirror view directly, so this handler
            // is not re-entered.
            editor.pasteHTML(
              editor.blocksToFullHTML(
                action.blocks as Parameters<typeof editor.blocksToFullHTML>[0]
              )
            )
        } catch {
          // Serializing or inserting can fail on content nothing anticipated.
          // Say so where the reader is looking, rather than throwing out of a
          // paste handler and taking the editor down with it.
          setNotice("unsupported")
          return false
        }
        // The guard runs inside the insert above, so report what happened
        // rather than what was intended.
        setNotice(edits.refused ? "unsupported" : action.notice)
        return true
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
          edits.refused = true
          setNotice("unsupported")
          return false
        }
        // This edit was fine, so whatever the last one said is no longer true.
        // The paste handler sets its own notice after its insert returns, so
        // clearing here cannot erase a report about the paste in progress.
        edits.refused = false
        setNotice("none")
      }),
    [editor, edits]
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
      onKeyDownCapture={(event) => {
        if (readOnly) return
        let where: "body" | "cell" | "code" = "body"
        try {
          where = placeOf(editor.getTextCursorPosition().block.type)
        } catch {
          // No cursor to speak of; nothing to refuse.
          return
        }
        if (
          !refusesLineBreak({ key: event.key, shiftKey: event.shiftKey, where })
        )
          return
        // Refuse the keystroke itself rather than letting it build a document
        // the guard will reject with a far vaguer complaint.
        event.preventDefault()
        event.stopPropagation()
        setNotice("cellbreak")
      }}
    >
      {(notice === "unsupported" || notice === "toolarge") && (
        <p role="alert" className="px-4 pb-3 text-sm text-destructive">
          {t(notice === "toolarge" ? "richTooLarge" : "richUnsupported")}
        </p>
      )}
      {notice === "cellbreak" && (
        <p role="status" className="px-4 pb-3 text-sm text-muted-foreground">
          {t("cellNoLineBreak")}
        </p>
      )}
      {(notice === "plaintext" || notice === "celllines") && (
        <p role="status" className="px-4 pb-3 text-sm text-muted-foreground">
          {t(notice === "celllines" ? "pastedCellLines" : "pastedPlainText")}
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
