"use client"

// The model / effort / speed controls, shared by every composer that can start
// a run: the new-chat composer, the saved Agent Chat transcript, and the agent
// mode of a team chat.
//
// Three rules shape this file:
//  - Nothing is hardcoded. Every model and every level comes from
//    `GET /api/neev/agent/capabilities`. A name absent from that response is one
//    the runner has not been shown to accept, and offering it is offering a
//    failed run.
//  - An impossible combination is DISABLED with a reason, never silently
//    repaired into a different one. In particular the model is never changed to
//    make Fast possible: someone who chose Haiku and Fast asked for two things,
//    and upgrading them to Opus spends money they did not agree to.
//  - What is shown is the SESSION's stored configuration, not local state, so it
//    is still accurate after a reload.

import { useId, useState } from "react"
import { ChevronDown, Gauge, Zap } from "lucide-react"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { cn } from "@/lib/utils"
import {
  NEEV_DEFAULT_MODEL,
  type NeevAgentCapabilities,
  type NeevAgentModel,
  type NeevAgentProvider,
  type NeevAgentProviderCatalog,
  type NeevAgentSettings,
  type NeevEffort,
} from "@/lib/neev-agent-sessions-client"
import { NativeDropdownMenuContent as DropdownMenuContent } from "./native-suspended-surface"

/** Levels this build has a translated name for; anything else shows verbatim. */
const NAMED_EFFORTS = [
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
] as const
type NamedEffort = (typeof NAMED_EFFORTS)[number]

function isNamedEffort(level: string): level is NamedEffort {
  return (NAMED_EFFORTS as readonly string[]).includes(level)
}

// The messages are typed, so a translator is not `(key: string) => string`.
// Inferring it from a real call keeps these helpers usable from any component
// in this namespace without restating next-intl's generics.
function useContentTranslations() {
  return useTranslations("NativeContent")
}
type ContentTranslator = ReturnType<typeof useContentTranslations>

/**
 * Why Fast cannot be switched on.
 *
 * `provider` = this runner has no speed control at all. `model` = it has one,
 * but not for the selected model. The distinction is the whole point: one of
 * them is fixable by choosing a different model and the other is not.
 */
export type SpeedBlock = "none" | "provider" | "model"

export type AgentControlsView = {
  /** The selected provider's catalogue entry, or null if we have none. */
  catalog: NeevAgentProviderCatalog | null
  /** Provider groups to offer, in the server's own order. */
  groups: readonly NeevAgentProviderCatalog[]
  /** The selected model's entry, or null when the catalogue does not list it. */
  model: NeevAgentModel | null
  /** Levels to offer. `null` leads, because the provider default is a choice. */
  efforts: readonly (NeevEffort | null)[]
  speed: SpeedBlock
}

/**
 * What the controls may offer, given the catalogue and the current selection.
 *
 * Pure, and exported for its own sake: which combinations are possible is the
 * part worth testing directly, and it is decided before anything is rendered or
 * sent.
 */
export function viewAgentControls(
  capabilities: NeevAgentCapabilities | null,
  provider: NeevAgentProvider,
  settings: NeevAgentSettings,
  { lockProvider = false }: { lockProvider?: boolean } = {}
): AgentControlsView {
  const catalog =
    capabilities?.providers.find((entry) => entry.provider === provider) ?? null
  const groups = !capabilities
    ? []
    : lockProvider
      ? capabilities.providers.filter((entry) => entry.provider === provider)
      : capabilities.providers
  const model =
    catalog?.models.find((entry) => entry.id === settings.model) ?? null
  // A model's own list is the authority when it has one; otherwise the
  // provider's applies. Neither means "every level certainly works" — it means
  // "this is what the server said it would accept".
  //
  // Nullish coalescing, and that is the contract, not a shorthand: `null` and
  // absent mean "fall back to the provider's list", while an EMPTY array means
  // "this model takes no level at all" and must NOT fall back. Rewriting this
  // as a truthiness check would turn the second case silently into the first
  // and offer levels the server had just said were unavailable.
  const efforts: (NeevEffort | null)[] = [
    null,
    ...(model?.efforts ?? catalog?.efforts ?? []),
  ]
  const speed: SpeedBlock = !catalog
    ? "provider"
    : !catalog.supportsSpeed
      ? "provider"
      : // A model the catalogue does not list cannot be claimed to support
        // speed, so it is treated as not supporting it rather than guessed at.
        model?.supportsSpeed
        ? "none"
        : "model"
  return { catalog, groups, model, efforts, speed }
}

/**
 * The settings that can actually be honoured, given the catalogue.
 *
 * Only ever subtracts. Fast is turned OFF when the chosen model cannot do it,
 * and a level the chosen model does not advertise falls back to the provider
 * default — but Fast is never turned on, and the model is never changed, because
 * both would spend money on something nobody asked for.
 */
export function reconcileAgentSettings(
  capabilities: NeevAgentCapabilities | null,
  provider: NeevAgentProvider,
  next: NeevAgentSettings
): NeevAgentSettings {
  const view = viewAgentControls(capabilities, provider, next)
  // With no catalogue we have been told nothing, so we correct nothing: the
  // server is the validator, and inventing a restriction here would refuse a
  // combination it would have accepted.
  if (!capabilities) return next
  return {
    model: next.model,
    effort:
      next.effort === null || view.efforts.includes(next.effort)
        ? next.effort
        : null,
    fast: next.fast && view.speed === "none",
  }
}

/**
 * A level's name, falling back to the server's own token for a new one.
 *
 * The fallback is the point: the catalogue may advertise a level this build has
 * never heard of, and showing `xhigh` verbatim is more use than hiding a choice
 * the runner would have accepted.
 */
export function effortLabel(
  t: ContentTranslator,
  level: NeevEffort | null
): string {
  if (level === null) return t("effortDefault")
  return isNamedEffort(level) ? t(`effortLevel_${level}`) : level
}

/** How a model is named on its own, falling back to its raw identifier. */
export function modelLabel(
  t: ContentTranslator,
  view: AgentControlsView,
  settings: NeevAgentSettings
): string {
  if (view.model) return view.model.label
  // The session stores a model this catalogue does not list — an older free-text
  // one, or a pin that moved. Show what it will actually run as, verbatim.
  return settings.model === NEEV_DEFAULT_MODEL
    ? t("providerDefaultModel")
    : settings.model
}

/**
 * Which agent will run, as the closed trigger has to say it.
 *
 * The provider is always named, because both providers call their default
 * model "Default": a trigger reading only that would leave the reader unable to
 * tell which agent their next turn goes to without opening the menu.
 */
export function providerModelLabel(
  t: ContentTranslator,
  view: AgentControlsView,
  settings: NeevAgentSettings,
  provider: NeevAgentProvider
): string {
  return `${view.catalog?.label ?? provider} · ${modelLabel(t, view, settings)}`
}

/**
 * One compact row: model, effort, speed. Not a form — a menu and two chips, the
 * way the thing it configures is used.
 */
export function AgentControls({
  capabilities,
  provider,
  settings,
  onChange,
  disabled = false,
  lockProvider = false,
  className,
}: {
  capabilities: NeevAgentCapabilities | null
  provider: NeevAgentProvider
  settings: NeevAgentSettings
  /**
   * The chosen configuration, already reconciled, and the provider it belongs
   * to. A provider other than the current one means a DIFFERENT chat: the
   * caller decides whether that is possible here, and `lockProvider` says it is
   * not.
   */
  onChange: (next: NeevAgentSettings, provider: NeevAgentProvider) => void
  disabled?: boolean
  /** An existing session: its provider is immutable, so only its group shows. */
  lockProvider?: boolean
  className?: string
}) {
  const t = useContentTranslations()
  const view = viewAgentControls(capabilities, provider, settings, {
    lockProvider,
  })
  // Set when a change we were asked for could not be honoured in full, so the
  // row can say what it did instead of quietly doing something else.
  const [dropped, setDropped] = useState<"none" | "fast" | "effort">("none")
  // Unique per mounted row. Several of these can be on the page at once — one
  // per open tab — and a hardcoded id would mean every Fast chip pointed at the
  // FIRST row's description, so a reader would be told about another chat's
  // model. `aria-describedby` resolves by id, so the ids have to be real.
  const speedNote = useId()

  function apply(next: NeevAgentSettings, nextProvider: NeevAgentProvider) {
    const settled = reconcileAgentSettings(capabilities, nextProvider, next)
    setDropped(
      settled.fast !== next.fast
        ? "fast"
        : settled.effort !== next.effort
          ? "effort"
          : "none"
    )
    onChange(settled, nextProvider)
  }

  // Nothing has been read yet, or the read failed. Say what it runs as now and
  // offer nothing: an empty menu would imply there is nothing to choose.
  if (!capabilities)
    return (
      <span
        className={cn(
          "flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground",
          className
        )}
      >
        <span className="truncate">
          {providerModelLabel(t, view, settings, provider)}
        </span>
        {settings.effort !== null && (
          <span className="truncate">· {effortLabel(t, settings.effort)}</span>
        )}
        {settings.fast && <span className="truncate">· {t("speedFast")}</span>}
      </span>
    )

  const fastBlocked = view.speed !== "none"
  const blockedReason =
    view.speed === "provider"
      ? t("speedNoProvider", { name: view.catalog?.label ?? provider })
      : t("speedNoModel", { name: modelLabel(t, view, settings) })

  return (
    <div className={cn("flex min-w-0 flex-wrap items-center gap-1", className)}>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={disabled}
            aria-label={t("agentModel")}
            className="h-7 gap-1 rounded-full px-2 text-xs font-normal"
          >
            <span className="max-w-56 truncate">
              {providerModelLabel(t, view, settings, provider)}
            </span>
            <ChevronDown aria-hidden className="size-3 opacity-60" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="min-w-52">
          {view.groups.map((group, index) => (
            <div key={group.provider}>
              {index > 0 && <DropdownMenuSeparator />}
              <DropdownMenuLabel className="text-xs text-muted-foreground">
                {group.label}
              </DropdownMenuLabel>
              <DropdownMenuRadioGroup
                // Named as well as headed: two providers can both call a model
                // "Default", so the grouping has to be announced, not just
                // drawn.
                aria-label={group.label}
                // Only the selected provider's group has a selection. A second
                // group showing one too would say the session was two things.
                value={group.provider === provider ? settings.model : ""}
                onValueChange={(next) =>
                  apply({ ...settings, model: next }, group.provider)
                }
              >
                {group.models.map((entry) => (
                  <DropdownMenuRadioItem key={entry.id} value={entry.id}>
                    {entry.label}
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
              {/* Without this, a provider whose discovery failed looks exactly
                  like a provider that genuinely has one model — so the menu
                  would claim Default is all there is, which the server never
                  said. What IS listed stays selectable: a catalogue that could
                  not be read must not block a run that would have worked. */}
              {group.discoveryUnavailable && (
                <p
                  role="status"
                  className="px-2 py-1.5 text-xs text-muted-foreground"
                >
                  {t("modelsIncomplete", { name: group.label })}
                </p>
              )}
            </div>
          ))}
          {lockProvider && (
            <>
              <DropdownMenuSeparator />
              {/* Switching provider would rewrite history that already ran, so
                  it is not offered here — it is a new chat, and saying so is
                  more use than a disabled row with no explanation. */}
              <p className="px-2 py-1.5 text-xs text-muted-foreground">
                {t("providerIsNewChat")}
              </p>
            </>
          )}
        </DropdownMenuContent>
      </DropdownMenu>

      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={disabled || view.efforts.length <= 1}
            aria-label={t("agentEffort")}
            className="h-7 gap-1 rounded-full px-2 text-xs font-normal"
          >
            <Gauge aria-hidden className="size-3 opacity-60" />
            <span className="max-w-28 truncate">
              {effortLabel(t, settings.effort)}
            </span>
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="min-w-40">
          <DropdownMenuLabel className="text-xs text-muted-foreground">
            {t("agentEffort")}
          </DropdownMenuLabel>
          <DropdownMenuRadioGroup
            // `null` is a real choice — the provider's own default, with no flag
            // passed — so it is carried as a value rather than as an absence.
            value={settings.effort ?? ""}
            onValueChange={(next) =>
              apply(
                { ...settings, effort: next === "" ? null : next },
                provider
              )
            }
          >
            {view.efforts.map((level) => (
              <DropdownMenuRadioItem key={level ?? ""} value={level ?? ""}>
                {effortLabel(t, level)}
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenu>

      <div
        role="group"
        aria-label={t("agentSpeed")}
        className="flex items-center rounded-full border p-0.5"
      >
        <Button
          type="button"
          variant={settings.fast ? "ghost" : "secondary"}
          size="sm"
          disabled={disabled}
          aria-pressed={!settings.fast}
          onClick={() => apply({ ...settings, fast: false }, provider)}
          className="h-6 rounded-full px-2 text-xs font-normal"
        >
          {t("speedStandard")}
        </Button>
        <Button
          type="button"
          variant={settings.fast ? "secondary" : "ghost"}
          size="sm"
          disabled={disabled || fastBlocked}
          aria-pressed={settings.fast}
          // Both states are described: the reason it cannot be used, or what
          // using it costs. Neither is discoverable from the word "Fast".
          aria-describedby={speedNote}
          title={fastBlocked ? blockedReason : t("speedFastCost")}
          onClick={() => apply({ ...settings, fast: true }, provider)}
          className="h-6 gap-1 rounded-full px-2 text-xs font-normal"
        >
          <Zap aria-hidden className="size-3" />
          {t("speedFast")}
        </Button>
      </div>

      {/* The chip stays visible when Fast is impossible, and says why, rather
          than disappearing and leaving the reader to wonder where it went. */}
      {fastBlocked ? (
        <p
          id={speedNote}
          role="status"
          className="w-full text-xs text-muted-foreground"
        >
          {blockedReason}
        </p>
      ) : (
        <p id={speedNote} className="w-full text-xs text-muted-foreground">
          {t("speedFastCost")}
        </p>
      )}
      {dropped !== "none" && (
        <p role="status" className="w-full text-xs text-muted-foreground">
          {dropped === "fast" ? t("speedTurnedOff") : t("effortReset")}
        </p>
      )}
    </div>
  )
}

/** The live run configuration, as one line of prose for a header or a summary. */
export function AgentConfigurationSummary({
  settings,
  capabilities,
  provider,
}: {
  settings: NeevAgentSettings
  capabilities: NeevAgentCapabilities | null
  provider: NeevAgentProvider
}) {
  const t = useContentTranslations()
  const view = viewAgentControls(capabilities, provider, settings)
  return (
    <>
      {providerModelLabel(t, view, settings, provider)}
      {settings.effort !== null ? ` · ${effortLabel(t, settings.effort)}` : ""}
      {settings.fast ? ` · ${t("speedFast")}` : ""}
    </>
  )
}
