"use client"

import { forwardRef } from "react"
import { useTranslations } from "next-intl"
import { AgentIcon } from "@/components/agent-icon"
import { getAgentLabel } from "@/lib/custom-agents"
import { cn } from "@/lib/utils"
import type { PkContestant } from "@/stores/pk-arena-store"

/**
 * One row per contestant: identity, live status, and the three scoreboard
 * numbers (duration / output tokens / turns). Forwarded as a ref so the
 * arena can export it as a share image without the dialog chrome.
 */

function formatDuration(ms: number): string {
  const seconds = Math.round(ms / 1000)
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  return `${minutes}m${String(seconds % 60).padStart(2, "0")}s`
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`
  return String(n)
}

const STATUS_DOT: Record<PkContestant["status"], string> = {
  preparing: "bg-muted-foreground/50",
  connecting: "bg-sky-500 animate-pulse",
  running: "bg-emerald-500 animate-pulse",
  done: "bg-emerald-600",
  error: "bg-red-500",
  canceled: "bg-muted-foreground/50",
}

/** Literal keys — next-intl's typed messages reject dynamic concatenation. */
function useContestantStatusLabel() {
  const t = useTranslations("PkArena.scoreboard")
  const labels = {
    preparing: t("status.preparing"),
    connecting: t("status.connecting"),
    running: t("status.running"),
    done: t("status.done"),
    error: t("status.error"),
    canceled: t("status.canceled"),
  }
  return (status: PkContestant["status"]) => labels[status]
}

export const PkScoreboard = forwardRef<
  HTMLDivElement,
  { contestants: PkContestant[]; now: number }
>(function PkScoreboard({ contestants, now }, ref) {
  const t = useTranslations("PkArena.scoreboard")
  const statusLabel = useContestantStatusLabel()

  return (
    <div
      ref={ref}
      className="grid gap-2 bg-background px-3 py-2"
      style={{
        gridTemplateColumns: `repeat(${contestants.length}, minmax(0, 1fr))`,
      }}
      data-testid="pk-scoreboard"
    >
      {contestants.map((contestant) => {
        const elapsed =
          contestant.durationMs ??
          (contestant.startedAt != null ? now - contestant.startedAt : null)
        const live =
          contestant.status === "running" || contestant.status === "connecting"
        return (
          <div
            key={contestant.agentType}
            className="flex min-w-0 items-center gap-2.5 rounded-lg border border-border bg-card px-3 py-2"
          >
            <span
              className={cn(
                "size-2 shrink-0 rounded-full",
                STATUS_DOT[contestant.status]
              )}
              aria-hidden
            />
            <span className="inline-flex h-7 w-7 shrink-0 items-center justify-center rounded-md border border-border bg-background">
              <AgentIcon agentType={contestant.agentType} className="size-4" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm font-medium text-foreground">
                {getAgentLabel(contestant.agentType)}
              </div>
              <div className="flex items-baseline gap-2 text-xs text-muted-foreground">
                <span className="tabular-nums">
                  {statusLabel(contestant.status)}
                </span>
                {elapsed != null ? (
                  <>
                    <span aria-hidden>·</span>
                    <span
                      className={cn(
                        "tabular-nums",
                        live && "font-medium text-foreground"
                      )}
                    >
                      {formatDuration(elapsed)}
                    </span>
                  </>
                ) : null}
                {contestant.usage ? (
                  <>
                    <span aria-hidden>·</span>
                    <span className="tabular-nums">
                      {formatTokens(contestant.usage.outputTokens)}{" "}
                      {t("tokensUnit")}
                    </span>
                    <span aria-hidden>·</span>
                    <span className="tabular-nums">
                      {contestant.usage.turnCount} {t("turnsUnit")}
                    </span>
                  </>
                ) : null}
              </div>
              {contestant.statusDetail ? (
                <div
                  className="truncate text-xs text-muted-foreground/80"
                  title={contestant.statusDetail}
                >
                  {contestant.statusDetail}
                </div>
              ) : null}
            </div>
          </div>
        )
      })}
    </div>
  )
})
