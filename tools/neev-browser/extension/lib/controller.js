/**
 * Run orchestration: native host on one side, the picked tab on the other,
 * the side panel in the middle.
 *
 * Every Chrome API this needs arrives through `env`, so the whole flow —
 * scope checks, approvals, revocation, stale-call rejection — runs in Node
 * against a fake. `background.js` is the only file that knows `chrome`.
 */

import * as P from "./protocol.js"
import { RunGuard } from "./guard.js"

/** Base64 ceiling for one screenshot, well under the host's 1 MiB frame cap. */
export const MAX_SCREENSHOT_BASE64 = 700000
export const SCREENSHOT_QUALITIES = [50, 30, 15]
export const MAX_TYPE_CHARS = 2000
export const MAX_LOG_ENTRIES = 400
/** How long to wait for the host's `run.ended` before tearing down anyway. */
export const END_GRACE_MS = 5000

class ToolError extends Error {
  constructor(slug) {
    super(slug)
    this.slug = P.normalizeErrorSlug(slug)
  }
}

function emptyState() {
  return {
    status: "idle",
    run: null,
    approval: null,
    handoff: null,
    ended: null,
    error: null,
  }
}

export class Controller {
  constructor(env) {
    this.env = env
    this.state = emptyState()
    this.log = []
    this.guard = null
    this.port = null
    this.approvals = new Map()
    this.handoffs = new Map()
    this.seq = 0
  }

  // ----------------------------------------------------------------- panel

  /** Everything a freshly opened panel needs to render the current run. */
  snapshotForPanel() {
    return { kind: "sync", state: this.state, log: this.log.slice() }
  }

  async onPanelMessage(message) {
    switch (message && message.kind) {
      case "panel.start":
        return this.start(message)
      case "panel.stop":
        return this.stop()
      case "panel.approval":
        return this.#settleApproval(message.id, message.decision)
      case "panel.handoff":
        return this.#settleHandoff(message.id, message.outcome)
      default:
        return undefined
    }
  }

  onPanelDisconnect() {
    if (this.guard && this.guard.active) this.revoke("panel_closed")
  }

  // ------------------------------------------------------------------ start

  async start(request) {
    if (this.guard) {
      this.#fail("A run is already attached. Stop it first.")
      return
    }
    const { provider, task, model, tabId } = request
    if (!P.PROVIDERS.includes(provider)) return this.#fail("Pick a provider.")
    const trimmed = typeof task === "string" ? task.trim() : ""
    if (!trimmed) return this.#fail("Type a task first.")
    if (trimmed.length > P.MAX_TASK_CHARS) {
      return this.#fail("That task is longer than " + P.MAX_TASK_CHARS + ".")
    }
    if (!Number.isInteger(tabId)) return this.#fail("Pick a tab to attach to.")

    const tab = await this.env.getTab(tabId)
    const origin = tab ? P.originOf(tab.url) : null
    if (!origin) {
      return this.#fail("That tab is gone, or is not an http(s) page.")
    }
    const pattern = P.originPattern(origin)
    if (!(await this.env.hasOrigin(pattern))) {
      return this.#fail("Access to " + origin + " was not granted.")
    }

    const guard = new RunGuard({
      runId: P.newRunId(this.env.random),
      tabId,
      origin,
      provider,
      token: this.env.newToken(),
    })

    try {
      await this.env.inject(tabId)
      await this.env.sendToTab(tabId, {
        neev: 1,
        kind: "attach",
        token: guard.token,
      })
    } catch (error) {
      return this.#fail("Could not attach to that tab: " + describe(error))
    }

    let port
    try {
      port = this.env.connectNative(P.HOST_NAME)
    } catch (error) {
      await this.env.sendToTab(tabId, { neev: 1, kind: "detach" }).catch(noop)
      return this.#fail(
        "Could not reach the Neev companion: " + describe(error)
      )
    }

    this.guard = guard
    this.port = port
    this.log = []
    this.state = emptyState()
    this.state.status = "starting"
    this.state.run = {
      runId: guard.runId,
      provider,
      model: model || null,
      tabId,
      origin,
      url: tab.url,
      title: typeof tab.title === "string" ? tab.title.slice(0, 200) : "",
      runDir: null,
      cli: null,
      startedAt: this.env.now(),
    }

    port.onMessage.addListener((raw) => this.onHostMessage(raw))
    port.onDisconnect.addListener(() => this.#onPortDisconnect())

    this.#log({ type: "status", text: "Attached to " + origin })
    this.#pushState()
    this.#send(
      P.runStart({
        runId: guard.runId,
        provider,
        task: trimmed,
        model: model || undefined,
        target: { tabId, url: tab.url },
      })
    )
  }

  // ------------------------------------------------------------- host input

  onHostMessage(raw) {
    const message = P.parseHostMessage(raw)
    if (!message) return
    const guard = this.guard
    if (!guard || message.runId !== guard.runId) return
    switch (message.type) {
      case "run.started":
        this.state.status = "running"
        if (this.state.run) {
          this.state.run.runDir = message.runDir
          this.state.run.cli = message.cli
        }
        this.#log({ type: "status", text: "Agent started — " + message.cli })
        this.#pushState()
        return
      case "run.output":
        this.#log({
          type: "output",
          channel: message.channel,
          text: message.text,
        })
        return
      case "error":
        this.#log({
          type: "error",
          text: message.code + ": " + message.message,
        })
        this.state.error = message.message || message.code
        if (message.code === "bad_message") {
          this.#pushState()
          return
        }
        this.#finish("revoked", message.code)
        return
      case "run.ended":
        this.#finish(message.state, message.reason)
        return
      case "tool.call":
        this.#handleToolCall(message)
        return
      default:
        return
    }
  }

  async #handleToolCall(message) {
    const guard = this.guard
    const { callId, runId, tool, args, tabId } = message
    if (!guard || !guard.active || runId !== guard.runId) {
      this.#replyFailure(runId, callId, "detached")
      return
    }
    // The tab is checked before it is looked up: a call naming someone else's
    // tab is that call's problem, and must not end this run.
    if (tabId !== guard.tabId) {
      this.#replyFailure(runId, callId, "wrong_tab")
      return
    }
    const tab = await this.env.getTab(tabId)
    if (!tab) {
      this.#replyFailure(runId, callId, "detached")
      this.revoke("tab_closed")
      return
    }
    const refusal = guard.checkCall({ runId, tabId, tool, args, url: tab.url })
    if (refusal) {
      this.#replyFailure(runId, callId, refusal)
      if (refusal === "wrong_origin") this.revoke("cross_origin_navigation")
      return
    }

    let result
    try {
      result = await this.#dispatch(tool, args, tab)
    } catch (error) {
      const slug = error instanceof ToolError ? error.slug : "action_failed"
      this.#replyFailure(runId, callId, slug)
      this.#log({ type: "notice", text: tool + " failed: " + slug })
      return
    }
    // Anything that happened while we were dispatching — Stop, a navigation,
    // the tab closing — makes this result a straggler. Drop it.
    if (!this.guard || this.guard !== guard || !guard.active) return
    const after = await this.env.getTab(tabId)
    this.#send(
      P.toolResult({
        runId,
        callId,
        url: after && after.url ? after.url : tab.url,
        result,
      })
    )
  }

  async #dispatch(tool, args, tab) {
    switch (tool) {
      case "browser_snapshot":
        return { snapshot: await this.#snapshot() }
      case "browser_click":
        return this.#clickOrType("browser_click", args)
      case "browser_type":
        return this.#clickOrType("browser_type", args)
      case "browser_scroll":
        return this.#scroll(args)
      case "browser_screenshot":
        return this.#screenshot(tab)
      case "browser_request_user_action":
        return this.#requestUserAction(args)
      default:
        throw new ToolError("not_actionable")
    }
  }

  async #clickOrType(tool, args) {
    const ref = args.ref
    const text = tool === "browser_type" ? String(args.text ?? "") : null
    if (text !== null && text.length > MAX_TYPE_CHARS) {
      throw new ToolError("not_actionable")
    }
    const described = await this.#toContent({ kind: "describe", ref })
    const node = described.node || {}
    // A password or payment field is a manual handoff, always. The model is
    // told the same thing by the snapshot, where the field has no name and a
    // redacted role.
    if (tool === "browser_type" && node.sensitive) {
      this.#log({
        type: "notice",
        text: "Refused to type into a protected field — that one is yours to fill.",
      })
      throw new ToolError("not_actionable")
    }
    if (node.leavesOrigin) {
      this.#log({
        type: "notice",
        text: "Refused a link that leaves this site — ask the person instead.",
      })
      throw new ToolError("not_actionable")
    }

    const decision = await this.#askApproval(tool, node, text)
    if (decision === "cleared") throw new ToolError("detached")
    if (decision !== "approved") {
      this.#log({
        type: "action",
        tool,
        name: node.name || "",
        approval: "denied",
      })
      return { ok: false, approval: "denied", snapshot: await this.#snapshot() }
    }

    await this.#toContent(
      tool === "browser_click"
        ? { kind: "click", ref }
        : { kind: "type", ref, text }
    )
    this.#log({
      type: "action",
      tool,
      name: node.name || "",
      approval: "approved",
    })
    return { ok: true, approval: "approved", snapshot: await this.#snapshot() }
  }

  async #scroll(args) {
    const ref = typeof args.ref === "string" ? args.ref : null
    // The host only epoch-checks click and type, so scrolling to an element
    // is checked here or nowhere.
    if (
      ref &&
      (!this.guard.refsValid || P.refEpoch(ref) !== this.guard.epoch)
    ) {
      throw new ToolError("stale_ref")
    }
    const amount = Math.min(10, Math.max(1, Number(args.amount) || 1))
    const direction = args.direction === "up" ? "up" : "down"
    await this.#toContent({ kind: "scroll", ref, direction, amount })
    return { ok: true, snapshot: await this.#snapshot() }
  }

  async #screenshot(tab) {
    if (!tab.active) throw new ToolError("screenshot_unavailable")
    for (const quality of SCREENSHOT_QUALITIES) {
      let dataUrl
      try {
        dataUrl = await this.env.captureTab(tab.windowId, quality)
      } catch {
        throw new ToolError("screenshot_unavailable")
      }
      const comma = typeof dataUrl === "string" ? dataUrl.indexOf(",") : -1
      if (comma < 0) throw new ToolError("screenshot_unavailable")
      const dataBase64 = dataUrl.slice(comma + 1)
      if (dataBase64.length <= MAX_SCREENSHOT_BASE64) {
        return { format: "jpeg", dataBase64 }
      }
    }
    throw new ToolError("screenshot_unavailable")
  }

  async #requestUserAction(args) {
    const id = ++this.seq
    const request = {
      id,
      reason: String(args.reason ?? "").slice(0, 400),
      instruction: String(args.instruction ?? "").slice(0, 1000),
    }
    this.state.handoff = request
    this.#log({ type: "handoff", text: request.instruction })
    this.#pushState()
    const outcome = await new Promise((resolve) => {
      this.handoffs.set(id, resolve)
    })
    this.state.handoff = null
    this.#pushState()
    if (outcome === "cleared") throw new ToolError("detached")
    return { outcome, snapshot: await this.#snapshot() }
  }

  async #snapshot() {
    const epoch = this.guard.nextEpoch()
    const reply = await this.#toContent({ kind: "snapshot", epoch })
    return reply.snapshot || { epoch, nodes: [] }
  }

  // -------------------------------------------------------------- approvals

  async #askApproval(tool, node, text) {
    const id = ++this.seq
    const request = {
      id,
      tool,
      name: node.name || "",
      role: node.role || "",
      tag: node.tag || "",
      consequential: !!node.consequential,
      text: text === null ? null : text.slice(0, 200),
      reason: node.consequential
        ? "This submits the form or leaves the page."
        : "Every click and keystroke is confirmed by you.",
    }
    this.state.approval = request
    this.#pushState()
    const decision = await new Promise((resolve) => {
      this.approvals.set(id, resolve)
    })
    this.state.approval = null
    this.#pushState()
    return decision
  }

  #settleApproval(id, decision) {
    const resolve = this.approvals.get(id)
    if (!resolve) return
    this.approvals.delete(id)
    resolve(decision === "approve" ? "approved" : "denied")
  }

  #settleHandoff(id, outcome) {
    const resolve = this.handoffs.get(id)
    if (!resolve) return
    this.handoffs.delete(id)
    resolve(outcome === "done" ? "done" : "skipped")
  }

  #clearPending() {
    for (const resolve of this.approvals.values()) resolve("cleared")
    for (const resolve of this.handoffs.values()) resolve("cleared")
    this.approvals.clear()
    this.handoffs.clear()
    this.state.approval = null
    this.state.handoff = null
  }

  // ------------------------------------------------------------ tab events

  onTabUpdated(tabId, changeInfo) {
    const guard = this.guard
    if (!guard || !guard.active || tabId !== guard.tabId) return
    const url = changeInfo && changeInfo.url
    if (!url) return
    const origin = P.originOf(url)
    // Same-origin counts: a new document means every outstanding ref points at
    // whatever now happens to sit in that position.
    guard.invalidateRefs("navigation")
    this.#toContent({ kind: "reset" }).catch(noop)
    if (origin) this.#send(P.targetNavigated(guard.runId, url))
    if (origin !== guard.origin) {
      this.#log({ type: "notice", text: "The tab left " + guard.origin + "." })
      this.revoke("cross_origin_navigation")
      return
    }
    if (this.state.run) this.state.run.url = url
    this.#log({
      type: "notice",
      text: "Page navigated — element references were reset.",
    })
    this.#pushState()
  }

  onTabRemoved(tabId) {
    if (this.guard && this.guard.tabId === tabId) this.revoke("tab_closed")
  }

  onTabReplaced(addedTabId, removedTabId) {
    if (this.guard && this.guard.tabId === removedTabId) {
      this.revoke("tab_replaced")
    }
  }

  onPermissionsRemoved(permissions) {
    const guard = this.guard
    if (!guard || !guard.active) return
    const origins = (permissions && permissions.origins) || []
    if (origins.some((pattern) => coversOrigin(pattern, guard.origin))) {
      this.revoke("permission_revoked")
    }
  }

  // ------------------------------------------------------------ termination

  stop() {
    const guard = this.guard
    if (!guard) return
    if (guard.active) {
      guard.stop("user")
      this.#send(P.runStop(guard.runId))
    }
    this.state.status = "stopping"
    this.#log({ type: "status", text: "Stopping…" })
    this.#standDown()
    this.#pushState()
    this.#scheduleGiveUp()
  }

  revoke(reason) {
    const guard = this.guard
    if (!guard || !guard.active) return
    guard.revoke(reason)
    this.#send(P.targetRevoked(guard.runId, reason))
    this.state.status = "stopping"
    this.#log({ type: "notice", text: "Access revoked (" + reason + ")." })
    this.#standDown()
    this.#pushState()
    this.#scheduleGiveUp()
  }

  #scheduleGiveUp() {
    const guard = this.guard
    const timer = this.env.setTimeout || globalThis.setTimeout
    timer(() => {
      if (this.guard === guard) this.#finish("stopped", "host_silent")
    }, END_GRACE_MS)
  }

  /** Pointer and badge off, page untouchable — before the host even answers. */
  #standDown() {
    const guard = this.guard
    this.#clearPending()
    if (!guard) return
    // Teardown must not be derailed by a tab that has already gone, so this
    // swallows a synchronous throw as well as a rejection.
    try {
      const sent = this.env.sendToTab(guard.tabId, {
        neev: 1,
        token: guard.token,
        kind: "detach",
      })
      if (sent && typeof sent.catch === "function") sent.catch(noop)
    } catch {
      /* the page is already beyond reach; the overlay went with it */
    }
  }

  #finish(state, reason) {
    if (!this.guard) return
    this.guard.revoke(reason)
    this.#standDown()
    this.guard = null
    this.#disconnect()
    this.state.status = "idle"
    this.state.ended = { state, reason, at: this.env.now() }
    this.#log({
      type: "status",
      text: "Run ended — " + state + " (" + reason + ")",
    })
    this.#pushState()
  }

  #onPortDisconnect() {
    this.port = null
    if (this.guard) this.#finish("revoked", "host_disconnected")
  }

  #disconnect() {
    const port = this.port
    this.port = null
    if (!port) return
    try {
      port.disconnect()
    } catch {
      /* already gone */
    }
  }

  // -------------------------------------------------------------- plumbing

  /**
   * One request to the content script, with a single re-attach retry: the
   * document may have been replaced since the last call, which takes the
   * script with it.
   */
  async #toContent(message) {
    const guard = this.guard
    if (!guard || !guard.active) throw new ToolError("detached")
    const payload = Object.assign({ neev: 1, token: guard.token }, message)
    let reply = await this.env
      .sendToTab(guard.tabId, payload)
      .catch(() => ({ error: "detached" }))
    if (!reply || reply.error === "detached" || reply.error === "wrong_run") {
      try {
        await this.env.inject(guard.tabId)
        await this.env.sendToTab(guard.tabId, {
          neev: 1,
          kind: "attach",
          token: guard.token,
        })
        reply = await this.env.sendToTab(guard.tabId, payload)
      } catch {
        throw new ToolError("detached")
      }
    }
    if (!reply) throw new ToolError("detached")
    if (reply.error) throw new ToolError(reply.error)
    return reply
  }

  #replyFailure(runId, callId, error) {
    if (!this.port) return
    try {
      this.port.postMessage(P.toolFailure({ runId, callId, error }))
    } catch {
      /* the host is gone; onDisconnect will clean up */
    }
  }

  #send(message) {
    if (!this.port) return
    try {
      this.port.postMessage(message)
    } catch {
      /* the host is gone; onDisconnect will clean up */
    }
  }

  #fail(text) {
    this.state.error = text
    this.#log({ type: "error", text })
    this.#pushState()
  }

  #log(entry) {
    const full = Object.assign({ at: this.env.now() }, entry)
    this.log.push(full)
    if (this.log.length > MAX_LOG_ENTRIES) this.log.shift()
    this.env.toPanel({ kind: "log", entry: full })
  }

  #pushState() {
    this.env.toPanel({ kind: "state", state: this.state })
  }
}

function coversOrigin(pattern, origin) {
  if (typeof pattern !== "string") return false
  if (pattern === "<all_urls>") return true
  const base = pattern.replace(/\/\*$/, "")
  if (base === origin) return true
  const wildcard = /^(https?|\*):\/\/\*\.(.+)$/.exec(base)
  if (!wildcard) return base === "*://*" || base === "http://*"
  try {
    return new URL(origin).hostname.endsWith(wildcard[2])
  } catch {
    return false
  }
}

function describe(error) {
  if (!error) return "unknown error"
  return String(error.message || error).slice(0, 200)
}

function noop() {}
