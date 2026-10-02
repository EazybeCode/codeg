"use client"

import {
  lazy,
  Suspense,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react"
import { useTranslations } from "next-intl"
import { Download, MoreHorizontal, Trash2 } from "lucide-react"
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { Button } from "@/components/ui/button"
import { attachmentUrl, type NeevAttachment } from "@/lib/neev-attachments"
import {
  archiveChatFile,
  previewKind,
  readTextPreview,
  readHtmlPreview,
} from "@/lib/neev-file-actions"
import { useOptionalNativeAuth } from "./native-auth-provider"
import { useTabVisible } from "./native-suspended-surface"
import { NativeError } from "./native-boundary"
import { cn } from "@/lib/utils"
const PdfPreview = lazy(() =>
  import("./native-pdf-preview").then((module) => ({
    default: module.NativePdfPreview,
  }))
)

function FilePreview({ file: incomingFile }: { file: NeevAttachment }) {
  const { key, fileName, contentType, byteLength } = incomingFile
  // Polling returns new attachment objects. Preserve an in-flight download
  // unless the actual attachment metadata changes.
  const file = useMemo(
    () => ({ key, fileName, contentType, byteLength }),
    [key, fileName, contentType, byteLength]
  )
  const t = useTranslations("NativeContent")
  const kind = previewKind(file)
  const [text, setText] = useState<{ text: string; truncated: boolean } | null>(
    null
  )
  const [html, setHtml] = useState<{ html: string; truncated: boolean } | null>(
    null
  )
  const [failed, setFailed] = useState(false)
  const [received, setReceived] = useState(0)
  const [attempt, setAttempt] = useState(0)
  useEffect(() => {
    if (kind !== "text") return
    const controller = new AbortController()
    void readTextPreview(file, controller.signal)
      .then((value) => {
        if (!controller.signal.aborted) setText(value)
      })
      .catch(() => {
        if (!controller.signal.aborted) setFailed(true)
      })
    return () => controller.abort()
  }, [file, kind])
  useEffect(() => {
    if (kind !== "html") return
    const controller = new AbortController()
    void readHtmlPreview(file, controller.signal, setReceived)
      .then((value) => {
        if (!controller.signal.aborted) setHtml(value)
      })
      .catch(() => {
        if (!controller.signal.aborted) setFailed(true)
      })
    return () => controller.abort()
  }, [file, kind, attempt])
  if (failed)
    return (
      <div className="space-y-3">
        <p role="alert">{t("previewError")}</p>
        {kind === "html" && (
          <Button
            variant="outline"
            onClick={() => {
              setFailed(false)
              setReceived(0)
              setAttempt((value) => value + 1)
            }}
          >
            {t("retryPreview")}
          </Button>
        )}
      </div>
    )
  if (kind === "unsupported")
    return (
      <p className="p-4 text-sm text-muted-foreground">
        {t("previewUnsupported")}
      </p>
    )
  if (kind === "pdf")
    return (
      <Suspense fallback={<p>{t("loadingFiles")}</p>}>
        <PdfPreview fileKey={file.key} />
      </Suspense>
    )
  if (kind === "image")
    return (
      <div className="flex h-full items-center justify-center overflow-auto bg-muted/30 p-4">
        {/* eslint-disable-next-line @next/next/no-img-element */}
        <img
          src={attachmentUrl(file.key)}
          alt={file.fileName}
          onError={() => setFailed(true)}
          className="max-h-full max-w-full object-contain"
        />
      </div>
    )
  if (kind === "html") {
    if (!html)
      return (
        <div className="max-w-sm space-y-3 py-4" role="status">
          <p className="text-sm text-muted-foreground">
            {t("previewTransfer", {
              received: (received / 1024 / 1024).toFixed(1),
              total: (byteLength / 1024 / 1024).toFixed(1),
            })}
          </p>
          <progress
            className="h-2 w-full accent-primary"
            value={received}
            max={Math.max(byteLength, received, 1)}
            aria-label={t("loadingFiles")}
          />
        </div>
      )
    return (
      <div className="h-full overflow-auto">
        {html.truncated && (
          <p className="mb-3 text-sm text-muted-foreground">
            {t("htmlPreviewTruncated")}
          </p>
        )}
        <p className="mb-3 text-sm text-muted-foreground">
          {t("htmlStaticPreview")}
        </p>
        <iframe
          title={file.fileName}
          srcDoc={html.html}
          sandbox=""
          className="size-full min-h-96 rounded-md border bg-white"
        />
      </div>
    )
  }
  if (!text) return <p role="status">{t("loadingFiles")}</p>
  return (
    <div className="h-full overflow-auto">
      {text.truncated && (
        <p className="mb-3 text-sm text-muted-foreground">
          {t("textPreviewTruncated")}
        </p>
      )}
      <pre className="whitespace-pre-wrap break-words p-4 font-mono text-sm">
        {text.text}
      </pre>
    </div>
  )
}

/** Same file actions in transcript chips and sidebar children. */
export function FileActions({
  file,
  children,
  className,
  archived = false,
}: {
  file: NeevAttachment
  children: ReactNode
  className?: string
  archived?: boolean
}) {
  const t = useTranslations("NativeContent")
  const auth = useOptionalNativeAuth()
  const visible = useTabVisible()
  const [open, setOpen] = useState(false)
  const [deleted, setDeleted] = useState(archived)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>(null)
  useEffect(() => {
    const changed = (event: Event) => {
      const detail = (event as CustomEvent<{ key: string; archived: boolean }>)
        .detail
      if (detail?.key === file.key) {
        setDeleted(detail.archived)
        setOpen(false)
      }
    }
    window.addEventListener("neev-files-changed", changed)
    return () => window.removeEventListener("neev-files-changed", changed)
  }, [file.key])
  useEffect(() => {
    if (!visible) setOpen(false)
  }, [visible])
  async function archive(value: boolean) {
    if (!auth?.session?.csrf) return
    setBusy(true)
    setError(null)
    try {
      await archiveChatFile(file.key, value, auth?.session.csrf)
      setDeleted(value)
      setOpen(false)
    } catch (cause) {
      setError(cause)
    } finally {
      setBusy(false)
    }
  }
  // Deleted chips are hidden in chat context; only shown in main trash
  if (deleted && !archived) return null
  return (
    <span
      className={cn(
        "inline-flex min-w-0 max-w-full flex-wrap items-center gap-0.5",
        className
      )}
    >
      {deleted ? (
        <>
          <span
            className="min-w-0 flex-1 truncate text-xs text-muted-foreground line-through"
            title={file.fileName}
          >
            {file.fileName}
          </span>
          <Button
            variant="ghost"
            size="sm"
            disabled={busy || !auth?.session?.csrf}
            onClick={() => void archive(false)}
          >
            {t("restoreFile")}
          </Button>
        </>
      ) : (
        <>
          <a
            href={attachmentUrl(file.key)}
            download={file.fileName}
            onClick={(event) => {
              event.preventDefault()
              setOpen(true)
            }}
            title={file.fileName}
            className="inline-flex min-w-0 flex-1 items-center gap-1.5 rounded px-1 py-1 text-start hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
          >
            {children}
          </a>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button
                type="button"
                aria-label={t("fileActions", { name: file.fileName })}
                className="flex size-6 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-accent"
              >
                <MoreHorizontal aria-hidden className="size-3.5" />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => setOpen(true)}>
                {t("openFile")}
              </DropdownMenuItem>
              <DropdownMenuItem asChild>
                <a href={attachmentUrl(file.key)} download={file.fileName}>
                  <Download aria-hidden className="size-3.5" />
                  {t("downloadFile")}
                </a>
              </DropdownMenuItem>
              <DropdownMenuItem
                disabled={busy || !auth?.session?.csrf}
                onSelect={() => void archive(true)}
              >
                <Trash2 aria-hidden className="size-3.5" />
                {t("deleteFile")}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </>
      )}
      {error !== null && (
        <span className="w-full text-xs">
          <NativeError error={error} />
        </span>
      )}
      <Dialog open={open && visible && !deleted} onOpenChange={setOpen}>
        <DialogContent className="flex h-[90dvh] w-[95vw] max-w-6xl flex-col gap-3 sm:max-w-6xl">
          <DialogTitle className="break-all pe-8 text-base">
            {file.fileName}
          </DialogTitle>
          <DialogDescription className="sr-only">
            {t("filePreview")}
          </DialogDescription>
          <div className="flex items-center gap-2">
            <Button asChild variant="outline" size="sm">
              <a href={attachmentUrl(file.key)} download={file.fileName}>
                <Download aria-hidden className="size-3.5" />
                {t("downloadFile")}
              </a>
            </Button>
            <Button
              variant="ghost"
              size="sm"
              disabled={busy || !auth?.session?.csrf}
              onClick={() => void archive(true)}
            >
              <Trash2 aria-hidden className="size-3.5" />
              {t("deleteFile")}
            </Button>
          </div>
          <div className="min-h-0 flex-1">
            <FilePreview key={file.key} file={file} />
          </div>
        </DialogContent>
      </Dialog>
    </span>
  )
}
