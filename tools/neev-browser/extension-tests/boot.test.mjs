/**
 * The panel's startup reporter.
 *
 * The case this exists for: `sidepanel.js` is a module, so one throw during
 * its evaluation registers none of the listeners below it, and Chrome says so
 * only in the panel's own devtools. These tests pin the banner that says it in
 * the panel instead — and, just as importantly, pin that a panel which loads
 * slowly but successfully never sees that banner.
 */

import assert from "node:assert/strict"
import test from "node:test"

import { makeWindow } from "./helpers/fake-dom.mjs"

/**
 * A fresh module instance per test — `lib/boot.js` holds the banner in module
 * scope, and a cache hit would leak one test into the next.
 */
let counter = 0
async function freshBoot() {
  counter += 1
  return import("../extension/lib/boot.js?case=" + counter)
}

function panelWindow() {
  const win = makeWindow({})

  // `makeWindow` keeps its listeners in a closure and only fires `{ type }`.
  // The reporter is judged on what it does with the payload, so record them
  // here and hand the handler a real event.
  const listeners = new Map()
  win.addEventListener = (type, handler) => {
    if (!listeners.has(type)) listeners.set(type, [])
    listeners.get(type).push(handler)
  }
  win.listenerCount = (type) => (listeners.get(type) || []).length
  win.emit = (type, payload) => {
    for (const handler of listeners.get(type) || [])
      handler({ type, ...payload })
  }

  const banner = win.document.createElement("p")
  banner.setAttribute("id", "boot")
  banner.hidden = true
  win.document.body.appendChild(banner)
  win.banner = banner
  return win
}

/** A load that stays pending until the test settles it by hand. */
function deferred() {
  let settle = {}
  const promise = new Promise((resolve, reject) => {
    settle = { resolve, reject }
  })
  return { load: () => promise, ...settle }
}

/** Let every already-queued microtask run. No timers, no wall clock. */
const drain = () => Promise.resolve().then().then().then()

test("a panel still loading shows no banner, however long it takes", async () => {
  const boot = await freshBoot()
  const win = panelWindow()
  const pending = deferred()

  const booting = boot.bootPanel({ win, doc: win.document, load: pending.load })
  await drain()

  // This is the regression: an earlier version queued a zero-delay timer and
  // called it proof the panel had died, which it is not — the import is still
  // in flight.
  assert.equal(win.banner.hidden, true, "banner shown while import pending")

  pending.resolve({})
  await booting
  await drain()

  assert.equal(win.banner.hidden, true, "banner shown after a clean load")
  assert.equal(win.banner.textContent, "")
})

test("a panel whose module throws says so in the panel", async () => {
  const boot = await freshBoot()
  const win = panelWindow()
  const pending = deferred()

  const booting = boot.bootPanel({ win, doc: win.document, load: pending.load })
  pending.reject(new TypeError("chrome.runtime is undefined"))
  await booting

  assert.equal(win.banner.hidden, false)
  assert.match(win.banner.textContent, /chrome\.runtime is undefined/)
  assert.match(win.banner.textContent, /reload the extension/i)
})

test("a module that is missing entirely is reported, not swallowed", async () => {
  const boot = await freshBoot()
  const win = panelWindow()

  await boot.bootPanel({
    win,
    doc: win.document,
    load: () =>
      Promise.reject(new Error("Failed to fetch dynamically imported module")),
  })

  assert.equal(win.banner.hidden, false)
  assert.match(win.banner.textContent, /dynamically imported module/)
})

test("the reporter is watching before the panel is loaded", async () => {
  const boot = await freshBoot()
  const win = panelWindow()
  const pending = deferred()

  boot.bootPanel({ win, doc: win.document, load: pending.load })

  assert.equal(win.listenerCount("error"), 1)
  assert.equal(win.listenerCount("unhandledrejection"), 1)
  pending.resolve({})
})

test("an uncaught error names itself in the banner", async () => {
  const boot = await freshBoot()
  const win = panelWindow()
  boot.installStartupReporter({ win, doc: win.document })

  win.emit("error", { error: new TypeError("connect is not a function") })

  assert.equal(win.banner.hidden, false)
  assert.match(win.banner.textContent, /connect is not a function/)
})

test("a rejected promise names itself in the banner", async () => {
  const boot = await freshBoot()
  const win = panelWindow()
  boot.installStartupReporter({ win, doc: win.document })

  win.emit("unhandledrejection", { reason: new Error("connectNative failed") })

  assert.equal(win.banner.hidden, false)
  assert.match(win.banner.textContent, /connectNative failed/)
})

test("the first message shown is the one kept", async () => {
  const boot = await freshBoot()
  const win = panelWindow()
  boot.installStartupReporter({ win, doc: win.document })

  boot.report("first")
  boot.report("second")

  assert.equal(win.banner.textContent, "first")
})

test("a failed import does not overwrite the error that caused it", async () => {
  const boot = await freshBoot()
  const win = panelWindow()

  boot.installStartupReporter({ win, doc: win.document })
  win.emit("error", { error: new Error("Cannot read properties of undefined") })
  await boot.bootPanel({
    win,
    doc: win.document,
    load: () => Promise.reject(new Error("module evaluation failed")),
  })

  assert.match(win.banner.textContent, /Cannot read properties/)
})

test("describe reduces every error shape to one line", async () => {
  const { describe } = await freshBoot()

  assert.equal(describe(new Error("boom")), "boom")
  assert.equal(describe({ error: new Error("nested") }), "nested")
  assert.equal(describe({ reason: "rejected" }), "rejected")
  assert.equal(describe("plain"), "plain")
  assert.equal(describe(null), "unknown error")
  assert.equal(describe(undefined), "unknown error")
})

test("a missing banner element is survivable, not a second failure", async () => {
  const boot = await freshBoot()
  const win = makeWindow({})

  // No `#boot` in the document at all.
  await assert.doesNotReject(
    boot.bootPanel({
      win,
      doc: win.document,
      load: () => Promise.reject(new Error("anything")),
    })
  )
})
