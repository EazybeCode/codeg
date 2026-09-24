/**
 * A fake `env` for the Controller: a native port that records what it was
 * sent, a tab registry we can mutate mid-test (that is how origin drift and
 * tab closure are simulated), and a stand-in content script.
 *
 * Nothing here schedules real time. `setTimeout` is captured rather than run,
 * so the five-second give-up timer only fires when a test says so.
 */

export function makeFakePort() {
  const port = {
    posted: [],
    disconnected: false,
    _onMessage: [],
    _onDisconnect: [],
    postMessage(message) {
      port.posted.push(message)
    },
    disconnect() {
      port.disconnected = true
    },
    onMessage: {
      addListener(fn) {
        port._onMessage.push(fn)
      },
    },
    onDisconnect: {
      addListener(fn) {
        port._onDisconnect.push(fn)
      },
    },
    /** Pretend the host sent something. */
    emit(message) {
      for (const fn of port._onMessage.slice()) fn(message)
    },
    drop() {
      port.disconnected = true
      for (const fn of port._onDisconnect.slice()) fn()
    },
    sentOfType(type) {
      return port.posted.filter((message) => message.type === type)
    },
  }
  return port
}

/** A content script that answers the way a healthy page would. */
export function makeFakeContent(overrides = {}) {
  const content = {
    calls: [],
    attached: false,
    token: null,
    nodes: overrides.nodes || [
      { role: "button", tag: "button", name: "Save", consequential: true },
      { role: "textbox", tag: "input", name: "Customer", filled: false },
    ],
    describe:
      overrides.describe ||
      (() => ({
        ok: true,
        node: {
          tag: "button",
          role: "button",
          name: "Save",
          consequential: true,
          sensitive: false,
          leavesOrigin: false,
          enabled: true,
        },
      })),
    fail: overrides.fail || null,
    async handle(message) {
      content.calls.push(message)
      if (content.fail) {
        const failure = content.fail(message)
        if (failure) return failure
      }
      switch (message.kind) {
        case "attach":
          content.attached = true
          content.token = message.token
          return { ok: true }
        case "detach":
          content.attached = false
          return { ok: true }
        case "reset":
          return { ok: true }
        case "snapshot":
          return {
            ok: true,
            snapshot: {
              epoch: message.epoch,
              url: "https://app.test/orders",
              title: "Orders",
              truncated: false,
              nodes: content.nodes.map((node, index) =>
                Object.assign({ ref: "e" + message.epoch + ":" + index }, node)
              ),
            },
          }
        case "describe":
          return content.describe(message)
        case "click":
        case "type":
        case "scroll":
          return { ok: true }
        default:
          return { error: "not_actionable" }
      }
    },
  }
  return content
}

export function makeEnv(options = {}) {
  const port = makeFakePort()
  const content = options.content || makeFakeContent()
  const tabs = new Map()
  for (const tab of options.tabs || [
    {
      id: 7,
      url: "https://app.test/orders",
      title: "Orders",
      windowId: 1,
      active: true,
    },
  ]) {
    tabs.set(tab.id, tab)
  }
  let clock = 1700000000000
  let ids = 0
  let tokens = 0

  const env = {
    port,
    content,
    tabs,
    panel: [],
    injected: [],
    timers: [],
    capture: options.capture || (() => "data:image/jpeg;base64,AAAA"),
    originGranted: options.originGranted !== false,
    random: {
      randomUUID() {
        ids += 1
        return "run00000-0000-4000-8000-" + String(ids).padStart(12, "0")
      },
    },
    now() {
      clock += 1000
      return clock
    },
    newToken() {
      tokens += 1
      return "secret-token-" + tokens
    },
    connectNative() {
      if (options.connectThrows) throw new Error("host not found")
      return port
    },
    async getTab(tabId) {
      return tabs.get(tabId) || null
    },
    async inject(tabId) {
      if (options.injectThrows) throw new Error("cannot inject")
      env.injected.push(tabId)
    },
    async sendToTab(tabId, message) {
      if (!tabs.has(tabId)) throw new Error("no receiving end")
      return content.handle(message)
    },
    async captureTab(windowId, quality) {
      return env.capture(windowId, quality)
    },
    async hasOrigin() {
      return env.originGranted
    },
    toPanel(message) {
      env.panel.push(message)
    },
    setTimeout(fn, ms) {
      env.timers.push({ fn, ms })
      return env.timers.length
    },
    /** Run every timer captured so far. */
    flushTimers() {
      const due = env.timers.splice(0, env.timers.length)
      for (const timer of due) timer.fn()
    },
    panelOfKind(kind) {
      return env.panel.filter((message) => message.kind === kind)
    },
    lastState() {
      const states = env.panelOfKind("state")
      return states.length ? states[states.length - 1].state : null
    },
    contentCalls(kind) {
      return content.calls.filter((call) => call.kind === kind)
    },
  }
  return env
}

/** Drive a controller through a successful attach + `run.started`. */
export async function startRun(controller, env, overrides = {}) {
  await controller.start(
    Object.assign(
      { provider: "claude", task: "check the orders", tabId: 7, model: null },
      overrides
    )
  )
  const start = env.port.posted.find((message) => message.type === "run.start")
  if (start) {
    env.port.emit({
      type: "run.started",
      runId: start.runId,
      runDir: "/tmp/runs/" + start.runId,
      cli: "2.1.247 (Claude Code)",
    })
  }
  return start
}

/** Deliver one `tool.call` and wait for the controller to finish with it. */
export async function callTool(controller, env, call) {
  const runId = controller.guard ? controller.guard.runId : "missing"
  env.port.emit(
    Object.assign(
      { type: "tool.call", runId, callId: "c1", tabId: 7, args: {} },
      call
    )
  )
  await settle()
  return env.port.posted.filter(
    (message) =>
      message.type === "tool.result" && message.callId === (call.callId || "c1")
  )[0]
}

/** Let every already-queued microtask and promise continuation run. */
export async function settle(rounds = 12) {
  for (let i = 0; i < rounds; i += 1) await Promise.resolve()
  await new Promise((resolve) => setImmediate(resolve))
  for (let i = 0; i < rounds; i += 1) await Promise.resolve()
}
