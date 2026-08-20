import { describe, expect, it } from "vitest"
import { buildPkReportHtml } from "@/lib/pk-report"
import type { PkRound } from "@/stores/pk-arena-store"

const round = {
  id: "7",
  task: "实现一个中文贪吃蛇",
  createdAt: Date.parse("2026-08-20T08:00:00Z"),
  status: "finished",
  judgeResult: {
    scores: [
      {
        agentType: "qoder",
        score: 88,
        rank: 1,
        comment: "结构清晰",
      },
    ],
    summary: "Qoder 获胜",
    rawText: "",
  },
  contestants: [
    {
      slot: 0,
      agentType: "qoder",
      label: null,
      status: "done",
      durationMs: 1200,
      usage: {
        inputTokens: 0,
        outputTokens: 0,
        turnCount: 3,
        tokensReported: false,
      },
      diff: "+const snake = true",
    },
  ],
} as PkRound

describe("buildPkReportHtml", () => {
  it("builds a self-contained localized battle report without inventing tokens", () => {
    const html = buildPkReportHtml(round, { "0": ["index.html"] }, "zh-CN")

    expect(html).toContain("智能体 PK 战报")
    expect(html).toContain("Qoder 获胜")
    expect(html).toContain("index.html")
    expect(html).toContain("未提供")
    expect(html).not.toContain(">0</td>")
    expect(html).not.toContain("https://")
  })
})
