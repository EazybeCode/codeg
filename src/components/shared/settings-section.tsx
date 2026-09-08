"use client"

/**
 * Page-level building blocks for a settings tab, one level above the row
 * grammar in `setting-card.tsx`: the titled block a page is made of, the
 * banner it reports a failed load through, and the footer it saves from.
 *
 * The split mirrors how the surfaces are read: `SettingsSection` says what a
 * group of options is for, `SettingCard` groups the options that are one
 * decision, and `SettingRow` is a single option. Before this existed every
 * section hand-rolled the same header markup, which is how the page ended up
 * with two different error banners and five subtly different save footers.
 */

import type { LucideIcon } from "lucide-react"
import { AlertCircle, Loader2 } from "lucide-react"

import { Button } from "@/components/ui/button"
import { Label } from "@/components/ui/label"
import { cn } from "@/lib/utils"

interface SettingsSectionProps {
  /** Small glyph in front of the heading; purely decorative. */
  icon?: LucideIcon
  title: React.ReactNode
  /** What this group of options is for, under the heading. */
  description?: React.ReactNode
  /**
   * Compact control pinned to the right of the heading, for a section that
   * *is* one option — a card holding a single row would only repeat the
   * heading back at the reader.
   */
  control?: React.ReactNode
  /** Wide controls sit below the explanation on narrow screens. */
  stackControlOnMobile?: boolean
  /** Ties the heading to the control it labels (`Switch`, `Select`, …). */
  htmlFor?: string
  children?: React.ReactNode
  className?: string
}

/**
 * One titled block on a settings page. Heading and purpose sit together at the
 * top (`space-y-1`, so they read as one unit rather than two stacked lines),
 * and the content below is spaced at the same rhythm the cards use inside a
 * dialog — the whole page then has a single vertical cadence.
 *
 * With `control`, the header doubles as the section's single {@link SettingRow}:
 * the heading labels the control instead of a row title one line below saying
 * the same thing. Whatever else the section holds still goes in cards below, so
 * "master switch + the options it gates" reads as one block that collapses to a
 * single line when the switch is off.
 *
 * The bordered `bg-card` surface is deliberately the same shell the other
 * settings tabs use, so adopting this component changes the grammar *inside* a
 * section without making one tab look foreign next to its siblings.
 */
export function SettingsSection({
  icon: Icon,
  title,
  description,
  control,
  stackControlOnMobile = false,
  htmlFor,
  children,
  className,
}: SettingsSectionProps) {
  const heading = (
    <>
      {Icon ? (
        <Icon
          className="size-4 shrink-0 text-muted-foreground"
          aria-hidden="true"
        />
      ) : null}
      {title}
    </>
  )

  return (
    <section
      className={cn("space-y-3 rounded-xl border bg-card p-4", className)}
    >
      {/* Same alignment rule as `SettingRow`: with an explanation the control
          sits on the heading's line, without one the two center on each other. */}
      <div
        className={cn(
          "flex justify-between gap-3",
          description ? "items-start" : "items-center",
          stackControlOnMobile &&
            "flex-col items-stretch sm:flex-row sm:items-start"
        )}
      >
        <div className="min-w-0 space-y-1">
          <h2 className="text-sm font-semibold">
            {htmlFor ? (
              // A `<label>` inside the heading, so clicking the title works the
              // control and the heading is still a heading to both roles.
              <Label
                htmlFor={htmlFor}
                className="gap-2 text-sm leading-normal font-semibold"
              >
                {heading}
              </Label>
            ) : (
              <span className="flex items-center gap-2">{heading}</span>
            )}
          </h2>
          {description ? (
            <p className="text-xs leading-5 text-muted-foreground">
              {description}
            </p>
          ) : null}
        </div>
        {control ? <div className="shrink-0">{control}</div> : null}
      </div>
      {children}
    </section>
  )
}

/**
 * A section couldn't load (or save) what it configures. Uses the `destructive`
 * tokens rather than a literal red so it stays legible in both themes and
 * follows a custom accent, and carries the glyph so the message reads as a
 * failure at a glance instead of as one more paragraph of hint text.
 */
export function SettingsError({
  children,
  className,
}: {
  children: React.ReactNode
  className?: string
}) {
  return (
    <div
      role="alert"
      className={cn(
        "flex gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs leading-5 text-destructive",
        className
      )}
    >
      {/* One text line tall so the glyph centers on the first line whatever
          the message's length — the same trick `SettingNote` uses. */}
      <span className="flex h-5 shrink-0 items-center">
        <AlertCircle className="size-3.5" aria-hidden="true" />
      </span>
      <span className="min-w-0 break-words">{children}</span>
    </div>
  )
}

interface SettingsSaveBarProps {
  onSave: () => void
  saving: boolean
  /** Blocks the button for reasons other than the save in flight (e.g. still loading). */
  disabled?: boolean
  label: React.ReactNode
  savingLabel: React.ReactNode
  className?: string
}

/**
 * Footer for a section whose values are only persisted on demand. The button
 * carries its own in-flight state, so a section never has to decide again how
 * a save renders — the spinner and the disabled window are the same everywhere.
 */
export function SettingsSaveBar({
  onSave,
  saving,
  disabled,
  label,
  savingLabel,
  className,
}: SettingsSaveBarProps) {
  return (
    <div className={cn("flex justify-end pt-1", className)}>
      <Button
        type="button"
        size="sm"
        onClick={onSave}
        disabled={disabled || saving}
      >
        {saving ? (
          <>
            <Loader2 className="size-3.5 animate-spin" aria-hidden="true" />
            {savingLabel}
          </>
        ) : (
          label
        )}
      </Button>
    </div>
  )
}
