/**
 * The panel's link to the service worker.
 *
 * The case this exists for: Chrome terminates an idle MV3 service worker on
 * purpose, and the panel's port dies with it. The old panel read that as a
 * fault and told the person to reload the extension by hand — for something
 * Chrome does every thirty idle seconds.
 *
 * These tests pin the three properties that make the repair safe rather than
 * merely automatic: nothing is ever replayed, recovery is bounded and never
 * periodic, and a port that opens but whose worker never speaks does not
 * reset the backoff.
 */

import assert from "node:assert/strict"
import test from "node:test"

import {
  PanelConnection,
  RECOVERY_DELAYS_MS,
  makePanelSlot,
} from "../extension/lib/connection.js"

/** A `chrome.runtime.Port` with the three members this file touches. */
function makePort() {
  const port = {
    posted: [],
    disconnected: false,
    _messages: [],
    _disconnects: [],
    throwOnPost: null,
    postMessage(message) {
      if (port.throwOnPost) throw new Error(port.throwOnPost)
      port.posted.push(message)
    },
    disconnect() {
      port.disconnected = true
    },
    onMessage: { addListener: (fn) => port._messages.push(fn) },
    onDisconnect: { addListener: (fn) => port._disconnects.push(fn) },
    emit: (message) => port._messages.slice().forEach((fn) => fn(message)),
    drop() {
      port.disconnected = true
      port._disconnects.slice().forEach((fn) => fn())
    },
  }
  return port
}

/** Timers are captured, never run, so nothing here waits on a clock. */
function makeHarness(options = {}) {
  const events = []
  const ports = []
  const timers = []
  let opens = 0

  const link = new PanelConnection({
    connect() {
      opens += 1
      if (options.failFor && opens <= options.failFor) {
        throw new Error("Could not establish connection")
      }
      const port = makePort()
      ports.push(port)
      return port
    },
    timer(fn, ms) {
      timers.push({ fn, ms })
    },
    handlers: {
      onMessage: (message) => events.push(["message", message]),
      onDrop: (info) => events.push(["drop", info]),
      onRestored: () => events.push(["restored"]),
      onGaveUp: () => events.push(["gaveUp"]),
    },
  })

  return {
    link,
    events,
    ports,
    timers,
    get opens() {
      return opens
    },
    names: () => events.map((event) => event[0]),
    last: () => ports[ports.length - 1],
    /** Fire every timer captured so far, in order. */
    flush() {
      const due = timers.splice(0, timers.length)
      for (const timer of due) timer.fn()
    },
  }
}

/** A connection already up and proven, the way the panel starts its life. */
function connected(options) {
  const harness = makeHarness(options)
  assert.equal(harness.link.open(), null)
  harness.last().emit({ kind: "sync", state: { status: "idle" } })
  return harness
}

test("open reports the reason instead of throwing it", () => {
  const harness = makeHarness({ failFor: 1 })

  const failure = harness.link.open()

  assert.match(failure, /Could not establish connection/)
  assert.equal(harness.link.connected, false)
})

test("messages from the worker reach the panel", () => {
  const harness = connected()

  harness.last().emit({ kind: "log", entry: { text: "hello" } })

  assert.deepEqual(harness.events, [
    ["message", { kind: "sync", state: { status: "idle" } }],
    ["message", { kind: "log", entry: { text: "hello" } }],
  ])
})

test("a dropped port is reported, and nothing reconnects on its own", () => {
  const harness = connected()

  harness.last().drop()

  assert.deepEqual(harness.names(), ["message", "drop"])
  assert.equal(harness.link.connected, false)
  // The idle panel must not wake the worker Chrome just put down — that only
  // restarts the same idle clock, forever.
  assert.equal(harness.timers.length, 0)
  assert.equal(harness.opens, 1)
})

test("post never queues: an undelivered message stays undelivered", () => {
  const harness = connected()
  harness.last().drop()

  const failure = harness.link.post({ kind: "panel.approval", decision: "yes" })

  assert.equal(failure, "not connected")

  // Reconnecting must not resurrect it. An approval is an answer to a
  // question the old worker asked; replaying it would act on the page with
  // consent nobody was asked for again.
  assert.equal(harness.link.ensure(), null)
  assert.deepEqual(harness.last().posted, [])
})

test("a post that throws drops the port once and says why", () => {
  const harness = connected()
  harness.last().throwOnPost = "Attempting to use a disconnected port object"

  const failure = harness.link.post({ kind: "panel.stop" })

  assert.match(failure, /disconnected port/)
  assert.equal(harness.link.connected, false)
  assert.deepEqual(harness.names(), ["message", "drop"])

  // The port's own onDisconnect may still be in flight; it must not double up.
  harness.last().drop()
  assert.deepEqual(harness.names(), ["message", "drop"])
})

test("ensure opens on demand and clears the backoff history", () => {
  const harness = connected()
  harness.last().drop()
  harness.link.recover()
  assert.equal(harness.link.attempt, 1)

  assert.equal(harness.link.ensure(), null)

  assert.equal(harness.link.connected, true)
  assert.equal(harness.link.attempt, 0)
})

test("recover reconnects after the first backoff delay", () => {
  const harness = connected()
  harness.last().drop()

  harness.link.recover()
  assert.equal(harness.timers.length, 1)
  assert.equal(harness.timers[0].ms, RECOVERY_DELAYS_MS[0])
  assert.equal(harness.link.connected, false, "reconnected before the delay")

  harness.flush()

  assert.equal(harness.link.connected, true)
  assert.equal(harness.opens, 2)
})

test("the worker's sync is what counts as recovered, not the connect", () => {
  const harness = connected()
  harness.last().drop()
  harness.link.recover()
  harness.flush()

  // Port open, worker silent: not recovered, and the counter stays advanced.
  assert.equal(harness.names().includes("restored"), false)
  assert.equal(harness.link.attempt, 1)

  harness.last().emit({ kind: "sync", state: { status: "idle" } })

  assert.equal(harness.names().includes("restored"), true)
  assert.equal(harness.link.attempt, 0)
})

test("a worker that will not start backs off instead of looping", () => {
  const harness = connected()
  harness.last().drop()

  // Chrome hands back a port even when the worker cannot register, then drops
  // it immediately. Resetting the counter on connect is how that becomes a
  // tight loop; the widening delays below are the proof it does not.
  const delays = []
  for (let round = 0; round < RECOVERY_DELAYS_MS.length; round += 1) {
    harness.link.recover()
    assert.equal(harness.timers.length, 1, "round " + round)
    delays.push(harness.timers[0].ms)
    harness.flush()
    harness.last().drop()
  }

  assert.deepEqual(delays, RECOVERY_DELAYS_MS)

  // Bounded: one more try schedules nothing and says so.
  harness.link.recover()
  assert.equal(harness.timers.length, 0)
  assert.equal(harness.names().at(-1), "gaveUp")
})

test("recover ignores a second call while one is already pending", () => {
  const harness = connected()
  harness.last().drop()

  harness.link.recover()
  harness.link.recover()
  harness.link.recover()

  assert.equal(harness.timers.length, 1)
})

test("close stops a retry that was already scheduled", () => {
  const harness = connected()
  harness.last().drop()
  harness.link.recover()

  harness.link.close()
  harness.flush()

  assert.equal(harness.opens, 1)
  assert.equal(harness.link.connected, false)
})

test("close disconnects the live port and silences the drop", () => {
  const harness = connected()

  harness.link.close()

  assert.equal(harness.last().disconnected, true)
  harness.last().drop()
  assert.deepEqual(harness.names(), ["message"])
})

// ------------------------------------------------------------ worker side

test("the panel slot ignores a superseded port's disconnect", () => {
  const slot = makePanelSlot()
  const first = makePort()
  const second = makePort()

  slot.adopt(first)
  // The panel outlived the previous worker and reconnected; the dead port's
  // onDisconnect arrives afterwards. Acting on it would end the run the
  // reconnect was opened to recover.
  slot.adopt(second)

  assert.equal(slot.release(first), false)
  assert.equal(slot.port, second)
})

test("the panel slot reports the live port's disconnect exactly once", () => {
  const slot = makePanelSlot()
  const port = makePort()
  slot.adopt(port)

  assert.equal(slot.release(port), true)
  assert.equal(slot.port, null)
  assert.equal(slot.release(port), false)
})
