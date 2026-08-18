"use client"

import { create } from "zustand"
import type { AgentType } from "@/lib/types"

/**
 * Agent PK arena — one task, N agents, isolated worktrees, a scoreboard.
 *
 * Pure data layer: no imports from React contexts so every reducer is
 * unit-testable. The orchestrator (`hooks/use-pk-round`) drives the state
 * machine; the view components only read it.
 *
 * Persistence: rounds live in localStorage so a finished round's scoreboard
 * and diff remain viewable after a restart. Live-only fields (connectionId)
 * are meaningless across restarts, and a round that was still running at
 * shutdown cannot reattach its agent processes — hydration marks those
 * `interrupted` rather than pretending they are live.
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

/** 统一的思考等级请求——「默认」表示各选手用自己的默认档。 */
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
  /** 悬浮球被用户手动关掉过——新回合/重新打开时复位。 */
  pillDismissed: boolean
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
  }): PkRound
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

const STORAGE_KEY = "codeg:pk-arena"
const LAUNCHER_LAST_KEY = "codeg:pk-launcher-last"
const MAX_PERSISTED_ROUNDS = 20

/** 上次开赛的配置,复赛时一键预填。 */
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
    // 记住配置失败不影响开赛。
  }
}

function newRoundId(): string {
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`
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

interface PersistedRound {
  id: string
  task: string
  folderId: number
  workingDir: string
  createdAt: number
  status: PkRoundStatus
  permissionMode?: PkPermissionMode
  bareMode?: boolean
  effort?: PkEffortLevel
  contestants: Array<Omit<PkContestant, "diff">>
}

function toPersisted(round: PkRound): PersistedRound {
  return {
    id: round.id,
    task: round.task,
    folderId: round.folderId,
    workingDir: round.workingDir,
    createdAt: round.createdAt,
    status: round.status,
    permissionMode: round.permissionMode,
    bareMode: round.bareMode,
    effort: round.effort,
    // eslint-disable-next-line @typescript-eslint/no-unused-vars -- diff is live-only and deliberately dropped here
    contestants: round.contestants.map(({ diff: _diff, ...rest }) => rest),
  }
}

/** A round that was mid-flight at shutdown: keep the record, drop the liveness. */
function revive(persisted: PersistedRound): PkRound {
  const wasLive = persisted.status === "ready" || persisted.status === "running"
  return {
    ...persisted,
    permissionMode: persisted.permissionMode ?? "default",
    bareMode: persisted.bareMode ?? false,
    effort: persisted.effort ?? "default",
    status: wasLive ? "interrupted" : persisted.status,
    contestants: persisted.contestants.map((c) => {
      if (!wasLive) return { ...c, diff: null }
      const settled =
        c.status === "done" || c.status === "error" || c.status === "canceled"
      return {
        ...c,
        contextKey: null,
        connectionId: null,
        status: settled ? c.status : "canceled",
        statusDetail: settled ? c.statusDetail : "interrupted",
        diff: null,
      }
    }),
  }
}

function loadPersisted(): PkRound[] {
  if (typeof window === "undefined") return []
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    if (!raw) return []
    const parsed = JSON.parse(raw) as PersistedRound[]
    if (!Array.isArray(parsed)) return []
    return parsed.slice(0, MAX_PERSISTED_ROUNDS).map(revive)
  } catch {
    return []
  }
}

function persist(rounds: PkRound[]): void {
  if (typeof window === "undefined") return
  try {
    const payload = rounds.slice(0, MAX_PERSISTED_ROUNDS).map(toPersisted)
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(payload))
  } catch {
    // Quota or serialization failure must never break the live round.
  }
}

export const usePkArenaStore = create<PkArenaState & PkArenaActions>(
  (set, get) => ({
    rounds: loadPersisted(),
    activeRoundId: null,
    launcherOpen: false,
    arenaOpen: false,
    pillDismissed: false,

    createRound: ({
      task,
      folderId,
      workingDir,
      agents,
      permissionMode,
      bareMode,
      effort,
    }) => {
      const round: PkRound = {
        id: newRoundId(),
        task,
        folderId,
        workingDir,
        createdAt: Date.now(),
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
      persist(get().rounds)
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
      persist(get().rounds)
    },

    markRound: (roundId, status) => {
      set((state) => ({
        rounds: state.rounds.map((round) =>
          round.id === roundId ? { ...round, status } : round
        ),
      }))
      persist(get().rounds)
    },

    removeRound: (roundId) => {
      set((state) => ({
        rounds: state.rounds.filter((round) => round.id !== roundId),
        activeRoundId:
          state.activeRoundId === roundId ? null : state.activeRoundId,
      }))
      persist(get().rounds)
    },

    setActiveRound: (roundId) => set({ activeRoundId: roundId }),
    setLauncherOpen: (open) => set({ launcherOpen: open }),
    setArenaOpen: (open) => set({ arenaOpen: open }),
    setPillDismissed: (dismissed) => set({ pillDismissed: dismissed }),
  })
)
