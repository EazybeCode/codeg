import type { PkRound } from "@/stores/pk-arena-store"

export type PkArenaCloseAction = "minimize"

/** Closing the arena is always navigation, independent of round lifecycle. */
export function getArenaCloseAction(): PkArenaCloseAction {
  return "minimize"
}

/** Pick the round represented by the minimized entry without hiding history. */
export function getArenaPillRound(
  rounds: readonly PkRound[],
  activeRoundId: string | null
): PkRound | null {
  return (
    rounds.find((round) => round.id === activeRoundId) ??
    rounds.find(
      (round) => round.status === "ready" || round.status === "running"
    ) ??
    rounds[0] ??
    null
  )
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
