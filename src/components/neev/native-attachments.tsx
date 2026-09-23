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

import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { FileText, Paperclip, X } from "lucide-react"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { NativeClientError } from "@/lib/neev-client"
import {
  admitFiles,
  attachmentUrl,
  dragCarriesFiles,
  filesFromDrop,
  filesFromPaste,
  formatBytes,
  NEEV_MAX_ATTACHMENTS,
  pasteIsFileOnly,
  uploadRejection,
  type NeevAttachment,
  type NeevAttachmentRejection,
  type NeevAttachmentType,
  type NeevRejectedFile,
} from "@/lib/neev-attachments"

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
  /** An object URL for images, so a preview needs no round trip. */
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
  /** Take these files, as far as the limits allow, and start uploading them. */
  add: (files: readonly File[]) => Promise<void>
  remove: (id: string) => void
  retry: (id: string) => void
  clear: () => void
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
  takeKeys: (uploader?: AttachmentUploader) => Promise<string[]>
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
  const previews = useRef(new Set<string>())
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
  useEffect(() => {
    alive.current = true
    // The maps themselves outlive every render; capturing them is what the
    // cleanup needs, and they are never replaced.
    const inFlight = flights.current
    const urls = previews.current
    return () => {
      alive.current = false
      for (const controller of inFlight.values()) controller.abort()
      inFlight.clear()
      for (const url of urls) URL.revokeObjectURL(url)
      urls.clear()
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
      if (disabled || files.length === 0 || !alive.current) return
      if (dropped !== era.current) return
      const { accepted, rejected: refused } = await admitFiles(
        files,
        itemsRef.current
      )
      if (!alive.current || dropped !== era.current) return
      if (refused.length) setRejected((old) => [...old, ...refused])
      if (!accepted.length) return
      const staged = accepted.map(({ file, contentType }) => {
        const previewUrl = contentType.startsWith("image/")
          ? URL.createObjectURL(file)
          : null
        if (previewUrl) previews.current.add(previewUrl)
        return {
          id: localId(),
          file,
          name: file.name.trim(),
          size: file.size,
          contentType,
          previewUrl,
          status: "queued" as const,
          attachment: null,
          reason: null,
          error: null,
        }
      })
      // Both refs and state: the next queued drop measures against these
      // before React has re-rendered.
      itemsRef.current = [...itemsRef.current, ...staged]
      setItems(itemsRef.current)
      const uploader = uploadRef.current
      if (!uploader) return
      await Promise.allSettled(staged.map((item) => send(item, uploader)))
    },
    [disabled, send]
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
    const going = itemsRef.current.find((item) => item.id === id)
    if (going?.previewUrl) {
      URL.revokeObjectURL(going.previewUrl)
      previews.current.delete(going.previewUrl)
    }
    itemsRef.current = itemsRef.current.filter((item) => item.id !== id)
    setItems(itemsRef.current)
  }, [])

  const clear = useCallback(() => {
    // Anything still being classified or queued belonged to what was just
    // cleared, and must not reappear in whatever is open next.
    era.current += 1
    for (const controller of flights.current.values()) controller.abort()
    flights.current.clear()
    for (const url of previews.current) URL.revokeObjectURL(url)
    previews.current.clear()
    itemsRef.current = []
    setItems([])
    setRejected([])
  }, [])

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
    async (uploader?: AttachmentUploader) => {
      const staged = itemsRef.current
      if (staged.length === 0) return []
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
    retry,
    clear,
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
    case "tooLarge":
      return t("attachmentTooLarge", { name })
    case "turnTooLarge":
      return t("attachmentTurnTooLarge", { name })
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

/** What is staged, what it is doing, and how to take it back out. */
export function AttachmentTrayView({
  tray,
  disabled = false,
}: {
  tray: AttachmentTray
  disabled?: boolean
}) {
  const t = useTranslations("NativeContent")
  const n = useTranslations("Native")
  const listId = useId()
  if (tray.items.length === 0 && tray.rejected.length === 0) return null
  return (
    <div className="space-y-1.5 px-1 pb-1.5">
      {tray.items.length > 0 && (
        <ul
          id={listId}
          aria-label={t("attachments")}
          className="flex flex-wrap gap-1.5"
        >
          {tray.items.map((item) => (
            <li
              key={item.id}
              className={cn(
                "flex max-w-56 items-center gap-2 rounded-lg border bg-muted/40 py-1 pe-1 ps-2",
                item.status === "failed" && "border-destructive/60"
              )}
            >
              {item.previewUrl ? (
                // Decorative: the name is right beside it, so announcing the
                // file twice would only make the row longer to hear.
                // eslint-disable-next-line @next/next/no-img-element
                <img
                  src={item.previewUrl}
                  alt=""
                  className="size-8 shrink-0 rounded object-cover"
                />
              ) : (
                <span className="flex size-8 shrink-0 items-center justify-center rounded bg-muted">
                  <FileText aria-hidden className="size-4" />
                </span>
              )}
              <span className="min-w-0 flex-1 py-0.5">
                <span className="block truncate text-xs" title={item.name}>
                  {item.name}
                </span>
                <span className="block text-[11px] text-muted-foreground">
                  {item.status === "uploading" ? (
                    <span role="status">{t("attachmentUploading")}</span>
                  ) : item.status === "failed" ? (
                    <span className="text-destructive">
                      {item.reason
                        ? rejectionMessage(t, item.reason, item.name)
                        : t("attachmentFailed")}
                    </span>
                  ) : (
                    formatBytes(item.size)
                  )}
                </span>
              </span>
              {item.status === "failed" && (
                <Button
                  type="button"
                  size="sm"
                  variant="ghost"
                  className="h-6 shrink-0 px-1.5 text-[11px]"
                  onClick={() => tray.retry(item.id)}
                >
                  {n("retry")}
                </Button>
              )}
              <Button
                type="button"
                size="icon"
                variant="ghost"
                className="size-6 shrink-0"
                aria-label={t("attachmentRemove", { name: item.name })}
                title={t("attachmentRemove", { name: item.name })}
                disabled={disabled}
                onClick={() => tray.remove(item.id)}
              >
                <X aria-hidden className="size-3.5" />
              </Button>
            </li>
          ))}
        </ul>
      )}
      {tray.rejected.length > 0 && (
        <div className="flex items-start gap-2">
          <ul role="alert" className="min-w-0 flex-1 space-y-0.5">
            {tray.rejected.slice(0, SHOWN_REJECTIONS).map((item, index) => (
              <li
                key={`${item.name}-${index}`}
                className="text-[11px] text-muted-foreground"
              >
                {rejectionMessage(t, item.reason, item.name)}
              </li>
            ))}
            {tray.rejected.length > SHOWN_REJECTIONS && (
              // Counted, never silently dropped: the rest are still refusals.
              <li className="text-[11px] text-muted-foreground">
                {t("attachmentMoreRejected", {
                  number: tray.rejected.length - SHOWN_REJECTIONS,
                })}
              </li>
            )}
          </ul>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="h-6 shrink-0 px-1.5 text-[11px]"
            onClick={tray.dismissRejected}
          >
            {t("attachmentDismiss")}
          </Button>
        </div>
      )}
    </div>
  )
}

/**
 * The attachments a message or a turn was actually sent with.
 *
 * Read from the server's stored metadata, so they survive a reload — that is
 * the whole point of rendering these rather than the staged list. An image is
 * shown; anything else is a link that downloads, because the server serves
 * these as attachments with `nosniff` and framing a PDF from the same origin
 * would be a different promise than the one it makes.
 */
export function SentAttachments({
  attachments,
}: {
  attachments: readonly NeevAttachment[]
}) {
  const t = useTranslations("NativeContent")
  const images = useMemo(
    () => attachments.filter((item) => item.contentType.startsWith("image/")),
    [attachments]
  )
  const files = useMemo(
    () => attachments.filter((item) => !item.contentType.startsWith("image/")),
    [attachments]
  )
  if (attachments.length === 0) return null
  return (
    <ul aria-label={t("attachments")} className="flex flex-wrap gap-2 py-1">
      {images.map((item) => (
        <li key={item.key}>
          {/* Same-origin and ACL-served: the endpoint re-checks the bound
              chat's audience on every read. */}
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img
            src={attachmentUrl(item.key)}
            alt={item.fileName}
            title={item.fileName}
            className="max-h-40 rounded-lg border object-contain"
          />
        </li>
      ))}
      {files.map((item) => (
        <li key={item.key}>
          <a
            href={attachmentUrl(item.key)}
            download={item.fileName}
            className="flex max-w-56 items-center gap-2 rounded-lg border px-2 py-1.5 hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
          >
            <FileText aria-hidden className="size-4 shrink-0" />
            <span className="min-w-0 flex-1">
              <span className="block truncate text-xs">{item.fileName}</span>
              <span className="block text-[11px] text-muted-foreground">
                {formatBytes(item.byteLength)}
              </span>
            </span>
          </a>
        </li>
      ))}
    </ul>
  )
}
