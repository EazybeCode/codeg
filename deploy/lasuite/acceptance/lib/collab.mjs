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
 * Open a collaboration session as one actor, on one document.
 *
 * Resolves to the outcome rather than throwing on refusal: "permission denied"
 * is a result these cases assert, not an error that should abort them.
 */
export async function connect(actor, documentId, { timeoutMs = 10_000 } = {}) {
  const { HocuspocusProvider, Y, WebSocket } = await loadClient()
  const doc = new Y.Doc()
  let settle
  const outcome = new Promise((resolve) => (settle = resolve))

  const provider = new HocuspocusProvider({
    url: `${wsUrl()}?room=${documentId}`,
    name: documentId,
    document: doc,
    WebSocketPolyfill: WebSocket,
    // The socket authenticates by cookie; without it the server closes with
    // `4001 No cookies`, which is a different failure from being refused.
    headers: { cookie: actor.cookieHeader },
    preserveConnection: false,
    onAuthenticated: () => settle({ ok: true }),
    onAuthenticationFailed: ({ reason }) => settle({ ok: false, reason }),
    onClose: ({ event }) =>
      settle({ ok: false, reason: event?.reason || "closed", code: event?.code }),
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
  const { HocuspocusProvider, HocuspocusProviderWebsocket, Y, WebSocket } =
    await loadClient()

  const socket = new HocuspocusProviderWebsocket({
    url: `${wsUrl()}?room=${authorisedId}`,
    WebSocketPolyfill: WebSocket,
    headers: { cookie: actor.cookieHeader },
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
    onClose: ({ event }) =>
      settle({ ok: false, reason: event?.reason || "closed", code: event?.code }),
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

/** Wait until `predicate` holds, or give up. Returns whether it held. */
export async function until(predicate, { timeoutMs = 10_000, everyMs = 100 } = {}) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (await predicate()) return true
    await new Promise((resolve) => setTimeout(resolve, everyMs))
  }
  return false
}
