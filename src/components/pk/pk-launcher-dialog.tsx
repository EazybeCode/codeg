"use client"

import { useEffect, useMemo, useState } from "react"
import { useTranslations } from "next-intl"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog"
import { AgentIcon } from "@/components/agent-icon"
import { useAcpAgents } from "@/hooks/use-acp-agents"
import { acpGetAgentStatus, getFolder, getGitBranch, gitInit } from "@/lib/api"
import { getAgentLabel } from "@/lib/custom-agents"
import type { AgentType } from "@/lib/types"
import { cn } from "@/lib/utils"
import {
  loadLastLauncherConfig,
  saveLastLauncherConfig,
  usePkArenaStore,
  type PkEffortLevel,
  type PkPermissionMode,
} from "@/stores/pk-arena-store"
import { useTabStore } from "@/stores/tab-store"

/**
 * Arena launcher: pick 2-4 installed agents, write the task, start the round.
 * Reads the ACTIVE tab for the target folder (an arena needs a real folder —
 * its git repo provides the per-contestant worktrees; chat mode has none).
 */

const MIN_CONTESTANTS = 2
const MAX_CONTESTANTS = 8

export function PkLauncherDialog() {
  const t = useTranslations("PkArena.launcher")
  const open = usePkArenaStore((s) => s.launcherOpen)
  const setLauncherOpen = usePkArenaStore((s) => s.setLauncherOpen)
  const setArenaOpen = usePkArenaStore((s) => s.setArenaOpen)
  const createRound = usePkArenaStore((s) => s.createRound)
  const { agents: rawAgents } = useAcpAgents()
  const activeTab = useTabStore((s) =>
    s.activeTabId
      ? (s.tabs.find((tab) => tab.id === s.activeTabId) ?? null)
      : null
  )

  const [selected, setSelected] = useState<AgentType[]>([])
  const [task, setTask] = useState("")
  const [workingDir, setWorkingDir] = useState<string | null>(null)
  const [folderId, setFolderId] = useState<number | null>(null)
  // null = unknown (still checking); false disables Start — worktrees need a
  // real git repo, and `git worktree add` in a plain folder fails instantly.
  const [isGitRepo, setIsGitRepo] = useState<boolean | null>(null)
  const [initializing, setInitializing] = useState(false)
  const [permissionMode, setPermissionMode] =
    useState<PkPermissionMode>("default")
  const [bareMode, setBareMode] = useState(false)
  const [effort, setEffort] = useState<PkEffortLevel>("default")
  const [startError, setStartError] = useState<string | null>(null)

  const checkGitRepo = (dir: string, cancelledRef: { current: boolean }) => {
    setIsGitRepo(null)
    getGitBranch(dir)
      .then((branch) => {
        if (!cancelledRef.current) setIsGitRepo(branch != null)
      })
      .catch(() => {
        if (!cancelledRef.current) setIsGitRepo(false)
      })
  }

  useEffect(() => {
    if (!open) return
    setSelected([])
    setTask("")
    setWorkingDir(null)
    setFolderId(null)
    setIsGitRepo(null)
    setPermissionMode("default")
    setBareMode(false)
    setEffort("default")
    setStartError(null)
    // 复赛预填:上次配置的选手若仍可参与则沿用。
    const last = loadLastLauncherConfig()
    if (last && last.agents.length > 0) {
      setSelected((prev) => (prev.length > 0 ? prev : last.agents))
      setTask(last.task ?? "")
      setPermissionMode(last.permissionMode)
      setBareMode(last.bareMode)
      setEffort(last.effort)
    }
    // The active tab decides where the arena runs. Draft tabs may lack a
    // workingDir; fall back to the folder's own path.
    if (activeTab?.folderId == null || activeTab.folderId < 0) return
    const cancelled = { current: false }
    const resolve = (id: number, dir: string) => {
      setFolderId(id)
      setWorkingDir(dir)
      checkGitRepo(dir, cancelled)
    }
    if (activeTab.workingDir) {
      resolve(activeTab.folderId, activeTab.workingDir)
    } else {
      getFolder(activeTab.folderId)
        .then((folder) => {
          if (!cancelled.current) resolve(folder.id, folder.path)
        })
        .catch(() => undefined)
    }
    return () => {
      cancelled.current = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open])

  const handleInitGit = async () => {
    if (!workingDir || initializing) return
    setInitializing(true)
    try {
      await gitInit(workingDir)
      setIsGitRepo(true)
    } catch {
      setIsGitRepo(false)
    } finally {
      setInitializing(false)
    }
  }

  // 只列真正能跑的:安装到位(installed_version) + 未禁用 + 可用。
  // 未安装的 agent 在 connect 的 preflight 会被拦,但那时回合/会话已创建,
  // 留下的宿主会话会一直空转——所以 PK 干脆只收已就绪的选手。
  const agents = useMemo(
    () =>
      rawAgents.filter(
        (a) => a.enabled && a.available && a.installed_version != null
      ),
    [rawAgents]
  )

  const noFolder = open && folderId == null && activeTab != null
  const taskValid = task.trim().length > 0
  const selectionValid =
    selected.length >= MIN_CONTESTANTS && selected.length <= MAX_CONTESTANTS
  const canStart =
    taskValid &&
    selectionValid &&
    folderId != null &&
    workingDir != null &&
    isGitRepo === true

  const toggle = (agentType: AgentType) => {
    setSelected((prev) =>
      prev.includes(agentType)
        ? prev.filter((a) => a !== agentType)
        : prev.length >= MAX_CONTESTANTS
          ? prev
          : [...prev, agentType]
    )
  }

  const handleStart = async () => {
    if (!canStart || folderId == null || workingDir == null) return
    // 开赛前预检:任何选手不可用就中止,不建回合、不留残留会话。
    for (const agentType of selected) {
      try {
        const status = await acpGetAgentStatus(agentType)
        if (!status.enabled || !status.available || !status.installed_version) {
          setStartError(t("agentNotReady", { agent: getAgentLabel(agentType) }))
          return
        }
      } catch {
        setStartError(
          t("agentCheckFailed", { agent: getAgentLabel(agentType) })
        )
        return
      }
    }
    setStartError(null)
    saveLastLauncherConfig({
      agents: selected,
      permissionMode,
      bareMode,
      effort,
      task: task.trim(),
    })
    void createRound({
      task: task.trim(),
      folderId,
      workingDir,
      agents: selected,
      permissionMode,
      bareMode,
      effort,
    })
    setLauncherOpen(false)
    setArenaOpen(true)
    // The orchestrator (in PkArenaHost) picks the new round up from the store.
  }

  return (
    <Dialog open={open} onOpenChange={setLauncherOpen}>
      <DialogContent className="flex max-h-[85vh] w-full max-w-lg flex-col gap-0 overflow-hidden rounded-2xl p-0">
        <DialogTitle className="border-b border-border px-5 py-3 text-base font-semibold">
          {t("title")}
        </DialogTitle>
        <DialogDescription className="sr-only">
          {t("description")}
        </DialogDescription>
        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-auto px-5 py-4">
          <div>
            <div className="mb-2 text-sm font-medium text-foreground">
              {t("contestantsLabel")}
            </div>
            {noFolder ? (
              <div className="text-xs text-muted-foreground">
                {t("noFolderHint")}
              </div>
            ) : null}
            {workingDir != null && isGitRepo === false ? (
              <div className="flex items-center gap-2 text-xs text-muted-foreground">
                <span className="min-w-0 flex-1">{t("notAGitRepo")}</span>
                <button
                  type="button"
                  onClick={() => void handleInitGit()}
                  disabled={initializing}
                  className="shrink-0 rounded-md border border-border px-2 py-1 text-xs text-foreground hover:bg-muted disabled:opacity-50"
                >
                  {initializing ? t("initializing") : t("initGitRepo")}
                </button>
              </div>
            ) : null}
            <div className="flex flex-wrap gap-2">
              {agents.map((agent) => {
                const isSelected = selected.includes(agent.agent_type)
                return (
                  <button
                    key={agent.agent_type}
                    type="button"
                    onClick={() => toggle(agent.agent_type)}
                    aria-pressed={isSelected}
                    className={cn(
                      "inline-flex items-center gap-1.5 rounded-full border px-3 py-1.5 text-sm transition-colors",
                      isSelected
                        ? "border-primary bg-primary/10 text-foreground"
                        : "border-border text-muted-foreground hover:bg-muted"
                    )}
                  >
                    <AgentIcon
                      agentType={agent.agent_type}
                      className="size-4"
                    />
                    {agent.name}
                  </button>
                )
              })}
            </div>
            {selected.length > 0 && selected.length < MIN_CONTESTANTS ? (
              <div className="mt-2 text-xs text-muted-foreground">
                {t("needMore", { count: MIN_CONTESTANTS })}
              </div>
            ) : null}
          </div>
          <div className="flex min-h-0 flex-1 flex-col">
            <label
              htmlFor="pk-task"
              className="mb-2 text-sm font-medium text-foreground"
            >
              {t("taskLabel")}
            </label>
            <textarea
              id="pk-task"
              value={task}
              onChange={(event) => setTask(event.target.value)}
              placeholder={t("taskPlaceholder")}
              rows={5}
              className="w-full resize-none rounded-lg border border-border bg-background px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
            />
          </div>
          <div>
            <div className="mb-2 text-sm font-medium text-foreground">
              {t("permissionLabel")}
            </div>
            <div className="flex flex-col gap-1.5">
              {(["default", "acceptEdits", "bypassPermissions"] as const).map(
                (mode) => (
                  <label
                    key={mode}
                    className="flex cursor-pointer items-center gap-2 text-sm text-foreground"
                  >
                    <input
                      type="radio"
                      name="pk-permission"
                      checked={permissionMode === mode}
                      onChange={() => setPermissionMode(mode)}
                      className="accent-foreground"
                    />
                    <span className="font-medium">
                      {t(`permissionOptions.${mode}`)}
                    </span>
                    <span className="text-xs text-muted-foreground">
                      {t(`permissionHints.${mode}`)}
                    </span>
                  </label>
                )
              )}
            </div>
            <div className="mt-1.5 text-xs text-muted-foreground">
              {t("permissionNote")}
            </div>
          </div>
          <div>
            <div className="mb-2 text-sm font-medium text-foreground">
              {t("effortLabel")}
            </div>
            <div className="flex flex-wrap gap-1.5">
              {(["default", "low", "medium", "high", "max"] as const).map(
                (level) => (
                  <button
                    key={level}
                    type="button"
                    onClick={() => setEffort(level)}
                    aria-pressed={effort === level}
                    className={cn(
                      "rounded-full border px-3 py-1 text-xs transition-colors",
                      effort === level
                        ? "border-primary bg-primary/10 text-foreground"
                        : "border-border text-muted-foreground hover:bg-muted"
                    )}
                  >
                    {t(`effortOptions.${level}`)}
                  </button>
                )
              )}
            </div>
            <div className="mt-1.5 text-xs text-muted-foreground">
              {t("effortNote")}
            </div>
          </div>
          <label className="flex cursor-pointer items-start gap-2 text-sm">
            <input
              type="checkbox"
              checked={bareMode}
              onChange={(event) => setBareMode(event.target.checked)}
              className="mt-0.5 accent-foreground"
            />
            <span>
              <span className="font-medium text-foreground">
                {t("bareModeLabel")}
              </span>
              <span className="block text-xs text-muted-foreground">
                {t("bareModeHint")}
              </span>
            </span>
          </label>
        </div>
        {startError != null ? (
          <div className="border-t border-border px-5 py-2 text-xs text-red-600 dark:text-red-400">
            {startError}
          </div>
        ) : null}
        <div className="flex items-center justify-between gap-3 border-t border-border px-5 py-3">
          <span className="text-xs text-muted-foreground">
            {t("selectedCount", {
              selected: selected.length,
              min: MIN_CONTESTANTS,
              max: MAX_CONTESTANTS,
            })}
          </span>
          <div className="flex gap-2">
            <button
              type="button"
              onClick={() => setLauncherOpen(false)}
              className="rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:bg-muted"
            >
              {t("cancel")}
            </button>
            <button
              type="button"
              disabled={!canStart}
              onClick={() => void handleStart()}
              className="rounded-md bg-primary px-4 py-1.5 text-sm font-medium text-primary-foreground disabled:opacity-50"
            >
              {t("start")}
            </button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
