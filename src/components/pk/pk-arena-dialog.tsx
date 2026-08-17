"use client"

import { useEffect, useMemo, useRef, useState } from "react"
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

  const { cancelRound, cleanupRound, fetchDiff } = usePkRound()
  const [tab, setTab] = useState<"battle" | "diff">("battle")
  const [now, setNow] = useState(() => Date.now())
  const [sharing, setSharing] = useState(false)
  const [diffLoading, setDiffLoading] = useState(false)
  const scoreboardRef = useRef<HTMLDivElement>(null)

  // Literal keys — next-intl's typed messages reject dynamic concatenation.
  const roundStatusLabel = useMemo(
    () => ({
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

  const live = round != null && round.status === "running"

  // 1s scoreboard clock while any contestant is live.
  useEffect(() => {
    if (!open || !live) return
    const timer = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(timer)
  }, [open, live])

  useEffect(() => {
    if (open) setNow(Date.now())
  }, [open])

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
                onClick={() => void handleShare()}
                disabled={sharing}
                className="rounded-md bg-primary px-3 py-1 text-xs font-medium text-primary-foreground disabled:opacity-50"
              >
                {sharing ? t("sharing") : t("share")}
              </button>
            </div>

            <PkScoreboard
              ref={scoreboardRef}
              contestants={round.contestants}
              now={now}
            />

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
                ? round.contestants.map((contestant) => (
                    <div
                      key={contestant.agentType}
                      className="flex min-h-0 flex-col overflow-hidden rounded-lg border border-border"
                    >
                      {contestant.conversationId != null ? (
                        <LiveTranscriptView
                          conversationId={contestant.conversationId}
                          connectionId={contestant.connectionId}
                          agentType={contestant.agentType}
                          kickoffText={round.task}
                        />
                      ) : (
                        <div className="flex flex-1 items-center justify-center px-3 text-xs text-muted-foreground">
                          {contestant.statusDetail ?? t("preparing")}
                        </div>
                      )}
                    </div>
                  ))
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
