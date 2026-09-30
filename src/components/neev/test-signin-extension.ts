// Test-only: the Neev Browser extension's sign-in relay, run for real.
//
// Loads the extension's OWN files — `content/signin-bridge.js` (the page
// bridge) and `lib/signin.js` (`SigninRelay`, `validateArm`, `ruleFor`) — and
// fakes only what Chrome would provide: `chrome.runtime`, the session-rule
// store, alarms and `tabs.create`. So the page's messages are checked by the
// extension's real validation, not by a stand-in with its own idea of the
// schema; a unit mismatch or a renamed field fails here.
//
// jsdom's `window.postMessage` sets neither `event.origin` nor
// `event.source`, which both sides check. `installBrowserPostMessage` restores
// the browser's behaviour for same-window messages; nothing else is changed.

import { readFileSync } from "node:fs"
import { resolve } from "node:path"
import { vi } from "vitest"
// Plain ES-module JS from the extension; typed by the cast below.
import * as relay from "../../../tools/neev-browser/extension/lib/signin.js"

const EXTENSION = resolve(__dirname, "../../../tools/neev-browser/extension")

export const signin = relay as unknown as {
  VERSION: string
  SIGNIN_ORIGIN: string
  MAX_RULES: number
  SigninRelay: new (env: unknown) => {
    handle: (message: unknown, context: unknown) => Promise<unknown>
  }
  validateArm: (
    message: unknown,
    context: unknown,
    nowMs: number
  ) => { ok: boolean; error?: string }
  ruleFor: (arm: unknown, id: number) => Rule
  targetFromVerificationUrl: (
    provider: string,
    url: string
  ) => { port: number; path: string; state: string } | null
}

export interface Rule {
  id: number
  action: { redirect: { regexSubstitution: string } }
  condition: {
    regexFilter: string
    resourceTypes: string[]
    requestMethods: string[]
    isUrlFilterCaseSensitive: boolean
  }
}

/** Browser semantics for `postMessage` to this same window. */
export function installBrowserPostMessage(): void {
  vi.spyOn(window, "postMessage").mockImplementation(((
    message: unknown,
    targetOrigin: string
  ) => {
    // The browser drops a message whose target origin is not this window's.
    if (targetOrigin !== "*" && targetOrigin !== window.location.origin) return
    const data = structuredClone(message)
    setTimeout(() =>
      window.dispatchEvent(
        new MessageEvent("message", {
          data,
          origin: window.location.origin,
          source: window,
        })
      )
    )
  }) as typeof window.postMessage)
}

/** A message from somewhere else, as a hostile frame or script would send. */
export function postForeign(
  data: unknown,
  { origin = window.location.origin, source = null as Window | null } = {}
): void {
  window.dispatchEvent(new MessageEvent("message", { data, origin, source }))
}

/**
 * Install the extension: its real relay behind a fake Chrome, and (when
 * `bridge`) its real content script in this window. Returns what the
 * extension did, for assertions.
 */
export function installSigninExtension({
  bridge = true,
  legacy = false,
  now = () => Date.now(),
  openTab = true as boolean | "throws",
}: {
  bridge?: boolean
  legacy?: boolean
  now?: () => number
  openTab?: boolean | "throws"
} = {}) {
  const rules = new Map<number, Rule>()
  let registry: { attemptId: string; ruleId: number; expiresAtMs: number }[] =
    []
  const opened: string[] = []
  const received: unknown[] = []
  const instance = new signin.SigninRelay({
    now,
    loadRegistry: async () => registry.map((entry) => ({ ...entry })),
    saveRegistry: async (entries: typeof registry) => {
      registry = entries.map((entry) => ({ ...entry }))
    },
    updateRules: async ({ add, remove }: { add: Rule[]; remove: number[] }) => {
      for (const id of remove) rules.delete(id)
      for (const rule of add) rules.set(rule.id, rule)
    },
    schedule: async () => {},
    unschedule: async () => {},
    openTab: async (url: string) => {
      if (openTab === "throws") throw new Error("tabs.create failed")
      opened.push(url)
      return openTab
    },
  })
  const origin = window.location.origin
  // What `background.js` does for each channel, with Chrome's `sender` facts.
  const runtime = {
    lastError: undefined as unknown,
    sendMessage(...args: unknown[]) {
      if (typeof args[0] === "string") {
        // Legacy: chrome.runtime.sendMessage(<id>, message, reply) from the page.
        const [, message, reply] = args as [
          string,
          unknown,
          (value: unknown) => void,
        ]
        if (!legacy) {
          // Chrome: no such extension → lastError, no answer.
          runtime.lastError = { message: "Could not establish connection." }
          reply(undefined)
          runtime.lastError = undefined
          return
        }
        received.push(message)
        void instance
          .handle(message, { origin, fromTab: true, bridge: false })
          .then(reply)
        return
      }
      // Bridge: the content script's runtime.sendMessage(envelope, reply).
      const [envelope, reply] = args as [
        { neevSigninBridge?: boolean; message?: unknown },
        (value: unknown) => void,
      ]
      if (!envelope || envelope.neevSigninBridge !== true) return
      received.push(envelope.message)
      void instance
        .handle(envelope.message, { origin, fromTab: true, bridge: true })
        .then(reply)
    },
  }
  vi.stubGlobal("chrome", { runtime })
  // The content script's own `message` listener, kept so `reset` removes it:
  // the jsdom window outlives a test, and a stale bridge would answer too.
  const listeners: EventListenerOrEventListenerObject[] = []
  if (bridge) {
    const source = readFileSync(
      resolve(EXTENSION, "content/signin-bridge.js"),
      "utf8"
    )
    const add = window.addEventListener.bind(window)
    window.addEventListener = ((
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: boolean | AddEventListenerOptions
    ) => {
      if (type === "message") listeners.push(listener)
      add(type, listener, options)
    }) as typeof window.addEventListener
    try {
      // Runs exactly as Chrome would inject it: a classic script in this window.
      new Function(source)()
    } finally {
      window.addEventListener = add
    }
  }
  return {
    rules,
    opened,
    received,
    registry: () => registry,
    /** Uninstall the bridge completely, so the next test starts clean. */
    reset() {
      for (const listener of listeners)
        window.removeEventListener("message", listener)
      delete (window as { __neevSigninBridge?: boolean }).__neevSigninBridge
    },
  }
}

/**
 * What Chrome does with a navigation under a DNR regex rule: match the URL
 * against `regexFilter` and build the redirect from `regexSubstitution`.
 * RE2 and JS agree on everything these rules use (`(?:…)`, classes, anchors).
 */
export function redirectFor(rules: Iterable<Rule>, url: string): string | null {
  for (const rule of rules) {
    const match = new RegExp(rule.condition.regexFilter).exec(url)
    if (!match) continue
    return rule.action.redirect.regexSubstitution.replace(
      /\\(\d)/g,
      (_, group: string) => match[Number(group)] ?? ""
    )
  }
  return null
}
