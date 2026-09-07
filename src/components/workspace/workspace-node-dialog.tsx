"use client"
import { useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import {
  createWorkspaceNode,
  connectWorkspaceRepository,
  type WorkspaceNodeKind,
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

export function WorkspaceNodeDialog({
  kind,
  parentId,
  onClose,
}: {
  kind: WorkspaceNodeKind
  parentId: number | null
  onClose: () => void
}) {
  const t = useTranslations("WorkspaceCatalog")
  const catalog = useWorkspaceCatalogContext()!
  const [name, setName] = useState("")
  const [repository, setRepository] = useState({ fullName: "", branch: "" })
  const [connect, setConnect] = useState(kind === "working_directory")
  const [busy, setBusy] = useState(false)
  const inFlight = useRef(false)
  const alive = useRef(true)
  useEffect(() => {
    alive.current = true
    return () => {
      alive.current = false
    }
  }, [])
  const [error, setError] = useState<unknown>(null)
  const valid =
    name.trim() &&
    (!connect || (repository.fullName.trim() && repository.branch.trim()))
  async function submit(event: React.FormEvent) {
    event.preventDefault()
    if (!valid || inFlight.current) return
    inFlight.current = true
    setBusy(true)
    setError(null)
    try {
      const repo = connect
        ? await connectWorkspaceRepository(
            repository.fullName.trim(),
            repository.branch.trim()
          )
        : null
      if (!alive.current) return
      const node = await createWorkspaceNode({
        parentId,
        kind,
        name: name.trim(),
        repositoryDefault: repo
          ? {
              repository_id: repo.id,
              starting_branch: repository.branch.trim(),
            }
          : null,
      })
      if (!alive.current) return
      await catalog.refresh()
      if (!alive.current) return
      if (parentId != null) catalog.expand(parentId)
      catalog.selectNode(node.id)
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
          <DialogTitle>{t(`new_${kind}`)}</DialogTitle>
          <DialogDescription>
            {parentId == null
              ? t("projectHint")
              : catalog
                  .pathFor(parentId)
                  .map((node) => node.name)
                  .join(" / ")}
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="space-y-4">
          <div className="space-y-1.5">
            <label className="text-sm font-medium" htmlFor="catalog-node-name">
              {t(`name_${kind}`)}
            </label>
            <Input
              autoFocus
              id="catalog-node-name"
              maxLength={120}
              value={name}
              disabled={busy}
              onChange={(event) => setName(event.target.value)}
            />
          </div>
          {kind === "project" && (
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={connect}
                disabled={busy}
                onChange={(event) => setConnect(event.target.checked)}
              />
              {t("optionalRepository")}
            </label>
          )}
          {connect && (
            <WorkspaceRepositoryPicker
              value={repository}
              onChange={setRepository}
              disabled={busy}
            />
          )}
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
              {busy ? t("creating") : t(`create_${kind}`)}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  )
}
