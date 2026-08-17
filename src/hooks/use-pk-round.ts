"use client"

import { useCallback, useEffect, useRef } from "react"
import {
  createConversation,
  getFolderConversation,
  gitDiff,
  gitRemoveWorktree,
  gitWorktreeAdd,
} from "@/lib/api"
import type { PromptInputBlock, SessionConfigOptionInfo } from "@/lib/types"
import {
  useAcpActions,
  useAcpEvent,
  useConnectionStore,
} from "@/contexts/acp-connections-context"
import {
  contestantBranchName,
  contestantContextKey,
  usePkArenaStore,
  type PkContestant,
  type PkContestantUsage,
  type PkEffortLevel,
  type PkPermissionMode,
  type PkRound,
} from "@/stores/pk-arena-store"

/**
 * Arena orchestrator — drives one round's contestants through the existing
 * connection machinery (no broker, no parent agent):
 *
 *   worktree → conversation row → connect(own contextKey) → same prompt
 *
 * Completion is detected from `status_changed` events per connection
 * (`prompting` → a settled state means the contestant's single turn ended),
 * which is exactly the signal the live transcript uses. Duration is measured
 * client-side between the prompt send and that transition; token/turn stats
 * are summed from the persisted conversation afterwards.
 */

/** 公平竞技规则块。裸机模式下追加到任务提示词——软约束(模型仍会看到
 * 全局技能目录的内容),但统一施加于所有选手,对比保持 apples-to-apples。 */
const BARE_MODE_RULES = [
  "FAIR-PLAY RULES (mandatory):",
  "This is a fair competition. Do NOT use any skills, slash commands, plugins,",
  "custom agents, or custom instructions — including anything from",
  "~/.claude/skills, ~/.codex/skills, ~/.agents/skills, or any other global",
  "skill store, and any .claude/skills / .agents/skills / .codex/skills",
  "directories in the repository. Use only your built-in capabilities",
  "(file read/write, running commands, web access).",
].join("\n")

function taskPromptBlocks(
  task: string,
  worktreePath: string,
  bareMode: boolean
): PromptInputBlock[] {
  return [
    {
      type: "text",
      text: [
        task,
        "",
        `Work inside this directory: ${worktreePath}`,
        "It is a fresh git worktree created for you — this is your isolated arena, no other agent writes here. Commit your work when done.",
        ...(bareMode ? ["", BARE_MODE_RULES] : []),
      ].join("\n"),
    },
  ]
}

/**
 * Apply the round's permission policy via `session/set_mode` once the agent
 * has advertised its modes. The requested mode id is only sent when the
 * agent actually advertises it: forcing an unknown id on an agent that would
 * reject it would fail the whole connect sequence, and an agent without the
 * mode simply keeps asking, exactly as before. "default" needs no call.
 *
 * The modes arrive as a `session_modes` EVENT shortly after session/new —
 * not in connect()'s resolution. The arena attaches the contestant as a
 * by-id delegation child right after connect, and the attach RE-ROUTES the
 * reverseMap to the by-id entry, so the event lands there, never on the
 * owner (contextKey) entry. Polling only the owner entry therefore times
 * out and the mode is silently skipped (field report: presets "did not
 * apply"). Poll both entries: pre-attach events land on the owner, post-
 * attach on the by-id entry.
 */
type ModesStore = {
  getConnection(key: string):
    | {
        modes?: { available_modes?: Array<{ id: string }> } | null
        configOptions?: SessionConfigOptionInfo[] | null
      }
    | undefined
}

/** 统一的思考等级 → 各 agent 通告值的最近匹配。词表各不相同
 * (claude: low/medium/high; codex: minimal/low/medium/high/max;
 * deepseek: off/low/medium/high),按规范序取最近邻,平局取更高档
 * (公平竞技下宁高勿低)。 */
const EFFORT_RANK: Record<string, number> = {
  off: 0,
  minimal: 1,
  low: 2,
  medium: 3,
  high: 4,
  max: 5,
}

function nearestEffort(requested: string, advertised: string[]): string | null {
  if (advertised.length === 0) return null
  const exact = advertised.find((v) => v === requested)
  if (exact) return exact
  const target = EFFORT_RANK[requested] ?? 3
  let best: string | null = null
  let bestDist = Number.POSITIVE_INFINITY
  for (const value of advertised) {
    const rank = EFFORT_RANK[value]
    if (rank === undefined) continue
    const dist = Math.abs(rank - target)
    if (
      dist < bestDist ||
      (dist === bestDist && best !== null && rank > (EFFORT_RANK[best] ?? 0))
    ) {
      best = value
      bestDist = dist
    }
  }
  return best
}

/** 把通告的 configOptions 折成竞技场需要的两份选项表。 */
function selectOptions(configOptions: SessionConfigOptionInfo[] | null): {
  modelOptions: Array<{ value: string; name: string }>
  effortOptions: string[]
} {
  const modelOptions: Array<{ value: string; name: string }> = []
  const effortOptions: string[] = []
  for (const option of configOptions ?? []) {
    if (option.kind?.type !== "select") continue
    if (option.id === "model" || option.id === "model_id") {
      for (const item of option.kind.options) {
        modelOptions.push({ value: item.value, name: item.name ?? item.value })
      }
    } else if (/effort|reasoning/i.test(option.id)) {
      for (const item of option.kind.options) {
        if (EFFORT_RANK[item.value] !== undefined)
          effortOptions.push(item.value)
      }
    }
  }
  return { modelOptions, effortOptions }
}

/** 双条目轮询:modes/configOptions 都走 attach 后的 by-id 路由
 * (见 applyPermissionMode 的注释)。 */
function waitForOptions(
  connectionStore: ModesStore,
  contextKey: string,
  connectionId: string | null,
  timeoutMs = 8000
): Promise<{
  modes?: { available_modes?: Array<{ id: string }> } | null
  configOptions?: SessionConfigOptionInfo[] | null
} | null> {
  return new Promise((resolve) => {
    const startedAt = Date.now()
    const poll = () => {
      const owner = connectionStore.getConnection(contextKey)
      const byId =
        connectionId != null
          ? connectionStore.getConnection(connectionId)
          : undefined
      const entry = owner ?? byId
      if (
        entry != null &&
        (entry.modes != null || entry.configOptions != null)
      ) {
        resolve(entry)
        return
      }
      if (Date.now() - startedAt >= timeoutMs) {
        resolve(null)
        return
      }
      setTimeout(poll, 200)
    }
    poll()
  })
}

/**
 * Apply the round's permission policy via `session/set_mode` once the agent
 * has advertised its modes. The requested mode id is only sent when the
 * agent actually advertises it: forcing an unknown id on an agent that would
 * reject it would fail the whole connect sequence, and an agent without the
 * mode simply keeps asking, exactly as before. "default" needs no call.
 *
 * The modes arrive as a `session_modes` EVENT shortly after session/new —
 * not in connect()'s resolution. The arena attaches the contestant as a
 * by-id delegation child right after connect, and the attach RE-ROUTES the
 * reverseMap to the by-id entry, so the event lands there, never on the
 * owner (contextKey) entry. Polling only the owner entry therefore times
 * out and the mode is silently skipped (field report: presets "did not
 * apply"). `waitForOptions` reads whichever entry the event landed on.
 */
async function applyPermissionMode(
  connectionStore: ModesStore,
  setMode: (contextKey: string, modeId: string) => Promise<void>,
  contextKey: string,
  connectionId: string | null,
  mode: PkPermissionMode
): Promise<void> {
  if (mode === "default") return
  const entry = await waitForOptions(connectionStore, contextKey, connectionId)
  const advertised = entry?.modes?.available_modes?.map((m) => m.id) ?? []
  if (!advertised.includes(mode)) return
  try {
    await setMode(contextKey, mode)
  } catch {
    // A rejected mode switch must not kill the round — the contestant just
    // runs with its default permission flow.
  }
}

async function applyPreparedOptions(
  connectionStore: ModesStore,
  setConfigOption: (
    contextKey: string,
    configId: string,
    valueId: string
  ) => Promise<void>,
  contextKey: string,
  connectionId: string | null,
  effort: PkEffortLevel
): Promise<{
  modelOptions: Array<{ value: string; name: string }>
  effortOptions: string[]
  selectedModel: string | null
  selectedEffort: string | null
}> {
  const entry = await waitForOptions(connectionStore, contextKey, connectionId)
  const options = entry?.configOptions ?? null
  const { modelOptions, effortOptions } = selectOptions(options)
  let selectedModel: string | null = null
  let selectedEffort: string | null = null
  const effortConfigId =
    (options ?? []).find(
      (o) => o.kind?.type === "select" && /effort|reasoning/i.test(o.id)
    )?.id ?? null
  if (effortConfigId) {
    const option = (options ?? []).find((o) => o.id === effortConfigId)
    const current =
      option?.kind?.type === "select" ? option.kind.current_value : null
    selectedEffort = current ?? null
  }
  const modelConfigId =
    (options ?? []).find(
      (o) =>
        o.kind?.type === "select" && (o.id === "model" || o.id === "model_id")
    )?.id ?? null
  if (modelConfigId) {
    const option = (options ?? []).find((o) => o.id === modelConfigId)
    const current =
      option?.kind?.type === "select" ? option.kind.current_value : null
    selectedModel = current ?? null
  }
  if (effort !== "default" && effortConfigId) {
    const target = nearestEffort(effort, effortOptions)
    if (target) {
      try {
        await setConfigOption(contextKey, effortConfigId, target)
        selectedEffort = target
      } catch {
        // 拒绝不致命——选手保持当前档位。
      }
    }
  }
  return { modelOptions, effortOptions, selectedModel, selectedEffort }
}

async function fetchUsage(
  conversationId: number
): Promise<PkContestantUsage | null> {
  try {
    const detail = await getFolderConversation(conversationId)
    let inputTokens = 0
    let outputTokens = 0
    let turnCount = 0
    for (const turn of detail.turns ?? []) {
      if (turn.role !== "assistant") continue
      turnCount += 1
      inputTokens += turn.usage?.input_tokens ?? 0
      outputTokens += turn.usage?.output_tokens ?? 0
    }
    return { inputTokens, outputTokens, turnCount }
  } catch {
    return null
  }
}

export function usePkRound(): {
  startRound: (round: PkRound) => Promise<void>
  startPrompt: (round: PkRound) => Promise<void>
  applyContestantSelection: (
    round: PkRound,
    contestant: PkContestant,
    configId: string,
    value: string
  ) => Promise<void>
  cancelRound: (round: PkRound) => Promise<void>
  cleanupRound: (round: PkRound, keepBranches: boolean) => Promise<void>
  fetchDiff: (round: PkRound, contestant: PkContestant) => Promise<void>
} {
  const {
    connect,
    sendPrompt,
    cancel,
    disconnect,
    setMode,
    setConfigOption,
    attachDelegationChild,
    detachDelegationChild,
  } = useAcpActions()
  const connectionStore = useConnectionStore()
  const updateContestant = usePkArenaStore((s) => s.updateContestant)
  const markRound = usePkArenaStore((s) => s.markRound)
  const roundsRef = useRef(usePkArenaStore.getState().rounds)
  useEffect(() => {
    const unsub = usePkArenaStore.subscribe((state) => {
      roundsRef.current = state.rounds
    })
    return unsub
  }, [])

  // Map connectionId → {roundId, agentType} so the event subscription can
  // resolve envelopes without re-subscribing as rounds change.
  const contestantsByConnection = useRef(
    new Map<string, { roundId: string; agentType: PkContestant["agentType"] }>()
  )

  const settleContestant = useCallback(
    async (
      roundId: string,
      agentType: PkContestant["agentType"],
      outcome: "done" | "error",
      detail?: string
    ) => {
      const endedAt = Date.now()
      const round = roundsRef.current.find((r) => r.id === roundId)
      const contestant = round?.contestants.find(
        (c) => c.agentType === agentType
      )
      if (!round || !contestant) return

      const startedAt = contestant.startedAt ?? endedAt
      updateContestant(roundId, agentType, {
        status: outcome,
        statusDetail: detail ?? null,
        endedAt,
        durationMs: endedAt - startedAt,
      })
      if (contestant.conversationId != null) {
        const usage = await fetchUsage(contestant.conversationId)
        if (usage) {
          updateContestant(roundId, agentType, { usage })
        }
      }

      const fresh = usePkArenaStore
        .getState()
        .rounds.find((r) => r.id === roundId)
      if (
        fresh &&
        fresh.contestants.every(
          (c) =>
            c.status === "done" ||
            c.status === "error" ||
            c.status === "canceled"
        )
      ) {
        markRound(roundId, "finished")
      }
    },
    [markRound, updateContestant]
  )

  useAcpEvent((envelope) => {
    if (
      envelope.type !== "status_changed" &&
      envelope.type !== "error" &&
      envelope.type !== "turn_complete"
    ) {
      return
    }
    const entry = contestantsByConnection.current.get(envelope.connection_id)
    if (!entry) return
    const round = roundsRef.current.find((r) => r.id === entry.roundId)
    const contestant = round?.contestants.find(
      (c) => c.agentType === entry.agentType
    )
    if (!round || !contestant) return

    if (envelope.type === "error") {
      if (
        contestant.status === "running" ||
        contestant.status === "connecting"
      ) {
        void settleContestant(
          entry.roundId,
          entry.agentType,
          "error",
          envelope.message
        )
      }
      return
    }

    // `turn_complete` is the REAL settle signal: the backend flips the turn
    // status at TurnComplete WITHOUT emitting a status_changed envelope
    // (session_state.rs: "bypassing StatusChanged entirely"), so waiting for
    // prompting→settled would leave finished contestants stuck on "running".
    if (envelope.type === "turn_complete") {
      if (contestant.status === "running") {
        void settleContestant(entry.roundId, entry.agentType, "done")
      }
      return
    }

    // status_changed: only the prompting edge matters here — the settle edge
    // does not exist (see turn_complete above).
    if (envelope.status === "prompting") {
      if (contestant.status === "connecting") {
        updateContestant(entry.roundId, entry.agentType, {
          status: "running",
          startedAt: Date.now(),
        })
      }
    }
  })

  const startRound = useCallback(
    async (round: PkRound) => {
      for (const agentType of round.contestants.map((c) => c.agentType)) {
        const contextKey = contestantContextKey(round.id, agentType)
        const branchName = contestantBranchName(round.id, agentType)
        const worktreePath = `${round.workingDir}/.codeg-pk/${round.id}/${agentType}`
        try {
          await gitWorktreeAdd(round.workingDir, branchName, worktreePath)
        } catch (error) {
          updateContestant(round.id, agentType, {
            status: "error",
            statusDetail: `worktree: ${String(error)}`,
          })
          continue
        }
        updateContestant(round.id, agentType, {
          branchName,
          worktreePath,
        })

        let conversationId: number | null = null
        try {
          const taskPreview = round.task.slice(0, 60)
          conversationId = await createConversation(
            round.folderId,
            agentType,
            `PK · ${taskPreview}${round.task.length > 60 ? "…" : ""}`
          )
        } catch (error) {
          updateContestant(round.id, agentType, {
            status: "error",
            statusDetail: `conversation: ${String(error)}`,
          })
          continue
        }
        updateContestant(round.id, agentType, {
          conversationId,
          contextKey,
          status: "connecting",
        })

        try {
          await connect(contextKey, agentType, worktreePath)
          const connectionId =
            connectionStore.getConnection(contextKey)?.connectionId ?? null
          if (connectionId) {
            contestantsByConnection.current.set(connectionId, {
              roundId: round.id,
              agentType,
            })
            updateContestant(round.id, agentType, { connectionId })
            // LiveTranscriptView resolves its connection via
            // useConnectionStateById, which looks the store up BY
            // connectionId — the entry shape only delegation children have
            // (attach registers contextKey == connectionId). Attach the
            // contestant the same way so the battle panes mirror the live
            // stream; done BEFORE the first prompt so the whole turn flows
            // through the by-id entry (no mid-turn hydrate needed).
            attachDelegationChild({
              connectionId,
              parentConnectionId: connectionId,
              parentToolUseId: `pk-arena-${round.id}`,
              agentType,
            })
          }
          await applyPermissionMode(
            connectionStore,
            setMode,
            contextKey,
            connectionId,
            round.permissionMode
          )
          const prepared = await applyPreparedOptions(
            connectionStore,
            setConfigOption,
            contextKey,
            connectionId,
            round.effort
          )
          updateContestant(round.id, agentType, {
            status: "ready",
            modelOptions: prepared.modelOptions,
            effortOptions: prepared.effortOptions,
            selectedModel: prepared.selectedModel,
            selectedEffort: prepared.selectedEffort,
          })
        } catch (error) {
          updateContestant(round.id, agentType, {
            status: "error",
            statusDetail: `connect/prompt: ${String(error)}`,
          })
        }
      }

      const fresh = usePkArenaStore
        .getState()
        .rounds.find((r) => r.id === round.id)
      if (fresh && fresh.contestants.every((c) => c.status === "error")) {
        markRound(round.id, "canceled")
      }
    },
    [
      connect,
      connectionStore,
      markRound,
      setMode,
      setConfigOption,
      updateContestant,
      attachDelegationChild,
    ]
  )

  const cancelRound = useCallback(
    async (round: PkRound) => {
      markRound(round.id, "canceled")
      for (const contestant of round.contestants) {
        if (
          contestant.status === "done" ||
          contestant.status === "error" ||
          contestant.status === "canceled"
        ) {
          continue
        }
        if (contestant.connectionId) {
          detachDelegationChild(contestant.connectionId)
        }
        if (contestant.contextKey) {
          try {
            await cancel(contestant.contextKey)
          } catch {
            // A connection that never came up has nothing to cancel.
          }
          void disconnect(contestant.contextKey).catch(() => undefined)
        }
        updateContestant(round.id, contestant.agentType, {
          status: "canceled",
          endedAt: Date.now(),
        })
      }
    },
    [cancel, detachDelegationChild, disconnect, markRound, updateContestant]
  )

  const cleanupRound = useCallback(
    async (round: PkRound, keepBranches: boolean) => {
      // Release the by-id viewer entries before the worktrees go.
      for (const contestant of round.contestants) {
        if (contestant.connectionId) {
          detachDelegationChild(contestant.connectionId)
        }
      }
      await Promise.allSettled(
        round.contestants
          .filter((c) => c.worktreePath != null && c.branchName != null)
          .map((c) =>
            gitRemoveWorktree(
              c.worktreePath as string,
              c.branchName as string,
              round.folderId,
              !keepBranches,
              true
            )
          )
      )
      for (const contestant of round.contestants) {
        updateContestant(round.id, contestant.agentType, {
          worktreePath: null,
        })
      }
    },
    [detachDelegationChild, updateContestant]
  )

  const fetchDiff = useCallback(
    async (round: PkRound, contestant: PkContestant) => {
      if (!contestant.worktreePath) return
      try {
        const diff = await gitDiff(contestant.worktreePath)
        updateContestant(round.id, contestant.agentType, { diff })
      } catch (error) {
        updateContestant(round.id, contestant.agentType, {
          diff: `diff unavailable: ${String(error)}`,
        })
      }
    },
    [updateContestant]
  )

  const startPrompt = useCallback(
    async (round: PkRound) => {
      markRound(round.id, "running")
      await Promise.allSettled(
        round.contestants
          .filter((c) => c.status === "ready" && c.contextKey != null)
          .map(async (contestant) => {
            const contextKey = contestant.contextKey as string
            try {
              await sendPrompt(
                contextKey,
                taskPromptBlocks(
                  round.task,
                  contestant.worktreePath ?? round.workingDir,
                  round.bareMode
                ),
                {
                  folderId: round.folderId,
                  conversationId: contestant.conversationId,
                }
              )
            } catch (error) {
              updateContestant(round.id, contestant.agentType, {
                status: "error",
                statusDetail: `prompt: ${String(error)}`,
              })
            }
          })
      )
      const fresh = usePkArenaStore
        .getState()
        .rounds.find((r) => r.id === round.id)
      if (fresh && fresh.contestants.every((c) => c.status === "error")) {
        markRound(round.id, "canceled")
      }
    },
    [markRound, sendPrompt, updateContestant]
  )

  const applyContestantSelection = useCallback(
    async (
      round: PkRound,
      contestant: PkContestant,
      configId: string,
      value: string
    ) => {
      if (!contestant.contextKey) return
      try {
        await setConfigOption(contestant.contextKey, configId, value)
        if (configId === "model" || configId === "model_id") {
          updateContestant(round.id, contestant.agentType, {
            selectedModel: value,
          })
        } else {
          updateContestant(round.id, contestant.agentType, {
            selectedEffort: value,
          })
        }
      } catch {
        // 选择被拒(模型临时下架等)不致命。
      }
    },
    [setConfigOption, updateContestant]
  )

  return {
    startRound,
    startPrompt,
    applyContestantSelection,
    cancelRound,
    cleanupRound,
    fetchDiff,
  }
}
