"use client"

import { memo, useEffect, useMemo, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog"
import { LiveTranscriptView } from "@/components/message/live-transcript-view"
import { PkDiffView } from "@/components/pk/pk-diff-view"
import { PkJudgePanel } from "@/components/pk/pk-judge-panel"
import { PkScoreboard } from "@/components/pk/pk-scoreboard"
import { usePkRound } from "@/hooks/use-pk-round"
import { AgentIcon } from "@/components/agent-icon"
import { getAgentLabel } from "@/lib/custom-agents"
import { getFileTree } from "@/lib/api"
import { buildPkReportHtml } from "@/lib/pk-report"
import type { FileTreeNode } from "@/lib/types"
import type { PkContestant, PkRound } from "@/stores/pk-arena-store"
import { cn } from "@/lib/utils"
import { usePkArenaStore } from "@/stores/pk-arena-store"

/**
 * The arena itself: scoreboard on top, one live transcript column per
 * contestant, a diff tab once the round settles, and a share button that
 * exports the scoreboard as a PNG. Round switching comes from the store's
 * `activeRoundId` — the launcher sets it on start, history can revisit any
 * finished round (live streams of an old round are gone; its persisted
 * conversation still renders through the same view).
 */

export function PkArenaDialog() {
  const t = useTranslations("PkArena.arena")
  const open = usePkArenaStore((s) => s.arenaOpen)
  const setArenaOpen = usePkArenaStore((s) => s.setArenaOpen)
  const setPillDismissed = usePkArenaStore((s) => s.setPillDismissed)
  const rounds = usePkArenaStore((s) => s.rounds)
  const activeRoundId = usePkArenaStore((s) => s.activeRoundId)
  const setActiveRound = usePkArenaStore((s) => s.setActiveRound)

  const round = useMemo(
    () => rounds.find((r) => r.id === activeRoundId) ?? null,
    [rounds, activeRoundId]
  )

  const {
    cancelRound,
    cleanupRound,
    fetchDiff,
    disconnectFinished,
    startPrompt,
    applyContestantSelection,
  } = usePkRound()
  const markRound = usePkArenaStore((s) => s.markRound)
  const removeRound = usePkArenaStore((s) => s.removeRound)
  const [tab, setTab] = useState<"battle" | "diff">("battle")
  const [sharing, setSharing] = useState(false)
  const [reportExporting, setReportExporting] = useState(false)
  const [diffLoading, setDiffLoading] = useState(false)
  const scoreboardRef = useRef<HTMLDivElement>(null)

  // 进行中(ready/running)的回合禁止 ESC / 点遮罩关闭——一个误触就把
  // 活着的比赛和它的实时流一起关了。只有 X 按钮能显式关闭。
  const liveRef = useRef(false)
  liveRef.current =
    round != null && (round.status === "ready" || round.status === "running")

  // Literal keys — next-intl's typed messages reject dynamic concatenation.
  const roundStatusLabel = useMemo(
    () => ({
      ready: t("roundStatus.ready"),
      running: t("roundStatus.running"),
      finished: t("roundStatus.finished"),
      canceled: t("roundStatus.canceled"),
      interrupted: t("roundStatus.interrupted"),
    }),
    [t]
  )
  const tabLabel = useMemo(
    () => ({ battle: t("tabs.battle"), diff: t("tabs.diff") }) as const,
    [t]
  )

  // Diff tab: fetch each contestant's worktree diff once per visit.
  useEffect(() => {
    if (!open || tab !== "diff" || !round) return
    let cancelled = false
    const pending = round.contestants.filter(
      (c) => c.diff == null && c.worktreePath
    )
    if (pending.length === 0) return
    setDiffLoading(true)
    void Promise.allSettled(pending.map((c) => fetchDiff(round, c))).then(
      () => {
        if (!cancelled) setDiffLoading(false)
      }
    )
    return () => {
      cancelled = true
    }
  }, [open, tab, round, fetchDiff])

  // 状态自愈:任何原因导致回合停在 ready/running 而选手已全部结算
  // (settle 事件漏一帧、重启后回放等),打开竞技场时立即收敛到 finished
  // 并断开残留连接——否则顶部状态永远停在"就绪"。
  useEffect(() => {
    if (!round) return
    const settled = (s: PkContestant["status"]) =>
      s === "done" || s === "error" || s === "canceled"
    if (round.status === "ready" || round.status === "running") {
      if (
        round.contestants.length > 0 &&
        round.contestants.every((c) => settled(c.status))
      ) {
        markRound(round.id, "finished")
        void disconnectFinished(round)
      }
    }
  }, [round, markRound, disconnectFinished])

  const handleExportReport = async () => {
    if (!round || reportExporting) return
    setReportExporting(true)
    try {
      // 确保 diff 已抓取(报告需要),并抓每个选手 worktree 的文件树。
      const pendingDiff = round.contestants.filter(
        (c) => c.diff == null && c.worktreePath
      )
      await Promise.allSettled(pendingDiff.map((c) => fetchDiff(round, c)))

      const fresh = usePkArenaStore
        .getState()
        .rounds.find((r) => r.id === round.id)
      const filesByAgent: Record<string, string[]> = {}
      for (const contestant of fresh?.contestants ?? []) {
        if (!contestant.worktreePath) {
          filesByAgent[contestant.slot] = []
          continue
        }
        try {
          const tree = await getFileTree(contestant.worktreePath, 6)
          filesByAgent[contestant.slot] = flattenTreeList(tree)
        } catch {
          filesByAgent[contestant.slot] = []
        }
      }
      const html = buildPkReportHtml(fresh ?? round, filesByAgent)
      const blob = new Blob([html], { type: "text/html;charset=utf-8" })
      const url = URL.createObjectURL(blob)
      const link = document.createElement("a")
      link.href = url
      link.download = `codeg-pk-${round.id}.html`
      link.click()
      URL.revokeObjectURL(url)
    } finally {
      setReportExporting(false)
    }
  }

  const handleShare = async () => {
    if (!scoreboardRef.current || sharing) return
    setSharing(true)
    try {
      const { toPng } = await import("html-to-image")
      const dataUrl = await toPng(scoreboardRef.current, {
        backgroundColor: getComputedStyle(document.body).backgroundColor,
        pixelRatio: 2,
      })
      const link = document.createElement("a")
      link.download = `codeg-pk-${round?.id ?? "round"}.png`
      link.href = dataUrl
      link.click()
    } catch {
      // Sharing is best-effort; a failed export must not disturb the round.
    } finally {
      setSharing(false)
    }
  }

  const roundLive = round != null && round.status === "running"

  const handleOpenChange = (next: boolean) => {
    if (!next) {
      // 准备态关闭 = 放弃本轮:清理会话连接,防止侧边栏残留空转。
      if (round && round.status === "ready") {
        void cancelRound(round)
      }
    }
    setArenaOpen(next)
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent
        showCloseButton={false}
        className="flex h-[92vh] w-full max-w-none flex-col gap-0 overflow-hidden rounded-2xl p-0 sm:max-w-none"
        onEscapeKeyDown={(event) => {
          if (liveRef.current) event.preventDefault()
        }}
        onPointerDownOutside={(event) => {
          if (liveRef.current) event.preventDefault()
        }}
      >
        <DialogTitle className="sr-only">{t("title")}</DialogTitle>
        <DialogDescription className="sr-only">
          {t("description")}
        </DialogDescription>
        {round ? (
          <div className="flex h-full min-h-0 flex-col">
            <div className="flex items-center gap-3 border-b border-border px-4 py-2 pr-12">
              <span className="text-base" aria-hidden>
                ⚔
              </span>
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-semibold text-foreground">
                  {round.task}
                </div>
                <div className="text-xs text-muted-foreground">
                  {roundStatusLabel[round.status]} ·{" "}
                  {new Date(round.createdAt).toLocaleString()}
                </div>
              </div>
              {rounds.length > 1 ? (
                <select
                  value={round.id}
                  onChange={(event) => setActiveRound(event.target.value)}
                  className="max-w-40 rounded-md border border-border bg-background px-2 py-1 text-xs text-foreground"
                  aria-label={t("roundPicker")}
                >
                  {rounds.map((r) => (
                    <option key={r.id} value={r.id}>
                      {new Date(r.createdAt).toLocaleTimeString()} ·{" "}
                      {r.contestants.length} {t("contestantsUnit")}
                    </option>
                  ))}
                </select>
              ) : null}
              {roundLive ? (
                <button
                  type="button"
                  onClick={() => void cancelRound(round)}
                  className="rounded-md border border-border px-3 py-1 text-xs text-foreground hover:bg-muted"
                >
                  {t("cancelRound")}
                </button>
              ) : round.contestants.some((c) => c.worktreePath) ? (
                <button
                  type="button"
                  onClick={() => void cleanupRound(round, true)}
                  className="rounded-md border border-border px-3 py-1 text-xs text-foreground hover:bg-muted"
                  title={t("cleanupHint")}
                >
                  {t("cleanupWorktrees")}
                </button>
              ) : null}
              <button
                type="button"
                onClick={() => usePkArenaStore.getState().setLauncherOpen(true)}
                className="rounded-md border border-border px-3 py-1 text-xs text-foreground hover:bg-muted"
              >
                {t("newRound")}
              </button>
              {roundLive ? (
                <button
                  type="button"
                  onClick={() => {
                    setPillDismissed(false)
                    setArenaOpen(false)
                  }}
                  className="rounded-md border border-border px-3 py-1 text-xs text-foreground hover:bg-muted"
                >
                  {t("minimize")}
                </button>
              ) : null}
              <button
                type="button"
                onClick={() => void handleExportReport()}
                disabled={reportExporting}
                className="rounded-md border border-border px-3 py-1 text-xs text-foreground hover:bg-muted disabled:opacity-50"
              >
                {reportExporting ? t("exporting") : t("exportReport")}
              </button>
              <button
                type="button"
                onClick={() => {
                  if (window.confirm(t("deleteConfirm"))) {
                    const nextRounds = rounds.filter((r) => r.id !== round.id)
                    removeRound(round.id)
                    if (nextRounds.length > 0) {
                      setActiveRound(nextRounds[0].id)
                    } else {
                      setArenaOpen(false)
                    }
                  }
                }}
                className="rounded-md px-3 py-1 text-xs text-muted-foreground hover:bg-red-500/10 hover:text-red-600"
                title={t("deleteRound")}
              >
                {t("deleteRound")}
              </button>
              <button
                type="button"
                onClick={() => void handleShare()}
                disabled={sharing}
                className="rounded-md bg-primary px-3 py-1 text-xs font-medium text-primary-foreground disabled:opacity-50"
              >
                {sharing ? t("sharing") : t("share")}
              </button>
            </div>

            {round.status === "ready" ? (
              <div className="flex items-center gap-3 border-b border-border bg-amber-500/5 px-4 py-2">
                <span className="min-w-0 flex-1 text-xs text-muted-foreground">
                  {t("readyNote")}
                </span>
                <button
                  type="button"
                  onClick={() => void startPrompt(round)}
                  disabled={
                    !round.contestants.some((c) => c.status === "ready")
                  }
                  className="rounded-md bg-primary px-4 py-1.5 text-sm font-medium text-primary-foreground disabled:opacity-50"
                >
                  {t("startMatch")}
                </button>
              </div>
            ) : null}
            <PkScoreboard ref={scoreboardRef} contestants={round.contestants} />

            {/* Judge verdict panel — shown when a judge is configured. */}
            {round.judgeAgent ? (
              <PkJudgePanel
                judgeStatus={round.judgeStatus}
                judgeResult={round.judgeResult}
                judgeAgent={round.judgeAgent}
              />
            ) : null}

            <div className="flex items-center gap-1 border-b border-border px-4">
              {(["battle", "diff"] as const).map((key) => (
                <button
                  key={key}
                  type="button"
                  onClick={() => setTab(key)}
                  className={cn(
                    "-mb-px border-b-2 px-3 py-1.5 text-sm",
                    tab === key
                      ? "border-primary font-medium text-foreground"
                      : "border-transparent text-muted-foreground hover:text-foreground"
                  )}
                >
                  {tabLabel[key]}
                </button>
              ))}
            </div>

            <div className="min-h-0 flex-1 overflow-hidden">
              <div className="flex h-full gap-2 overflow-x-auto p-2">
                {tab === "battle"
                  ? round.contestants.map((contestant) =>
                      round.status === "ready" ? (
                        <PkReadyPane
                          key={contestant.slot}
                          round={round}
                          contestant={contestant}
                          onSelect={applyContestantSelection}
                        />
                      ) : (
                        <PkBattlePane
                          key={contestant.slot}
                          conversationId={contestant.conversationId}
                          connectionId={contestant.connectionId}
                          agentType={contestant.agentType}
                          task={round.task}
                          statusDetail={contestant.statusDetail}
                          preparingLabel={t("preparing")}
                        />
                      )
                    )
                  : round.contestants.map((contestant) => (
                      <PkDiffView
                        key={contestant.slot}
                        agentType={contestant.agentType}
                        diff={contestant.diff}
                        loading={diffLoading && contestant.diff == null}
                      />
                    ))}
              </div>
            </div>
          </div>
        ) : (
          <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
            {t("noRound")}
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}

/**
 * One battle column. Memoized on stable props: the dialog re-renders on
 * every contestant store update (status/usage/diff of ANY contestant), and
 * an unmemoized pane re-rendered four streaming markdown transcripts each
 * time — the field-reported arena lag.
 */
const PkBattlePane = memo(function PkBattlePane({
  conversationId,
  connectionId,
  agentType,
  task,
  statusDetail,
  preparingLabel,
}: {
  conversationId: number | null
  connectionId: string | null
  agentType: PkContestant["agentType"]
  task: string
  statusDetail: string | null
  preparingLabel: string
}) {
  return (
    <div className="flex h-full w-80 shrink-0 flex-col overflow-hidden rounded-lg border border-border">
      {conversationId != null ? (
        <LiveTranscriptView
          conversationId={conversationId}
          connectionId={connectionId}
          agentType={agentType}
          kickoffText={task}
        />
      ) : (
        <div className="flex flex-1 items-center justify-center px-3 text-xs text-muted-foreground">
          {statusDetail ?? preparingLabel}
        </div>
      )}
    </div>
  )
})

/**
 * 准备阶段的面板:模型 + 思考等级选择器(选项来自握手通告的 configOptions)。
 * 只读竞技场 store 的选项表,变更经 onSelect 直接下发给后端连接。
 */
const PkReadyPane = memo(function PkReadyPane({
  round,
  contestant,
  onSelect,
}: {
  round: PkRound
  contestant: PkContestant
  onSelect: (
    round: PkRound,
    contestant: PkContestant,
    configId: string,
    value: string
  ) => Promise<void>
}) {
  const t = useTranslations("PkArena.arena")
  const showPickers =
    contestant.modelOptions.length > 0 || contestant.effortOptions.length > 0
  return (
    <div className="flex h-full w-80 shrink-0 flex-col overflow-hidden rounded-lg border border-border">
      <div className="flex items-center gap-2.5 border-b border-border bg-muted/30 px-4 py-3">
        <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-border bg-background">
          <AgentIcon agentType={contestant.agentType} className="size-5" />
        </span>
        <div className="min-w-0">
          <div className="truncate text-sm font-semibold text-foreground">
            {getAgentLabel(contestant.agentType)}
          </div>
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <span className="size-1.5 rounded-full bg-amber-400" aria-hidden />
            {t("readyTag")}
          </div>
        </div>
      </div>
      {showPickers ? (
        <div className="flex flex-col gap-4 overflow-auto px-4 py-4">
          {contestant.modelOptions.length > 0 ? (
            <label className="block">
              <span className="mb-1.5 block text-xs font-medium text-muted-foreground">
                {t("modelLabel")}
              </span>
              <select
                value={contestant.selectedModel ?? ""}
                onChange={(event) =>
                  void onSelect(round, contestant, "model", event.target.value)
                }
                className="w-full rounded-md border border-border bg-background px-2 py-1 text-xs text-foreground"
              >
                {contestant.modelOptions.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.name}
                  </option>
                ))}
              </select>
            </label>
          ) : null}
          {contestant.effortOptions.length > 0 ? (
            <label className="block">
              <span className="mb-1.5 block text-xs font-medium text-muted-foreground">
                {t("effortLabel")}
              </span>
              <select
                value={contestant.selectedEffort ?? ""}
                onChange={(event) =>
                  void onSelect(round, contestant, "effort", event.target.value)
                }
                className="w-full rounded-md border border-border bg-background px-2 py-1 text-xs text-foreground"
              >
                {contestant.effortOptions.map((value) => (
                  <option key={value} value={value}>
                    {value}
                  </option>
                ))}
              </select>
            </label>
          ) : null}
        </div>
      ) : (
        <div className="flex flex-1 items-center justify-center px-4 text-xs text-muted-foreground">
          {contestant.statusDetail ?? t("preparing")}
        </div>
      )}
    </div>
  )
})

/** 把文件树拍平成相对路径清单(跳过目录,只留文件)。 */
function flattenTreeList(nodes: FileTreeNode[], prefix = ""): string[] {
  const files: string[] = []
  for (const node of nodes) {
    const rel = prefix ? `${prefix}/${node.name}` : node.name
    if (node.kind === "file") {
      files.push(rel)
    } else {
      files.push(...flattenTreeList(node.children, rel))
    }
  }
  return files
}
