/**
 * Direct sign-in page bridge (direct-signin-ui-contract-v2.md §1).
 *
 * Injected ONLY into https://app.neev.so/* (manifest content_scripts). Lets the
 * Neev page reach the extension with window.postMessage, so the page needs no
 * extension id: an unpacked install on any path, the existing id, or a store
 * build all answer the same way.
 *
 * It forwards only well-formed `neev-signin` requests from this very window on
 * this exact origin, and posts the answer back to that origin only. It carries
 * no secret: the messages hold an attempt id and the provider's public
 * authorize URL, and the extension re-checks everything (origin from Chrome's
 * `sender`, never from the message).
 */
;(function installNeevSigninBridge(win, runtime) {
  "use strict"
  var ORIGIN = "https://app.neev.so"
  var CHANNEL = "neev-signin"
  var ID = /^[A-Za-z0-9_-]{1,64}$/
  var MAX_BYTES = 8192

  if (!win || !runtime || win.__neevSigninBridge) return
  if (win.location && win.location.origin !== ORIGIN) return
  win.__neevSigninBridge = true

  function post(payload) {
    win.postMessage(payload, ORIGIN)
  }

  win.addEventListener("message", function (event) {
    if (event.source !== win || event.origin !== ORIGIN) return
    var data = event.data
    if (!data || typeof data !== "object") return
    if (data.channel !== CHANNEL || data.direction !== "page") return
    if (typeof data.id !== "string" || !ID.test(data.id)) return
    var message = data.message
    var size
    try {
      size = JSON.stringify(message).length
    } catch (error) {
      return
    }
    if (!message || typeof message !== "object" || size > MAX_BYTES) return
    var id = data.id
    try {
      runtime.sendMessage({ neevSigninBridge: true, message: message }, function (response) {
        // Reading lastError is what keeps Chrome from logging an unchecked one.
        var failed = !!runtime.lastError
        post({
          channel: CHANNEL,
          direction: "extension",
          id: id,
          response: failed ? { ok: false, error: "internal" } : response || null,
        })
      })
    } catch (error) {
      post({ channel: CHANNEL, direction: "extension", id: id, response: { ok: false, error: "internal" } })
    }
  })

  post({ channel: CHANNEL, direction: "extension", type: "ready", version: "0.2.1" })
})(
  typeof window !== "undefined" ? window : undefined,
  typeof chrome !== "undefined" && chrome.runtime ? chrome.runtime : undefined
)
