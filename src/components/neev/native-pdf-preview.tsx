"use client"

import { useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import type { PDFDocumentProxy } from "pdfjs-dist"
import { attachmentUrl } from "@/lib/neev-attachments"
import { Button } from "@/components/ui/button"

/** Render one PDF page to canvas; no annotations, links, forms or PDF scripts. */
export function NativePdfPreview({ fileKey }: { fileKey: string }) {
  const t = useTranslations("NativeContent")
  const canvas = useRef<HTMLCanvasElement>(null)
  const [pdf, setPdf] = useState<PDFDocumentProxy | null>(null)
  const [page, setPage] = useState(1)
  const [error, setError] = useState(false)
  const [rendering, setRendering] = useState(true)
  useEffect(() => {
    let cancelled = false
    let destroy: (() => Promise<void>) | undefined
    void import("pdfjs-dist")
      .then(async (engine) => {
        if (cancelled) return
        engine.GlobalWorkerOptions.workerSrc = new URL(
          "pdfjs-dist/build/pdf.worker.min.mjs",
          import.meta.url
        ).toString()
        const task = engine.getDocument({
          url: attachmentUrl(fileKey),
          disableFontFace: true,
          useSystemFonts: true,
          useWasm: false,
          withCredentials: true,
        })
        destroy = () => task.destroy()
        const document = await task.promise
        if (!cancelled) setPdf(document)
      })
      .catch(() => {
        if (!cancelled) setError(true)
      })
    return () => {
      cancelled = true
      void destroy?.()
    }
  }, [fileKey])
  useEffect(() => {
    if (!pdf) return
    let cancelled = false
    let cancelRender: (() => void) | undefined
    setRendering(true)
    void pdf
      .getPage(page)
      .then(async (documentPage) => {
        if (cancelled || !canvas.current) return
        const original = documentPage.getViewport({ scale: 1 })
        const width = Math.min(
          1200,
          canvas.current.parentElement?.clientWidth || 800
        )
        const viewport = documentPage.getViewport({
          scale: Math.min(width / original.width, 2),
        })
        canvas.current.width = viewport.width
        canvas.current.height = viewport.height
        const task = documentPage.render({
          canvas: canvas.current,
          viewport,
          annotationMode: 0,
        })
        cancelRender = () => task.cancel()
        await task.promise
        if (!cancelled) setRendering(false)
      })
      .catch(() => {
        if (!cancelled) {
          setError(true)
          setRendering(false)
        }
      })
    return () => {
      cancelled = true
      cancelRender?.()
    }
  }, [pdf, page])
  if (error) return <p role="alert">{t("previewError")}</p>
  return (
    <div className="flex h-full min-h-0 flex-col gap-3">
      <div className="flex items-center justify-center gap-3">
        <Button
          variant="outline"
          size="sm"
          disabled={!pdf || page <= 1 || rendering}
          onClick={() => setPage((n) => n - 1)}
        >
          {t("previousFilePage")}
        </Button>
        <span className="text-sm tabular-nums">
          {pdf ? `${page} / ${pdf.numPages}` : t("loadingFiles")}
        </span>
        <Button
          variant="outline"
          size="sm"
          disabled={!pdf || page >= pdf.numPages || rendering}
          onClick={() => setPage((n) => n + 1)}
        >
          {t("nextFilePage")}
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-auto bg-muted/40 p-3">
        {rendering && (
          <span role="status" className="text-sm text-muted-foreground">
            {t("loadingFiles")}
          </span>
        )}
        <canvas
          ref={canvas}
          className="mx-auto max-w-full bg-white"
          aria-label={t("filePreviewPage", { page })}
        />
      </div>
    </div>
  )
}
