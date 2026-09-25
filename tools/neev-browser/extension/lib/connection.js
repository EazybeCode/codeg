/**
 * The panel's link to the background service worker, on both ends.
 *
 * An MV3 service worker is not a process the panel can lean on. Its port can
 * close because Chrome shut an idle worker down, because the worker crashed,
 * or because the extension was reloaded — and **the panel cannot tell those
 * apart**. A closed port says only that the port closed. So this file never
 * names a cause; it treats every drop the same way and lets the worker's own
 * snapshot, after a reconnect, be the only evidence of what survived.
 *
 * Four rules keep the repair honest:
 *
 * - **Nothing is replayed.** A message that was not delivered stays
 *   undelivered. `post` reports the failure and the caller decides what to
 *   say; there is no queue in this file and there must never be one. Replaying
 *   an approval would be acting on the page with an answer the person gave to
 *   a question that no longer exists.
 * - **Recovery is bounded, and never periodic.** An idle panel does not
 *   reconnect at all: it waits for the next thing the person does. Chrome's
 *   lifecycle documentation puts the idle shutdown at 30 seconds and says
 *   opening a port does not reset that timer, so a reconnect on a timer would
 *   buy 30 seconds and then do it again, forever — an extension keeping its
 *   worker alive indefinitely, which the same document tells you not to build.
 * - **A reconnect counts as healthy only once the worker speaks.** The attempt
 *   counter resets on the `sync` the worker posts on connect, not on the
 *   connect itself — `chrome.runtime.connect` hands back a port even when the
 *   worker will not start, so resetting there is how you get a tight loop.
 * - **Only the live port is listened to.** A superseded port can deliver a
 *   late message after the replacement is already adopted, and taking it would
 *   mean stale state and a backoff reset earned by a connection that is gone.
 *   Messages are gated on identity exactly as disconnects are.
 */

import { describe } from "./boot.js"

/** Bounded backoff. Four tries, about eight seconds, then the panel says so. */
export const RECOVERY_DELAYS_MS = [250, 750, 2000, 5000]

export class PanelConnection {
  /**
   * @param connect  opens a port; may throw, may hand back a dead one
   * @param timer    injected `setTimeout` so tests never wait
   * @param handlers onMessage, onDrop, onRestored, onGaveUp — all optional
   */
  constructor({ connect, timer, handlers = {} }) {
    this.connect = connect
    this.timer = timer || globalThis.setTimeout
    this.handlers = handlers
    this.port = null
    this.attempt = 0
    this.pending = false
    this.closed = false
  }

  get connected() {
    return this.port !== null
  }

  /**
   * Open a port now. Returns `null` on success, or a one-line reason.
   *
   * Success here only means Chrome accepted the call. Whether the worker
   * actually started is answered by the first message, not by this.
   */
  open() {
    if (this.closed) return "the panel is closing"
    if (this.port) return null
    let port
    try {
      port = this.connect()
    } catch (error) {
      return describe(error)
    }
    if (!port) return "Chrome returned no port"
    this.port = port
    // Both listeners carry the port they belong to. Chrome does not detach a
    // superseded port's listeners for us, so identity is the only thing that
    // separates the live connection from one we have already given up on.
    port.onMessage.addListener((message) => this.#onMessage(port, message))
    port.onDisconnect.addListener(() => this.#onDisconnect(port))
    return null
  }

  /**
   * Open a port on demand, as a fresh attempt.
   *
   * This is the idle path: the worker was shut down, nothing was lost, and the
   * person has just asked for something. The backoff history is irrelevant to
   * a deliberate action, so it is cleared.
   */
  ensure() {
    if (this.port) return null
    this.attempt = 0
    return this.open()
  }

  /** Send one message. Returns `null` on success, or why it did not go. */
  post(message) {
    if (!this.port) return "not connected"
    try {
      this.port.postMessage(message)
      return null
    } catch (error) {
      // Chrome only throws here when the port is already dead; `onDisconnect`
      // may or may not still be coming, so drop it ourselves either way.
      this.port = null
      const reason = describe(error)
      this.#drop(reason)
      return reason
    }
  }

  /**
   * Try to get back, with backoff, because something was riding on this port.
   *
   * Only worth calling when the panel had a live run: reconnecting is how it
   * learns the run's actual fate. An idle panel calls `ensure` later instead.
   */
  recover() {
    if (this.closed || this.port || this.pending) return
    if (this.attempt >= RECOVERY_DELAYS_MS.length) {
      this.#notify("onGaveUp")
      return
    }
    const delay = RECOVERY_DELAYS_MS[this.attempt]
    this.attempt += 1
    this.pending = true
    this.timer(() => {
      this.pending = false
      if (this.closed || this.port) return
      // A port that opens and then drops comes back through `#onDisconnect`
      // with the counter still advanced, so the backoff keeps widening.
      if (this.open()) this.recover()
    }, delay)
  }

  /** Stop everything. A scheduled retry that fires after this does nothing. */
  close() {
    this.closed = true
    const port = this.port
    this.port = null
    if (!port) return
    try {
      port.disconnect()
    } catch {
      /* already gone */
    }
  }

  #onMessage(port, message) {
    // A late message from a port we have already replaced or closed must not
    // reach the panel: it would overwrite current state with older state, and
    // a `sync` among it would reset the backoff on a dead connection's credit.
    if (this.closed || this.port !== port) return
    // The worker posts its snapshot the moment it accepts the connection, so
    // this is the first proof that it is really running. Nothing else counts.
    if (message && message.kind === "sync") {
      const recovered = this.attempt > 0
      this.attempt = 0
      if (recovered) this.#notify("onRestored")
    }
    this.#notify("onMessage", message)
  }

  #onDisconnect(port) {
    if (this.port !== port) return
    this.port = null
    this.#drop(null)
  }

  #drop(reason) {
    if (this.closed) return
    this.#notify("onDrop", { reason })
  }

  #notify(name, payload) {
    const handler = this.handlers[name]
    if (typeof handler === "function") handler(payload)
  }
}

/**
 * The worker side of the same link: which port is *the* panel.
 *
 * A panel that outlives the worker reconnects, so for a moment two ports
 * exist and the dead one's events can arrive after the live one is already
 * adopted. Ending the run on a late `onDisconnect` would kill the very run
 * the reconnect was opened to recover, and acting on a late `onMessage` would
 * let a connection nobody is listening to any more drive the page. Both are
 * gated on the same thing: is this the current port?
 */
export function makePanelSlot() {
  let current = null
  return {
    get port() {
      return current
    },
    adopt(port) {
      current = port
    },
    /** True while this port is the one the worker should be acting for. */
    holds(port) {
      return current !== null && current === port
    },
    /** True when this disconnect is the live panel's, not a superseded one. */
    release(port) {
      if (current !== port) return false
      current = null
      return true
    },
  }
}
