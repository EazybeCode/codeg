"use client"
import { useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import {
  connectWorkspaceRepository,
  createWorkspaceChatDraft,
} from "@/lib/workspace-catalog"
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { WorkspaceRepositoryPicker } from "./workspace-repository-picker"
import { WorkspaceCatalogError } from "./workspace-catalog-error"
import { useWorkspaceCatalogContext } from "./workspace-catalog-provider"

export function WorkspaceChatDialog({
  nodeId,
  onClose,
}: {
  nodeId: number
  onClose: () => void
}) {
  const t = useTranslations("WorkspaceCatalog")
  const catalog = useWorkspaceCatalogContext()!
  const path = catalog.pathFor(nodeId)
  const inherited = [...path]
    .reverse()
    .find((node) => node.repository_default)?.repository_default
  const inheritedRepository = catalog.repositories.find(
    (repo) => repo.id === inherited?.repository_id
  )
  const [override, setOverride] = useState(false)
  const [repository, setRepository] = useState({ fullName: "", branch: "" })
  const [title, setTitle] = useState("")
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>(null)
  const attempt = useRef<{ signature: string; id: string } | null>(null)
  const inFlight = useRef(false)
  const alive = useRef(true)
  useEffect(() => {
    alive.current = true
    return () => {
      alive.current = false
    }
  }, [])
  const needsSelection = !inherited || override
  const valid =
    !needsSelection || (repository.fullName.trim() && repository.branch.trim())
  async function submit(event: React.FormEvent) {
    event.preventDefault()
    if (!valid || inFlight.current) return
    inFlight.current = true
    setBusy(true)
    setError(null)
    try {
      const repo = needsSelection
        ? await connectWorkspaceRepository(
            repository.fullName.trim(),
            repository.branch.trim()
          )
        : null
      if (!alive.current) return
      const body = {
        nodeId,
        repositoryOverride: repo
          ? {
              repository_id: repo.id,
              starting_branch: repository.branch.trim(),
            }
          : null,
        title: title.trim() || null,
      }
      const signature = JSON.stringify(body)
      if (attempt.current?.signature !== signature)
        attempt.current = { signature, id: crypto.randomUUID() }
      const draft = await createWorkspaceChatDraft({
        ...body,
        requestId: attempt.current.id,
      })
      if (!alive.current) return
      catalog.expand(nodeId)
      catalog.openChat(draft.id)
      onClose()
    } catch (cause) {
      if (alive.current) setError(cause)
    } finally {
      inFlight.current = false
      if (alive.current) setBusy(false)
    }
  }
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) onClose()
      }}
    >
      <DialogContent className="max-h-[85dvh] overflow-y-auto sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{t("new_chat")}</DialogTitle>
          <DialogDescription>
            {path.map((node) => node.name).join(" / ")}
          </DialogDescription>
        </DialogHeader>
        <form className="space-y-4" onSubmit={submit}>
          <div className="space-y-1.5">
            <label htmlFor="catalog-chat-title" className="text-sm font-medium">
              {t("chatTitle")}
            </label>
            <Input
              autoFocus
              id="catalog-chat-title"
              value={title}
              disabled={busy}
              onChange={(event) => setTitle(event.target.value)}
            />
          </div>
          {inherited && (
            <div className="space-y-2 rounded-md bg-muted/50 p-3 text-xs">
              <p>
                {t("inherited")}:{" "}
                {inheritedRepository?.full_name ?? t("repository")} ·{" "}
                {inherited.starting_branch}
              </p>
              <label className="flex items-center gap-2">
                <input
                  type="checkbox"
                  disabled={busy}
                  checked={override}
                  onChange={(event) => setOverride(event.target.checked)}
                />
                {t("override")}
              </label>
            </div>
          )}
          {needsSelection && (
            <WorkspaceRepositoryPicker
              value={repository}
              onChange={setRepository}
              disabled={busy}
            />
          )}
          <p className="text-xs text-muted-foreground">{t("chatHint")}</p>
          {error != null && <WorkspaceCatalogError error={error} />}
          <div className="flex justify-end gap-2">
            <Button
              type="button"
              variant="ghost"
              disabled={busy}
              onClick={onClose}
            >
              {t("cancel")}
            </Button>
            <Button type="submit" disabled={!valid || busy}>
              {busy ? t("creating") : t("create_chat")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  )
}
