import { test } from "node:test"
import assert from "node:assert/strict"
import { webcrypto } from "node:crypto"

import * as P from "../extension/lib/protocol.js"

// Node 18 does not expose Web Crypto globally the way an MV3 service worker
// does. Standing it up here keeps the default code path under test instead of
// testing an injected stub.
if (!globalThis.crypto) globalThis.crypto = webcrypto

test("origins are derived only from plain http(s) URLs", () => {
  assert.equal(P.originOf("https://App.Test/a/b?c=d#e"), "https://app.test")
  assert.equal(P.originOf("http://x.test:8080/"), "http://x.test:8080")
  assert.equal(P.originOf("https://x.test:443/"), "https://x.test")
  assert.equal(P.originOf("file:///etc/passwd"), null)
  assert.equal(P.originOf("chrome://settings"), null)
  assert.equal(P.originOf("javascript:alert(1)"), null)
  assert.equal(P.originOf(""), null)
  assert.equal(P.originOf(null), null)
})

test("a URL carrying userinfo never yields an origin", () => {
  // https://app.test@evil.test/ is an evil.test URL that reads like app.test.
  assert.equal(P.originOf("https://app.test@evil.test/"), null)
  assert.equal(P.originOf("https://user:pw@app.test/"), null)
})

test("the permission pattern covers one origin and nothing else", () => {
  assert.equal(P.originPattern("https://app.test"), "https://app.test/*")
  assert.equal(P.originPattern("http://x.test:8080"), "http://x.test:8080/*")
})

test("refs carry the epoch they were minted in", () => {
  assert.equal(P.makeRef(4, 17), "e4:17")
  assert.equal(P.refEpoch("e4:17"), 4)
  assert.equal(P.refEpoch("e12:0"), 12)
  assert.equal(P.refEpoch("e4"), null)
  assert.equal(P.refEpoch("nope"), null)
  assert.equal(P.refEpoch(17), null)
  assert.equal(P.refEpoch("e4:17x"), null)
})

test("run ids are generated in the shape the host accepts", () => {
  const runId = P.newRunId()
  assert.ok(P.isRunId(runId), runId)
  assert.ok(!runId.includes("-"))
  assert.ok(!P.isRunId(""))
  assert.ok(!P.isRunId("has spaces"))
  assert.ok(!P.isRunId("x".repeat(65)))
})

const validStart = {
  runId: "r1",
  provider: "claude",
  task: "do the thing",
  target: { tabId: 3, url: "https://a.test/x" },
}

test("run.start is built only from a valid request", () => {
  const message = P.runStart(validStart)
  assert.equal(message.type, "run.start")
  assert.equal(message.provider, "claude")
  assert.equal(message.target.tabId, 3)
  assert.ok(!("model" in message), "no model key unless one was asked for")
  assert.equal(P.runStart({ ...validStart, model: "sonnet" }).model, "sonnet")
})

test("run.start refuses anything the host would reject", () => {
  const bad = [
    { ...validStart, provider: "rm -rf" },
    { ...validStart, task: "" },
    { ...validStart, task: "x".repeat(P.MAX_TASK_CHARS + 1) },
    { ...validStart, target: { tabId: "3", url: "https://a.test/" } },
    { ...validStart, target: { tabId: 3, url: "chrome://settings" } },
    { ...validStart, target: { tabId: 3, url: "https://a@b.test/" } },
    { ...validStart, runId: "not a run id" },
    { ...validStart, model: "x".repeat(101) },
  ]
  for (const request of bad) {
    assert.throws(() => P.runStart(request), JSON.stringify(request))
  }
})

test("target.navigated refuses a non-http destination", () => {
  assert.throws(() => P.targetNavigated("r1", "chrome://newtab"))
  assert.equal(
    P.targetNavigated("r1", "https://a.test/next").url,
    "https://a.test/next"
  )
})

test("failure slugs are clamped to the vocabulary the host knows", () => {
  const failure = P.toolFailure({
    runId: "r1",
    callId: "c1",
    error: "something we invented",
  })
  assert.equal(failure.error, "action_failed")
  assert.equal(
    P.toolFailure({ runId: "r1", callId: "c1", error: "stale_ref" }).error,
    "stale_ref"
  )
})

test("inbound host messages are shape-checked before the panel sees them", () => {
  assert.equal(P.parseHostMessage({ type: "run.eval", runId: "r1" }), null)
  assert.equal(P.parseHostMessage(null), null)
  assert.equal(
    P.parseHostMessage({
      type: "tool.call",
      runId: "r1",
      callId: "c1",
      tool: "browser_eval",
      args: {},
    }),
    null,
    "a tool outside the allowlist is not a message we understand"
  )
  const call = P.parseHostMessage({
    type: "tool.call",
    runId: "r1",
    callId: "c1",
    tool: "browser_click",
    args: { ref: "e1:0" },
    tabId: 7,
  })
  assert.equal(call.tool, "browser_click")
  assert.deepEqual(call.args, { ref: "e1:0" })

  const output = P.parseHostMessage({
    type: "run.output",
    runId: "r1",
    channel: "weird",
    text: "y".repeat(9000),
  })
  assert.equal(output.channel, "stdout")
  assert.equal(output.text.length, 4000)

  const ended = P.parseHostMessage({
    type: "run.ended",
    runId: "r1",
    state: "revoked",
    reason: "cross_origin_navigation",
  })
  assert.equal(ended.state, "revoked")
  assert.equal(ended.reason, "cross_origin_navigation")
})

test("the tool allowlist has no escape hatch in it", () => {
  const blob = JSON.stringify(P.ALLOWED_TOOLS).toLowerCase()
  for (const forbidden of ["eval", "navigate", "cookie", "storage", "script"]) {
    assert.ok(!blob.includes(forbidden), forbidden + " must not be a tool")
  }
  assert.equal(P.ALLOWED_TOOLS.length, 6)
})
