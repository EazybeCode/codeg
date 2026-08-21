import { describe, expect, it } from "vitest"
import type { PkRound } from "@/stores/pk-arena-store"
import { getArenaPillRound, getEffortControl } from "./pk-arena-policy"

describe("PK arena lifecycle policy", () => {
  it("keeps a terminal round available in the minimized entry", () => {
    const finished = { id: "7", status: "finished" } as PkRound
    expect(getArenaPillRound([finished], "7")).toBe(finished)
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
