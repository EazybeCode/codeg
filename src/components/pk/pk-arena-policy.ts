import type { PkRoundStatus } from "@/stores/pk-arena-store"

export type PkArenaCloseAction = "minimize" | "close"

/** Closing the arena is navigation, not a destructive round action. */
export function getArenaCloseAction(status: PkRoundStatus): PkArenaCloseAction {
  return status === "ready" || status === "running" ? "minimize" : "close"
}

export type PkEffortControl =
  | {
      kind: "select"
      configId: string
      options: readonly string[]
    }
  | { kind: "unsupported" }

/** Preserve each agent's advertised effort levels; never invent global ones. */
export function getEffortControl(
  options: readonly string[],
  configId: string | null
): PkEffortControl {
  if (!configId || options.length === 0) return { kind: "unsupported" }
  return { kind: "select", configId, options }
}
