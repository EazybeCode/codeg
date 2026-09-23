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

import { useId, useState, useSyncExternalStore } from "react"
import { ChevronDown, Settings2, Zap } from "lucide-react"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubTrigger,
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
import {
  effectivePins,
  NEEV_SHORTLIST_MAX,
  NO_SHORTLISTS,
  resolveShortlist,
  shortlistSnapshot,
  subscribeShortlists,
  writeShortlist,
} from "@/lib/neev-model-shortlist"
import {
  NativeDropdownMenuContent as DropdownMenuContent,
  NativeDropdownMenuSubContent as DropdownMenuSubContent,
} from "./native-suspended-surface"

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
 * Which agent will run, named unambiguously.
 *
 * The provider is prefixed only when the model's own name does not identify it.
 * Both providers call their default model "Default", so a trigger reading only
 * that would leave the reader unable to tell which agent their next turn goes
 * to — but "Opus" or "GPT-5.5" names its provider by naming itself, and
 * "Claude Code · Opus" in a composer chip is width spent saying it twice.
 */
export function providerModelLabel(
  t: ContentTranslator,
  view: AgentControlsView,
  settings: NeevAgentSettings,
  provider: NeevAgentProvider
): string {
  const model = modelLabel(t, view, settings)
  const generic =
    settings.model === NEEV_DEFAULT_MODEL || view.model?.isDefault === true
  return generic ? `${view.catalog?.label ?? provider} · ${model}` : model
}

/**
 * ONE trigger, and one menu behind it: model, reasoning, speed.
 *
 * This used to be three controls side by side — a model menu, a level menu and
 * a Standard/Fast pair — which took most of a composer row on its own and put
 * three separate decisions in front of someone who wanted to type. They are one
 * decision ("how should this run"), they are read together, and they now open
 * together: a shortlist of models at the top, reasoning as a submenu, Fast as a
 * toggle that leaves the menu open, and the shortlist itself editable at the
 * bottom.
 *
 * What is offered is unchanged. Everything still comes from the catalogue, an
 * impossible combination is still disabled with its reason rather than repaired
 * into a different one, and the shortlist reorders the menu without ever adding
 * to it.
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
  // Shared across every mounted composer, so pinning a model in one tab pins
  // it in all of them. Server-rendered there is nothing saved at all.
  const pins = useSyncExternalStore(
    subscribeShortlists,
    shortlistSnapshot,
    () => NO_SHORTLISTS
  )

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

  // Everything stable the reader might want to know rides on the Fast row —
  // as a tooltip for a mouse, and as its accessible description for everyone
  // else. It used to be a paragraph under the row, which meant the composer
  // grew a second and third line of standing explanation that never changed
  // and that nobody needed twice.
  const speedDescription = fastBlocked ? blockedReason : t("speedFastCost")

  const models = view.catalog?.models ?? []
  const available = models.map((entry) => entry.id)
  // `undefined` is "never pinned anything" and `[]` is "pinned nothing", and
  // the two behave differently — so this is read without a `?? []`.
  const saved = pins[provider]
  const shortlist = resolveShortlist({
    available,
    saved,
    selected: settings.model,
  })
  // What the editor ticks AND what a toggle starts from: one value, so the
  // first click on an unticked row cannot mean "remove".
  const baseline = effectivePins({ available, saved })
  const pinned = new Set(baseline)
  // A shortlist that would show every model there is has nothing to shorten,
  // and a "More models" leading to the same three is a promise of more.
  const hasMore = models.length > shortlist.length || view.groups.length > 1

  function togglePin(id: string) {
    writeShortlist(
      provider,
      pinned.has(id)
        ? baseline.filter((entry) => entry !== id)
        : // Appended, so the order is the order they were pinned in. The cap
          // is what keeps a shortlist from becoming the full list again.
          [...baseline, id].slice(0, NEEV_SHORTLIST_MAX)
    )
  }

  /** One model, as a choice. Used in the shortlist and in "More models". */
  function modelItem(entry: NeevAgentModel) {
    return (
      <DropdownMenuRadioItem key={entry.id} value={entry.id}>
        <span className="truncate">{entry.label}</span>
      </DropdownMenuRadioItem>
    )
  }

  return (
    <div className={cn("flex min-w-0 items-center", className)}>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={disabled}
            // Still named "Model" rather than something that lists everything
            // behind it: the model is what the trigger leads with, and it is
            // what someone reaching for this control is looking for.
            aria-label={t("agentModel")}
            title={`${providerModelLabel(t, view, settings, provider)} · ${effortLabel(
              t,
              settings.effort
            )}${settings.fast ? ` · ${t("speedFast")}` : ""}`}
            className="h-7 min-w-0 max-w-64 shrink gap-1.5 rounded-full px-2 text-xs font-normal"
          >
            <span className="truncate">
              {providerModelLabel(t, view, settings, provider)}
            </span>
            {/* The level rides on the same trigger, muted. It is the second
                thing anyone checks before a costly turn, and a whole control
                of its own was more row than it was worth. */}
            <span className="shrink-0 text-muted-foreground">
              {effortLabel(t, settings.effort)}
            </span>
            {settings.fast && (
              <Zap
                aria-hidden
                className="size-3 shrink-0 text-muted-foreground"
              />
            )}
            <ChevronDown aria-hidden className="size-3 shrink-0 opacity-60" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="min-w-60">
          <DropdownMenuRadioGroup
            // Named as well as drawn: with two providers in the catalogue, a
            // bare list of model names does not say whose they are.
            aria-label={view.catalog?.label ?? provider}
            value={settings.model}
            onValueChange={(next) =>
              apply({ ...settings, model: next }, provider)
            }
          >
            {shortlist.map((id) => {
              const entry = models.find((model) => model.id === id)
              return entry ? modelItem(entry) : null
            })}
          </DropdownMenuRadioGroup>
          {/* Honesty before compactness: a provider whose discovery failed
              looks exactly like one that genuinely has a single model, and
              this menu would otherwise claim its shortlist was all there is. */}
          {view.catalog?.discoveryUnavailable && (
            <p
              role="status"
              className="px-3 py-1.5 text-xs text-muted-foreground"
            >
              {t("modelsIncomplete", { name: view.catalog.label })}
            </p>
          )}

          {hasMore && (
            <DropdownMenuSub>
              <DropdownMenuSubTrigger>
                <span className="truncate">{t("moreModels")}</span>
              </DropdownMenuSubTrigger>
              <DropdownMenuSubContent
                aria-label={t("moreModels")}
                className="max-h-80 min-w-52 overflow-y-auto"
              >
                {view.groups.map((group, index) => (
                  <div key={group.provider}>
                    {index > 0 && <DropdownMenuSeparator />}
                    <DropdownMenuLabel className="text-xs text-muted-foreground">
                      {group.label}
                    </DropdownMenuLabel>
                    <DropdownMenuRadioGroup
                      aria-label={group.label}
                      // Only the selected provider's group has a selection. A
                      // second group showing one too would say the session was
                      // two things.
                      value={group.provider === provider ? settings.model : ""}
                      onValueChange={(next) =>
                        apply({ ...settings, model: next }, group.provider)
                      }
                    >
                      {group.models.map(modelItem)}
                    </DropdownMenuRadioGroup>
                    {group.discoveryUnavailable && (
                      <p
                        role="status"
                        className="px-3 py-1.5 text-xs text-muted-foreground"
                      >
                        {t("modelsIncomplete", { name: group.label })}
                      </p>
                    )}
                  </div>
                ))}
                {lockProvider && (
                  <>
                    <DropdownMenuSeparator />
                    {/* Switching provider would rewrite history that already
                        ran, so it is not offered — it is a new chat, and
                        saying so is more use than a disabled row with no
                        explanation. */}
                    <p className="px-3 py-1.5 text-xs text-muted-foreground">
                      {t("providerIsNewChat")}
                    </p>
                  </>
                )}
              </DropdownMenuSubContent>
            </DropdownMenuSub>
          )}

          <DropdownMenuSeparator />

          <DropdownMenuSub>
            {/* Disabled when the model takes no level at all: `efforts` always
                carries the provider default, so one entry means there is
                nothing to choose between. */}
            <DropdownMenuSubTrigger
              disabled={view.efforts.length <= 1}
              aria-label={t("agentEffort")}
            >
              <span className="truncate">{t("agentEffort")}</span>
              <span className="ms-auto ps-4 text-muted-foreground">
                {effortLabel(t, settings.effort)}
              </span>
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent
              aria-label={t("agentEffort")}
              className="min-w-40"
            >
              <DropdownMenuRadioGroup
                aria-label={t("agentEffort")}
                // `null` is a real choice — the provider's own default, with no
                // flag passed — so it is carried as a value rather than as an
                // absence.
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
            </DropdownMenuSubContent>
          </DropdownMenuSub>

          <DropdownMenuCheckboxItem
            checked={settings.fast}
            disabled={fastBlocked}
            // Both states are described: the reason it cannot be used, or what
            // using it costs. Neither is discoverable from the word "Fast".
            aria-describedby={speedNote}
            title={speedDescription}
            // The menu stays open. Speed is read against the model right above
            // it, and closing on every toggle would make comparing the two a
            // matter of reopening the menu.
            onSelect={(event) => event.preventDefault()}
            onCheckedChange={(next) =>
              apply({ ...settings, fast: next === true }, provider)
            }
          >
            <Zap aria-hidden className="size-3.5 opacity-60" />
            <span className="truncate">{t("speedFast")}</span>
          </DropdownMenuCheckboxItem>

          <DropdownMenuSeparator />

          <DropdownMenuSub>
            <DropdownMenuSubTrigger>
              <Settings2 aria-hidden className="size-3.5 opacity-60" />
              <span className="truncate">{t("editShortlist")}</span>
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent
              aria-label={t("editShortlist")}
              className="max-h-80 min-w-52 overflow-y-auto"
            >
              <DropdownMenuLabel className="text-xs text-muted-foreground">
                {t("shortlistHint", { number: NEEV_SHORTLIST_MAX })}
              </DropdownMenuLabel>
              {/* Only this provider's models, and only ones the catalogue
                  lists. Pinning is about which real models come first; it can
                  never introduce one the runner has not offered. */}
              {models.map((entry) => (
                <DropdownMenuCheckboxItem
                  key={entry.id}
                  checked={pinned.has(entry.id)}
                  disabled={
                    !pinned.has(entry.id) && pinned.size >= NEEV_SHORTLIST_MAX
                  }
                  onSelect={(event) => event.preventDefault()}
                  onCheckedChange={() => togglePin(entry.id)}
                >
                  <span className="truncate">{entry.label}</span>
                </DropdownMenuCheckboxItem>
              ))}
            </DropdownMenuSubContent>
          </DropdownMenuSub>
        </DropdownMenuContent>
      </DropdownMenu>

      {/* Outside the menu, so the id resolves whether or not the menu is open —
          and one per mounted row, because several composers can be on the page
          at once and a shared id would point every Fast row at the first
          row's model. */}
      <span id={speedNote} className="sr-only">
        {speedDescription}
      </span>
      {/* Announced once, and only when a choice was NOT honoured in full.
          Turning Fast off because the new model cannot run it is a change
          nobody asked for, so it is said out loud — but the standing reason
          lives on the disabled row, which is where someone looking for it
          would point. */}
      <span role="status" className="sr-only">
        {dropped === "fast"
          ? t("speedTurnedOff")
          : dropped === "effort"
            ? t("effortReset")
            : ""}
      </span>
    </div>
  )
}
