"use client"

import { create } from "zustand"
import type { AgentType, PkRoundConfig, PkRoundInfo } from "@/lib/types"
import { pkRoundCreate, pkRoundUpdateStatus, pkRoundDelete } from "@/lib/api"

/**
 * Agent PK arena — one task, N agents, isolated worktrees, a scoreboard.
 *
 * Pure data layer: no imports from React contexts so every reducer is
 * unit-testable. The orchestrator (`hooks/use-pk-round`) drives the state
 * machine; the view components only read it.
 *
 * Persistence: round metadata (task, agents, config, status) lives in the DB
 * (`pk_round` table). Live-only fields (connectionId, diff, usage) stay in
 * the Zustand store — they are meaningless across restarts. A round that was
 * still running at shutdown is marked `interrupted` on hydration.
 */

export type PkContestantStatus =
  | "preparing"
  | "connecting"
  | "ready"
  | "running"
  | "done"
  | "error"
  | "canceled"

export interface PkContestantUsage {
  inputTokens: number
  outputTokens: number
  turnCount: number
}

/** Unified reasoning-effort request — "default" means each contestant uses its own default. */
export type PkEffortLevel = "default" | "low" | "medium" | "high" | "max"

export interface PkContestant {
  /** Wire name — unique within a round (one slot per agent). */
  agentType: AgentType
  /** Advertised model options (handshake `configOptions`), for the arena pickers. */
  modelOptions: Array<{ value: string; name: string }>
  /** Advertised effort option values, for the arena picker. */
  effortOptions: string[]
  selectedModel: string | null
  selectedEffort: string | null
  /** Connections-context key; null until the orchestrator connects. */
  contextKey: string | null
  connectionId: string | null
  conversationId: number | null
  worktreePath: string | null
  branchName: string | null
  status: PkContestantStatus
  statusDetail: string | null
  startedAt: number | null
  endedAt: number | null
  durationMs: number | null
  usage: PkContestantUsage | null
  /** Populated lazily when the Diff tab opens; never persisted (can be huge). */
  diff: string | null
}

export type PkRoundStatus =
  | "ready"
  | "running"
  | "finished"
  | "canceled"
  | "interrupted"

/**
 * Round-level permission policy, applied to every contestant right after
 * connect via `setMode` — the ACP-standard mode ids (Claude Code, Codex and
 * Qoder all advertise these exact spellings). Agents that don't advertise a
 * matching mode keep their default and simply ask, as before.
 */
export type PkPermissionMode = "default" | "acceptEdits" | "bypassPermissions"

export interface PkRound {
  /** DB id of the pk_round row, as a string (used in branch names, context keys). */
  id: string
  task: string
  folderId: number
  workingDir: string
  createdAt: number
  status: PkRoundStatus
  permissionMode: PkPermissionMode
  /** Bare mode: contestants are instructed to use no skills at all. */
  bareMode: boolean
  /** Uniform reasoning-effort request, applied to every contestant. */
  effort: PkEffortLevel
  contestants: PkContestant[]
}

interface PkArenaState {
  rounds: PkRound[]
  activeRoundId: string | null
  launcherOpen: boolean
  arenaOpen: boolean
  /** The pill was manually dismissed — reset on new round / reopen. */
  pillDismissed: boolean
  /** True while the store is loading rounds from the DB on startup. */
  hydrating: boolean
}

interface PkArenaActions {
  createRound(config: {
    task: string
    folderId: number
    workingDir: string
    agents: AgentType[]
    permissionMode?: PkPermissionMode
    bareMode?: boolean
    effort?: PkEffortLevel
  }): Promise<PkRound>
  hydrateFromDb(rounds: PkRoundInfo[]): void
  updateContestant(
    roundId: string,
    agentType: AgentType,
    patch: Partial<PkContestant>
  ): void
  markRound(roundId: string, status: PkRoundStatus): void
  removeRound(roundId: string): void
  setActiveRound(roundId: string | null): void
  setLauncherOpen(open: boolean): void
  setArenaOpen(open: boolean): void
  setPillDismissed(dismissed: boolean): void
}

const LAUNCHER_LAST_KEY = "codeg:pk-launcher-last"

/** Last launcher config, for one-click prefill on rematch. */
export interface PkLauncherLastConfig {
  agents: AgentType[]
  permissionMode: PkPermissionMode
  bareMode: boolean
  effort: PkEffortLevel
  task: string
}

export function loadLastLauncherConfig(): PkLauncherLastConfig | null {
  if (typeof window === "undefined") return null
  try {
    const raw = window.localStorage.getItem(LAUNCHER_LAST_KEY)
    return raw ? (JSON.parse(raw) as PkLauncherLastConfig) : null
  } catch {
    return null
  }
}

export function saveLastLauncherConfig(config: PkLauncherLastConfig): void {
  if (typeof window === "undefined") return
  try {
    window.localStorage.setItem(LAUNCHER_LAST_KEY, JSON.stringify(config))
  } catch {
    // Failing to remember the config doesn't affect the round.
  }
}

/** Branch names ride `codeg-pk/<round>/<agent>` — slug-safe and greppable. */
export function contestantBranchName(
  roundId: string,
  agentType: AgentType
): string {
  return `codeg-pk/${roundId}/${agentType}`
}

export function contestantContextKey(
  roundId: string,
  agentType: AgentType
): string {
  return `pk:${roundId}:${agentType}`
}

/** Convert a DB PkRoundInfo row to a PkRound for the store, reviving
 * interrupted rounds and seeding empty live contestant state. */
export function dbRoundToStoreRound(
  info: PkRoundInfo,
  workingDir: string
): PkRound {
  const wasLive = info.status === "ready" || info.status === "running"
  const status: PkRoundStatus = wasLive ? "interrupted" : info.status
  return {
    id: String(info.id),
    task: info.task,
    folderId: info.folder_id,
    workingDir,
    createdAt: new Date(info.created_at).getTime(),
    status,
    permissionMode:
      (info.config.permission_mode as PkPermissionMode) ?? "default",
    bareMode: info.config.bare_mode ?? false,
    effort: (info.config.effort as PkEffortLevel) ?? "default",
    contestants: info.config.agents.map((agentType) => ({
      agentType: agentType as AgentType,
      modelOptions: [],
      effortOptions: [],
      selectedModel: null,
      selectedEffort: null,
      contextKey: null,
      connectionId: null,
      conversationId: null,
      worktreePath: null,
      branchName: null,
      status: wasLive ? "canceled" : "done",
      statusDetail: wasLive ? "interrupted" : null,
      startedAt: null,
      endedAt: null,
      durationMs: null,
      usage: null,
      diff: null,
    })),
  }
}

export const usePkArenaStore = create<PkArenaState & PkArenaActions>((set) => ({
  rounds: [],
  activeRoundId: null,
  launcherOpen: false,
  arenaOpen: false,
  pillDismissed: false,
  hydrating: true,

  hydrateFromDb: (dbRounds) => {
    set({ rounds: dbRounds, hydrating: false })
  },

  createRound: async ({
    task,
    folderId,
    workingDir,
    agents,
    permissionMode,
    bareMode,
    effort,
  }) => {
    const config: PkRoundConfig = {
      agents: agents as string[],
      permission_mode: permissionMode ?? "default",
      bare_mode: bareMode ?? false,
      effort: effort ?? "default",
    }
    const info = await pkRoundCreate(folderId, task, config)
    const round: PkRound = {
      id: String(info.id),
      task,
      folderId,
      workingDir,
      createdAt: new Date(info.created_at).getTime(),
      status: "ready",
      permissionMode: permissionMode ?? "default",
      bareMode: bareMode ?? false,
      effort: effort ?? "default",
      contestants: agents.map((agentType) => ({
        agentType,
        modelOptions: [],
        effortOptions: [],
        selectedModel: null,
        selectedEffort: null,
        contextKey: null,
        connectionId: null,
        conversationId: null,
        worktreePath: null,
        branchName: null,
        status: "preparing",
        statusDetail: null,
        startedAt: null,
        endedAt: null,
        durationMs: null,
        usage: null,
        diff: null,
      })),
    }
    set((state) => ({
      rounds: [round, ...state.rounds],
      activeRoundId: round.id,
    }))
    return round
  },

  updateContestant: (roundId, agentType, patch) => {
    set((state) => ({
      rounds: state.rounds.map((round) =>
        round.id !== roundId
          ? round
          : {
              ...round,
              contestants: round.contestants.map((c) =>
                c.agentType !== agentType ? c : { ...c, ...patch }
              ),
            }
      ),
    }))
  },

  markRound: (roundId, status) => {
    set((state) => ({
      rounds: state.rounds.map((round) =>
        round.id === roundId ? { ...round, status } : round
      ),
    }))
    // Sync status to DB (fire-and-forget — the store update is the source of
    // truth for the live UI; the DB row is for persistence across restarts).
    void pkRoundUpdateStatus(Number(roundId), status).catch(() => undefined)
  },

  removeRound: (roundId) => {
    set((state) => ({
      rounds: state.rounds.filter((round) => round.id !== roundId),
      activeRoundId:
        state.activeRoundId === roundId ? null : state.activeRoundId,
    }))
    void pkRoundDelete(Number(roundId)).catch(() => undefined)
  },

  setActiveRound: (roundId) => set({ activeRoundId: roundId }),
  setLauncherOpen: (open) => set({ launcherOpen: open }),
  setArenaOpen: (open) => set({ arenaOpen: open }),
  setPillDismissed: (dismissed) => set({ pillDismissed: dismissed }),
}))
