/* eslint-disable @typescript-eslint/no-require-imports */
const { test } = require("node:test")
const assert = require("node:assert/strict")
const fs = require("node:fs")
const os = require("node:os")
const path = require("node:path")
const {
  createFixtureRoot,
  assertDiagnostics,
} = require("./workspace-e2e-support.cjs")
test("creates a missing ignored scratch parent and refuses occupied explicit roots", () => {
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), "workspace-harness-"))
  const root = createFixtureRoot(path.join(parent, "missing"))
  assert(fs.statSync(root).isDirectory())
  const occupied = path.join(parent, "occupied")
  fs.mkdirSync(occupied)
  fs.writeFileSync(path.join(occupied, "keep"), "user data")
  assert.throws(() => createFixtureRoot(parent, occupied), /empty/)
  assert.equal(
    fs.readFileSync(path.join(occupied, "keep"), "utf8"),
    "user data"
  )
  assert.throws(() => createFixtureRoot(parent, "relative"), /absolute/)
})
test("rejects unexpected errors and duplicate expected errors even inside failure windows", () => {
  const expected = { failure: ["expected resource error"] }
  const entry = {
    phase: "failure",
    type: "error",
    text: "expected resource error",
  }
  assertDiagnostics([], [entry], expected)
  assert.throws(() =>
    assertDiagnostics(
      [],
      [entry, { ...entry, text: "unrelated failure" }],
      expected
    )
  )
  assert.throws(() => assertDiagnostics([], [entry, entry], expected))
  assert.throws(() =>
    assertDiagnostics([], [{ ...entry, phase: "successful retry" }], expected)
  )
  assert.throws(() =>
    assertDiagnostics([], [{ ...entry, type: "warning" }], expected)
  )
  assert.throws(() => assertDiagnostics(["page exception"], [entry], expected))
})
