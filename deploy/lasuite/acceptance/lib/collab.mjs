import { BASE_URL, MODULE_ROOT, REQUIRED_VERSIONS } from "./config.mjs"
import { readFile } from "node:fs/promises"

/**
 * The collaboration client.
 *
 * Every socket here is opened by the **official** `HocuspocusProvider` at the
 * version the app ships, driving a real `Y.Doc`. Nothing in this file writes a
 * protocol frame by hand.
 *
 * That restraint is the point rather than fastidiousness. A hand-rolled frame
 * proves the server accepts *that frame*, which is a statement about the test,
 * not about the product — and it is exactly how a convergence result gets
 * claimed for a client nobody runs. The one deliberate exception is documented
 * where it happens, in `openWithForeignDocumentName`.
 */

let cached = null

async function versionOf(pkg) {
  const text = await readFile(`${MODULE_ROOT}/${pkg}/package.json`, "utf8")
  return JSON.parse(text).version
}

export async function loadClient() {
  if (cached) return cached
  for (const [pkg, want] of Object.entries(REQUIRED_VERSIONS)) {
    const got = await versionOf(pkg)
    if (got !== want)
      throw new Error(
        `${pkg} is ${got}, expected ${want}. A convergence result from a different ` +
          `protocol version says nothing about the client the app ships.`
      )
  }
  const [{ HocuspocusProvider, HocuspocusProviderWebsocket }, Y, ws] =
    await Promise.all([
      import(`${MODULE_ROOT}/@hocuspocus/provider/dist/hocuspocus-provider.esm.js`),
      import(`${MODULE_ROOT}/yjs/dist/yjs.mjs`),
      import(`${MODULE_ROOT}/ws/wrapper.mjs`),
    ])
  cached = { HocuspocusProvider, HocuspocusProviderWebsocket, Y, WebSocket: ws.default }
  return cached
}

const wsUrl = () => {
  const url = new URL("/document-engine/collaboration/ws/", BASE_URL)
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:"
  return url.toString()
}

/**
 * A WebSocket class that carries one actor's headers.
 *
 * `HocuspocusProvider` has no `headers` option — it constructs the socket as
 * `new WebSocketPolyfill(url)`, one argument, nothing else. Passing `headers`
 * in the provider configuration is silently ignored, so every socket opened
 * that way arrives with no cookie and no Origin and is refused at the upgrade.
 *
 * That cost real time and, worse, produced misleading evidence: the refusals
 * surfaced as close 1006, which reads as "the upgrade hit a route the server
 * does not mount" and was reported as such. The server was answering correctly
 * the whole time. Binding the headers into the class is the only place they can
 * go, because the provider controls the call.
 *
 * A browser has no equivalent problem: it attaches cookies and Origin itself.
 */
function socketFor(actor) {
  const { WebSocket } = cached
  const headers = {
    cookie: actor.cookieHeader,
    origin: new URL(BASE_URL).origin,
  }
  return class extends WebSocket {
    constructor(url) {
      super(url, [], { headers })
      /**
       * An `error` event with no listener is thrown, and on an EventEmitter
       * that takes the whole process down — one refused upgrade during a
       * teardown and the entire run dies with a stack trace instead of a
       * result table. Every outcome here is decided from close codes and
       * authentication callbacks, so the error itself carries nothing this
       * harness needs; what matters is that it cannot be fatal.
       */
      this.on("error", () => {})
    }
  }
}

/**
 * Open a collaboration session as one actor, on one document.
 *
 * Resolves to the outcome rather than throwing on refusal: "permission denied"
 * is a result these cases assert, not an error that should abort them.
 */
export async function connect(actor, documentId, { timeoutMs = 10_000 } = {}) {
  const { HocuspocusProvider, Y } = await loadClient()
  const doc = new Y.Doc()
  let settle
  const outcome = new Promise((resolve) => (settle = resolve))

  /**
   * What happened to the socket AFTER it was authenticated.
   *
   * `connect` resolves once, on the first outcome, which is all most cases
   * need. Revoking access on a live session is the exception: the question
   * there is what becomes of a connection that already succeeded, so the close
   * that matters arrives long after this function has returned.
   */
  const live = { closed: false, code: null, reason: null }

  const provider = new HocuspocusProvider({
    url: `${wsUrl()}?room=${documentId}`,
    name: documentId,
    document: doc,
    // Both headers are required and each has its own refusal — without the
    // cookie the upgrade is 401, without an allowed Origin it is 403. They ride
    // on the socket class because the provider offers nowhere else to put them.
    WebSocketPolyfill: socketFor(actor),
    preserveConnection: false,
    onAuthenticated: () => settle({ ok: true }),
    onAuthenticationFailed: ({ reason }) => settle({ ok: false, reason }),
    // Keep the code and the reason separate. Substituting a placeholder string
    // for an empty reason hides the close code behind it, and the code is the
    // part that says WHY — 1005 in particular means the upgrade reached a route
    // the server does not mount.
    onClose: ({ event }) => {
      live.closed = true
      live.code = event?.code ?? null
      live.reason = event?.reason || null
      settle({ ok: false, reason: live.reason, code: live.code })
    },
  })

  const timer = setTimeout(
    () => settle({ ok: false, reason: "timeout" }),
    timeoutMs
  )
  const result = await outcome
  clearTimeout(timer)

  return {
    ...result,
    doc,
    provider,
    live,
    text: () => doc.getXmlFragment("document-store").toString(),
    destroy: () => {
      try {
        provider.destroy()
      } catch {
        /* already gone */
      }
    },
  }
}

/**
 * Open a socket authorised for one document, then name a different one.
 *
 * This is the one place a frame is not produced by ordinary provider use, and
 * it has to be: the document name travels as a leading varstring in **every**
 * message, not once per socket, so a socket can carry frames for a document it
 * was never authorised for. No amount of ordinary provider use produces that,
 * because the provider always names the document it was constructed with.
 *
 * Upstream already defends this — `hocuspocusServer.onConnect` rejects
 * `documentName !== roomParam` with "Wrong room name: Unauthorized", and the
 * hook runs per document rather than per socket. So this case asserts a defence
 * that is *expected to hold*, and exists to catch a proxy that terminates the
 * socket in a way that loses it.
 *
 * Even here the frame is built by the official provider: a second provider is
 * attached to the same underlying socket with a different name, which is the
 * library's own multiplexing path.
 */
export async function openWithForeignDocumentName(
  actor,
  authorisedId,
  foreignId,
  { timeoutMs = 10_000 } = {}
) {
  const { HocuspocusProvider, HocuspocusProviderWebsocket, Y } =
    await loadClient()

  const socket = new HocuspocusProviderWebsocket({
    url: `${wsUrl()}?room=${authorisedId}`,
    WebSocketPolyfill: socketFor(actor),
  })

  let settle
  const outcome = new Promise((resolve) => (settle = resolve))

  const provider = new HocuspocusProvider({
    websocketProvider: socket,
    // Authorised for `authorisedId`; this names `foreignId` on every frame.
    name: foreignId,
    document: new Y.Doc(),
    onAuthenticated: () => settle({ ok: true }),
    onAuthenticationFailed: ({ reason }) => settle({ ok: false, reason }),
    // Keep the code and the reason separate. Substituting a placeholder string
    // for an empty reason hides the close code behind it, and the code is the
    // part that says WHY — 1005 in particular means the upgrade reached a route
    // the server does not mount.
    onClose: ({ event }) =>
      settle({ ok: false, reason: event?.reason || null, code: event?.code ?? null }),
  })

  const timer = setTimeout(
    () => settle({ ok: false, reason: "timeout" }),
    timeoutMs
  )
  const result = await outcome
  clearTimeout(timer)

  const destroy = () => {
    try {
      provider.destroy()
      socket.destroy()
    } catch {
      /* already gone */
    }
  }
  return { ...result, destroy }
}

/**
 * What a refusal actually means, from its close code.
 *
 * These are distinguishable and they matter: every one of them produces "the
 * socket did not work", and asserting on that alone is how a negative case ends
 * up passing because collaboration is broken rather than because access was
 * denied. Codes confirmed against the canary's own y-provider.
 */
export function explainClose({ code, reason }) {
  const text = String(reason ?? "")
  /**
   * 1005 and 1006 say only that the connection ended with no close frame. They
   * do NOT identify a cause, and an earlier version of this claimed they meant
   * the socket path was wrong — which sent people looking at routing while the
   * real answer was that the upgrade had been rejected outright, 401 for a
   * missing cookie or 403 for a disallowed Origin. A refused HTTP upgrade never
   * becomes a WebSocket, so there is no close frame to carry a reason.
   *
   * Say what is known and name the candidates, rather than picking one.
   */
  if (code === 1005 || code === 1006)
    return `closed with code ${code} and no close frame — the upgrade itself was refused or the route is not mounted; check the HTTP status of the upgrade (401 = no cookie, 403 = Origin, 404 = route)`
  if (/origin/i.test(text)) return "Origin header missing or not in the allowed list"
  if (/no cookies/i.test(text)) return "the session cookie was not forwarded"
  if (/permission/i.test(text)) return "authorisation refused — a real denial"
  if (text) return text
  if (code) return `closed with code ${code}`
  return "closed with no code and no reason"
}

/** Wait until `predicate` holds, or give up. Returns whether it held. */
export async function until(predicate, { timeoutMs = 10_000, everyMs = 100 } = {}) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (await predicate()) return true
    await new Promise((resolve) => setTimeout(resolve, everyMs))
  }
  return false
}
