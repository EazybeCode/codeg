/**
 * The extension's own scope guard.
 *
 * The host checks tab, origin and ref epoch before it sends a `tool.call`, and
 * it checks the origin again when the result comes back. This class is the
 * second, independent check: it runs in the process that actually touches the
 * page, against state Chrome told us, not state the host told us. Neither side
 * trusts the other to be the only one looking.
 *
 * A run starts `active` and leaves that state exactly once, for good.
 */

import { ALLOWED_TOOLS, REF_TOOLS, originOf, refEpoch } from "./protocol.js"

export class RunGuard {
  constructor({ runId, tabId, origin, provider, token }) {
    this.runId = runId
    this.tabId = tabId
    this.origin = origin
    this.provider = provider
    /** Private per-run identifier; the content script will not act without it. */
    this.token = token
    this.state = "active"
    this.endReason = null
    /**
     * The snapshot counter lives here, in the worker, because the content
     * script dies with its document and would restart at 1 after every
     * navigation — which is exactly how a ref from the old page gets applied
     * to whatever now sits in that position.
     */
    this.epoch = 0
    this.refsValid = false
    this.refInvalidReason = null
  }

  get active() {
    return this.state === "active"
  }

  /** Mint the epoch for a snapshot that is about to be taken. */
  nextEpoch() {
    this.epoch += 1
    this.refsValid = true
    this.refInvalidReason = null
    return this.epoch
  }

  /** A new document is loaded: every outstanding ref is dead. */
  invalidateRefs(reason) {
    this.refsValid = false
    this.refInvalidReason = reason
  }

  stop(reason) {
    this.#terminate("stopped", reason)
  }

  revoke(reason) {
    this.#terminate("revoked", reason)
  }

  #terminate(state, reason) {
    if (this.state !== "active") return
    this.state = state
    this.endReason = reason
    this.refsValid = false
  }

  /**
   * Returns `null` when the call may proceed, or the contract error slug that
   * must be sent back instead. `url` is the URL Chrome reports for the tab
   * right now — never one taken from the message.
   */
  checkCall({ runId, tabId, tool, args, url }) {
    if (!this.active) return "detached"
    if (runId !== this.runId) return "detached"
    if (!ALLOWED_TOOLS.includes(tool)) return "not_actionable"
    if (tabId !== this.tabId) return "wrong_tab"
    const origin = originOf(url)
    if (!origin || origin !== this.origin) return "wrong_origin"
    if (REF_TOOLS.includes(tool)) {
      const epoch = refEpoch((args || {}).ref)
      if (epoch === null) return "stale_ref"
      if (!this.refsValid) return "stale_ref"
      if (epoch !== this.epoch) return "stale_ref"
    }
    return null
  }
}
