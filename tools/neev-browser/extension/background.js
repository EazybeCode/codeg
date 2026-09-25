/**
 * Service worker: the only file that touches `chrome`.
 *
 * It wires real Chrome APIs onto the `env` the Controller expects and forwards
 * events. All of the judgement — scope checks, approvals, revocation — lives
 * in `lib/controller.js`, where the tests can reach it.
 */

import { Controller } from "./lib/controller.js"
import { makePanelSlot } from "./lib/connection.js"

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
