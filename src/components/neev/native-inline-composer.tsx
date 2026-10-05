"use client"

// One editable document per composer: words, a file, more words, another file.
//
// The tray this replaces was honest about upload state and dishonest about
// ORDER. Five files under a box of text say nothing about which sentence each
// belongs to, and "compare the screenshot with the log" is exactly the ask that
// needs to say so. Here a file is an atomic node sitting in the text where it
// was dropped, so the caret walks over it, Backspace deletes it, and the
// sentence keeps its shape all the way to the provider.
//
// The node carries a LOCAL id and nothing else. Bytes, upload state and the
// server's key stay in the tray, which the chip reads through context — so a
// file still uploading says so inside the sentence, and a node can never be
// mistaken for a file the server has.

import { continueNumberedList } from "@/lib/neev-numbered-list"
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useId,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react"
import { useTranslations } from "next-intl"
import { Editor, Extension, Node, mergeAttributes } from "@tiptap/core"
import type { JSONContent } from "@tiptap/core"
import type { Node as ProseMirrorNode } from "@tiptap/pm/model"
import { history, redo, undo } from "@tiptap/pm/history"
import {
  EditorContent,
  NodeViewWrapper,
  ReactNodeViewRenderer,
  type NodeViewProps,
} from "@tiptap/react"
import { X } from "lucide-react"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { sendsOnEnter } from "@/lib/neev-composer-keys"
import {
  filesFromDrop,
  filesFromPaste,
  formatBytes,
  NEEV_MAX_ATTACHMENTS,
  pasteIsFileOnly,
} from "@/lib/neev-attachments"
import {
  mentionText,
  promptIsEmpty,
  sameSegments,
  type PromptSegment,
} from "@/lib/neev-inline-prompt"
import {
  MAX_MENTIONS_PER_MESSAGE,
  type MentionCandidate,
} from "@/lib/neev-mentions"
import {
  AttachmentFailures,
  AttachmentRejections,
  AttachmentThumb,
  type AttachmentTray,
  type DraftAttachment,
} from "./native-attachments"

const ATTACHMENT_NODE = "neevAttachment"
const MENTION_NODE = "neevMention"

/**
 * What may sit between the start of a word and the `@` that opens the picker.
 *
 * An `@` mid-word is an email address, a handle somebody is quoting, a path —
 * none of them an attempt to name a coworker. The picker opens at a word
 * boundary and nowhere else.
 *
 * `￼` is how a paragraph reports an atom node (a file, or another
 * mention), so the query can never run backwards through one.
 */
const MENTION_TRIGGER = /(?:^|[\s([{"'«¿¡-])@([^\s@￼]{0,40})$/u

/**
 * How many released files an undo can reach back through.
 *
 * Each one is a `File` still held in memory, so this is deliberately smaller
 * than the number a message may carry: far enough back to cover the mistake
 * somebody is actually undoing, not a second staging area.
 */
const RELEASED_LIMIT = 8

/**
 * The tray the chips in one editor belong to.
 *
 * Node views are rendered by the editor rather than by the composer's own JSX,
 * so a context is the only way a chip can know whether its bytes are up yet.
 */
const TrayContext = createContext<{
  tray: AttachmentTray
  disabled: boolean
} | null>(null)

/** A file, inside the sentence, while it is being written. */
function AttachmentChip({ deleteNode, node, selected }: NodeViewProps) {
  const t = useTranslations("NativeContent")
  const n = useTranslations("Native")
  const context = useContext(TrayContext)
  const id = String(node.attrs.localId ?? "")
  const item = context?.tray.items.find((entry) => entry.id === id)
  // A node whose file is gone is on its way out — the composer reconciles it
  // away on this same pass. Drawing a half-chip meanwhile would flash a
  // nameless box into the sentence.
  if (!item)
    return (
      <NodeViewWrapper as="span" contentEditable={false} className="hidden" />
    )
  const failed = item.status === "failed"
  return (
    <NodeViewWrapper
      as="span"
      contentEditable={false}
      data-attachment-id={id}
      className={cn(
        "mx-0.5 inline-flex max-w-64 select-none items-center gap-1 rounded-md border bg-muted/40 py-0.5 pe-0.5 ps-1 align-middle text-xs",
        failed && "border-destructive/60",
        selected && "ring-2 ring-ring"
      )}
      title={`${item.name} · ${formatBytes(item.size)}`}
    >
      <AttachmentThumb
        src={item.previewUrl}
        contentType={item.contentType}
        name={item.name}
        className="size-4"
      />
      <span className="min-w-0 truncate">{item.name}</span>
      {item.status === "uploading" && (
        <span role="status" className="shrink-0 text-muted-foreground">
          {t("attachmentUploading")}
        </span>
      )}
      {failed && (
        <>
          <span className="shrink-0 text-destructive">
            {t("attachmentFailed")}
          </span>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="h-5 shrink-0 px-1 text-2xs"
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => context?.tray.retry(id)}
          >
            {n("retry")}
          </Button>
        </>
      )}
      <Button
        type="button"
        size="icon"
        variant="ghost"
        className="size-5 shrink-0"
        aria-label={t("attachmentRemove", { name: item.name })}
        title={t("attachmentRemove", { name: item.name })}
        disabled={context?.disabled}
        onMouseDown={(event) => event.preventDefault()}
        // Deletes the REFERENCE, not the file. Taking the file out of the tray
        // directly would leave the node behind for the reconcile to sweep up as
        // something it cannot account for — no history entry, and nothing
        // remembered, so Ctrl+Z had nothing to bring back. Editing the document
        // is what an edit to the document is: the reconcile then releases the
        // file and remembers it, exactly as deleting the reference with a
        // keystroke does.
        onClick={() => deleteNode()}
      >
        <X aria-hidden className="size-3" />
      </Button>
    </NodeViewWrapper>
  )
}

/**
 * The schema, written out rather than taken from a starter kit.
 *
 * A composer is plain text with files in it. Bold, headings, lists and their
 * input rules all change what pressing a key does in a box whose only job is to
 * hold a sentence, so none of them exist here.
 */
const AttachmentExtension = Node.create({
  name: ATTACHMENT_NODE,
  group: "inline",
  inline: true,
  atom: true,
  selectable: true,
  draggable: false,
  addAttributes: () => ({
    localId: {
      default: null,
      parseHTML: (element) => element.getAttribute("data-attachment-id"),
      renderHTML: (attributes) => ({
        "data-attachment-id": String(attributes.localId ?? ""),
      }),
    },
  }),
  parseHTML: () => [{ tag: "span[data-attachment-id]" }],
  renderHTML: ({ HTMLAttributes }) => [
    "span",
    mergeAttributes(HTMLAttributes, { "data-attachment": "" }),
  ],
  addNodeView: () => ReactNodeViewRenderer(AttachmentChip, { as: "span" }),
})

/** A coworker, inside the sentence, while it is being written. */
function MentionChip({ node, selected }: NodeViewProps) {
  const name = String(node.attrs.name ?? "")
  return (
    <NodeViewWrapper
      as="span"
      contentEditable={false}
      data-mention-user={String(node.attrs.userId ?? "")}
      className={cn(
        "rounded bg-primary/10 px-0.5 font-medium text-primary",
        selected && "ring-2 ring-ring"
      )}
    >
      {mentionText(name)}
    </NodeViewWrapper>
  )
}

/**
 * A mention: one atom, carrying the id of the person it means.
 *
 * Atomic so the caret steps over it and Backspace takes the whole name rather
 * than leaving "@An" behind pointing at nobody — and so undo brings back a
 * person, not a fragment.
 *
 * **It has no `parseHTML` rule, deliberately.** Everything else in this schema
 * can be built from markup; a mention cannot, because a mention is an identity.
 * Pasted or dropped HTML claiming to be one parses as the plain text inside it,
 * which is what `@Ana` written by anything other than the picker actually is.
 */
const MentionExtension = Node.create({
  name: MENTION_NODE,
  group: "inline",
  inline: true,
  atom: true,
  selectable: true,
  draggable: false,
  addAttributes: () => ({
    userId: { default: null },
    name: { default: "" },
  }),
  renderHTML: ({ node, HTMLAttributes }) => [
    "span",
    mergeAttributes(HTMLAttributes, {
      "data-mention-user": String(node.attrs.userId ?? ""),
    }),
    mentionText(String(node.attrs.name ?? "")),
  ],
  addNodeView: () => ReactNodeViewRenderer(MentionChip, { as: "span" }),
})

const DocumentExtension = Node.create({
  name: "doc",
  topNode: true,
  content: "paragraph+",
})

const ParagraphExtension = Node.create({
  name: "paragraph",
  group: "block",
  content: "inline*",
  parseHTML: () => [{ tag: "p" }],
  renderHTML: ({ HTMLAttributes }) => ["p", mergeAttributes(HTMLAttributes), 0],
})

const TextExtension = Node.create({ name: "text", group: "inline" })

const HardBreakExtension = Node.create({
  name: "hardBreak",
  group: "inline",
  inline: true,
  selectable: false,
  parseHTML: () => [{ tag: "br" }],
  renderHTML: ({ HTMLAttributes }) => ["br", mergeAttributes(HTMLAttributes)],
  addKeyboardShortcuts() {
    const insert = () =>
      this.editor.commands.insertContent({ type: "hardBreak" })
    // Shift+Enter always inserts a line break. Mod+Enter is handled by the
    // numbered list extension or falls through to the composer's send logic.
    return { "Shift-Enter": insert }
  },
})

/** Undo and redo, which a box someone writes in is expected to have. */
const HistoryExtension = Extension.create({
  name: "neevHistory",
  addProseMirrorPlugins: () => [history()],
  addKeyboardShortcuts: () => ({
    "Mod-z": ({ editor }) => undo(editor.state, editor.view.dispatch),
    "Shift-Mod-z": ({ editor }) => redo(editor.state, editor.view.dispatch),
    "Mod-y": ({ editor }) => redo(editor.state, editor.view.dispatch),
  }),
})

const extensions = [
  DocumentExtension,
  ParagraphExtension,
  TextExtension,
  HardBreakExtension,
  AttachmentExtension,
  MentionExtension,
  HistoryExtension,
]

/** The document, read back as the ordered thing that gets sent. */
export function segmentsFromDoc(doc: ProseMirrorNode): PromptSegment[] {
  const out: PromptSegment[] = []
  const write = (text: string) => {
    if (!text) return
    const last = out[out.length - 1]
    if (last && last.kind === "text") last.text += text
    else out.push({ kind: "text", text })
  }
  doc.forEach((block, _offset, index) => {
    // A paragraph boundary is a newline and nothing more: this is a prompt,
    // not a document.
    if (index > 0) write("\n")
    block.forEach((child) => {
      if (child.isText) write(child.text ?? "")
      else if (child.type.name === "hardBreak") write("\n")
      else if (child.type.name === ATTACHMENT_NODE)
        out.push({ kind: "attachment", id: String(child.attrs.localId ?? "") })
      else if (child.type.name === MENTION_NODE) {
        const userId = Number(child.attrs.userId)
        const name = String(child.attrs.name ?? "")
        // A node that cannot name a person is not one. Written out as the text
        // it shows rather than dropped, so nothing anybody can read disappears.
        if (Number.isSafeInteger(userId) && userId > 0 && name)
          out.push({ kind: "mention", userId, name })
        else write(mentionText(name))
      }
    })
  })
  return out
}

/** And the same thing the other way, for a draft being put back. */
export function docFromSegments(
  segments: readonly PromptSegment[]
): JSONContent {
  const paragraphs: JSONContent[][] = [[]]
  const current = () => paragraphs[paragraphs.length - 1]
  for (const segment of segments) {
    if (segment.kind === "attachment") {
      current().push({ type: ATTACHMENT_NODE, attrs: { localId: segment.id } })
      continue
    }
    if (segment.kind === "mention") {
      current().push({
        type: MENTION_NODE,
        attrs: { userId: segment.userId, name: segment.name },
      })
      continue
    }
    segment.text.split("\n").forEach((line, index) => {
      if (index > 0) paragraphs.push([])
      if (line) current().push({ type: "text", text: line })
    })
  }
  return {
    type: "doc",
    content: paragraphs.map((content) => ({
      type: "paragraph",
      ...(content.length ? { content } : {}),
    })),
  }
}

/**
 * Where the picker is, and what it has been asked for.
 *
 * `from`/`to` are the positions the chosen mention replaces — the `@` and
 * everything typed after it — captured when the query was read rather than
 * recomputed on selection, so the answer lands where the question was asked.
 */
interface PickerState {
  from: number
  to: number
  query: string
  active: number
}

/** The directory this composer may offer, if it may offer one at all. */
export interface MentionSource {
  /**
   * The people this query names, already narrowed to those permitted HERE.
   *
   * The composer never decides who may be mentioned. A surface with no
   * audience — a private chat, an agent session — passes no source at all, and
   * `@` is then an ordinary character.
   */
  candidates: (query: string) => readonly MentionCandidate[]
  /** Called when the picker opens, so a caller can load its directory lazily. */
  onOpen?: () => void
}

export interface InlineComposerHandle {
  focus: () => void
  /** Stage these files and put them where the caret is. */
  addFiles: (files: readonly File[]) => Promise<void>
}

export function NativeInlineComposer({
  ref,
  value,
  onChange,
  tray,
  label,
  placeholder,
  onSubmit,
  busy = false,
  readOnly = false,
  attachDisabled = false,
  autoFocus = false,
  maxChars,
  mentions,
  retain,
  className,
}: {
  ref?: React.Ref<InlineComposerHandle>
  value: readonly PromptSegment[]
  onChange: (next: PromptSegment[]) => void
  tray: AttachmentTray
  /** The accessible name of the box. */
  label: string
  placeholder: string
  onSubmit: () => void
  /** Enter must not send right now; it still makes a line. */
  busy?: boolean
  /** Archived, revoked: the words are readable but not editable. */
  readOnly?: boolean
  /** Editable, but no more files may be staged. */
  attachDisabled?: boolean
  autoFocus?: boolean
  maxChars?: number
  /** Absent: this conversation has nobody to name, so `@` is just `@`. */
  mentions?: MentionSource
  /**
   * Files that belong to a message still on its way. The box was emptied when
   * it was sent, but its files must stay staged until the server has them (or
   * come back with the words if it does not), so leaving the box does not
   * unstage them the way deleting a chip does.
   */
  retain?: ReadonlySet<string>
  className?: string
}) {
  const c = useTranslations("NativeContent")
  // Read by editor callbacks, which are registered once and must not close over
  // the first render's props. Written after the render rather than during it:
  // nothing here is read while rendering, and a ref written in render is a
  // render that cannot be replayed.
  const latest = useRef({ onSubmit, onChange, busy, maxChars, attachDisabled })
  useEffect(() => {
    latest.current = { onSubmit, onChange, busy, maxChars, attachDisabled }
  })
  // The picker, and the keys it answers. The handlers live in a ref because
  // `handleKeyDown` is registered with the editor and must not close over the
  // render that registered it — the same reason `latest` exists.
  const [picker, setPicker] = useState<PickerState | null>(null)
  const keys = useRef<{
    open: boolean
    move: (delta: number) => void
    choose: () => boolean
    close: () => void
  }>({
    open: false,
    move: () => {},
    choose: () => false,
    close: () => {},
  })
  const emitted = useRef<readonly PromptSegment[]>(value)
  // Ids this editor has put in the document and the tray has not confirmed
  // yet, and ids it HAS confirmed. Anything in the document that is in neither
  // is not a file: a node undo brought back after its bytes were released, or
  // one that arrived in pasted HTML. Those are deleted on sight.
  const pending = useRef(new Set<string>())
  const confirmed = useRef(new Set<string>())
  // The tray's files as they were last seen, so a file that has just left it
  // can still be described — and the ones whose reference was deleted, in the
  // order they were released.
  const known = useRef(new Map<string, DraftAttachment>())
  const released = useRef(new Map<string, DraftAttachment>())
  const trayRef = useRef(tray)
  useEffect(() => {
    trayRef.current = tray
  })
  const sourceRef = useRef(mentions)
  useEffect(() => {
    sourceRef.current = mentions
  })

  /**
   * Owned here, not by `useEditor`.
   *
   * A composer inside a hidden tab is still holding somebody's unsent message.
   * `useEditor` destroys its editor in an effect cleanup, and `Activity` runs
   * effect cleanups when it hides — so the draft, and the caret, and the undo
   * history would all be thrown away by switching tabs. Creating it here means
   * hiding does nothing to it; `ownHost` below is what notices REAL removal.
   *
   * Null on the server: `new Editor` needs a document, and this shell is
   * prerendered by the static export.
   */
  const [editor] = useState<Editor | null>(() =>
    typeof document === "undefined"
      ? null
      : new Editor({
          extensions,
          content: docFromSegments(value),
          editable: !readOnly,
          autofocus: autoFocus ? "end" : false,
        })
  )
  const insert = useCallback(
    async (files: readonly File[], at?: { from: number; to: number }) => {
      // Read BEFORE the await, always: classification reads the whole file, and
      // the answer must land where the caret was when the gesture happened rather
      // than wherever it has drifted to since. The file picker is the case this
      // is really about — it has no event carrying a position of its own.
      const selection = editor?.state.selection
      const anchor = at ?? {
        from: selection?.from ?? 0,
        to: selection?.to ?? 0,
      }
      const staged = await trayRef.current.add(files)
      const current = editor
      if (!current || current.isDestroyed || staged.length === 0) return
      const size = current.state.doc.content.size
      // Selected text is REPLACED, the way anything else put at a selection is.
      const range = {
        from: Math.min(anchor.from, size),
        to: Math.min(Math.max(anchor.to, anchor.from), size),
      }
      const pos = range.from
      const content: JSONContent[] = staged.map((item) => ({
        type: ATTACHMENT_NODE,
        attrs: { localId: item.id },
      }))
      // Spaces on either side, so the reference is a word among words — both in
      // the box and in the sentence the provider is finally asked. Dropping a
      // file at the end of "Read this" must not produce "Read this[file]".
      const before = current.state.doc.textBetween(Math.max(pos - 1, 0), pos)
      if (before.trim()) content.unshift({ type: "text", text: " " })
      content.push({ type: "text", text: " " })
      for (const item of staged) pending.current.add(item.id)
      current.chain().focus().insertContentAt(range, content).run()
    },
    [editor]
  )

  /**
   * The editor's behaviour, attached after the render that created it.
   *
   * These read props and call back into the composer, and a constructor option
   * is built DURING render — where neither a ref nor a callback declared below
   * it can be reached. Applied here instead, and re-applied whenever what they
   * close over changes.
   */
  useEffect(() => {
    if (!editor) return
    editor.setOptions({
      editorProps: {
        attributes: {
          role: "textbox",
          "aria-label": label,
          "aria-multiline": "true",
          class: cn(
            "w-full whitespace-pre-wrap break-words px-1 py-1.5 text-base outline-none sm:text-sm [&_p]:m-0 [&_p]:min-h-[1.5em]",
            className
          ),
        },
        handleKeyDown: (_view, event) => {
          // While the picker is up it owns the arrows, Enter, Tab and Escape —
          // Enter CHOOSES and never sends, which is the difference between
          // naming a coworker and posting the half-typed name of one.
          //
          // Never during composition: an IME's own candidate window is driven
          // by exactly these keys, and taking them would make the composer
          // fight the input method for the same keystroke.
          const composing = event.isComposing || event.keyCode === 229
          if (keys.current.open && !composing) {
            if (event.key === "ArrowDown" || event.key === "ArrowUp") {
              event.preventDefault()
              keys.current.move(event.key === "ArrowDown" ? 1 : -1)
              return true
            }
            if (event.key === "Enter" || event.key === "Tab") {
              if (!keys.current.choose()) return false
              event.preventDefault()
              return true
            }
            if (event.key === "Escape") {
              event.preventDefault()
              keys.current.close()
              return true
            }
          }

          // Handle Enter for numbered list continuation/exit.
          // This runs BEFORE send logic: within a list, Enter continues;
          // Mod+Enter always sends; Shift+Enter is a hard break (handled by
          // extension); IME composition is never intercepted.
          if (
            event.key === "Enter" &&
            !composing &&
            !event.shiftKey &&
            !event.metaKey &&
            !event.ctrlKey &&
            editor
          ) {
            if (continueNumberedList(_view.state, _view.dispatch)) {
              event.preventDefault()
              return true
            }
          }

          if (
            !sendsOnEnter(
              {
                key: event.key,
                shiftKey: event.shiftKey,
                isComposing: event.isComposing,
                nativeEvent: {
                  isComposing: event.isComposing,
                  keyCode: event.keyCode,
                },
              },
              { busy: latest.current.busy }
            )
          )
            return false
          event.preventDefault()
          latest.current.onSubmit()
          return true
        },
        // A ceiling the box enforces rather than one the server refuses.
        handleTextInput: (view, _from, _to, text) => {
          const limit = latest.current.maxChars
          if (limit === undefined) return false
          return view.state.doc.textContent.length + text.length > limit
        },
        // Paste is handled all the way, never handed back to ProseMirror.
        //
        // The default would parse the clipboard's HTML, and this schema has a
        // node that parses from `span[data-attachment-id]` — so pasted markup
        // could mint a chip carrying any id it liked, next to a file it does not
        // own. A file gets into this document one way: a gesture that hands over
        // a real `File`, which the tray admits and holds the bytes for. Everything
        // else on a clipboard is text.
        handlePaste: (view, event) => {
          const files = latest.current.attachDisabled
            ? []
            : filesFromPaste(event.clipboardData)
          // Text alongside the files means the person copied something carrying
          // both; the text is still theirs to paste.
          const text =
            files.length > 0 && pasteIsFileOnly(event.clipboardData)
              ? ""
              : (event.clipboardData?.getData("text/plain") ?? "")
          event.preventDefault()
          // Text first, replacing the selection, and the files after it — which
          // is the order they were copied in.
          if (text)
            view.dispatch(view.state.tr.insertText(text).scrollIntoView())
          if (files.length > 0) void insert(files)
          return true
        },
        handleDrop: (view, event) => {
          if (latest.current.attachDisabled) return false
          const files = filesFromDrop(event.dataTransfer)
          if (files.length === 0) return false
          // The point of dropping ONTO a sentence is landing where the pointer
          // is, so the position comes from the coordinates rather than from
          // wherever the caret happened to be.
          const at = view.posAtCoords({
            left: event.clientX,
            top: event.clientY,
          })
          const target = at?.pos ?? view.state.selection.to
          event.preventDefault()
          // The card around this composer also listens for drops; without this
          // the same files would be staged twice.
          event.stopPropagation()
          void insert(files, { from: target, to: target })
          return true
        },
      },
    })
  }, [editor, insert, label, className])

  // Reported from a listener rather than a constructor option: the option is
  // built during render, and the props it would have to read are not readable
  // there.
  useEffect(() => {
    if (!editor) return
    const report = () => {
      const next = segmentsFromDoc(editor.state.doc)
      emitted.current = next
      latest.current.onChange(next)
    }
    editor.on("update", report)
    return () => {
      editor.off("update", report)
    }
  }, [editor])
  /**
   * The picker follows the caret.
   *
   * Read from the paragraph the caret is in, up to the caret, with every atom
   * node reported as one placeholder character — so a query can neither run
   * backwards past a file nor past another mention, and a position in the
   * string is a position in the document.
   *
   * Nothing is parsed out of the document by this: it decides only whether a
   * LIST is on screen. What ends up in the message is the node the person then
   * chose, never the characters they typed.
   */
  // Whether a query is up right now, for the one thing the state itself cannot
  // answer without being read inside its own updater: was this `@` the one that
  // opened the picker? That transition is when a caller is told to fetch its
  // directory, and telling it twice per keystroke would be a request per letter.
  const asking = useRef(false)
  useEffect(() => {
    if (!editor) return
    const close = () => {
      asking.current = false
      setPicker(null)
    }
    const detect = () => {
      if (editor.isDestroyed || !sourceRef.current || !editor.isEditable)
        return close()
      const { selection } = editor.state
      if (!selection.empty) return close()
      const { $from } = selection
      const before = $from.parent.textBetween(0, $from.parentOffset, "\n", "￼")
      const match = MENTION_TRIGGER.exec(before)
      if (!match) return close()
      const query = match[1]
      const to = $from.pos
      const from = to - query.length - 1
      // Asked for the first time in this run of typing: the surface may have a
      // directory to fetch, and this is the moment it learns anybody wants one.
      if (!asking.current) {
        asking.current = true
        sourceRef.current.onOpen?.()
      }
      setPicker((previous) =>
        previous &&
        previous.from === from &&
        previous.to === to &&
        previous.query === query
          ? previous
          : { from, to, query, active: 0 }
      )
    }
    editor.on("update", detect)
    editor.on("selectionUpdate", detect)
    editor.on("blur", close)
    return () => {
      editor.off("update", detect)
      editor.off("selectionUpdate", detect)
      editor.off("blur", close)
    }
  }, [editor])

  // The single live observer, and the one-shot latch that says this editor has
  // already been released. Mirrors the rich editor, for the same reason:
  // Activity and Strict Mode detach refs without removing anything.
  const observerRef = useRef<MutationObserver | null>(null)
  const releasedRef = useRef(false)
  const ownHost = useCallback(
    (host: HTMLDivElement | null) => {
      if (!host || !editor || observerRef.current || releasedRef.current) return
      const observer = new MutationObserver(() => {
        if (host.isConnected) return
        releasedRef.current = true
        observer.disconnect()
        observerRef.current = null
        editor.destroy()
      })
      observerRef.current = observer
      observer.observe(host.ownerDocument, { childList: true, subtree: true })
    },
    [editor]
  )

  useImperativeHandle(
    ref,
    () => ({
      focus: () => {
        editor?.commands.focus("end")
      },
      addFiles: (files: readonly File[]) => insert(files),
    }),
    [editor, insert]
  )

  // A draft put back from outside — cleared after a send, restored on a tab.
  // The editor is left alone while it holds what it last reported, or every
  // keystroke would round-trip through the parent and move the caret.
  useEffect(() => {
    if (!editor || editor.isDestroyed) return
    if (sameSegments(value, emitted.current)) return
    emitted.current = value
    editor.commands.setContent(docFromSegments(value), { emitUpdate: false })
  }, [editor, value])

  useEffect(() => {
    if (!editor || editor.isDestroyed) return
    editor.setEditable(!readOnly)
  }, [editor, readOnly])

  // Files whose reference was deleted from the sentence, kept so that undo can
  // put both back.
  //
  // Deleting a word and pressing Ctrl+Z restores the word; deleting a file has
  // to restore the file, or undo is a promise the composer only half keeps. The
  // store is small and it forgets: at most `RELEASED_LIMIT` files, oldest
  // evicted first, and everything dropped the moment the tray is cleared — a
  // sent message's files are gone, and nothing is holding their bytes waiting
  // for an undo that can no longer mean anything.
  useEffect(() => {
    released.current.clear()
  }, [tray.generation])

  // The document and the tray, kept honest about each other:
  //
  //  - a file dropped from the tray (its own ✕) takes its reference with it;
  //  - a reference deleted from the sentence releases the file, aborting an
  //    upload that is still running, and is remembered;
  //  - a reference undo brings BACK is re-attached to the file it named;
  //  - a reference this editor cannot account for at all — one that arrived
  //    inside pasted markup, or whose file is long forgotten — is deleted
  //    rather than left in the sentence as an invisible thing the caret has to
  //    step over. Nothing could have been sent for it either way: only a file
  //    the tray holds becomes a marker.
  useEffect(() => {
    if (!editor || editor.isDestroyed) return
    const held = new Map(tray.items.map((item) => [item.id, item]))
    const inDoc = new Map<string, number>()
    editor.state.doc.descendants((node, pos) => {
      if (node.type.name === ATTACHMENT_NODE)
        inDoc.set(String(node.attrs.localId ?? ""), pos)
    })
    for (const [id, item] of held) known.current.set(id, item)
    // Sent, not deleted: forgotten here, and never unstaged by this editor.
    for (const id of retain ?? []) {
      if (inDoc.has(id)) continue
      confirmed.current.delete(id)
      released.current.delete(id)
    }
    for (const id of inDoc.keys()) {
      if (held.has(id)) {
        pending.current.delete(id)
        confirmed.current.add(id)
        continue
      }
      const back = released.current.get(id)
      if (!back) continue
      released.current.delete(id)
      pending.current.add(id)
      trayRef.current.restore(back)
    }
    const stale = [...inDoc.entries()]
      .filter(
        ([id]) =>
          !held.has(id) && !pending.current.has(id) && !released.current.has(id)
      )
      .map(([, pos]) => pos)
      .sort((a, b) => b - a)
    if (stale.length) {
      const tr = editor.state.tr
      for (const pos of stale) tr.delete(pos, pos + 1)
      editor.view.dispatch(tr)
      return
    }
    for (const id of [...confirmed.current]) {
      if (inDoc.has(id)) continue
      confirmed.current.delete(id)
      if (!held.has(id)) continue
      const snapshot = known.current.get(id)
      if (snapshot) {
        // Re-inserted at the end, so eviction takes the oldest first.
        released.current.delete(id)
        released.current.set(id, snapshot)
        while (released.current.size > RELEASED_LIMIT)
          released.current.delete(released.current.keys().next().value!)
      }
      known.current.delete(id)
      trayRef.current.remove(id)
    }
  }, [editor, tray.items, tray.generation, value, retain])

  const context = useMemo(
    () => ({ tray, disabled: readOnly }),
    [tray, readOnly]
  )
  const empty = promptIsEmpty(value)

  // How many people this message already names. The ceiling is the server's,
  // enforced here so the tenth mention is the last one offered rather than the
  // reason a finished message is refused.
  const named = value.reduce(
    (total, segment) => total + (segment.kind === "mention" ? 1 : 0),
    0
  )
  const atLimit = named >= MAX_MENTIONS_PER_MESSAGE
  const options =
    picker && mentions && !atLimit ? mentions.candidates(picker.query) : []
  const activeIndex = picker
    ? Math.min(picker.active, Math.max(options.length - 1, 0))
    : 0
  // Open means a list is actually on screen. Nobody to offer is not an open
  // picker, so Enter goes back to meaning Send.
  const pickerOpen = options.length > 0
  const listId = useId()
  const optionId = (userId: number) => `${listId}-${userId}`
  const insertMention = (candidate: MentionCandidate) => {
    if (!editor || editor.isDestroyed || !picker) return
    const size = editor.state.doc.content.size
    const from = Math.max(Math.min(picker.from, size), 0)
    const to = Math.min(Math.max(picker.to, from), size)
    setPicker(null)
    editor
      .chain()
      .focus()
      .insertContentAt({ from, to }, [
        {
          type: MENTION_NODE,
          attrs: { userId: candidate.userId, name: candidate.name },
        },
        // A name is a word among words: the next one must not be glued to it.
        { type: "text", text: " " },
      ])
      .run()
  }
  // Re-read every render, since the editor's key handler holds this object and
  // not the render that filled it in.
  useEffect(() => {
    keys.current = {
      open: pickerOpen,
      move: (delta) =>
        setPicker((previous) =>
          previous && options.length > 0
            ? {
                ...previous,
                active:
                  (((activeIndex + delta) % options.length) + options.length) %
                  options.length,
              }
            : previous
        ),
      choose: () => {
        const option = options[activeIndex]
        if (!option) return false
        insertMention(option)
        return true
      },
      close: () => setPicker(null),
    }
    // The box IS the combobox, so the state belongs on it. Applied here rather
    // than in the editor's own attributes because those are set once, and this
    // changes on every arrow key. Removed entirely where there is no
    // directory: a textbox that claims a list it does not have is worse for a
    // screen reader than a plain one.
    const dom = editor?.view.dom
    if (!dom) return
    if (!mentions) {
      for (const name of [
        "aria-autocomplete",
        "aria-expanded",
        "aria-controls",
        "aria-activedescendant",
      ])
        dom.removeAttribute(name)
      return
    }
    dom.setAttribute("aria-autocomplete", "list")
    dom.setAttribute("aria-expanded", pickerOpen ? "true" : "false")
    dom.setAttribute("aria-controls", listId)
    const active = pickerOpen ? options[activeIndex] : undefined
    if (active)
      dom.setAttribute("aria-activedescendant", optionId(active.userId))
    else dom.removeAttribute("aria-activedescendant")
  })

  return (
    <TrayContext.Provider value={context}>
      <div className="relative">
        {empty && (
          <p
            aria-hidden
            className="pointer-events-none absolute left-1 top-1.5 text-base text-muted-foreground sm:text-sm"
          >
            {placeholder}
          </p>
        )}
        <div ref={ownHost}>
          <EditorContent editor={editor} className="max-h-40 overflow-y-auto" />
        </div>
        {/* Above the box, never over it: what is being typed has to stay
            readable while the list narrows. */}
        {pickerOpen && (
          <ul
            id={listId}
            role="listbox"
            aria-label={c("mentionList")}
            className="absolute bottom-full z-20 mb-1 max-h-48 w-64 overflow-y-auto rounded-md border bg-popover p-1 text-popover-foreground shadow-md"
          >
            {options.map((option, index) => (
              <li
                key={option.userId}
                id={optionId(option.userId)}
                role="option"
                aria-selected={index === activeIndex}
                className={cn(
                  "cursor-pointer truncate rounded px-2 py-1 text-sm",
                  index === activeIndex && "bg-accent text-accent-foreground"
                )}
                // The caret stays where it is: a click that took focus out of
                // the box would insert the name into nothing.
                onMouseDown={(event) => event.preventDefault()}
                onMouseEnter={() =>
                  setPicker((previous) =>
                    previous ? { ...previous, active: index } : previous
                  )
                }
                onClick={() => insertMention(option)}
              >
                {option.name}
              </li>
            ))}
          </ul>
        )}
        {/* Said rather than silently ignored: a picker that stops opening with
            no explanation reads as a broken one. */}
        {picker !== null && atLimit && (
          <p
            role="status"
            className="absolute bottom-full z-20 mb-1 rounded-md border bg-popover px-2 py-1 text-xs text-muted-foreground shadow-md"
          >
            {c("mentionLimit", { count: MAX_MENTIONS_PER_MESSAGE })}
          </p>
        )}
      </div>
      <AttachmentFailures items={tray.items} />
      <AttachmentRejections
        rejected={tray.rejected}
        onDismiss={tray.dismissRejected}
      />
    </TrayContext.Provider>
  )
}

/** How many more files this composer will take. */
export function remainingAttachments(tray: AttachmentTray): number {
  return NEEV_MAX_ATTACHMENTS - tray.items.length
}
