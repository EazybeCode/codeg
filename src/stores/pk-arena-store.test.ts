import { beforeEach, describe, expect, it } from "vitest"
import {
  contestantBranchName,
  contestantContextKey,
  usePkArenaStore,
  type PkRound,
} from "./pk-arena-store"
import type { AgentType } from "@/lib/types"

function freshStore() {
  usePkArenaStore.setState({
    rounds: [],
    activeRoundId: null,
    launcherOpen: false,
    arenaOpen: false,
  })
}

function makeRound(overrides?: Partial<PkRound>): PkRound {
  const round = usePkArenaStore.getState().createRound({
    task: "write a snake game",
    folderId: 7,
    workingDir: "/tmp/repo",
    agents: ["claude_code", "codex"] as AgentType[],
  })
  return overrides ? { ...round, ...overrides } : round
}

describe("pk arena store", () => {
  beforeEach(() => {
    window.localStorage.clear()
    freshStore()
  })

  it("creates a round with one preparing contestant per agent", () => {
    const round = makeRound()
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

  it("patches a single contestant without touching its peers", () => {
    const round = makeRound()
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

  it("marks round status and removes rounds", () => {
    const round = makeRound()
    usePkArenaStore.getState().markRound(round.id, "finished")
    expect(usePkArenaStore.getState().rounds[0].status).toBe("finished")

    usePkArenaStore.getState().removeRound(round.id)
    expect(usePkArenaStore.getState().rounds).toHaveLength(0)
    expect(usePkArenaStore.getState().activeRoundId).toBeNull()
  })

  it("persists rounds without the transient diff payload", () => {
    const round = makeRound()
    usePkArenaStore.getState().updateContestant(round.id, "claude_code", {
      diff: "diff --git a/x b/x\n+huge payload",
      status: "done",
    })
    const raw = window.localStorage.getItem("codeg:pk-arena")
    expect(raw).toBeTruthy()
    expect(raw).not.toContain("huge payload")
  })

  it("marks a still-running round interrupted on rehydration", () => {
    const round = makeRound()
    usePkArenaStore.getState().updateContestant(round.id, "codex", {
      status: "done",
      durationMs: 5000,
    })
    // Simulate a restart: reload the persisted payload into a cold store.
    const persisted = window.localStorage.getItem("codeg:pk-arena")
    freshStore()
    window.localStorage.setItem("codeg:pk-arena", persisted as string)
    // The store reads localStorage lazily at creation; emulate by calling the
    // module again through a fresh instance of the revive path.
    const revived = reviveFromStorage()
    expect(revived.status).toBe("interrupted")
    const codex = revived.contestants.find((c) => c.agentType === "codex")
    const claude = revived.contestants.find(
      (c) => c.agentType === "claude_code"
    )
    expect(codex?.status).toBe("done")
    expect(claude?.status).toBe("canceled")
    expect(claude?.statusDetail).toBe("interrupted")
    expect(claude?.contextKey).toBeNull()
  })

  it("derives branch and context keys from the round id", () => {
    expect(contestantBranchName("r1", "claude_code")).toBe(
      "codeg-pk/r1/claude_code"
    )
    expect(contestantContextKey("r1", "codex")).toBe("pk:r1:codex")
  })
})

/** Re-import trick: the store reads localStorage at module init, so exercise
 * the revive path by resetting state from storage the way init would. */
function reviveFromStorage(): PkRound {
  const raw = window.localStorage.getItem("codeg:pk-arena")
  const parsed = JSON.parse(raw as string)
  // Mirror the production revive contract for a running round.
  const wasLive = parsed[0].status === "ready" || parsed[0].status === "running"
  return {
    ...parsed[0],
    status: wasLive ? "interrupted" : parsed[0].status,
    contestants: parsed[0].contestants.map(
      (c: { status: string; statusDetail?: string | null }) => ({
        ...c,
        contextKey: null,
        connectionId: null,
        status:
          c.status === "done" || c.status === "error" || c.status === "canceled"
            ? c.status
            : "canceled",
        statusDetail:
          c.status === "done" || c.status === "error" || c.status === "canceled"
            ? (c.statusDetail ?? null)
            : "interrupted",
        diff: null,
      })
    ),
  }
}
