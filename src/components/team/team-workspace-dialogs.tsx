"use client"

import { useEffect, useId, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import type {
  CreateTeamChatInput,
  CreateTeamNodeInput,
  TeamCatalogChat,
  TeamCatalogNode,
  TeamCatalogRepository,
  TeamChatVisibility,
  TeamNodeKind,
  UpdateTeamNodeInput,
} from "@/lib/team-catalog-client"

function MutationError() {
  const t = useTranslations("TeamWorkspace")
  return (
    <p
      role="alert"
      className="rounded-lg bg-destructive/10 p-3 text-sm text-destructive"
    >
      {t("mutationError")}
    </p>
  )
}

function RepositoryFields({
  fullName,
  branch,
  disabled,
  onFullName,
  onBranch,
}: {
  fullName: string
  branch: string
  disabled: boolean
  onFullName: (value: string) => void
  onBranch: (value: string) => void
}) {
  const t = useTranslations("TeamWorkspace")
  const id = useId()
  return (
    <fieldset disabled={disabled} className="space-y-3 rounded-xl border p-3">
      <div className="space-y-1.5">
        <label htmlFor={`${id}-repository`} className="text-sm font-medium">
          {t("repository")}
        </label>
        <Input
          id={`${id}-repository`}
          placeholder="owner/repository"
          value={fullName}
          onChange={(event) => onFullName(event.target.value)}
        />
      </div>
      <div className="space-y-1.5">
        <label htmlFor={`${id}-branch`} className="text-sm font-medium">
          {t("startingBranch")}
        </label>
        <Input
          id={`${id}-branch`}
          value={branch}
          onChange={(event) => onBranch(event.target.value)}
        />
      </div>
      <p className="text-xs text-muted-foreground">{t("repositoryHint")}</p>
    </fieldset>
  )
}

export function TeamNodeDialog({
  mode,
  kind,
  parentId,
  node,
  repository,
  path,
  connectRepository,
  createNode,
  updateNode,
  onClose,
}: {
  mode: "create" | "edit"
  kind: TeamNodeKind
  parentId: number | null
  node: TeamCatalogNode | null
  repository?: TeamCatalogRepository | null
  path: string
  connectRepository: (
    fullName: string,
    startingBranch: string
  ) => Promise<TeamCatalogRepository>
  createNode: (input: CreateTeamNodeInput) => Promise<TeamCatalogNode>
  updateNode: (
    node: TeamCatalogNode,
    input: UpdateTeamNodeInput
  ) => Promise<TeamCatalogNode>
  onClose: () => void
}) {
  const t = useTranslations("TeamWorkspace")
  const [name, setName] = useState(node?.name ?? "")
  const [useRepository, setUseRepository] = useState(
    kind === "working_directory" || Boolean(node?.repositoryDefault)
  )
  const [fullName, setFullName] = useState(repository?.fullName ?? "")
  const [branch, setBranch] = useState(
    node?.repositoryDefault?.startingBranch ?? ""
  )
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState(false)
  const alive = useRef(true)
  useEffect(
    () => () => {
      alive.current = false
    },
    []
  )
  const needsRepository = kind === "working_directory" || useRepository
  const valid =
    Boolean(name.trim()) &&
    (!needsRepository || Boolean(fullName.trim() && branch.trim()))

  async function submit(event: React.FormEvent) {
    event.preventDefault()
    if (!valid || busy) return
    setBusy(true)
    setError(false)
    try {
      const connected = needsRepository
        ? await connectRepository(fullName.trim(), branch.trim())
        : null
      if (!alive.current) return
      const repositoryDefault = connected
        ? { repositoryId: connected.id, startingBranch: branch.trim() }
        : null
      if (mode === "edit" && node) {
        await updateNode(node, {
          expectedRevision: node.revision,
          parentId: node.parentId,
          name: name.trim(),
          repositoryDefault,
        })
      } else {
        await createNode({
          parentId,
          kind,
          name: name.trim(),
          repositoryDefault,
        })
      }
      if (alive.current) onClose()
    } catch {
      if (alive.current) setError(true)
    } finally {
      if (alive.current) setBusy(false)
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {mode === "edit" ? t("editDetails") : t(`new_${kind}`)}
          </DialogTitle>
          <DialogDescription>{path}</DialogDescription>
        </DialogHeader>
        <form className="space-y-4" onSubmit={submit}>
          <div className="space-y-1.5">
            <label htmlFor="team-node-name" className="text-sm font-medium">
              {t(`name_${kind}`)}
            </label>
            <Input
              autoFocus
              id="team-node-name"
              maxLength={120}
              disabled={busy}
              value={name}
              onChange={(event) => setName(event.target.value)}
            />
          </div>
          {kind !== "working_directory" ? (
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={useRepository}
                disabled={busy}
                onChange={(event) => setUseRepository(event.target.checked)}
              />
              {t("associateRepository")}
            </label>
          ) : null}
          {needsRepository ? (
            <RepositoryFields
              fullName={fullName}
              branch={branch}
              disabled={busy}
              onFullName={setFullName}
              onBranch={setBranch}
            />
          ) : null}
          {mode === "edit" ? (
            <p className="text-xs text-muted-foreground">
              {t("editSnapshotHint")}
            </p>
          ) : null}
          {error ? <MutationError /> : null}
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
              {busy
                ? t("saving")
                : mode === "edit"
                  ? t("saveDetails")
                  : t(`create_${kind}`)}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  )
}

export function TeamChatDialog({
  node,
  path,
  inheritedRepository,
  inheritedBranch,
  createChat,
  connectRepository,
  onClose,
}: {
  node: TeamCatalogNode
  path: string
  inheritedRepository: TeamCatalogRepository | null
  inheritedBranch?: string | null
  createChat: (input: CreateTeamChatInput) => Promise<TeamCatalogChat>
  connectRepository: (
    fullName: string,
    startingBranch: string
  ) => Promise<TeamCatalogRepository>
  onClose: () => void
}) {
  const t = useTranslations("TeamWorkspace")
  const hasInherited = Boolean(inheritedRepository && inheritedBranch)
  const [override, setOverride] = useState(false)
  const [title, setTitle] = useState("")
  const [fullName, setFullName] = useState("")
  const [branch, setBranch] = useState("")
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState(false)
  const requestId = useRef<string | null>(null)
  const needsRepository = !hasInherited || override
  const valid = !needsRepository || Boolean(fullName.trim() && branch.trim())

  async function submit(event: React.FormEvent) {
    event.preventDefault()
    if (!valid || busy) return
    setBusy(true)
    setError(false)
    try {
      const connected = needsRepository
        ? await connectRepository(fullName.trim(), branch.trim())
        : null
      requestId.current ??= crypto.randomUUID()
      await createChat({
        nodeId: node.id,
        repositoryOverride: connected
          ? { repositoryId: connected.id, startingBranch: branch.trim() }
          : null,
        title: title.trim() || null,
        requestId: requestId.current,
        visibility: "private",
      })
      onClose()
    } catch {
      setError(true)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("new_chat")}</DialogTitle>
          <DialogDescription>{path}</DialogDescription>
        </DialogHeader>
        <form className="space-y-4" onSubmit={submit}>
          <div className="space-y-1.5">
            <label htmlFor="team-chat-title" className="text-sm font-medium">
              {t("chatTitle")}
            </label>
            <Input
              autoFocus
              id="team-chat-title"
              maxLength={200}
              value={title}
              disabled={busy}
              onChange={(event) => setTitle(event.target.value)}
            />
          </div>
          <p className="rounded-xl bg-muted/50 p-3 text-sm">
            {t("privateBeforeCreate")}
          </p>
          {hasInherited ? (
            <div className="space-y-2 rounded-xl border p-3 text-sm">
              <p>
                {t("nearestDefault")}: {inheritedRepository?.fullName} ·{" "}
                {inheritedBranch}
              </p>
              <label className="flex items-center gap-2">
                <input
                  type="checkbox"
                  checked={override}
                  disabled={busy}
                  onChange={(event) => setOverride(event.target.checked)}
                />
                {t("overrideRepository")}
              </label>
            </div>
          ) : null}
          {needsRepository ? (
            <RepositoryFields
              fullName={fullName}
              branch={branch}
              disabled={busy}
              onFullName={setFullName}
              onBranch={setBranch}
            />
          ) : null}
          {error ? <MutationError /> : null}
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

export function TeamShareDialog({
  chat,
  shareChat,
  onClose,
}: {
  chat: TeamCatalogChat
  shareChat: (
    chat: TeamCatalogChat,
    visibility: TeamChatVisibility
  ) => Promise<TeamCatalogChat>
  onClose: () => void
}) {
  const t = useTranslations("TeamWorkspace")
  const [visibility, setVisibility] = useState<TeamChatVisibility>(
    chat.visibility
  )
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState(false)
  async function submit(event: React.FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError(false)
    try {
      await shareChat(chat, visibility)
      onClose()
    } catch {
      setError(true)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("share")}</DialogTitle>
          <DialogDescription>
            {chat.title ?? t("untitledChat")}
          </DialogDescription>
        </DialogHeader>
        <form className="space-y-4" onSubmit={submit}>
          <p className="text-sm">{t("shareHistoryDisclosure")}</p>
          <p className="text-sm text-muted-foreground">
            {t("githubAudienceDisclosure")}
          </p>
          <fieldset className="space-y-2">
            <legend className="text-sm font-medium">{t("audience")}</legend>
            {(["private", "organization"] as const).map((choice) => (
              <label key={choice} className="flex items-center gap-2 text-sm">
                <input
                  type="radio"
                  name="team-chat-visibility"
                  value={choice}
                  checked={visibility === choice}
                  disabled={busy}
                  onChange={() => setVisibility(choice)}
                />
                {t(`visibility_${choice}`)}
              </label>
            ))}
          </fieldset>
          {error ? <MutationError /> : null}
          <div className="flex justify-end gap-2">
            <Button
              type="button"
              variant="ghost"
              disabled={busy}
              onClick={onClose}
            >
              {t("cancel")}
            </Button>
            <Button
              type="submit"
              disabled={busy || visibility === chat.visibility}
            >
              {busy ? t("saving") : t("share")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  )
}
