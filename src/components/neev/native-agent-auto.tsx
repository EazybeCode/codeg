"use client"

// Auto model routing in the composer: one chip to switch it on, and one short
// line saying what the last turn ran on and, when Auto could not decide, why.
//
// Nothing here decides anything. The server routes over the caller's own
// connected agents and falls back to the manual selection, which stays on
// screen beside the chip because it IS the fallback.

import { useEffect, useState } from "react"
import { Sparkles } from "lucide-react"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import type {
  NeevActivityItem,
  NeevRoute,
} from "@/lib/neev-agent-sessions-client"
import {
  supervisionOf,
  type NeevSupervisionStep,
} from "@/lib/neev-agent-activity"
import {
  loadRoutingStatus,
  readAutoPreference,
  writeAutoPreference,
  type NeevRoutingStatus,
} from "@/lib/neev-auto-route"

/**
 * The router's status, read once per mount, and this device's Auto switch.
 *
 * `active` is true only when the switch is on AND the router is ready: a
 * missing key never sends a turn as "Auto".
 */
export function useAutoRouting(scopeKey: unknown) {
  const [status, setStatus] = useState<NeevRoutingStatus | null>(null)
  // Read once on mount. The chip renders only after the status arrives, so a
  // prerendered page never shows a switch state that differs from this.
  const [on, setOn] = useState(readAutoPreference)
  useEffect(() => {
    const controller = new AbortController()
    loadRoutingStatus(controller.signal)
      .then((value) => {
        if (!controller.signal.aborted) setStatus(value)
      })
      .catch(() => {
        // Unknown is treated as off: Auto is never offered on a guess.
        if (!controller.signal.aborted) setStatus(null)
      })
    return () => controller.abort()
  }, [scopeKey])
  return {
    status,
    on,
    active: on && status?.state === "ready",
    setOn(next: boolean) {
      writeAutoPreference(next)
      setOn(next)
    },
  }
}

/** The Auto switch. Absent when no router is configured. */
export function AutoRouteChip({
  status,
  on,
  onChange,
  disabled,
}: {
  status: NeevRoutingStatus | null
  on: boolean
  onChange: (next: boolean) => void
  disabled?: boolean
}) {
  const t = useTranslations("NativeContent")
  if (!status || status.state === "off") return null
  const missing = status.state === "missing_credential"
  const explanation = missing
    ? t("autoRouteMissingKey")
    : [
        t("autoRouteExplain", {
          chars: status.maxTaskChars,
          previous: status.maxPreviousChars,
        }),
        status.supervision !== "off" ? t("autoSupervisionExplain") : null,
      ]
        .filter(Boolean)
        .join(" ")
  return (
    <Button
      type="button"
      size="sm"
      variant={on && !missing ? "secondary" : "ghost"}
      aria-pressed={missing ? undefined : on}
      aria-label={`${t("autoRoute")}: ${explanation}`}
      title={explanation}
      disabled={disabled || missing}
      onClick={() => onChange(!on)}
      className={cn(
        "h-7 shrink-0 gap-1 rounded-full px-2 text-xs font-normal",
        missing && "text-muted-foreground line-through"
      )}
    >
      <Sparkles aria-hidden className="size-3.5" />
      {t("autoRoute")}
    </Button>
  )
}

const REASONS = [
  "not_configured",
  "missing_credential",
  "no_candidates",
  "timeout",
  "unavailable",
  "malformed",
  "rejected",
  "low_confidence",
  "usage_limited",
  "throttled",
] as const

const SUPERVISION_REASONS = [
  "no_claude_connection",
  "no_supervisor_model",
  "unsupported",
] as const

function clock(seconds: number): string {
  return new Date(seconds * 1000).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  })
}

/** One line: what the last Auto turn ran on, or why it used the selection. */
export function RouteNote({
  route,
  deciding,
  className,
}: {
  route: NeevRoute | null
  deciding?: boolean
  className?: string
}) {
  const t = useTranslations("NativeContent")
  if (deciding)
    return (
      <p
        role="status"
        className={cn("text-xs text-muted-foreground", className)}
      >
        {t("autoRouteDeciding")}
      </p>
    )
  if (!route) return null
  const choice = [route.label ?? route.model, route.effort]
    .filter(Boolean)
    .join(" · ")
  let message: string
  if (route.outcome === "routed") message = t("autoRouteRouted", { choice })
  else if (route.outcome === "kept") message = t("autoRouteKept", { choice })
  else {
    const reason = (REASONS as readonly string[]).includes(route.reason ?? "")
      ? t(`autoReason_${route.reason as (typeof REASONS)[number]}`)
      : t("autoReason_unavailable")
    message = t("autoRouteFallback", { choice, reason })
  }
  const extras: string[] = []
  const supervision = route.supervision
  if (supervision?.state === "active" && supervision.model)
    extras.push(
      t(
        supervision.phases === "plan_review"
          ? "autoSupervisedPlanReview"
          : "autoSupervisedReview",
        { model: supervision.model }
      )
    )
  else if (supervision?.state === "unavailable")
    extras.push(
      t(
        (SUPERVISION_REASONS as readonly string[]).includes(
          supervision.reason ?? ""
        )
          ? `autoSupervisionUnavailable_${supervision.reason as (typeof SUPERVISION_REASONS)[number]}`
          : "autoSupervisionUnavailable_unsupported"
      )
    )
  for (const held of route.withheld)
    extras.push(
      held.until !== null
        ? t("autoWithheldUntil", {
            provider: held.provider,
            time: clock(held.until),
          })
        : t("autoWithheld", { provider: held.provider })
    )
  if (extras.length) message = [message, ...extras].join(" · ")
  return (
    <p
      role="status"
      title={message}
      className={cn("truncate text-xs text-muted-foreground", className)}
    >
      {message}
    </p>
  )
}

/**
 * The supervision of one turn, compact: one line per step, its text one click
 * away. Plain text only — the steps are a model's reading and say so.
 */
export function SupervisionSummary({
  items,
}: {
  items: readonly NeevActivityItem[]
}) {
  const t = useTranslations("NativeContent")
  const { plan, review } = supervisionOf(items)
  if (!plan && !review) return null
  const line = (kind: "plan" | "review", step: NeevSupervisionStep) => {
    const label = step.pending
      ? t(kind === "plan" ? "supervisionPlanning" : "supervisionReviewing")
      : kind === "plan"
        ? t(
            step.verdict === "ready"
              ? "supervisionPlanReady"
              : "supervisionPlanUnavailable"
          )
        : step.verdict === "pass" ||
            step.verdict === "concerns" ||
            step.verdict === "fail"
          ? t(`supervisionReview_${step.verdict}`)
          : t("supervisionReviewUnavailable")
    return (
      <details key={kind} className="group">
        <summary
          className={cn(
            "cursor-pointer list-none text-xs text-muted-foreground",
            step.verdict === "fail" && "text-destructive",
            step.verdict === "concerns" && "text-amber-600 dark:text-amber-400"
          )}
        >
          {label}
        </summary>
        {step.detail && (
          <p className="mt-1 whitespace-pre-wrap text-xs text-muted-foreground">
            {step.detail}
          </p>
        )}
      </details>
    )
  }
  return (
    <div className="space-y-1 pt-2" aria-label={t("supervision")}>
      {plan && line("plan", plan)}
      {review && line("review", review)}
    </div>
  )
}
