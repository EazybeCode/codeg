import { describe, expect, it } from "vitest"
import { getArenaCloseAction, getEffortControl } from "./pk-arena-policy"

describe("PK arena lifecycle policy", () => {
  it("minimizes a prepared round instead of canceling it", () => {
    expect(getArenaCloseAction("ready")).toBe("minimize")
  })

  it("keeps an explicit cancel action separate from closing the arena", () => {
    expect(getArenaCloseAction("running")).toBe("minimize")
    expect(getArenaCloseAction("finished")).toBe("close")
  })
})

describe("PK contestant reasoning capability", () => {
  it("shows the exact levels advertised by Qoder", () => {
    expect(getEffortControl(["low", "medium"], "reasoning_effort")).toEqual({
      kind: "select",
      configId: "reasoning_effort",
      options: ["low", "medium"],
    })
  })

  it("shows an unsupported state instead of silently hiding the field", () => {
    expect(getEffortControl([], null)).toEqual({ kind: "unsupported" })
  })
})
