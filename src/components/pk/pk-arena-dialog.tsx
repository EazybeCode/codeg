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
import { PkScoreboard } from "@/components/pk/pk-scoreboard"
import { usePkRound } from "@/hooks/use-pk-round"
import { AgentIcon } from "@/components/agent-icon"
import { getAgentLabel } from "@/lib/custom-agents"
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
    startPrompt,
    applyContestantSelection,
  } = usePkRound()
  const [tab, setTab] = useState<"battle" | "diff">("battle")
  const [sharing, setSharing] = useState(false)
  const [diffLoading, setDiffLoading] = useState(false)
  const scoreboardRef = useRef<HTMLDivElement>(null)

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

  return (
    <Dialog open={open} onOpenChange={setArenaOpen}>
      <DialogContent
        closeButtonClassName="top-2 right-2"
        className="flex h-[92vh] w-full max-w-none flex-col gap-0 overflow-hidden rounded-2xl p-0 sm:max-w-none"
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

            <div
              className="grid min-h-0 flex-1 gap-2 p-2"
              style={{
                gridTemplateColumns: `repeat(${round.contestants.length}, minmax(0, 1fr))`,
              }}
            >
              {tab === "battle"
                ? round.contestants.map((contestant) =>
                    round.status === "ready" ? (
                      <PkReadyPane
                        key={contestant.agentType}
                        round={round}
                        contestant={contestant}
                        onSelect={applyContestantSelection}
                      />
                    ) : (
                      <PkBattlePane
                        key={contestant.agentType}
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
                      key={contestant.agentType}
                      agentType={contestant.agentType}
                      diff={contestant.diff}
                      loading={diffLoading && contestant.diff == null}
                    />
                  ))}
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
    <div className="flex min-h-0 flex-col overflow-hidden rounded-lg border border-border">
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
  return (
    <div className="flex min-h-0 flex-col gap-2 overflow-hidden rounded-lg border border-border p-3">
      <div className="flex items-center gap-2">
        <AgentIcon agentType={contestant.agentType} className="size-4" />
        <span className="text-sm font-medium text-foreground">
          {getAgentLabel(contestant.agentType)}
        </span>
      </div>
      {contestant.modelOptions.length > 0 ? (
        <label className="block">
          <span className="mb-1 block text-xs text-muted-foreground">
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
          <span className="mb-1 block text-xs text-muted-foreground">
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
      {contestant.modelOptions.length === 0 &&
      contestant.effortOptions.length === 0 ? (
        <div className="flex flex-1 items-center justify-center text-xs text-muted-foreground">
          {contestant.statusDetail ?? t("preparing")}
        </div>
      ) : null}
    </div>
  )
})
