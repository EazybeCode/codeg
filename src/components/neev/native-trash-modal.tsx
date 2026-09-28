"use client"

import { useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { FileText, Loader2 } from "lucide-react"
import { formatFileSize } from "@/lib/neev-session-files"
import {
  listOrgArchivedAttachments,
  type OrgArchivedAttachment,
} from "@/lib/neev-file-actions"
import { FileActions } from "./native-file-actions"

/** Attachments within the existing main Trash; never rendered inside a chat. */
export function AttachmentTrash() {
  const t = useTranslations("NativeContent")
  const [files, setFiles] = useState<OrgArchivedAttachment[] | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<unknown>(null)
  const [revision, setRevision] = useState(0)

  useEffect(() => {
    const changed = () => setRevision((n) => n + 1)
    window.addEventListener("neev-files-changed", changed)
    return () => window.removeEventListener("neev-files-changed", changed)
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    async function load() {
      setLoading(true)
      setError(null)
      try {
        const result = await listOrgArchivedAttachments(controller.signal)
        if (!controller.signal.aborted) {
          setFiles(result)
          setLoading(false)
        }
      } catch (cause) {
        if (!controller.signal.aborted) {
          setError(cause)
          setLoading(false)
        }
      }
    }
    void load()
    return () => controller.abort()
  }, [revision])

  return (
    <section
      aria-label={t("deletedAttachments")}
      className="mt-3 border-t border-sidebar-border pt-3"
    >
      <h3 className="px-2 pb-2 text-xs font-medium text-muted-foreground">
        {t("deletedAttachments")}
      </h3>
      <div className="min-h-0 flex-1 overflow-auto">
        {loading ? (
          <div className="flex items-center gap-1.5 px-2 py-1 text-sm text-muted-foreground">
            <Loader2 aria-hidden className="size-4 animate-spin" />
            <span>{t("loadingFiles")}</span>
          </div>
        ) : error ? (
          <div role="alert" className="px-2 py-1 text-sm text-destructive">
            {t("filesError")}
          </div>
        ) : !files?.length ? (
          <div className="px-2 py-1 text-sm text-muted-foreground">
            {t("noDeletedFiles")}
          </div>
        ) : (
          <ul className="space-y-0.5">
            {files.map((file) => (
              <li key={file.key}>
                <FileActions
                  file={file}
                  archived
                  className="w-full text-sm text-muted-foreground"
                >
                  <FileText aria-hidden className="size-3.5 shrink-0" />
                  <span className="min-w-0 flex-1 truncate">
                    {file.fileName}
                  </span>
                  <span className="shrink-0 text-xs text-muted-foreground/70">
                    {formatFileSize(file.byteLength)}
                  </span>
                </FileActions>
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  )
}
