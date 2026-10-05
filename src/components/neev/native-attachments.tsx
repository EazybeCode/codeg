"use client"

// Files in a composer: dragged in, pasted, or picked — and then actually sent.
//
// The whole surface is built on one refusal: a name is not a file. A staged
// attachment carries no meaning until the server has its bytes and has answered
// with a key, so `takeKeys` is the only way out of here and it either returns
// keys for every staged file or throws. Nothing downstream is ever handed a
// filename to mention in a prompt.
//
// It is deliberately ignorant of WHERE the bytes go. A team chat uploads to the
// chat, a saved agent chat to the session, and a chat that does not exist yet
// cannot upload at all until its first message creates one — so the uploader is
// a parameter, and a tray with no uploader simply holds its files until send.

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { FileText, ImageIcon, ImageOff, Paperclip } from "lucide-react"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { FileActions } from "./native-file-actions"
import { NativeClientError } from "@/lib/neev-client"
import {
  admitFiles,
  attachmentUrl,
  dragCarriesFiles,
  filesFromDrop,
  filesFromPaste,
  formatBytes,
  imagePreviewDataUrl,
  NEEV_MAX_ATTACHMENTS,
  pasteIsFileOnly,
  uploadRejection,
  type NeevAttachment,
  type NeevAttachmentRejection,
  type NeevAttachmentType,
  type NeevRejectedFile,
} from "@/lib/neev-attachments"
import {
  attachmentMarker,
  mentionText,
  parseSentPrompt,
  type PromptSegment,
} from "@/lib/neev-inline-prompt"
import { countCodePoints, type MentionSpan } from "@/lib/neev-mentions"
import { MentionedText } from "./native-mentions"

/** Where one file's bytes go. Resolves to the server's own row for them. */
export type AttachmentUploader = (
  file: File,
  signal?: AbortSignal
) => Promise<NeevAttachment>

export type DraftAttachmentStatus =
  | "queued"
  | "uploading"
  | "uploaded"
  | "failed"

export interface DraftAttachment {
  /** Local only; the server's key appears on `attachment` once it is stored. */
  id: string
  file: File
  name: string
  size: number
  contentType: NeevAttachmentType
  /**
   * A bounded `data:` URL for a small image, so a preview needs no round trip.
   *
   * Never an object URL: this app's CSP is `img-src 'self' data:`, so a `blob:`
   * source is blocked and renders as a broken image. `null` is the normal case
   * for anything that is not a small image, and the chip then shows its type
   * icon — the file itself is unaffected either way.
   */
  previewUrl: string | null
  status: DraftAttachmentStatus
  attachment: NeevAttachment | null
  /** Why the upload was refused, when the refusal is about this file. */
  reason: NeevAttachmentRejection | null
  /** Anything else that went wrong, for the shared error surface. */
  error: unknown
}

let counter = 0
function localId(): string {
  counter += 1
  return `attachment-${counter}`
}

export interface AttachmentTray {
  items: DraftAttachment[]
  rejected: NeevRejectedFile[]
  /**
   * Take these files, as far as the limits allow, and start uploading them.
   *
   * Resolves with what was actually staged, in order, because the composer has
   * to put a node for each one where the caret is — and a file that was refused
   * must not leave a chip behind in the sentence.
   */
  add: (files: readonly File[]) => Promise<DraftAttachment[]>
  remove: (id: string) => void
  /**
   * Put a released file back, under the id it already had.
   *
   * For undo, and only for undo: a reference that comes back into the sentence
   * has to come back attached to the same bytes, or it is a reference to
   * nothing. A file the server already stored keeps its key and is not sent up
   * a second time.
   */
  restore: (item: DraftAttachment) => void
  retry: (id: string) => void
  clear: () => void
  /**
   * Bumped by `clear`, and by nothing else.
   *
   * What was cleared is gone for good — a sent message's files, or another
   * chat's — so anything holding on to them for an undo has to let go.
   */
  generation: number
  dismissRejected: () => void
  /** An upload is in flight, so sending would race it. */
  uploading: boolean
  /** Something staged has no key, so a send would drop it silently. */
  blocked: boolean
  /** Nothing staged at all. */
  empty: boolean
  /**
   * The keys to send, uploading anything still staged first.
   *
   * Throws rather than returning a short list: a message that quietly leaves
   * out the file someone attached is worse than one that refuses to send.
   */
  takeKeys: (
    uploader?: AttachmentUploader,
    /**
     * Local ids in the order the message mentions them.
     *
     * The tray holds files in the order they arrived; the sentence may mention
     * them in another, and the server stores the keys in the order it is given.
     * Anything staged but not listed is still uploaded and still sent — the
     * ordering is a preference, never a filter.
     */
    order?: readonly string[]
  ) => Promise<string[]>
}

/**
 * Hold a composer's files, and the truth about each one's upload.
 *
 * `upload` may be null — a new agent chat has no session to upload to until its
 * first message creates one. Files then stay queued and go up inside `takeKeys`
 * with the uploader the caller finally has.
 */
export function useAttachmentTray(options: {
  upload: AttachmentUploader | null
  /** Archived, unauthorized, no runtime: nothing may be staged. */
  disabled?: boolean
}): AttachmentTray {
  const { upload, disabled = false } = options
  const [items, setItems] = useState<DraftAttachment[]>([])
  const [rejected, setRejected] = useState<NeevRejectedFile[]>([])
  // The list as it is RIGHT NOW, for callbacks that must not see a stale render
  // — and for deciding whether an upload that has just come back still belongs
  // to anything.
  const itemsRef = useRef<DraftAttachment[]>([])
  itemsRef.current = items
  const uploadRef = useRef<AttachmentUploader | null>(upload)
  uploadRef.current = upload
  // One controller per staged file: removing it, or leaving the conversation,
  // aborts its upload rather than letting the answer arrive for a composer that
  // no longer exists.
  const flights = useRef(new Map<string, AbortController>())
  // Two drops landing together must not both measure the same empty tray, so
  // admission runs one at a time and each sees what the previous one staged.
  const queue = useRef<Promise<unknown>>(Promise.resolve())
  const alive = useRef(true)
  /**
   * Which tray these files were dropped into.
   *
   * Not every composer unmounts when the conversation changes — the saved agent
   * chat view is re-rendered with a new session rather than replaced — so
   * `alive` alone does not say "this is a different chat now". Classification
   * reads the whole file to decide whether it is text, which for a few megabytes
   * is long enough to switch chats in, and admission can also be sitting in the
   * queue behind another drop. `clear()` bumps this, and work that started in an
   * earlier era is dropped instead of landing in the chat that is open now.
   */
  const era = useRef(0)
  const [generation, setGeneration] = useState(0)
  useEffect(() => {
    alive.current = true
    // The map itself outlives every render; capturing it is what the cleanup
    // needs, and it is never replaced.
    const inFlight = flights.current
    return () => {
      alive.current = false
      for (const controller of inFlight.values()) controller.abort()
      inFlight.clear()
      // Previews are `data:` URLs held in state and are collected with it.
      // There is nothing to revoke, which is the other reason they are not
      // object URLs: the old lifetime bookkeeping is simply gone.
    }
  }, [])

  /** Apply a change only while this file is still staged here. */
  const patch = useCallback((id: string, change: Partial<DraftAttachment>) => {
    if (!alive.current) return
    setItems((old) =>
      old.some((item) => item.id === id)
        ? old.map((item) => (item.id === id ? { ...item, ...change } : item))
        : old
    )
  }, [])

  const send = useCallback(
    async (
      item: DraftAttachment,
      uploader: AttachmentUploader
    ): Promise<NeevAttachment> => {
      const controller = new AbortController()
      flights.current.get(item.id)?.abort()
      flights.current.set(item.id, controller)
      patch(item.id, { status: "uploading", reason: null, error: null })
      try {
        const stored = await uploader(item.file, controller.signal)
        // Removed while it was in flight, or the composer is gone: the bytes
        // are the server's problem now, and this key belongs to no message.
        if (controller.signal.aborted || !alive.current)
          throw new NativeClientError("aborted")
        patch(item.id, { status: "uploaded", attachment: stored })
        return stored
      } catch (cause) {
        if (!controller.signal.aborted)
          patch(item.id, {
            status: "failed",
            reason: uploadRejection(cause),
            error: uploadRejection(cause) === null ? cause : null,
          })
        throw cause
      } finally {
        if (flights.current.get(item.id) === controller)
          flights.current.delete(item.id)
      }
    },
    [patch]
  )

  const stage = useCallback(
    async (files: readonly File[], dropped: number) => {
      // Checked on the way in as well as on the way out: a drop that waited in
      // the queue behind another one may already belong to a closed chat.
      if (disabled || files.length === 0 || !alive.current) return []
      if (dropped !== era.current) return []
      const { accepted, rejected: refused } = await admitFiles(
        files,
        itemsRef.current
      )
      if (!alive.current || dropped !== era.current) return []
      if (refused.length) setRejected((old) => [...old, ...refused])
      if (!accepted.length) return []
      const staged = accepted.map(({ file, contentType }) => ({
        id: localId(),
        file,
        name: file.name.trim(),
        size: file.size,
        contentType,
        // Filled in below, if there is one to fill in. The chip appears now,
        // with its file reference, rather than waiting on an encode.
        previewUrl: null as string | null,
        status: "queued" as const,
        attachment: null,
        reason: null,
        error: null,
      }))
      // Both refs and state: the next queued drop measures against these
      // before React has re-rendered.
      itemsRef.current = [...itemsRef.current, ...staged]
      setItems(itemsRef.current)
      // Deliberately not awaited before the upload starts: bytes going up is
      // the part that matters, and a thumbnail arriving a moment later costs
      // nothing. `patch` drops it if the file has since been removed, and an
      // era change means the chat it belonged to is gone.
      void Promise.all(
        staged.map(async (item) => {
          const url = await imagePreviewDataUrl(item.file, item.contentType)
          if (url && alive.current && dropped === era.current)
            patch(item.id, { previewUrl: url })
        })
      )
      const uploader = uploadRef.current
      // Deliberately not awaited: the caller puts a chip in the sentence the
      // moment the file is admitted, and waiting for the bytes to land first
      // would leave the caret sitting in front of nothing for the length of an
      // upload. Each chip says what its own upload is doing.
      if (uploader)
        void Promise.allSettled(staged.map((item) => send(item, uploader)))
      return staged
    },
    [disabled, patch, send]
  )

  const add = useCallback(
    (files: readonly File[]) => {
      // Stamped at the drop, not when the queue gets to it.
      const dropped = era.current
      const next = queue.current.then(() => stage(files, dropped))
      queue.current = next.catch(() => undefined)
      return next
    },
    [stage]
  )

  const drop = useCallback((id: string) => {
    flights.current.get(id)?.abort()
    flights.current.delete(id)
    itemsRef.current = itemsRef.current.filter((item) => item.id !== id)
    setItems(itemsRef.current)
  }, [])

  const clear = useCallback(() => {
    // Anything still being classified or queued belonged to what was just
    // cleared, and must not reappear in whatever is open next.
    era.current += 1
    setGeneration((value) => value + 1)
    for (const controller of flights.current.values()) controller.abort()
    flights.current.clear()
    itemsRef.current = []
    setItems([])
    setRejected([])
  }, [])

  const restore = useCallback(
    (item: DraftAttachment) => {
      if (!alive.current) return
      if (itemsRef.current.length >= NEEV_MAX_ATTACHMENTS) return
      if (itemsRef.current.some((entry) => entry.id === item.id)) return
      // Already stored means already paid for: the key is still good, and
      // re-sending the bytes would leave a second copy behind.
      const back: DraftAttachment = item.attachment
        ? { ...item, status: "uploaded", reason: null, error: null }
        : { ...item, status: "queued", attachment: null, reason: null }
      itemsRef.current = [...itemsRef.current, back]
      setItems(itemsRef.current)
      const uploader = uploadRef.current
      if (!back.attachment && uploader)
        void send(back, uploader).catch(() => {
          // The chip says so.
        })
    },
    [send]
  )

  const retry = useCallback(
    (id: string) => {
      const item = itemsRef.current.find((entry) => entry.id === id)
      const uploader = uploadRef.current
      // Only a file with no key is re-sent: one already stored would become a
      // second copy of itself.
      if (!item || !uploader || item.attachment) return
      void send(item, uploader).catch(() => {
        // The chip already says so.
      })
    },
    [send]
  )

  const takeKeys = useCallback(
    async (uploader?: AttachmentUploader, order?: readonly string[]) => {
      const held = itemsRef.current
      if (held.length === 0) return []
      // The sentence's order first, then anything it did not mention. A file
      // left out of the list is still someone's file: it goes up and it is
      // sent, just at the end.
      const staged = order
        ? [
            ...order.flatMap((id) => held.filter((item) => item.id === id)),
            ...held.filter((item) => !order.includes(item.id)),
          ]
        : held
      const using = uploader ?? uploadRef.current
      const keys: string[] = []
      for (const item of staged) {
        // Already stored: reuse the key. A send that failed downstream must
        // never upload the same bytes twice.
        if (item.attachment) {
          keys.push(item.attachment.key)
          continue
        }
        if (!using) throw new NativeClientError("invalid_request")
        keys.push((await send(item, using)).key)
      }
      return keys
    },
    [send]
  )

  return {
    items,
    rejected,
    add,
    remove: drop,
    restore,
    retry,
    clear,
    generation,
    dismissRejected: () => setRejected([]),
    uploading: items.some((item) => item.status === "uploading"),
    blocked: items.some((item) => item.status !== "uploaded"),
    empty: items.length === 0,
    takeKeys,
  }
}

/**
 * A drop target that does not let the browser navigate.
 *
 * Dropping a PDF anywhere on a page normally replaces the app with that PDF,
 * losing whatever was typed. The window-level handlers cancel that for the
 * whole document while a composer is mounted; the element-level ones are what
 * actually accept the file. Only file drags are touched — dragging selected
 * text inside the page keeps working exactly as it did.
 */
export function useFileDrop(options: {
  onFiles: (files: File[]) => void
  disabled?: boolean
}) {
  const { onFiles, disabled = false } = options
  const [over, setOver] = useState(false)
  // Drag events fire for every child element; a counter is what keeps the
  // highlight from flickering as the pointer crosses the textarea.
  const depth = useRef(0)

  useEffect(() => {
    const swallow = (event: DragEvent) => {
      if (!dragCarriesFiles(event.dataTransfer)) return
      event.preventDefault()
      if (event.type === "drop" && event.dataTransfer)
        event.dataTransfer.dropEffect = "none"
    }
    window.addEventListener("dragover", swallow)
    window.addEventListener("drop", swallow)
    return () => {
      window.removeEventListener("dragover", swallow)
      window.removeEventListener("drop", swallow)
    }
  }, [])

  const reset = () => {
    depth.current = 0
    setOver(false)
  }

  return {
    /** Whether a file is currently hovering this composer. */
    over: over && !disabled,
    handlers: {
      onDragEnter: (event: React.DragEvent) => {
        if (disabled || !dragCarriesFiles(event.dataTransfer)) return
        event.preventDefault()
        depth.current += 1
        setOver(true)
      },
      onDragOver: (event: React.DragEvent) => {
        if (disabled || !dragCarriesFiles(event.dataTransfer)) return
        // Without this the drop never fires and the browser navigates instead.
        event.preventDefault()
        event.dataTransfer.dropEffect = "copy"
      },
      onDragLeave: (event: React.DragEvent) => {
        if (disabled || !dragCarriesFiles(event.dataTransfer)) return
        depth.current -= 1
        if (depth.current <= 0) reset()
      },
      onDrop: (event: React.DragEvent) => {
        if (!dragCarriesFiles(event.dataTransfer)) return
        event.preventDefault()
        event.stopPropagation()
        reset()
        if (disabled) return
        const files = filesFromDrop(event.dataTransfer)
        if (files.length) onFiles(files)
      },
    },
  }
}

/**
 * Take an image off the clipboard, and leave an ordinary paste alone.
 *
 * Returns whether the paste was consumed, so the caller can let the textarea
 * have it otherwise.
 */
export function handleAttachmentPaste(
  event: React.ClipboardEvent,
  onFiles: (files: File[]) => void
): boolean {
  const files = filesFromPaste(event.clipboardData)
  if (files.length === 0) return false
  onFiles(files)
  // Text on the clipboard as well means the person copied something that has
  // both; the text is what they meant, so it still goes in.
  if (pasteIsFileOnly(event.clipboardData)) {
    event.preventDefault()
    return true
  }
  return false
}

/** The paperclip: the same staging path as a drop, for people who don't drag. */
export function AttachmentButton({
  onFiles,
  disabled = false,
  remaining = NEEV_MAX_ATTACHMENTS,
}: {
  onFiles: (files: File[]) => void
  disabled?: boolean
  /** How many more this composer will take, so the picker says so. */
  remaining?: number
}) {
  const t = useTranslations("NativeContent")
  const input = useRef<HTMLInputElement>(null)
  return (
    <>
      <Button
        type="button"
        size="icon"
        variant="ghost"
        className="size-7 shrink-0 rounded-md"
        aria-label={t("attachFiles")}
        title={t("attachFiles")}
        disabled={disabled || remaining <= 0}
        onClick={() => input.current?.click()}
      >
        <Paperclip aria-hidden className="size-4" />
      </Button>
      <input
        ref={input}
        type="file"
        multiple
        // Advisory only — the bytes decide, here and on the server. It keeps a
        // system file dialog from showing files that would only be refused.
        accept="image/png,image/jpeg,image/webp,application/pdf,text/*"
        className="hidden"
        tabIndex={-1}
        aria-hidden
        onChange={(event) => {
          const files = Array.from(event.target.files ?? [])
          // Reset first: picking the same file twice in a row is a legitimate
          // thing to do, and without this the second change never fires.
          event.target.value = ""
          if (files.length) onFiles(files)
        }}
      />
    </>
  )
}

/** One reason a file was not taken, in a sentence naming the file. */
function rejectionMessage(
  t: ReturnType<typeof useTranslations<"NativeContent">>,
  reason: NeevAttachmentRejection,
  name: string
): string {
  switch (reason) {
    case "tooMany":
      return t("attachmentTooMany", { name })
    case "badName":
      return t("attachmentBadName", { name })
    case "empty":
      return t("attachmentEmpty", { name })
    case "quota":
      return t("attachmentQuota", { name })
    default:
      return t("attachmentUnsupported", { name })
  }
}

/**
 * How many refusals are worth printing above a composer.
 *
 * Dropping a folder of sixty unsupported files should say so in a line, not
 * push the conversation off the screen with sixty of them.
 */
const SHOWN_REJECTIONS = 5

/**
 * The small square at the head of a file reference.
 *
 * Three outcomes, and they are deliberately three rather than two:
 *
 *  - a thumbnail, when there is one and it loads;
 *  - a type icon, when there was never a preview to show (not an image, or an
 *    image too large to encode inline) — an ordinary state, said quietly;
 *  - a BROKEN-preview icon with a reason, when a preview was offered and the
 *    browser refused it. That is the case worth keeping visible. Falling back
 *    to the plain file icon here would have made the CSP-blocked previews this
 *    change fixes look like a design choice instead of a bug.
 *
 * The file is attached and will be sent in all three.
 */
export function AttachmentThumb({
  src,
  contentType,
  name,
  className,
}: {
  src: string | null
  contentType: string
  name: string
  className?: string
}) {
  const t = useTranslations("NativeContent")
  const [failed, setFailed] = useState(false)
  // A new source is a new attempt: the previous failure said nothing about it.
  const [attempted, setAttempted] = useState(src)
  if (attempted !== src) {
    setAttempted(src)
    setFailed(false)
  }
  const box = cn(
    "flex size-6 shrink-0 items-center justify-center rounded",
    className
  )
  if (src && !failed)
    return (
      // Decorative: the name is right beside it, so announcing the file twice
      // would only make the row longer to hear.
      // eslint-disable-next-line @next/next/no-img-element
      <img
        src={src}
        alt=""
        onError={() => setFailed(true)}
        className={cn(box, "border object-cover")}
      />
    )
  if (failed)
    return (
      <span
        className={cn(box, "bg-destructive/10 text-destructive")}
        title={t("attachmentPreviewFailed", { name })}
      >
        <ImageOff aria-hidden className="size-3.5" />
        <span className="sr-only">
          {t("attachmentPreviewFailed", { name })}
        </span>
      </span>
    )
  return (
    <span className={cn(box, "bg-muted text-muted-foreground")}>
      {contentType.startsWith("image/") ? (
        <ImageIcon aria-hidden className="size-3.5" />
      ) : (
        <FileText aria-hidden className="size-3.5" />
      )}
    </span>
  )
}

/**
 * The uploads that failed, said in full.
 *
 * The chip in the sentence says THAT a file failed and offers the retry; a
 * whole sentence explaining why would make the line it sits in unreadable. So
 * the reason is written out once, underneath, where a refusal is written.
 */
export function AttachmentFailures({
  items,
}: {
  items: readonly DraftAttachment[]
}) {
  const t = useTranslations("NativeContent")
  const failed = items.filter((item) => item.status === "failed")
  if (failed.length === 0) return null
  return (
    <ul role="alert" className="space-y-0.5 px-1 pb-1.5">
      {failed.map((item) => (
        <li key={item.id} className="text-2xs leading-4 text-muted-foreground">
          {item.reason
            ? rejectionMessage(t, item.reason, item.name)
            : t("attachmentFailed")}
        </li>
      ))}
    </ul>
  )
}

/**
 * The files this composer refused, and why.
 *
 * The staged files themselves are no longer listed here — they are IN the
 * sentence now, as nodes, which is the whole point. A refusal has no node to
 * live in: nothing was staged, so the only place left to say so is above the
 * row of controls, once, with a way to dismiss it.
 */
export function AttachmentRejections({
  rejected,
  onDismiss,
}: {
  rejected: readonly NeevRejectedFile[]
  onDismiss: () => void
}) {
  const t = useTranslations("NativeContent")
  if (rejected.length === 0) return null
  return (
    <div className="flex items-start gap-2 px-1 pb-1.5">
      <ul role="alert" className="min-w-0 flex-1 space-y-0.5">
        {rejected.slice(0, SHOWN_REJECTIONS).map((item, index) => (
          <li
            key={`${item.name}-${index}`}
            className="text-2xs leading-4 text-muted-foreground"
          >
            {rejectionMessage(t, item.reason, item.name)}
          </li>
        ))}
        {rejected.length > SHOWN_REJECTIONS && (
          // Counted, never silently dropped: the rest are still refusals.
          <li className="text-2xs leading-4 text-muted-foreground">
            {t("attachmentMoreRejected", {
              number: rejected.length - SHOWN_REJECTIONS,
            })}
          </li>
        )}
      </ul>
      <Button
        type="button"
        size="sm"
        variant="ghost"
        className="h-6 shrink-0 px-1.5 text-2xs"
        onClick={onDismiss}
      >
        {t("attachmentDismiss")}
      </Button>
    </div>
  )
}

/**
 * The attachments a message or a turn was actually sent with.
 *
 * Read from the server's stored metadata, so they survive a reload — that is
 * the whole point of rendering these rather than the staged list.
 *
 * Every one of them is the SAME compact reference, image or not. A message
 * used to inline its images at up to 160px tall, which meant three screenshots
 * pushed the reply that discussed them off the screen. What a reader needs in
 * the transcript is that the file was sent and which file it was; opening it
 * is one click on the reference. The thumbnail is served same-origin and ACL
 * checked on every read, and it degrades to a named broken-preview marker
 * rather than to silence.
 */
export function SentAttachments({
  attachments,
}: {
  attachments: readonly NeevAttachment[]
}) {
  const t = useTranslations("NativeContent")
  if (attachments.length === 0) return null
  return (
    <ul aria-label={t("attachments")} className="flex flex-wrap gap-1.5 py-1">
      {attachments.map((item) => (
        <li key={item.key}>
          <SentReference attachment={item} />
        </li>
      ))}
    </ul>
  )
}

/** One stored file, as a thing a reader can open. */
function SentReference({
  attachment,
  className,
}: {
  attachment: NeevAttachment
  className?: string
}) {
  // `attachmentUrl` validates the key and throws on one that is not shaped
  // like a key, which must not take down a transcript that is otherwise fine.
  const href = useMemo(() => {
    try {
      return attachmentUrl(attachment.key)
    } catch {
      return null
    }
  }, [attachment.key])
  const inside = (
    <>
      <AttachmentThumb
        src={href && attachment.contentType.startsWith("image/") ? href : null}
        contentType={attachment.contentType}
        name={attachment.fileName}
      />
      <span className="min-w-0 truncate text-xs">{attachment.fileName}</span>
      <span className="shrink-0 whitespace-nowrap text-2xs leading-4 text-muted-foreground">
        {formatBytes(attachment.byteLength)}
      </span>
    </>
  )
  if (!href)
    // Stored metadata this build cannot address. It is still named, because
    // the file WAS sent and the agent did read it.
    return (
      <span
        className={cn(
          "inline-flex h-8 max-w-64 items-center gap-1.5 rounded-md border border-dashed px-1 align-middle opacity-70",
          className
        )}
      >
        {inside}
      </span>
    )
  return (
    <FileActions
      file={attachment}
      className={cn(
        "h-8 max-w-72 rounded-md border px-1 align-middle",
        className
      )}
    >
      {inside}
    </FileActions>
  )
}

/**
 * A sent message, read back in the order it was written.
 *
 * The composer put a numbered marker where each file sat; `parseSentPrompt`
 * pairs marker N with the Nth stored attachment and refuses to pair anything it
 * is not sure about. So a message written before this existed, or one where
 * somebody simply typed square brackets, renders as the text they wrote with
 * its files listed after it — and a message written here renders as the
 * sentence they built, files and all, in place.
 *
 * Markdown is deliberately not applied to a message that HAS inline files:
 * block rendering would put each run of text on its own line and destroy the
 * one thing this is for.
 */
/**
 * A message on its way, drawn from what was typed: the words, each file as a
 * chip where it sat, each person as their name. Nothing here has a key yet,
 * so files are named rather than linked — an attachment-only message still
 * shows what was sent.
 */
export function OutgoingPrompt({
  segments,
  names,
}: {
  segments: readonly PromptSegment[]
  /** The staged files' names at send time, by local id. */
  names: ReadonlyMap<string, string>
}) {
  return (
    <p className="whitespace-pre-wrap break-words">
      {segments.map((segment, index) =>
        segment.kind === "text" ? (
          <span key={index}>{segment.text}</span>
        ) : segment.kind === "attachment" ? (
          <span
            key={index}
            className="mx-0.5 inline-flex max-w-full items-center gap-1 rounded-md border px-1.5 align-baseline text-xs"
          >
            <Paperclip aria-hidden className="size-3 shrink-0" />
            <span className="truncate">{names.get(segment.id) ?? ""}</span>
          </span>
        ) : (
          <span key={index} className="font-medium">
            {mentionText(segment.name)}
          </span>
        )
      )}
    </p>
  )
}

export function SentPrompt({
  body,
  attachments,
  mentions,
}: {
  body: string
  attachments: readonly NeevAttachment[]
  /**
   * The people this message named, as stored spans over the WHOLE body.
   *
   * Passed through rather than searched for: the runs of text below are pieces
   * of that body, so each one is decorated with the spans that fall inside it
   * and nothing here ever reads the text looking for an `@`.
   */
  mentions?: readonly MentionSpan[]
}) {
  const t = useTranslations("NativeContent")
  const { parts, trailing } = useMemo(
    () => parseSentPrompt(body, attachments),
    [body, attachments]
  )
  // Where each run begins in the body, in code points — the unit the spans are
  // measured in. An attachment part stands for exactly the marker that was
  // matched to it, so its width is known rather than guessed.
  const offsets = useMemo(() => {
    const starts: number[] = []
    let at = 0
    for (const part of parts) {
      starts.push(at)
      at +=
        part.kind === "text"
          ? countCodePoints(part.text)
          : countCodePoints(
              attachmentMarker(part.index + 1, attachments[part.index].fileName)
            )
    }
    return starts
  }, [parts, attachments])
  return (
    <>
      <p className="whitespace-pre-wrap break-words">
        {parts.map((part, index) =>
          part.kind === "text" ? (
            mentions?.length ? (
              <MentionedText
                key={index}
                text={part.text}
                spans={mentions}
                offset={offsets[index]}
              />
            ) : (
              <span key={index}>{part.text}</span>
            )
          ) : (
            <SentReference
              key={index}
              attachment={attachments[part.index]}
              className="mx-0.5"
            />
          )
        )}
      </p>
      {trailing.length > 0 && (
        <ul
          aria-label={t("attachments")}
          className="flex flex-wrap gap-1.5 py-1"
        >
          {trailing.map((index) => (
            <li key={attachments[index].key}>
              <SentReference attachment={attachments[index]} />
            </li>
          ))}
        </ul>
      )}
    </>
  )
}
