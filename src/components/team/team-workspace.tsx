"use client"

import { useRef, useState } from "react"
import {
  Building2,
  Github,
  Layers3,
  Menu,
  MessageSquare,
  Plus,
  Settings,
} from "lucide-react"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import type { TeamNodeKind } from "@/lib/team-catalog-client"
import type { TeamSession } from "@/lib/team-client"
import {
  TeamChatDialog,
  TeamNodeDialog,
  TeamShareDialog,
} from "./team-workspace-dialogs"
import { TeamWorkspaceTree } from "./team-workspace-tree"
import { useTeamCatalog } from "./use-team-catalog"

type DialogState =
  | {
      type: "node"
      mode: "create"
      kind: TeamNodeKind
      parentId: number | null
    }
  | { type: "node"; mode: "edit"; nodeId: number }
  | { type: "chat"; nodeId: number }
  | { type: "share"; chatId: number }

export function TeamWorkspace({
  session,
  logout,
  logoutError,
}: {
  session: TeamSession
  logout: () => Promise<void>
  logoutError: boolean
}) {
  const t = useTranslations("TeamWorkspace")
  const catalog = useTeamCatalog()
  const organization = session.organization.name ?? session.organization.login
  const [mobileOpen, setMobileOpen] = useState(false)
  const [dialog, setDialog] = useState<DialogState | null>(null)
  const restoreFocus = useRef<HTMLElement | null>(null)

  const selectedNode = catalog.nodes.find(
    (node) => node.id === catalog.selectedNodeId
  )
  const selectedChat =
    catalog.selectedChat?.id === catalog.selectedChatId
      ? catalog.selectedChat
      : null
  const path = selectedNode ? catalog.pathFor(selectedNode.id) : []

  function openDialog(next: DialogState) {
    restoreFocus.current = document.activeElement as HTMLElement | null
    setDialog(next)
  }

  function closeDialog() {
    setDialog(null)
    window.setTimeout(() => restoreFocus.current?.focus(), 0)
  }

  function create(kind: TeamNodeKind | "chat", parentId?: number | null) {
    if (kind === "project") {
      openDialog({ type: "node", mode: "create", kind, parentId: null })
    } else if (parentId != null && kind === "chat") {
      openDialog({ type: "chat", nodeId: parentId })
    } else if (
      parentId != null &&
      (kind === "folder" || kind === "working_directory")
    ) {
      openDialog({ type: "node", mode: "create", kind, parentId })
    }
  }

  const navigation = (
    <nav
      aria-label={t("workspaceNavigation")}
      className="flex h-full min-h-0 flex-col"
    >
      <div className="border-b border-sidebar-border px-3 py-3">
        <a
          href="/workspace"
          className="flex items-center gap-2 rounded-lg px-2 py-1.5 font-semibold tracking-tight focus-visible:ring-2 focus-visible:ring-ring"
        >
          <Layers3 aria-hidden className="size-4" />
          Neev
        </a>
      </div>
      <div className="space-y-2 border-b border-sidebar-border px-3 py-3">
        <IdentityLine
          icon={<Github aria-hidden className="size-4" />}
          label={t("personLabel")}
          name={session.user.name ?? session.user.login}
          login={session.user.login}
          image={session.user.avatarUrl}
        />
        <IdentityLine
          icon={<Building2 aria-hidden className="size-4" />}
          label={t("organizationLabel")}
          name={organization}
          login={session.organization.login}
          image={session.organization.avatarUrl}
        />
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-2 py-3">
        <Button
          type="button"
          variant="ghost"
          className="mb-3 w-full justify-start"
          onClick={() => create("project")}
        >
          <Plus aria-hidden />
          {t("new_project")}
        </Button>
        <p className="mb-2 px-2 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
          {t("projects")}
        </p>
        {catalog.loading ? (
          <p role="status" className="px-2 py-2 text-sm text-muted-foreground">
            {t("loading")}
          </p>
        ) : null}
        {catalog.error ? (
          <div
            role="alert"
            className="space-y-2 rounded-xl border bg-muted/40 p-3 text-sm"
          >
            <p>{t("loadError")}</p>
            <Button
              size="sm"
              variant="outline"
              onClick={() => void catalog.refresh()}
            >
              {t("retry")}
            </Button>
          </div>
        ) : null}
        {!catalog.loading && !catalog.error && catalog.nodes.length === 0 ? (
          <p className="px-2 py-2 text-sm leading-relaxed text-muted-foreground">
            {t("empty")}
          </p>
        ) : null}
        <TeamWorkspaceTree
          nodes={catalog.nodes}
          chats={catalog.chats}
          expanded={catalog.expanded}
          selectedNodeId={catalog.selectedNodeId}
          selectedChatId={catalog.selectedChatId}
          onToggle={catalog.toggleNode}
          onSelectNode={(id) => {
            catalog.selectNode(id)
            setMobileOpen(false)
          }}
          onSelectChat={(id) => {
            catalog.selectChat(id)
            setMobileOpen(false)
          }}
          onCreate={create}
          onEdit={(node) =>
            openDialog({ type: "node", mode: "edit", nodeId: node.id })
          }
          onLoadMore={() => void catalog.loadMoreChats()}
          hasMore={catalog.hasMoreChats}
          loadingMore={catalog.loadingMoreChats}
        />
      </div>
      <div className="space-y-1 border-t border-sidebar-border p-2">
        <Button asChild variant="ghost" className="w-full justify-start">
          <a href="/settings">
            <Settings aria-hidden />
            {t("settings")}
          </a>
        </Button>
        <Button
          type="button"
          variant="ghost"
          className="w-full justify-start"
          onClick={() => void logout()}
        >
          {t("signOut")}
        </Button>
        {logoutError ? (
          <p role="alert" className="px-2 text-xs text-destructive">
            {t("logoutFailed")}
          </p>
        ) : null}
      </div>
    </nav>
  )

  const dialogNode =
    dialog?.type === "node" && dialog.mode === "edit"
      ? (catalog.nodes.find((node) => node.id === dialog.nodeId) ?? null)
      : null
  const dialogParent =
    dialog?.type === "node" && dialog.mode === "create"
      ? dialog.parentId
      : (dialogNode?.parentId ?? null)
  const dialogPath = dialogParent
    ? catalog
        .pathFor(dialogParent)
        .map((node) => node.name)
        .join(" / ")
    : organization
  const editRepository = dialogNode?.repositoryDefault
    ? (catalog.repositories.find(
        (repository) =>
          repository.id === dialogNode.repositoryDefault?.repositoryId
      ) ?? null)
    : null
  const dialogChatNode =
    dialog?.type === "chat"
      ? (catalog.nodes.find((node) => node.id === dialog.nodeId) ?? null)
      : null
  const inheritedNode = dialogChatNode
    ? [...catalog.pathFor(dialogChatNode.id)]
        .reverse()
        .find((node) => node.repositoryDefault)
    : null
  const inheritedRepository = inheritedNode?.repositoryDefault
    ? (catalog.repositories.find(
        (repository) =>
          repository.id === inheritedNode.repositoryDefault?.repositoryId
      ) ?? null)
    : null
  const shareChat =
    dialog?.type === "share" && selectedChat?.id === dialog.chatId
      ? selectedChat
      : null

  return (
    <main className="flex min-h-screen bg-background text-foreground">
      <button
        type="button"
        aria-label={t(mobileOpen ? "closeNavigation" : "openNavigation")}
        aria-expanded={mobileOpen}
        onClick={() => setMobileOpen((open) => !open)}
        className="fixed left-3 top-3 z-40 flex size-9 items-center justify-center rounded-full border bg-background shadow-sm focus-visible:ring-2 focus-visible:ring-ring md:hidden"
      >
        <Menu aria-hidden className="size-4" />
      </button>
      {mobileOpen ? (
        <button
          type="button"
          aria-label={t("closeNavigation")}
          className="fixed inset-0 z-20 bg-black/40 md:hidden"
          onClick={() => setMobileOpen(false)}
        />
      ) : null}
      <aside
        className={cn(
          "fixed inset-y-0 left-0 z-30 w-[260px] border-r border-sidebar-border bg-sidebar text-sidebar-foreground md:static md:block",
          mobileOpen ? "block" : "hidden"
        )}
      >
        {navigation}
      </aside>
      <section className="min-w-0 flex-1">
        {selectedChat ? (
          <div className="mx-auto max-w-3xl px-6 py-16 sm:py-24">
            <p className="mb-3 text-xs text-muted-foreground">
              {catalog
                .pathFor(selectedChat.nodeId)
                .map((node) => node.name)
                .join(" / ")}
            </p>
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div>
                <div className="mb-3 flex size-10 items-center justify-center rounded-xl bg-muted">
                  <MessageSquare aria-hidden className="size-5" />
                </div>
                <h1 className="text-2xl font-semibold tracking-tight">
                  {selectedChat.title ?? t("untitledChat")}
                </h1>
              </div>
              <div className="flex items-center gap-2">
                <span className="rounded-full border px-2.5 py-1 text-xs text-muted-foreground">
                  {selectedChat.visibility === "organization"
                    ? t("visibility_organization_named", { organization })
                    : t("visibility_private")}
                </span>
                {selectedChat.canShare ? (
                  <Button
                    size="sm"
                    onClick={() =>
                      openDialog({ type: "share", chatId: selectedChat.id })
                    }
                  >
                    {t("share")}
                  </Button>
                ) : null}
              </div>
            </div>
            <dl className="mt-8 grid gap-4 rounded-2xl border bg-card p-5 sm:grid-cols-2">
              <Metadata label={t("repository")} value={selectedChat.fullName} />
              <Metadata
                label={t("startingBranch")}
                value={selectedChat.startingBranch}
              />
            </dl>
            <div className="mt-6 rounded-2xl border border-dashed p-5">
              <h2 className="font-medium">{t("runtimeDisabledTitle")}</h2>
              <p className="mt-2 text-sm leading-relaxed text-muted-foreground">
                {t("runtimeDisabledDescription")}
              </p>
            </div>
          </div>
        ) : selectedNode ? (
          <div className="mx-auto max-w-2xl px-6 py-16 sm:py-24">
            <p className="mb-3 text-xs text-muted-foreground">
              {path.map((node) => node.name).join(" / ")}
            </p>
            <h1 className="text-2xl font-semibold tracking-tight">
              {selectedNode.name}
            </h1>
            <p className="mt-3 text-sm text-muted-foreground">
              {t("nodeEmpty")}
            </p>
            <div className="mt-6 flex flex-wrap gap-2">
              {(["folder", "working_directory", "chat"] as const).map(
                (kind) => (
                  <Button
                    key={kind}
                    variant={kind === "chat" ? "default" : "outline"}
                    onClick={() => create(kind, selectedNode.id)}
                  >
                    {t(`new_${kind}`)}
                  </Button>
                )
              )}
            </div>
          </div>
        ) : (
          <div className="mx-auto max-w-xl px-6 py-16 sm:py-24">
            <Layers3 aria-hidden className="size-9 text-muted-foreground" />
            <h1 className="mt-5 text-2xl font-semibold tracking-tight">
              {t("welcome", {
                organization: organization,
              })}
            </h1>
            <p className="mt-3 text-sm leading-relaxed text-muted-foreground">
              {t("welcomeHint")}
            </p>
            <Button className="mt-6" onClick={() => create("project")}>
              <Plus aria-hidden />
              {t("new_project")}
            </Button>
          </div>
        )}
      </section>

      {dialog?.type === "node" ? (
        <TeamNodeDialog
          key={
            dialog.mode === "edit"
              ? `edit:${dialog.nodeId}`
              : `new:${dialog.kind}:${dialog.parentId}`
          }
          mode={dialog.mode}
          kind={
            dialog.mode === "edit"
              ? (dialogNode?.kind ?? "folder")
              : dialog.kind
          }
          parentId={dialogParent}
          node={dialogNode}
          repository={editRepository}
          path={dialogPath}
          connectRepository={catalog.connectRepository}
          createNode={catalog.createNode}
          updateNode={catalog.updateNode}
          onClose={closeDialog}
        />
      ) : null}
      {dialog?.type === "chat" && dialogChatNode ? (
        <TeamChatDialog
          node={dialogChatNode}
          path={catalog
            .pathFor(dialogChatNode.id)
            .map((node) => node.name)
            .join(" / ")}
          organization={organization}
          inheritedRepository={inheritedRepository}
          inheritedBranch={
            inheritedNode?.repositoryDefault?.startingBranch ?? null
          }
          createChat={catalog.createChat}
          connectRepository={catalog.connectRepository}
          onClose={closeDialog}
        />
      ) : null}
      {dialog?.type === "share" && shareChat?.canShare ? (
        <TeamShareDialog
          chat={shareChat}
          organization={organization}
          shareChat={catalog.shareChat}
          onClose={closeDialog}
        />
      ) : null}
    </main>
  )
}

function IdentityLine({
  icon,
  label,
  name,
  login,
  image,
}: {
  icon: React.ReactNode
  label: string
  name: string
  login: string
  image: string | null
}) {
  return (
    <div className="flex min-w-0 items-center gap-2 rounded-lg px-2 py-1">
      {image ? (
        // Team auth permits only normalized stored image URLs.
        // eslint-disable-next-line @next/next/no-img-element
        <img
          src={image}
          alt=""
          referrerPolicy="no-referrer"
          className="size-7 shrink-0 rounded-lg object-cover"
        />
      ) : (
        <span className="flex size-7 shrink-0 items-center justify-center rounded-lg bg-sidebar-accent">
          {icon}
        </span>
      )}
      <div className="min-w-0">
        <p className="truncate text-[11px] text-muted-foreground">{label}</p>
        <p className="truncate text-sm font-medium">{name}</p>
        <p className="sr-only">@{login}</p>
      </div>
    </div>
  )
}

function Metadata({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        {label}
      </dt>
      <dd className="mt-1 text-sm">{value}</dd>
    </div>
  )
}
