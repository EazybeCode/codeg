/* eslint-disable @typescript-eslint/no-require-imports */
const fs = require("node:fs")
const path = require("node:path")
const assert = require("node:assert/strict")

function createFixtureRoot(scratch, explicitRoot) {
  let root
  if (explicitRoot) {
    assert(path.isAbsolute(explicitRoot), "WORKOS_E2E_ROOT must be absolute")
    root = explicitRoot
    if (fs.existsSync(root)) {
      assert(
        !fs.lstatSync(root).isSymbolicLink() && fs.statSync(root).isDirectory(),
        "WORKOS_E2E_ROOT must be a real directory"
      )
      assert.equal(
        fs.readdirSync(root).length,
        0,
        "WORKOS_E2E_ROOT must be empty"
      )
    } else fs.mkdirSync(root)
  } else {
    fs.mkdirSync(scratch, { recursive: true })
    root = fs.mkdtempSync(path.join(scratch, "task5-fixture-"))
  }
  fs.writeFileSync(
    path.join(root, "fixture-owned"),
    "Isolated workspace browser fixture\n",
    { flag: "wx" }
  )
  return root
}
function assertDiagnostics(exceptions, entries, expected) {
  assert.deepEqual(exceptions, [])
  const remaining = Object.fromEntries(
    Object.entries(expected).map(([phase, texts]) => [phase, [...texts]])
  )
  for (const entry of entries) {
    assert.equal(entry.type, "error", "Warnings are never expected")
    const allowed = remaining[entry.phase] ?? []
    const index = allowed.indexOf(entry.text)
    assert(
      index >= 0,
      `Unexpected console diagnostic: ${JSON.stringify(entry)}`
    )
    allowed.splice(index, 1)
  }
  assert(
    Object.values(remaining).every((texts) => texts.length === 0),
    "Expected diagnostic counts did not match"
  )
}
module.exports = { createFixtureRoot, assertDiagnostics }
