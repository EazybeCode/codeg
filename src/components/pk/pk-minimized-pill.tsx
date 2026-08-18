"use client"

import { useEffect, useState } from "react"
import { useTranslations } from "next-intl"
import { Swords, X } from "lucide-react"
import { cn } from "@/lib/utils"
import { usePkArenaStore } from "@/stores/pk-arena-store"

/**
 * 竞技场最小化胶囊。大窗关闭后,只要还有进行中(ready/running)的回合,
 * 右下角常驻这个小胶囊:显示 ⚔ + 已完成/总数,随时点开回到全屏——
 * 比赛在后台继续,用户该干嘛干嘛。手动 ✕ 只把它藏起来,回合不受影响;
 * 左上角 ⚔ 或新回合会自动把它唤回来。
 */
export function PkMinimizedPill() {
  const t = useTranslations("PkArena.minimized")
  const rounds = usePkArenaStore((s) => s.rounds)
  const arenaOpen = usePkArenaStore((s) => s.arenaOpen)
  const pillDismissed = usePkArenaStore((s) => s.pillDismissed)
  const setArenaOpen = usePkArenaStore((s) => s.setArenaOpen)
  const setPillDismissed = usePkArenaStore((s) => s.setPillDismissed)

  const liveRound = rounds.find(
    (r) => r.status === "ready" || r.status === "running"
  )
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!liveRound || arenaOpen) return
    const timer = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(timer)
  }, [liveRound, arenaOpen])

  const visible = liveRound != null && !arenaOpen && !pillDismissed
  if (!visible || !liveRound) {
    // 兜底:何时该显示但被 dismiss 挡住时,新回合会复位——见创建处。
    return null
  }

  const done = liveRound.contestants.filter(
    (c) =>
      c.status === "done" || c.status === "error" || c.status === "canceled"
  ).length
  const total = liveRound.contestants.length
  const elapsed = Math.round((now - liveRound.createdAt) / 1000)

  return (
    <div
      className="fixed bottom-16 right-4 z-50 flex items-center gap-2 rounded-full border border-border bg-card px-3 py-1.5 shadow-lg"
      data-testid="pk-minimized-pill"
    >
      <button
        type="button"
        onClick={() => {
          setPillDismissed(false)
          setArenaOpen(true)
        }}
        className="flex items-center gap-2 text-sm text-foreground hover:opacity-80"
        title={t("restore")}
      >
        <Swords className="size-4 text-primary" />
        <span className="tabular-nums font-medium">
          {done}/{total}
        </span>
        <span className="hidden text-xs text-muted-foreground sm:inline">
          {t("live")} {elapsed > 0 ? `· ${elapsed}s` : ""}
        </span>
        <span
          className={cn(
            "size-2 rounded-full bg-emerald-500",
            liveRound.status === "running" && "animate-pulse"
          )}
          aria-hidden
        />
      </button>
      <button
        type="button"
        onClick={() => setPillDismissed(true)}
        className="text-muted-foreground hover:text-foreground"
        title={t("dismiss")}
        aria-label={t("dismiss")}
      >
        <X className="size-3.5" />
      </button>
    </div>
  )
}
