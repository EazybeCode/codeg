/**
 * Service worker: the only file that touches `chrome`.
 *
 * It wires real Chrome APIs onto the `env` the Controller expects and forwards
 * events. All of the judgement — scope checks, approvals, revocation — lives
 * in `lib/controller.js`, where the tests can reach it.
 */

import { Controller } from "./lib/controller.js"
import { makePanelSlot } from "./lib/connection.js"
import { ALARM_PREFIX, SIGNIN_ORIGIN, SigninRelay } from "./lib/signin.js"

const panel = makePanelSlot()

const env = {
  random: globalThis.crypto,

  now() {
    return Date.now()
  },

  /** A private per-run identifier. The page never sees it as page data. */
  newToken() {
    const bytes = new Uint8Array(16)
    globalThis.crypto.getRandomValues(bytes)
    return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join(
      ""
    )
  },

  connectNative(hostName) {
    return chrome.runtime.connectNative(hostName)
  },

  async getTab(tabId) {
    try {
      return await chrome.tabs.get(tabId)
    } catch {
      return null
    }
  },

  async inject(tabId) {
    await chrome.scripting.executeScript({
      target: { tabId },
      files: ["content/agent.js"],
    })
  },

  async sendToTab(tabId, message) {
    return chrome.tabs.sendMessage(tabId, message)
  },

  async captureTab(windowId, quality) {
    return chrome.tabs.captureVisibleTab(windowId, {
      format: "jpeg",
      quality,
    })
  },

  async hasOrigin(pattern) {
    return chrome.permissions.contains({ origins: [pattern] })
  },

  toPanel(message) {
    const port = panel.port
    if (!port) return
    try {
      port.postMessage(message)
    } catch {
      panel.release(port)
    }
  },
}

const controller = new Controller(env)

chrome.runtime.onConnect.addListener((port) => {
  if (port.name !== "neev-panel") return
  panel.adopt(port)
  // Posted before any listener is registered, on purpose: this snapshot is
  // also the panel's proof that this worker really started, which is what
  // tells it a reconnect succeeded.
  try {
    port.postMessage(controller.snapshotForPanel())
  } catch {
    /* the panel closed between connect and this line */
  }
  port.onMessage.addListener((message) => {
    // A superseded port must not drive the run. Chrome leaves its listeners
    // attached, so without this a stale panel connection could still start,
    // stop or approve — the same guard the disconnect below needs.
    if (!panel.holds(port)) return
    Promise.resolve(controller.onPanelMessage(message)).catch(() => {})
  })
  port.onDisconnect.addListener(() => {
    // A panel that outlived a previous worker reconnects, so a superseded
    // port can disconnect after the live one is already adopted. Ending the
    // run on that would kill the run the reconnect just recovered.
    if (!panel.release(port)) return
    // Closing the panel takes the approvals with it, so the run ends too.
    controller.onPanelDisconnect()
  })
})

chrome.tabs.onUpdated.addListener((tabId, changeInfo, tab) => {
  controller.onTabUpdated(tabId, changeInfo, tab)
})

chrome.tabs.onRemoved.addListener((tabId) => {
  controller.onTabRemoved(tabId)
})

chrome.tabs.onReplaced.addListener((addedTabId, removedTabId) => {
  controller.onTabReplaced(addedTabId, removedTabId)
})

chrome.permissions.onRemoved.addListener((permissions) => {
  controller.onPermissionsRemoved(permissions)
})

if (chrome.sidePanel && chrome.sidePanel.setPanelBehavior) {
  chrome.sidePanel
    .setPanelBehavior({ openPanelOnActionClick: true })
    .catch(() => {})
}

// Direct sign-in relay (lib/signin.js, direct-signin-ui-contract-v2.md).
//
// Two ways in, the same checks: the app.neev.so page bridge (content script,
// any extension id) and the legacy `externally_connectable` channel. The
// origin always comes from Chrome's `sender`, never from the message.
const REGISTRY_KEY = "neevSigninRules"
const signin = new SigninRelay({
  now: () => Date.now(),
  async loadRegistry() {
    const stored = await chrome.storage.session.get(REGISTRY_KEY)
    return Array.isArray(stored[REGISTRY_KEY]) ? stored[REGISTRY_KEY] : []
  },
  async saveRegistry(entries) {
    await chrome.storage.session.set({ [REGISTRY_KEY]: entries })
  },
  async updateRules({ add, remove }) {
    await chrome.declarativeNetRequest.updateSessionRules({
      removeRuleIds: remove,
      addRules: add,
    })
  },
  async schedule(attemptId, whenMs) {
    chrome.alarms.create(ALARM_PREFIX + attemptId, { when: whenMs })
  },
  async unschedule(attemptId) {
    await chrome.alarms.clear(ALARM_PREFIX + attemptId)
  },
  async openTab(url) {
    const tab = await chrome.tabs.create({ url, active: true })
    return !!tab
  },
})

function senderOrigin(sender) {
  if (sender && typeof sender.origin === "string") return sender.origin
  try {
    return sender && sender.url ? new URL(sender.url).origin : ""
  } catch {
    return ""
  }
}

function answer(promise, reply) {
  promise.then(reply, () => reply({ ok: false, error: "internal" }))
  return true
}

// Page bridge: only our own content script, only from a tab on SIGNIN_ORIGIN.
chrome.runtime.onMessage.addListener((envelope, sender, reply) => {
  if (!envelope || envelope.neevSigninBridge !== true) return false
  if (sender.id !== chrome.runtime.id || !sender.tab) return false
  const context = {
    origin: senderOrigin(sender),
    fromTab: true,
    bridge: true,
  }
  if (context.origin !== SIGNIN_ORIGIN) return false
  return answer(signin.handle(envelope.message, context), reply)
})

// Legacy: chrome.runtime.sendMessage(<extension id>, …) from app.neev.so.
chrome.runtime.onMessageExternal.addListener((message, sender, reply) => {
  const context = { origin: senderOrigin(sender), fromTab: !!sender.tab, bridge: false }
  return answer(signin.handle(message, context), reply)
})

chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name.startsWith(ALARM_PREFIX)) {
    signin.disarm(alarm.name.slice(ALARM_PREFIX.length)).catch(() => {})
  }
})

// A restarted worker reconciles: expired attempts lose their rules.
signin.prune().catch(() => {})
