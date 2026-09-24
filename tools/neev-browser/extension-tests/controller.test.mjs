import { test } from "node:test"
import assert from "node:assert/strict"

import { Controller } from "../extension/lib/controller.js"
import {
  makeEnv,
  makeFakeContent,
  settle,
  startRun,
} from "./helpers/fake-env.mjs"

function setup(options = {}) {
  const env = makeEnv(options)
  return { env, controller: new Controller(env) }
}

function results(env) {
  return env.port.posted.filter((message) => message.type === "tool.result")
}

async function emitCall(env, controller, call) {
  env.port.emit(
    Object.assign(
      {
        type: "tool.call",
        runId: controller.guard ? controller.guard.runId : "r-missing",
        callId: "c1",
        tabId: 7,
        args: {},
      },
      call
    )
  )
  await settle()
}

// ------------------------------------------------------------------- attach

test("attaching injects the page agent, hands it the run token, and starts the run", async () => {
  const { env, controller } = setup()
  await startRun(controller, env, { task: "read the orders" })

  assert.deepEqual(env.injected, [7])
  const attach = env.contentCalls("attach")[0]
  assert.equal(attach.token, controller.guard.token)

  const start = env.port.sentOfType("run.start")[0]
  assert.equal(start.provider, "claude")
  assert.equal(start.task, "read the orders")
  assert.deepEqual(start.target, { tabId: 7, url: "https://app.test/orders" })
  assert.ok(!("model" in start))
  assert.equal(env.lastState().status, "running")
  assert.equal(env.lastState().run.runDir, "/tmp/runs/" + start.runId)
})

test("attaching is refused when the origin permission was not granted", async () => {
  const { env, controller } = setup({ originGranted: false })
  await controller.start({ provider: "claude", task: "go", tabId: 7 })

  assert.equal(env.port.posted.length, 0, "no native port before consent")
  assert.equal(env.injected.length, 0)
  assert.match(env.lastState().error, /not granted/)
})

test("attaching is refused for a tab that is not an http(s) page", async () => {
  const { env, controller } = setup({
    tabs: [{ id: 7, url: "chrome://settings", windowId: 1, active: true }],
  })
  await controller.start({ provider: "claude", task: "go", tabId: 7 })
  assert.equal(env.injected.length, 0)
  assert.match(env.lastState().error, /not an http/)
})

// -------------------------------------------------------------------- reads

test("a snapshot mints a new epoch every time, and the host is told the tab's URL", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)

  await emitCall(env, controller, { tool: "browser_snapshot" })
  await emitCall(env, controller, { tool: "browser_snapshot", callId: "c2" })

  const [first, second] = results(env)
  assert.equal(first.result.snapshot.epoch, 1)
  assert.equal(second.result.snapshot.epoch, 2)
  assert.equal(first.url, "https://app.test/orders")
  assert.equal(first.result.snapshot.nodes[0].ref, "e1:0")
})

// ---------------------------------------------------------------- the scope

test("a call naming a different tab is refused without touching the page", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_snapshot", tabId: 9 })

  assert.equal(results(env)[0].error, "wrong_tab")
  assert.equal(env.contentCalls("snapshot").length, 0)
})

test("origin drift the host did not notice fails the call and revokes the run", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  // The tab is somewhere else by the time the call arrives.
  env.tabs.set(7, {
    id: 7,
    url: "https://evil.test/collect",
    windowId: 1,
    active: true,
  })

  await emitCall(env, controller, { tool: "browser_snapshot" })

  assert.equal(results(env)[0].error, "wrong_origin")
  assert.equal(env.contentCalls("snapshot").length, 0)
  assert.equal(
    env.port.sentOfType("target.revoked")[0].reason,
    "cross_origin_navigation"
  )
  assert.equal(env.contentCalls("detach").length, 1)
})

test("a call for a tab that has gone is refused and ends the run", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  env.tabs.delete(7)

  await emitCall(env, controller, { tool: "browser_snapshot" })

  assert.equal(results(env)[0].error, "detached")
  assert.equal(env.port.sentOfType("target.revoked")[0].reason, "tab_closed")
})

test("a ref from an earlier reading of the page is stale, not applied", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_snapshot" })
  await emitCall(env, controller, { tool: "browser_snapshot", callId: "c2" })

  await emitCall(env, controller, {
    tool: "browser_click",
    callId: "c3",
    args: { ref: "e1:0" },
  })

  assert.equal(results(env)[2].error, "stale_ref")
  assert.equal(env.contentCalls("click").length, 0)
})

// ----------------------------------------------------------------- approval

test("a click waits for the person, then runs and re-reads the page", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_snapshot" })

  await emitCall(env, controller, {
    tool: "browser_click",
    callId: "c2",
    args: { ref: "e1:0" },
  })

  const pending = env.lastState().approval
  assert.equal(pending.tool, "browser_click")
  assert.equal(pending.name, "Save")
  assert.equal(pending.consequential, true)
  assert.equal(env.contentCalls("click").length, 0, "nothing happens yet")

  await controller.onPanelMessage({
    kind: "panel.approval",
    id: pending.id,
    decision: "approve",
  })
  await settle()

  assert.equal(env.contentCalls("click").length, 1)
  const result = results(env)[1].result
  assert.equal(result.ok, true)
  assert.equal(result.approval, "approved")
  assert.equal(result.snapshot.epoch, 2, "the action is followed by a re-read")
  assert.equal(env.lastState().approval, null)
})

test("Deny is a normal result with a fresh snapshot, not an error", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_snapshot" })
  await emitCall(env, controller, {
    tool: "browser_click",
    callId: "c2",
    args: { ref: "e1:0" },
  })

  await controller.onPanelMessage({
    kind: "panel.approval",
    id: env.lastState().approval.id,
    decision: "deny",
  })
  await settle()

  const result = results(env)[1].result
  assert.equal(result.ok, false)
  assert.equal(result.approval, "denied")
  assert.equal(result.snapshot.epoch, 2)
  assert.equal(env.contentCalls("click").length, 0)
})

test("typing into a protected field is refused before anyone is asked to approve it", async () => {
  const content = makeFakeContent({
    describe: () => ({
      ok: true,
      node: {
        tag: "input",
        role: "password",
        name: "",
        sensitive: true,
        leavesOrigin: false,
        consequential: false,
        enabled: true,
      },
    }),
  })
  const { env, controller } = setup({ content })
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_snapshot" })

  await emitCall(env, controller, {
    tool: "browser_type",
    callId: "c2",
    args: { ref: "e1:0", text: "hunter2" },
  })

  assert.equal(results(env)[1].error, "not_actionable")
  assert.equal(env.lastState().approval, null, "no approval was ever offered")
  assert.equal(env.contentCalls("type").length, 0)
})

test("a link that leaves the site is refused; that is the user's click to make", async () => {
  const content = makeFakeContent({
    describe: () => ({
      ok: true,
      node: {
        tag: "a",
        role: "link",
        name: "Pay with PayPal",
        sensitive: false,
        leavesOrigin: true,
        consequential: true,
        enabled: true,
      },
    }),
  })
  const { env, controller } = setup({ content })
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_snapshot" })
  await emitCall(env, controller, {
    tool: "browser_click",
    callId: "c2",
    args: { ref: "e1:0" },
  })

  assert.equal(results(env)[1].error, "not_actionable")
  assert.equal(env.contentCalls("click").length, 0)
})

test("the handoff tool blocks on the panel and returns what the person chose", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  await emitCall(env, controller, {
    tool: "browser_request_user_action",
    args: { reason: "login required", instruction: "Sign in, then continue." },
  })

  const handoff = env.lastState().handoff
  assert.equal(handoff.instruction, "Sign in, then continue.")

  await controller.onPanelMessage({
    kind: "panel.handoff",
    id: handoff.id,
    outcome: "done",
  })
  await settle()

  assert.equal(results(env)[0].result.outcome, "done")
  assert.equal(results(env)[0].result.snapshot.epoch, 1)
  assert.equal(env.lastState().handoff, null)
})

// -------------------------------------------------------------- screenshots

test("a screenshot of a background tab is unavailable, not a picture of something else", async () => {
  const { env, controller } = setup({
    tabs: [
      {
        id: 7,
        url: "https://app.test/orders",
        windowId: 1,
        active: false,
      },
    ],
  })
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_screenshot" })
  assert.equal(results(env)[0].error, "screenshot_unavailable")
})

test("an oversized screenshot is retried at lower quality and then refused", async () => {
  const attempts = []
  const { env, controller } = setup({
    capture: (windowId, quality) => {
      attempts.push(quality)
      return "data:image/jpeg;base64," + "A".repeat(800000)
    },
  })
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_screenshot" })

  assert.deepEqual(attempts, [50, 30, 15])
  assert.equal(results(env)[0].error, "screenshot_unavailable")
})

// ------------------------------------------------------------- revocation

test("a same-origin navigation is reported and kills the outstanding refs", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_snapshot" })

  controller.onTabUpdated(7, { url: "https://app.test/orders/9" })
  await settle()

  assert.equal(
    env.port.sentOfType("target.navigated")[0].url,
    "https://app.test/orders/9"
  )
  assert.equal(env.contentCalls("reset").length, 1, "the pointer goes away")
  assert.equal(
    controller.guard.active,
    true,
    "same origin does not end the run"
  )

  await emitCall(env, controller, {
    tool: "browser_click",
    callId: "c2",
    args: { ref: "e1:0" },
  })
  assert.equal(results(env)[1].error, "stale_ref")
})

test("a cross-origin navigation revokes the run immediately", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)

  controller.onTabUpdated(7, { url: "https://evil.test/" })
  await settle()

  assert.equal(
    env.port.sentOfType("target.revoked")[0].reason,
    "cross_origin_navigation"
  )
  assert.equal(env.contentCalls("detach").length, 1)
  assert.equal(controller.guard.active, false)
})

test("closing the tab, replacing it, or dropping the permission all revoke", async () => {
  for (const [name, act] of [
    ["tab_closed", (controller) => controller.onTabRemoved(7)],
    ["tab_replaced", (controller) => controller.onTabReplaced(9, 7)],
    [
      "permission_revoked",
      (controller) =>
        controller.onPermissionsRemoved({ origins: ["https://app.test/*"] }),
    ],
    ["panel_closed", (controller) => controller.onPanelDisconnect()],
  ]) {
    const { env, controller } = setup()
    await startRun(controller, env)
    act(controller)
    await settle()
    assert.equal(env.port.sentOfType("target.revoked")[0].reason, name)
  }
})

test("an unrelated permission removal does not touch the run", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  controller.onPermissionsRemoved({ origins: ["https://other.test/*"] })
  await settle()
  assert.equal(env.port.sentOfType("target.revoked").length, 0)
  assert.equal(controller.guard.active, true)
})

// -------------------------------------------------------------------- stop

test("Stop clears the pending approval and the queued call never reaches the page", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_snapshot" })
  await emitCall(env, controller, {
    tool: "browser_click",
    callId: "c2",
    args: { ref: "e1:0" },
  })
  assert.ok(env.lastState().approval, "an approval is waiting")

  controller.stop()
  await settle()

  assert.equal(env.port.sentOfType("run.stop").length, 1)
  assert.equal(env.lastState().approval, null, "the prompt is gone")
  assert.equal(env.contentCalls("click").length, 0, "the click never happened")
  assert.equal(env.contentCalls("detach").length, 1)
  assert.equal(results(env)[1].error, "detached")
})

test("run.ended detaches, disconnects, and leaves the panel idle", async () => {
  const { env, controller } = setup()
  const start = await startRun(controller, env)

  env.port.emit({
    type: "run.ended",
    runId: start.runId,
    state: "stopped",
    reason: "agent_exited",
  })
  await settle()

  assert.equal(env.contentCalls("detach").length, 1)
  assert.equal(env.port.disconnected, true)
  const state = env.lastState()
  assert.equal(state.status, "idle")
  assert.deepEqual(
    { state: state.ended.state, reason: state.ended.reason },
    { state: "stopped", reason: "agent_exited" }
  )
  assert.equal(controller.guard, null)
})

test("a straggling tool call after the run ended is refused, not executed", async () => {
  const { env, controller } = setup()
  const start = await startRun(controller, env)
  env.port.emit({
    type: "run.ended",
    runId: start.runId,
    state: "stopped",
    reason: "user",
  })
  await settle()

  env.port.emit({
    type: "tool.call",
    runId: start.runId,
    callId: "c9",
    tool: "browser_click",
    args: { ref: "e1:0" },
    tabId: 7,
  })
  await settle()

  assert.equal(env.contentCalls("click").length, 0)
  assert.equal(env.contentCalls("snapshot").length, 0)
})

test("losing the native port ends the run on our side too", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  env.port.drop()
  await settle()

  assert.equal(controller.guard, null)
  assert.equal(env.lastState().ended.reason, "host_disconnected")
  assert.equal(env.contentCalls("detach").length, 1)
})

test("a host that never answers Stop is given up on after the grace period", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  controller.stop()
  await settle()
  assert.equal(env.lastState().status, "stopping")

  env.flushTimers()
  await settle()
  assert.equal(env.lastState().status, "idle")
  assert.equal(env.lastState().ended.reason, "host_silent")
})

test("a start_failed error from the host ends the run and is shown verbatim", async () => {
  const { env, controller } = setup()
  await controller.start({ provider: "codex", task: "go", tabId: 7 })
  const start = env.port.sentOfType("run.start")[0]

  env.port.emit({
    type: "error",
    runId: start.runId,
    code: "start_failed",
    message: "no working codex CLI found, tried /usr/local/bin/codex",
  })
  await settle()

  assert.match(env.lastState().error, /no working codex CLI/)
  assert.equal(env.lastState().status, "idle")
  assert.equal(controller.guard, null)
})

test("scrolling to an element from a dead reading is refused too", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_snapshot" })

  controller.onTabUpdated(7, { url: "https://app.test/orders/9" })
  await settle()

  await emitCall(env, controller, {
    tool: "browser_scroll",
    callId: "c2",
    args: { ref: "e1:0" },
  })
  assert.equal(results(env)[1].error, "stale_ref")
  assert.equal(env.contentCalls("scroll").length, 0)
})

test("an approval answered after the host gave up never reaches the page", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_snapshot" })
  await emitCall(env, controller, {
    tool: "browser_click",
    callId: "c2",
    args: { ref: "e1:0" },
  })
  const pending = env.lastState().approval
  assert.ok(pending)

  // The host abandons the call at 120 s; our prompt expires just inside that.
  const expiry = env.timers.find((timer) => timer.ms === 110000)
  assert.ok(expiry, "the prompt is on a timer")
  expiry.fn()
  await settle()

  assert.equal(env.lastState().approval, null, "the prompt is taken down")
  assert.equal(results(env)[1].error, "detached")

  // Approving afterwards is a no-op: the prompt is gone and so is the call.
  await controller.onPanelMessage({
    kind: "panel.approval",
    id: pending.id,
    decision: "approve",
  })
  await settle()
  assert.equal(env.contentCalls("click").length, 0)
  assert.equal(env.contentCalls("snapshot").length, 1)
})

test("a handoff prompt expires inside the host's longer window", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  await emitCall(env, controller, {
    tool: "browser_request_user_action",
    args: { reason: "login", instruction: "Sign in." },
  })

  const expiry = env.timers.find((timer) => timer.ms === 870000)
  assert.ok(expiry, "870 s, inside the host's 900 s")
  expiry.fn()
  await settle()
  assert.equal(env.lastState().handoff, null)
  assert.equal(results(env)[0].error, "detached")
})

test("a reload at the same URL invalidates refs even with no url in the change", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_snapshot" })
  assert.equal(controller.guard.refsValid, true)

  // Chrome reports `url` only when it changes; a reload is a bare status.
  controller.onTabUpdated(7, { status: "loading" }, env.tabs.get(7))
  await settle()

  assert.equal(controller.guard.refsValid, false)
  assert.equal(controller.guard.active, true)
  assert.equal(env.port.sentOfType("target.navigated").length, 1)
  assert.equal(env.contentCalls("reset").length, 1)

  await emitCall(env, controller, {
    tool: "browser_click",
    callId: "c2",
    args: { ref: "e1:0" },
  })
  assert.equal(results(env)[1].error, "stale_ref")
})

test("one navigation is not reported twice", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  await emitCall(env, controller, { tool: "browser_snapshot" })

  const tab = env.tabs.get(7)
  controller.onTabUpdated(7, { status: "loading" }, tab)
  controller.onTabUpdated(7, { status: "complete" }, tab)
  controller.onTabUpdated(7, { status: "loading" }, tab)
  await settle()

  assert.equal(env.port.sentOfType("target.navigated").length, 1)
})

test("a navigation whose URL Chrome will not show us is treated as leaving", async () => {
  const { env, controller } = setup()
  await startRun(controller, env)
  // Host permission covers one origin, so a URL we cannot see is a URL that
  // is no longer ours.
  controller.onTabUpdated(7, { status: "loading" }, { id: 7 })
  await settle()

  assert.equal(
    env.port.sentOfType("target.revoked")[0].reason,
    "cross_origin_navigation"
  )
  assert.equal(env.port.sentOfType("target.navigated").length, 0)
  assert.equal(controller.guard.active, false)
})
