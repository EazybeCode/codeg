import { test } from "node:test"
import assert from "node:assert/strict"

import { RunGuard } from "../extension/lib/guard.js"

function guard() {
  const run = new RunGuard({
    runId: "r1",
    tabId: 7,
    origin: "https://app.test",
    provider: "claude",
    token: "secret",
  })
  run.nextEpoch() // the snapshot the model's refs came from
  return run
}

function call(overrides) {
  return Object.assign(
    {
      runId: "r1",
      tabId: 7,
      tool: "browser_click",
      args: { ref: "e1:3" },
      url: "https://app.test/orders?x=1",
    },
    overrides
  )
}

test("a call inside the scope is allowed", () => {
  assert.equal(guard().checkCall(call()), null)
})

test("another tab is refused even at the right origin", () => {
  assert.equal(guard().checkCall(call({ tabId: 8 })), "wrong_tab")
})

test("another origin is refused even in the right tab", () => {
  const run = guard()
  assert.equal(
    run.checkCall(call({ url: "https://evil.test/" })),
    "wrong_origin"
  )
  assert.equal(
    run.checkCall(call({ url: "http://app.test/" })),
    "wrong_origin",
    "scheme is part of the origin"
  )
  assert.equal(
    run.checkCall(call({ url: "https://app.test@evil.test/" })),
    "wrong_origin",
    "userinfo must not smuggle a different host past the check"
  )
  assert.equal(
    run.checkCall(call({ url: "chrome://settings" })),
    "wrong_origin"
  )
})

test("a call quoting another run's id is refused", () => {
  assert.equal(guard().checkCall(call({ runId: "r2" })), "detached")
})

test("a tool outside the allowlist is refused", () => {
  assert.equal(
    guard().checkCall(call({ tool: "browser_eval" })),
    "not_actionable"
  )
})

test("a ref from an older snapshot is stale", () => {
  const run = guard()
  run.nextEpoch() // the page was read again; refs are now e2:*
  assert.equal(run.checkCall(call({ args: { ref: "e1:3" } })), "stale_ref")
  assert.equal(run.checkCall(call({ args: { ref: "e2:3" } })), null)
  assert.equal(run.checkCall(call({ args: { ref: "junk" } })), "stale_ref")
  assert.equal(run.checkCall(call({ args: {} })), "stale_ref")
})

test("a same-origin navigation kills every outstanding ref", () => {
  const run = guard()
  run.invalidateRefs("navigation")
  assert.equal(
    run.checkCall(call({ args: { ref: "e1:3" } })),
    "stale_ref",
    "the epoch still matches, but it describes a document that is gone"
  )
  // A fresh reading brings them back.
  run.nextEpoch()
  assert.equal(run.checkCall(call({ args: { ref: "e2:3" } })), null)
})

test("reads are refused after Stop, not just writes", () => {
  const run = guard()
  run.stop("user")
  assert.equal(
    run.checkCall(call({ tool: "browser_snapshot", args: {} })),
    "detached"
  )
  assert.equal(run.state, "stopped")
})

test("a terminated run never becomes active again", () => {
  const run = guard()
  run.stop("user")
  run.revoke("late_revocation")
  assert.equal(run.state, "stopped")
  assert.equal(run.endReason, "user")
  assert.equal(run.active, false)

  const revoked = guard()
  revoked.revoke("cross_origin_navigation")
  revoked.stop("user")
  assert.equal(revoked.state, "revoked")
  assert.equal(revoked.endReason, "cross_origin_navigation")
})

test("the epoch counter only ever moves forward", () => {
  const run = new RunGuard({ runId: "r1", tabId: 1, origin: "https://a.test" })
  assert.equal(run.epoch, 0)
  assert.equal(run.refsValid, false)
  assert.equal(run.nextEpoch(), 1)
  assert.equal(run.nextEpoch(), 2)
  run.invalidateRefs("navigation")
  assert.equal(run.nextEpoch(), 3, "navigation resets refs, never the counter")
})
