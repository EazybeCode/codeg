"use client"

import { useEffect, useMemo, useRef } from "react"
import { useTranslations } from "next-intl"
import { Check, ChevronDown } from "lucide-react"
import { useAcpAgents } from "@/hooks/use-acp-agents"
import type { AgentType, AcpAgentInfo } from "@/lib/types"
import { getAgentLabel } from "@/lib/custom-agents"
import { AgentIcon } from "@/components/agent-icon"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { cn } from "@/lib/utils"

// Simplicity rewrite (Eazybe WorkOS): the upstream selector rendered every
// enabled agent as an inline pill row with bespoke fit-measurement so it could
// collapse into a "more" popover. For a general-org workspace (sales, marketing,
// not just engineers) a wall of ~14 agent glyphs reads as noise. This replaces
// the row with a single, plain dropdown — one visible control showing the
// current agent, click to pick another — while preserving the component's public
// contract exactly (props, the auto-fallback effect, the empty state). The
// heavy measurement machinery is intentionally gone; a dropdown needs none of it.

interface AgentSelectorProps {
  defaultAgentType?: AgentType
  /** Fires on user click. The caller should treat this as confirmation. */
  onSelect: (agentType: AgentType) => void
  /**
   * Fires when `defaultAgentType` is missing/unavailable and the selector had
   * to pick a substitute on its own. Distinct from `onSelect` so the caller can
   * avoid promoting a system pick to a confirmed user choice. When omitted,
   * falls back to `onSelect` for backwards compatibility.
   */
  onFallback?: (agentType: AgentType) => void
  onAgentsLoaded?: (agents: AcpAgentInfo[]) => void
  onOpenAgentsSettings?: () => void
  disabled?: boolean
  align?: "start" | "center"
}

export function AgentSelector({
  defaultAgentType,
  onSelect,
  onFallback,
  onAgentsLoaded,
  onOpenAgentsSettings,
  disabled = false,
  align = "start",
}: AgentSelectorProps) {
  const t = useTranslations("Folder.chat.agentSelector")
  const { agents: rawAgents } = useAcpAgents()
  const agents = useMemo<AcpAgentInfo[]>(
    () => rawAgents.filter((a) => a.enabled),
    [rawAgents]
  )

  // Effective selection. Priority: prop default (when still available) → first
  // available. Kept identical to the previous implementation so callers see the
  // same selection semantics.
  const selected = useMemo<AgentType | null>(() => {
    const found = defaultAgentType
      ? agents.find((a) => a.agent_type === defaultAgentType && a.available)
      : null
    if (found) return found.agent_type
    const first = agents.find((a) => a.available)
    return first?.agent_type ?? null
  }, [agents, defaultAgentType])

  const onSelectRef = useRef(onSelect)
  const onFallbackRef = useRef(onFallback)
  const onAgentsLoadedRef = useRef(onAgentsLoaded)
  useEffect(() => {
    onSelectRef.current = onSelect
  }, [onSelect])
  useEffect(() => {
    onFallbackRef.current = onFallback
  }, [onFallback])
  useEffect(() => {
    onAgentsLoadedRef.current = onAgentsLoaded
  }, [onAgentsLoaded])

  // Notify parent when the agent list changes, and emit a *fallback* event (not
  // onSelect) when the requested preferred agent is unavailable and we had to
  // pick a substitute. Preserved verbatim from the original — see the long note
  // there: splitting the channel keeps a hydrated draft from being locked onto
  // the first agent before the folder default is applied.
  useEffect(() => {
    onAgentsLoadedRef.current?.(agents)
    const found = defaultAgentType
      ? agents.find((a) => a.agent_type === defaultAgentType && a.available)
      : null
    if (found) return
    const first = agents.find((a) => a.available)
    if (!first) return
    const fallback = onFallbackRef.current
    if (fallback) {
      fallback(first.agent_type)
    } else {
      onSelectRef.current(first.agent_type)
    }
  }, [agents, defaultAgentType])

  // Discovery includes disabled/unavailable agents; only the enabled list above
  // participates in selection and fallback. Installation is not proof of login.
  if (rawAgents.length === 0) {
    return (
      <div className="rounded-lg border border-dashed bg-muted/30 px-4 py-3 text-center text-sm text-muted-foreground">
        <div>{t("noEnabledAgents")}</div>
        {onOpenAgentsSettings ? (
          <button
            type="button"
            onClick={onOpenAgentsSettings}
            className="mt-2 inline-flex items-center rounded-md border px-2 py-1 text-xs text-foreground transition-colors hover:bg-accent cursor-pointer"
          >
            {t("openAgentsSettings")}
          </button>
        ) : null}
      </div>
    )
  }

  const selectedAgent = agents.find((a) => a.agent_type === selected) ?? null
  const selectedLabel = selectedAgent
    ? getAgentLabel(selectedAgent.agent_type)
    : ""

  return (
    <div
      data-slot="agent-selector"
      className={cn(
        "flex min-w-0 items-center",
        align === "center" && "justify-center"
      )}
    >
      <DropdownMenu>
        <DropdownMenuTrigger asChild disabled={disabled}>
          <button
            type="button"
            disabled={disabled}
            data-slot="agent-selector-trigger"
            aria-label={selectedLabel || t("chooseAgent")}
            className={cn(
              "inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border/50 bg-muted/50 px-3 py-1.5 text-xs font-medium text-foreground transition-colors",
              disabled
                ? "cursor-not-allowed opacity-40"
                : "cursor-pointer hover:bg-muted"
            )}
          >
            {selectedAgent ? (
              <span className="relative inline-flex shrink-0 items-center">
                <AgentIcon
                  agentType={selectedAgent.agent_type}
                  className="h-4 w-4"
                />
              </span>
            ) : null}
            <span className="min-w-0 truncate">
              {selectedLabel || t("chooseAgent")}
            </span>
            <ChevronDown
              className="h-3.5 w-3.5 shrink-0 text-muted-foreground"
              aria-hidden
            />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent
          align={align === "center" ? "center" : "start"}
          className="w-56"
        >
          <DropdownMenuLabel>{t("title")}</DropdownMenuLabel>
          <DropdownMenuSeparator />
          {rawAgents.map((agent) => {
            const needsSetup = !agent.enabled || !agent.available
            const notInstalled = !needsSetup && !agent.installed_version
            const label = getAgentLabel(agent.agent_type)
            const isSelected = selected === agent.agent_type
            return (
              <DropdownMenuItem
                key={agent.agent_type}
                disabled={disabled || (needsSetup && !onOpenAgentsSettings)}
                onSelect={() => {
                  if (needsSetup) onOpenAgentsSettings?.()
                  else onSelect(agent.agent_type)
                }}
                className="gap-2"
              >
                <span className="relative flex shrink-0 items-center">
                  <AgentIcon agentType={agent.agent_type} className="h-4 w-4" />
                  {notInstalled ? (
                    <span
                      aria-hidden
                      className="absolute -top-0.5 -right-0.5 h-1.5 w-1.5 rounded-full bg-amber-500 ring-1 ring-background"
                    />
                  ) : null}
                </span>
                <span className="min-w-0 flex-1 truncate">{label}</span>
                {needsSetup ? (
                  <span className="shrink-0 text-[0.625rem] text-muted-foreground">
                    {t(agent.available ? "disabled" : "unavailable")}
                  </span>
                ) : null}
                {notInstalled ? (
                  <span className="shrink-0 text-[0.625rem] text-amber-600">
                    {t("notInstalled")}
                  </span>
                ) : null}
                {isSelected ? (
                  <Check className="h-3.5 w-3.5 shrink-0" aria-hidden />
                ) : null}
              </DropdownMenuItem>
            )
          })}
          {onOpenAgentsSettings ? (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuItem onSelect={() => onOpenAgentsSettings()}>
                {t("openAgentsSettings")}
              </DropdownMenuItem>
            </>
          ) : null}
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}
