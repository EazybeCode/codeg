import { describe, expect, it } from "vitest"
import { buildJudgePrompt, mapPermissionToAgentMode } from "./use-pk-round"

describe("mapPermissionToAgentMode", () => {
  it("never maps full auto to Claude's deny-without-asking mode", () => {
    expect(
      mapPermissionToAgentMode("bypassPermissions", [
        "default",
        "acceptEdits",
        "dontAsk",
        "auto",
      ])
    ).toBe("auto")
  })
})

describe("buildJudgePrompt", () => {
  it("requires judge prose to follow the current interface locale", () => {
    const [block] = buildJudgePrompt(
      "实现一个页面",
      [{ agentType: "qoder", diff: "+hello" }],
      null,
      "zh-CN"
    )

    expect(block.type).toBe("text")
    if (block.type === "text") {
      expect(block.text).toContain("locale zh-CN")
      expect(block.text).toContain("Keep JSON property names unchanged")
    }
  })
})
