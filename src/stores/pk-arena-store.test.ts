import { beforeEach, describe, expect, it, vi } from "vitest"
import {
  contestantBranchName,
  contestantContextKey,
  dbRoundToStoreRound,
  usePkArenaStore,
  type PkRound,
} from "./pk-arena-store"
import type { AgentType, PkRoundInfo } from "@/lib/types"

// Mock the API calls so createRound/markRound/removeRound don't hit the network.
vi.mock("@/lib/api", () => ({
  pkRoundCreate: vi.fn().mockResolvedValue({
    id: 1,
    folder_id: 7,
    task: "write a snake game",
    config: {
      agents: ["claude_code", "codex"],
      permission_mode: "default",
      bare_mode: false,
      effort: "default",
    },
    status: "ready",
    failure_reason: null,
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    finished_at: null,
  }),
  pkRoundUpdateStatus: vi.fn().mockResolvedValue(undefined),
  pkRoundDelete: vi.fn().mockResolvedValue(undefined),
}))

function freshStore() {
  usePkArenaStore.setState({
    rounds: [],
    activeRoundId: null,
    launcherOpen: false,
    arenaOpen: false,
    pillDismissed: false,
    hydrating: false,
  })
}

async function makeRound(overrides?: Partial<PkRound>): Promise<PkRound> {
  const round = await usePkArenaStore.getState().createRound({
    task: "write a snake game",
    folderId: 7,
    workingDir: "/tmp/repo",
    agents: ["claude_code", "codex"] as AgentType[],
  })
  return overrides ? { ...round, ...overrides } : round
}

describe("pk arena store", () => {
  beforeEach(() => {
    freshStore()
    vi.clearAllMocks()
  })

  it("creates a round with one preparing contestant per agent", async () => {
    const round = await makeRound()
    expect(round.status).toBe("ready")
    expect(round.contestants.map((c) => c.agentType)).toEqual([
      "claude_code",
      "codex",
    ])
    expect(
      round.contestants.every(
        (c) =>
          c.status === "preparing" &&
          c.contextKey === null &&
          c.connectionId === null &&
          c.conversationId === null
      )
    ).toBe(true)
    expect(usePkArenaStore.getState().activeRoundId).toBe(round.id)
    expect(usePkArenaStore.getState().rounds).toHaveLength(1)
  })

  it("patches a single contestant without touching its peers", async () => {
    const round = await makeRound()
    usePkArenaStore.getState().updateContestant(round.id, "codex", {
      status: "running",
      startedAt: 1234,
      connectionId: "conn-1",
    })
    const codex = usePkArenaStore
      .getState()
      .rounds[0].contestants.find((c) => c.agentType === "codex")
    const claude = usePkArenaStore
      .getState()
      .rounds[0].contestants.find((c) => c.agentType === "claude_code")
    expect(codex).toMatchObject({ status: "running", connectionId: "conn-1" })
    expect(claude?.status).toBe("preparing")
  })

  it("marks round status and removes rounds", async () => {
    const round = await makeRound()
    usePkArenaStore.getState().markRound(round.id, "finished")
    expect(usePkArenaStore.getState().rounds[0].status).toBe("finished")

    usePkArenaStore.getState().removeRound(round.id)
    expect(usePkArenaStore.getState().rounds).toHaveLength(0)
    expect(usePkArenaStore.getState().activeRoundId).toBeNull()
  })

  it("derives branch and context keys from the round id", () => {
    expect(contestantBranchName("r1", "claude_code")).toBe(
      "codeg-pk/r1/claude_code"
    )
    expect(contestantContextKey("r1", "codex")).toBe("pk:r1:codex")
  })

  it("revives a running round as interrupted from DB hydration", () => {
    const dbRound: PkRoundInfo = {
      id: 42,
      folder_id: 7,
      task: "write a snake game",
      config: {
        agents: ["claude_code", "codex"],
        permission_mode: "default",
        bare_mode: false,
        effort: "default",
      },
      status: "running",
      failure_reason: null,
      created_at: "2026-01-01T00:00:00Z",
      updated_at: "2026-01-01T00:00:00Z",
      finished_at: null,
    }
    const revived = dbRoundToStoreRound(dbRound, "/tmp/repo")
    expect(revived.id).toBe("42")
    expect(revived.status).toBe("interrupted")
    expect(revived.contestants.map((c) => c.status)).toEqual([
      "canceled",
      "canceled",
    ])
    expect(revived.contestants.every((c) => c.contextKey === null)).toBe(true)
  })

  it("revives a finished round unchanged from DB hydration", () => {
    const dbRound: PkRoundInfo = {
      id: 43,
      folder_id: 7,
      task: "done task",
      config: {
        agents: ["codex"],
        permission_mode: "default",
        bare_mode: false,
        effort: "default",
      },
      status: "finished",
      failure_reason: null,
      created_at: "2026-01-01T00:00:00Z",
      updated_at: "2026-01-01T01:00:00Z",
      finished_at: "2026-01-01T01:00:00Z",
    }
    const revived = dbRoundToStoreRound(dbRound, "/tmp/repo")
    expect(revived.status).toBe("finished")
    expect(revived.contestants[0].status).toBe("done")
  })
})
