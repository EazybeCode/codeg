import { describe, expect, it } from "vitest"
import { parseLibraryItems } from "./neev-library"

const skill = {
  id: "d230e18b-8102-4560-a358-6d8aac05c712",
  name: "Brand writing",
  description: "Team tone",
  kind: "skill",
  enabled: false,
  revision: "1",
  files: ["SKILL.md", "references/tone.md"],
  setupNote: null,
}
describe("shared library responses", () => {
  it("reads team items without needing configuration or credentials", () => {
    expect(parseLibraryItems({ items: [skill] })).toEqual([skill])
  })
  it.each([
    { ...skill, id: "../escape" },
    { ...skill, kind: "script" },
    { ...skill, revision: 1 },
    { ...skill, enabled: "true" },
    { ...skill, files: [false] },
  ])("rejects malformed library rows", (row) => {
    expect(() => parseLibraryItems({ items: [row] })).toThrow()
  })
})
