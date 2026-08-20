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
        slot: 0,
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
    const html = buildPkReportHtml(
      round,
      { "0": [{ path: "index.html" }] },
      "zh-CN"
    )

    expect(html).toContain("智能体 PK 战报")
    expect(html).toContain("Qoder 获胜")
    expect(html).toContain("index.html")
    expect(html).toContain("未提供")
    expect(html).not.toContain(">0</td>")
    expect(html).not.toContain("https://")
  })

  it("embeds a single HTML artifact as a sandboxed runnable preview", () => {
    const contentBase64 = btoa("<h1>Playable</h1><script>game()</script>")
    const html = buildPkReportHtml(
      round,
      { "0": [{ path: "index.html", contentBase64 }] },
      "en"
    )

    expect(html).toContain(`data-artifact-html="${contentBase64}"`)
    expect(html).toContain('sandbox="allow-scripts allow-pointer-lock"')
    expect(html).toContain("data-open-artifact")
    expect(html).toContain("Open and run")
    expect(html).not.toContain("+const snake = true")
    expect(html).not.toContain("<h1>Playable</h1>")
  })

  it("keeps scores attached to repeated-agent contestant slots", () => {
    const repeated = {
      ...round,
      judgeResult: {
        scores: [
          {
            slot: 0,
            agentType: "qoder",
            score: 91,
            rank: 1,
            comment: "first",
          },
          {
            slot: 1,
            agentType: "qoder",
            score: 42,
            rank: 2,
            comment: "second",
          },
        ],
        summary: "first wins",
        rawText: "",
      },
      contestants: [
        { ...round.contestants[0], slot: 0, label: "Model A" },
        { ...round.contestants[0], slot: 1, label: "Model B" },
      ],
    } as PkRound

    const html = buildPkReportHtml(repeated, {}, "en")

    expect(html).toMatch(
      /Model A<\/small><\/td>\s*<td><strong class="score">91/
    )
    expect(html).toMatch(
      /Model B<\/small><\/td>\s*<td><strong class="score">42/
    )
  })
})
