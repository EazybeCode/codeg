import { describe, expect, it } from "vitest"
import { JSDOM } from "jsdom"
import {
  buildPkReportHtml,
  pickRunnableHtmlPath,
  reportableArtifactPaths,
} from "@/lib/pk-report"
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
  it("ignores the worktree .git control file when selecting a runnable HTML artifact", () => {
    expect(pickRunnableHtmlPath([".git", "index.html"])).toBe("index.html")
    expect(pickRunnableHtmlPath([".git\\config", "index.html"])).toBe(
      "index.html"
    )
    expect(reportableArtifactPaths([".git", "index.html"])).toEqual([
      "index.html",
    ])
  })

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
    expect(html).toContain("data-showcase")
    expect(html).toContain("data-artifact-trigger")
    expect(html).toContain('sandbox="allow-scripts allow-pointer-lock"')
    expect(html).toContain("data-open-artifact")
    expect(html).toContain("Open and run")
    expect(html.indexOf("data-showcase")).toBeLessThan(html.indexOf("Results"))
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

  it("runs the first embedded entry immediately and switches entries on click", () => {
    const twoEntries = {
      ...round,
      contestants: [
        { ...round.contestants[0], slot: 0, label: "First" },
        { ...round.contestants[0], slot: 1, label: "Second" },
      ],
    } as PkRound
    const html = buildPkReportHtml(
      twoEntries,
      {
        "0": [
          {
            path: "index.html",
            contentBase64: btoa("<h1>First entry</h1>"),
          },
        ],
        "1": [
          {
            path: "game.html",
            contentBase64: btoa("<h1>Second entry</h1>"),
          },
        ],
      },
      "en"
    )
    const dom = new JSDOM(html, {
      runScripts: "dangerously",
      beforeParse(window) {
        Object.defineProperty(window, "TextDecoder", { value: TextDecoder })
      },
    })
    const frame = dom.window.document.querySelector("iframe")
    const triggers = dom.window.document.querySelectorAll<HTMLElement>(
      "[data-artifact-trigger]"
    )

    expect(frame?.srcdoc).toContain("First entry")
    triggers[1].click()
    expect(frame?.srcdoc).toContain("Second entry")
    expect(triggers[0].getAttribute("aria-pressed")).toBe("false")
    expect(triggers[1].getAttribute("aria-pressed")).toBe("true")
  })
})
