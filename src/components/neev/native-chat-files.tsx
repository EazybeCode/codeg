"use client"

import { useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { FileText, Loader2 } from "lucide-react"
import {
  listResourceChatFiles,
  listAgentSessionFiles,
  formatFileSize,
} from "@/lib/neev-session-files"
import { listTrashedChatFiles } from "@/lib/neev-file-actions"
import type { NeevAttachment } from "@/lib/neev-attachments"
import { FileActions } from "./native-file-actions"

interface ChatFilesListProps {
  id: string
  kind: "chat" | "agent"
  scopeKey: string | null
  isRunning?: boolean
}

/** Only mounted when expanded. Reads current authorized files, never bytes. */
export function ChatFilesList({
  id,
  kind,
  scopeKey,
  isRunning = false,
}: ChatFilesListProps) {
  const t = useTranslations("NativeContent")
  const [files, setFiles] = useState<NeevAttachment[] | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<unknown>(null)
  const [trash, setTrash] = useState(false)
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
        const result = await (trash
          ? listTrashedChatFiles(kind, id, controller.signal)
          : kind === "chat"
            ? listResourceChatFiles(id, controller.signal)
            : listAgentSessionFiles(id, controller.signal))
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
  }, [id, kind, scopeKey, isRunning, revision, trash])
  return (
    <div>
      <div className="flex justify-end px-2">
        <button
          type="button"
          className="py-1 text-[11px] text-muted-foreground hover:text-foreground"
          onClick={() => setTrash((value) => !value)}
        >
          {t(trash ? "activeFiles" : "fileTrash")}
        </button>
      </div>
      {loading ? (
        <div className="flex items-center gap-1.5 px-2 py-1 text-xs text-muted-foreground">
          <Loader2 aria-hidden className="size-3 animate-spin" />
          <span>{t("loadingFiles")}</span>
        </div>
      ) : error ? (
        <div role="alert" className="px-2 py-1 text-xs text-destructive">
          {t("filesError")}
        </div>
      ) : !files?.length ? (
        <div className="px-2 py-1 text-xs text-muted-foreground">
          {t(trash ? "noDeletedFiles" : "noGeneratedFiles")}
        </div>
      ) : (
        <ul className="space-y-0.5">
          {files.map((file) => (
            <li key={`${trash}:${file.key}`}>
              <FileActions
                file={file}
                archived={trash}
                className="w-full text-xs text-muted-foreground"
              >
                <FileText aria-hidden className="size-3 shrink-0" />
                <span className="min-w-0 flex-1 truncate">{file.fileName}</span>
                <span className="shrink-0 text-[10px] text-muted-foreground/70">
                  {formatFileSize(file.byteLength)}
                </span>
              </FileActions>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
