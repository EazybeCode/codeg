"use client"

// File list for chats and agent sessions in the sidebar.
//
// Mounted when a chat/session row is expanded. Fetches files on mount, shows
// loading state, then renders a compact list with download links. Files are
// agent-generated attachments only (excludes human uploads).

import { useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { Download, FileText, Loader2 } from "lucide-react"
import { cn } from "@/lib/utils"
import {
  listResourceChatFiles,
  listAgentSessionFiles,
  formatFileSize,
  type SessionFile,
} from "@/lib/neev-session-files"
import { attachmentUrl } from "@/lib/neev-attachments"

interface ChatFilesListProps {
  /** The chat or session ID. */
  id: string
  /** Whether this is a resource chat or agent session. */
  kind: "chat" | "agent"
  /** Key that changes on org/user change to reset state. */
  scopeKey: string | null
  /** Whether an agent run is currently active (for refresh). */
  isRunning?: boolean
}

/**
 * File list component for expanded chats/sessions in the sidebar.
 *
 * Fetches files on mount and when `isRunning` transitions from true to false.
 * Shows only agent-generated files (human uploads are excluded).
 */
export function ChatFilesList({
  id,
  kind,
  scopeKey,
  isRunning = false,
}: ChatFilesListProps) {
  const t = useTranslations("NativeContent")
  const [files, setFiles] = useState<SessionFile[] | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<unknown>(null)

  useEffect(() => {
    const controller = new AbortController()

    async function fetchFiles() {
      setLoading(true)
      setError(null)

      try {
        const result =
          kind === "chat"
            ? await listResourceChatFiles(id, controller.signal)
            : await listAgentSessionFiles(id, controller.signal)

        if (!controller.signal.aborted) {
          setFiles(result)
          setLoading(false)
        }
      } catch (err) {
        if (!controller.signal.aborted) {
          setError(err)
          setLoading(false)
        }
      }
    }

    void fetchFiles()

    return () => {
      controller.abort()
    }
  }, [id, kind, scopeKey, isRunning])

  if (loading) {
    return (
      <div className="flex items-center gap-1.5 px-2 py-1 text-xs text-muted-foreground">
        <Loader2 aria-hidden className="size-3 animate-spin" />
        <span>{t("loadingFiles")}</span>
      </div>
    )
  }

  if (error) {
    return (
      <div className="px-2 py-1 text-xs text-destructive">
        {t("filesError")}
      </div>
    )
  }

  if (!files || files.length === 0) {
    return (
      <div className="px-2 py-1 text-xs text-muted-foreground">
        {t("noGeneratedFiles")}
      </div>
    )
  }

  return (
    <ul className="space-y-0.5">
      {files.map((file) => (
        <li key={file.key}>
          <a
            href={attachmentUrl(file.key)}
            download={file.fileName}
            className={cn(
              "flex min-h-6 items-center gap-1.5 rounded px-2 py-0.5 text-xs",
              "text-muted-foreground hover:bg-sidebar-accent/60 hover:text-foreground",
              "focus-visible:ring-2 focus-visible:ring-ring"
            )}
            title={`${file.fileName} (${formatFileSize(file.byteLength)})`}
          >
            <FileText aria-hidden className="size-3 shrink-0" />
            <span className="min-w-0 flex-1 truncate">{file.fileName}</span>
            <span className="shrink-0 text-[10px] text-muted-foreground/70">
              {formatFileSize(file.byteLength)}
            </span>
            <Download aria-hidden className="size-3 shrink-0 opacity-50" />
          </a>
        </li>
      ))}
    </ul>
  )
}
