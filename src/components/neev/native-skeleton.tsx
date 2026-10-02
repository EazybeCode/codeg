import { cn } from "@/lib/utils"

/** Varied widths so a placeholder reads as rows of text, not a table. */
const WIDTHS = ["w-3/4", "w-1/2", "w-2/3", "w-5/12", "w-7/12"]

/**
 * A few muted bars where a list or transcript is about to appear, so it does
 * not jump when it loads. The label is what assistive technology hears (once,
 * as a status); the bars are decoration. Without a label it is decoration
 * only, for a place that already announces its own loading status. Pulses
 * only when motion is welcome.
 */
export function NativeSkeletonRows({
  label,
  rows = 3,
  className,
  as: Tag = "div",
}: {
  label?: string
  rows?: number
  className?: string
  /** `li` inside a list, so the list stays valid markup. */
  as?: "div" | "li"
}) {
  return (
    <Tag
      role={label ? "status" : undefined}
      aria-hidden={label ? undefined : true}
      className={cn("space-y-2 py-1", className)}
    >
      {label && <span className="sr-only">{label}</span>}
      {Array.from({ length: rows }, (_, index) => (
        <span
          key={index}
          aria-hidden
          className={cn(
            "block h-3 rounded bg-muted motion-safe:animate-pulse",
            WIDTHS[index % WIDTHS.length]
          )}
        />
      ))}
    </Tag>
  )
}
