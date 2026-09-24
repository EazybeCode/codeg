import { cn } from "@/lib/utils"

/** Shared, decorative activity mark. The surrounding control supplies its label. */
export function AgentRunIndicator({ className }: { className?: string }) {
  return (
    <svg
      aria-hidden
      viewBox="0 0 16 16"
      className={cn("neev-run-spinner size-3.5 shrink-0", className)}
    >
      <circle
        cx="8"
        cy="8"
        r="5.5"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        opacity="0.18"
      />
      <path
        d="M8 2.5a5.5 5.5 0 0 1 5.5 5.5"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  )
}
