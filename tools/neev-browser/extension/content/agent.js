/**
 * The page half of Neev Browser: bounded snapshots, real DOM actions, and a
 * pointer that moves only when something is actually about to be dispatched.
 *
 * Injected as a classic script (`chrome.scripting.executeScript` with
 * `files`), so it has no imports and no exports. Everything lives inside
 * `createAgent(win)`, which takes its window as an argument — that is what
 * lets the Node tests drive it against a fake document instead of a browser.
 *
 * Two rules this file exists to keep:
 *   1. No field value ever leaves the page. Text inputs report `filled`, a
 *      boolean, and nothing else. Password, hidden and payment-shaped fields
 *      report a role and no name at all.
 *   2. A ref is only good for the snapshot that minted it. The map is rebuilt
 *      from scratch every time, so a ref from the previous reading is simply
 *      absent — `stale_ref`, not a click on whatever moved into its place.
 */
;(function () {
  "use strict"

  var MAX_NODES = 200
  var MAX_STRING = 120
  var MAX_TEXT_NODES = 80
  var MAX_SNAPSHOT_BYTES = 49152
  var POINTER_SETTLE_MS = 180

  var SKIP_TAGS = [
    "SCRIPT",
    "STYLE",
    "NOSCRIPT",
    "TEMPLATE",
    "HEAD",
    "META",
    "LINK",
    "TITLE",
    "IFRAME",
    "FRAME",
    "OBJECT",
    "EMBED",
    "CANVAS",
    "SVG",
    "AUDIO",
    "VIDEO",
  ]
  var TEXT_TAGS = [
    "P",
    "LI",
    "TD",
    "TH",
    "DT",
    "DD",
    "CAPTION",
    "LEGEND",
    "FIGCAPTION",
    "SUMMARY",
    "BLOCKQUOTE",
  ]
  var HEADING_TAGS = ["H1", "H2", "H3", "H4", "H5", "H6"]
  var TYPEABLE_INPUT_TYPES = [
    "text",
    "search",
    "url",
    "tel",
    "email",
    "number",
    "date",
    "time",
    "datetime-local",
    "month",
    "week",
    "color",
    "range",
  ]

  /**
   * Names that suggest a credential or a payment instrument. False positives
   * cost the model one `browser_request_user_action`; false negatives cost the
   * user their card number, so this leans wide.
   */
  var SENSITIVE_NAME_RE =
    /pass(word|wd)?|secret|token|auth|otp|one[-_ ]?time|cvv|cvc|csc|card[-_ ]?(number|num)|cardnumber|credit|iban|sort[-_ ]?code|routing|account[-_ ]?number|ssn|social[-_ ]?security|security[-_ ]?code|\bpin\b/i
  var SENSITIVE_AUTOCOMPLETE_RE =
    /^(cc-|current-password|new-password|one-time-code)/i

  var OVERLAY_CSS = [
    ":host{all:initial}",
    ".layer{position:fixed;inset:0;pointer-events:none;z-index:2147483647}",
    ".pointer{position:fixed;left:0;top:0;width:20px;height:20px;",
    "margin:-10px 0 0 -10px;border-radius:50%;pointer-events:none;opacity:0;",
    "transform:translate(-9999px,-9999px);",
    "background:radial-gradient(circle at 34% 32%,#fff 0%,#a996ff 38%,#6d4aff 72%,#4322b8 100%);",
    "box-shadow:0 0 0 2px rgba(255,255,255,.92),0 8px 22px rgba(67,34,184,.45);",
    "transition:transform 180ms cubic-bezier(.2,.7,.3,1),opacity 120ms linear}",
    ".pointer.reduced{transition:none}",
    ".ring{position:absolute;inset:-7px;border-radius:50%;",
    "border:2px solid rgba(109,74,255,.85);opacity:0;pointer-events:none}",
    ".pointer.pulse .ring{animation:neev-pulse 420ms ease-out 1}",
    "@keyframes neev-pulse{from{opacity:.9;transform:scale(.55)}",
    "to{opacity:0;transform:scale(1.9)}}",
    ".badge{position:fixed;right:14px;bottom:14px;pointer-events:none;",
    "display:flex;align-items:center;gap:8px;padding:7px 13px;border-radius:999px;",
    "font:500 12px/1.35 ui-sans-serif,-apple-system,system-ui,'Segoe UI',sans-serif;",
    "color:#f4f2ff;background:rgba(21,18,36,.93);",
    "border:1px solid rgba(124,92,255,.55);",
    "box-shadow:0 8px 24px rgba(0,0,0,.28)}",
    ".dot{width:7px;height:7px;border-radius:50%;background:#7c5cff;",
    "box-shadow:0 0 0 3px rgba(124,92,255,.25)}",
  ].join("")

  function includes(list, value) {
    return list.indexOf(value) !== -1
  }

  function upper(value) {
    return String(value || "").toUpperCase()
  }

  function lower(value) {
    return String(value || "").toLowerCase()
  }

  function clean(value) {
    return String(value == null ? "" : value)
      .replace(/\s+/g, " ")
      .trim()
  }

  function clip(value) {
    var text = clean(value)
    return text.length > MAX_STRING ? text.slice(0, MAX_STRING) : text
  }

  function attr(el, name) {
    return el && el.getAttribute ? el.getAttribute(name) : null
  }

  function hasAttr(el, name) {
    return !!(el && el.hasAttribute && el.hasAttribute(name))
  }

  function createAgent(win) {
    var doc = win.document
    var token = null
    var epoch = 0
    var refs = new Map()
    var overlay = null
    var reduced = false
    try {
      reduced = !!(
        win.matchMedia &&
        win.matchMedia("(prefers-reduced-motion: reduce)").matches
      )
    } catch {
      reduced = false
    }

    function onPageHide() {
      // The document is going away; take the overlay with it rather than
      // leaving a pointer painted over a page nobody authorised.
      teardownOverlay()
      refs = new Map()
    }
    if (win.addEventListener) win.addEventListener("pagehide", onPageHide)

    // ------------------------------------------------------------- overlay

    function ensureOverlay() {
      if (overlay && overlay.host && overlay.host.isConnected) return overlay
      var host = doc.createElement("div")
      host.setAttribute("data-neev-overlay", "")
      host.style.position = "fixed"
      host.style.left = "0"
      host.style.top = "0"
      host.style.width = "0"
      host.style.height = "0"
      // A zero-sized, click-through host: the overlay can never take a click
      // that belonged to the page.
      host.style.pointerEvents = "none"
      host.style.zIndex = "2147483647"

      var root = host.attachShadow
        ? host.attachShadow({ mode: "closed" })
        : host
      var style = doc.createElement("style")
      style.textContent = OVERLAY_CSS
      root.appendChild(style)

      var layer = doc.createElement("div")
      layer.className = "layer"
      var pointer = doc.createElement("div")
      pointer.className = reduced ? "pointer reduced" : "pointer"
      var ring = doc.createElement("div")
      ring.className = "ring"
      pointer.appendChild(ring)
      var badge = doc.createElement("div")
      badge.className = "badge"
      var dot = doc.createElement("div")
      dot.className = "dot"
      var label = doc.createElement("span")
      label.textContent = "Neev is controlling this tab"
      badge.appendChild(dot)
      badge.appendChild(label)
      layer.appendChild(pointer)
      layer.appendChild(badge)
      root.appendChild(layer)
      ;(doc.body || doc.documentElement).appendChild(host)

      overlay = { host: host, pointer: pointer, badge: badge, root: root }
      return overlay
    }

    function teardownOverlay() {
      if (!overlay) return
      try {
        if (overlay.host.remove) overlay.host.remove()
        else if (overlay.host.parentNode)
          overlay.host.parentNode.removeChild(overlay.host)
      } catch {
        /* the document may already be torn down */
      }
      overlay = null
    }

    function frame() {
      return new Promise(function (resolve) {
        if (win.requestAnimationFrame)
          win.requestAnimationFrame(function () {
            resolve()
          })
        else win.setTimeout(resolve, 16)
      })
    }

    function delay(ms) {
      return new Promise(function (resolve) {
        win.setTimeout(resolve, ms)
      })
    }

    /**
     * Put the pointer on the element and wait for it to land.
     *
     * Called from the click/type/scroll-to-element path only — never on a
     * timer, never for a snapshot, never for a refused or denied call.
     * Coordinates come from `getBoundingClientRect()` and the pointer is
     * `position: fixed`, so both are viewport CSS pixels and stay aligned
     * through scrolling and browser zoom without any arithmetic.
     */
    function movePointer(el) {
      var ui = ensureOverlay()
      if (el.scrollIntoView) {
        try {
          el.scrollIntoView({ block: "center", inline: "nearest" })
        } catch {
          el.scrollIntoView()
        }
      }
      var rect = el.getBoundingClientRect()
      var point = {
        x: rect.left + rect.width / 2,
        y: rect.top + rect.height / 2,
      }
      ui.pointer.style.transform =
        "translate(" +
        Math.round(point.x) +
        "px, " +
        Math.round(point.y) +
        "px)"
      ui.pointer.style.opacity = "1"
      if (!reduced && ui.pointer.classList) {
        ui.pointer.classList.remove("pulse")
        ui.pointer.classList.add("pulse")
      }
      return frame()
        .then(function () {
          return reduced ? null : delay(POINTER_SETTLE_MS)
        })
        .then(function () {
          return point
        })
    }

    function hidePointer() {
      if (!overlay) return
      overlay.pointer.style.opacity = "0"
      overlay.pointer.style.transform = "translate(-9999px,-9999px)"
      if (overlay.pointer.classList) overlay.pointer.classList.remove("pulse")
    }

    // ------------------------------------------------------------ snapshot

    function visible(el) {
      if (!el.getBoundingClientRect) return true
      var rect = el.getBoundingClientRect()
      if (!rect) return true
      return rect.width > 0 && rect.height > 0
    }

    function hiddenByAttribute(el) {
      if (hasAttr(el, "hidden")) return true
      return attr(el, "aria-hidden") === "true"
    }

    function ancestor(el, tagName, limit) {
      var node = el.parentNode
      var depth = 0
      while (node && depth < (limit || 12)) {
        if (upper(node.tagName) === tagName) return node
        node = node.parentNode
        depth += 1
      }
      return null
    }

    function ownText(el) {
      var kids = el.childNodes || []
      var parts = []
      for (var i = 0; i < kids.length; i++) {
        if (kids[i] && kids[i].nodeType === 3) parts.push(kids[i].nodeValue)
      }
      return clean(parts.join(" "))
    }

    function textOf(el) {
      return clean(el && el.textContent)
    }

    function collectLabels(root, into) {
      var kids = (root && root.childNodes) || []
      for (var i = 0; i < kids.length; i++) {
        var el = kids[i]
        if (!el || el.nodeType !== 1) continue
        if (upper(el.tagName) === "LABEL") {
          var target = attr(el, "for")
          if (target && !into.has(target)) into.set(target, textOf(el))
        }
        collectLabels(el, into)
      }
      return into
    }

    function accessibleName(el, labels) {
      var aria = attr(el, "aria-label")
      if (aria) return clip(aria)
      var labelledBy = attr(el, "aria-labelledby")
      if (labelledBy && doc.getElementById) {
        var referenced = doc.getElementById(labelledBy.split(/\s+/)[0])
        if (referenced) return clip(textOf(referenced))
      }
      var id = attr(el, "id")
      if (id && labels.has(id)) return clip(labels.get(id))
      var wrapper = ancestor(el, "LABEL", 4)
      if (wrapper) return clip(textOf(wrapper))
      var placeholder = attr(el, "placeholder")
      if (placeholder) return clip(placeholder)
      var title = attr(el, "title")
      if (title) return clip(title)
      var alt = attr(el, "alt")
      if (alt) return clip(alt)
      return clip(textOf(el))
    }

    function isSensitive(el) {
      var tag = upper(el.tagName)
      if (tag !== "INPUT" && tag !== "TEXTAREA" && tag !== "SELECT")
        return false
      var type = lower(attr(el, "type") || "text")
      if (tag === "INPUT" && (type === "password" || type === "hidden")) {
        return true
      }
      var autocomplete = lower(attr(el, "autocomplete") || "")
      if (autocomplete && SENSITIVE_AUTOCOMPLETE_RE.test(autocomplete)) {
        return true
      }
      var haystack = [
        attr(el, "name"),
        attr(el, "id"),
        attr(el, "aria-label"),
        attr(el, "placeholder"),
        autocomplete,
      ]
        .filter(Boolean)
        .join(" ")
      if (haystack && SENSITIVE_NAME_RE.test(haystack)) return true
      return !!ancestorWithAttribute(el, "data-neev-redact")
    }

    function ancestorWithAttribute(el, name) {
      var node = el
      var depth = 0
      while (node && depth < 20) {
        if (hasAttr(node, name)) return node
        node = node.parentNode
        depth += 1
      }
      return null
    }

    function resolvedHref(el) {
      var href = attr(el, "href")
      if (!href) return null
      try {
        return new URL(href, win.location.href)
      } catch {
        return null
      }
    }

    /** True when following this element would take the user off the origin. */
    function leavesOrigin(el) {
      if (upper(el.tagName) !== "A") return false
      if (hasAttr(el, "download")) return true
      var target = lower(attr(el, "target") || "")
      if (target && target !== "_self") return true
      var url = resolvedHref(el)
      if (!url) return false
      if (url.protocol !== "http:" && url.protocol !== "https:") return true
      var here = null
      try {
        here = new URL(win.location.href).origin
      } catch {
        return true
      }
      return url.origin !== here
    }

    function insideForm(el) {
      return !!ancestor(el, "FORM", 12)
    }

    function isConsequential(el, tag, type) {
      if (tag === "INPUT" && (type === "submit" || type === "image"))
        return true
      if (hasAttr(el, "formaction")) return true
      if (tag === "BUTTON") {
        var buttonType = lower(attr(el, "type") || "submit")
        if (buttonType === "submit" && insideForm(el)) return true
      }
      if (tag === "A") return leavesOrigin(el)
      if (attr(el, "aria-haspopup") === "dialog") return true
      return false
    }

    function filledFlag(el) {
      // The only thing this file ever learns from a value: whether it has one.
      var value = el.value
      return typeof value === "string" && value.length > 0
    }

    function classify(el, tag, labels, redacted, counters) {
      var type = lower(attr(el, "type") || "")
      if (tag === "INPUT") {
        var inputType = type || "text"
        if (inputType === "hidden") return { role: "hidden", tag: "input" }
        if (redacted || isSensitive(el)) {
          return { role: "password", tag: "input" }
        }
        if (!visible(el)) return null
        if (inputType === "checkbox" || inputType === "radio") {
          return {
            role: inputType,
            tag: "input",
            name: accessibleName(el, labels),
            checked: !!el.checked,
            enabled: !el.disabled,
          }
        }
        if (
          inputType === "submit" ||
          inputType === "button" ||
          inputType === "reset" ||
          inputType === "image"
        ) {
          return {
            role: "button",
            tag: "input",
            name: clip(attr(el, "value") || accessibleName(el, labels)),
            enabled: !el.disabled,
            consequential: isConsequential(el, tag, inputType),
          }
        }
        if (!includes(TYPEABLE_INPUT_TYPES, inputType)) {
          return {
            role: "other",
            tag: "input",
            name: accessibleName(el, labels),
            enabled: !el.disabled,
          }
        }
        return {
          role: "textbox",
          tag: "input",
          name: accessibleName(el, labels),
          placeholder: clip(attr(el, "placeholder") || ""),
          filled: filledFlag(el),
          enabled: !el.disabled,
        }
      }
      if (tag === "TEXTAREA") {
        if (redacted || isSensitive(el)) {
          return { role: "password", tag: "textarea" }
        }
        if (!visible(el)) return null
        return {
          role: "textbox",
          tag: "textarea",
          name: accessibleName(el, labels),
          placeholder: clip(attr(el, "placeholder") || ""),
          filled: filledFlag(el),
          enabled: !el.disabled,
        }
      }
      if (redacted) {
        if (tag === "SELECT" || tag === "BUTTON" || tag === "A") {
          return { role: "hidden", tag: lower(tag) }
        }
        return null
      }
      if (tag === "SELECT") {
        if (!visible(el)) return null
        return {
          role: "select",
          tag: "select",
          name: accessibleName(el, labels),
          filled: filledFlag(el),
          enabled: !el.disabled,
        }
      }
      if (tag === "BUTTON" || attr(el, "role") === "button") {
        if (!visible(el)) return null
        return {
          role: "button",
          tag: lower(tag),
          name: accessibleName(el, labels),
          enabled: !el.disabled,
          consequential: isConsequential(el, tag, type),
        }
      }
      if (tag === "A" && hasAttr(el, "href")) {
        if (!visible(el)) return null
        return {
          role: "link",
          tag: "a",
          name: accessibleName(el, labels),
          consequential: leavesOrigin(el),
        }
      }
      if (includes(HEADING_TAGS, tag) || attr(el, "role") === "heading") {
        var heading = clip(textOf(el))
        if (!heading || !visible(el)) return null
        return { role: "heading", tag: lower(tag), name: heading }
      }
      if (tag === "IMG") {
        var alt = clip(attr(el, "alt") || "")
        if (!alt || !visible(el)) return null
        return { role: "image", tag: "img", name: alt }
      }
      if (includes(TEXT_TAGS, tag)) {
        if (counters.text >= MAX_TEXT_NODES) return null
        var text = clip(ownText(el))
        if (!text || !visible(el)) return null
        counters.text += 1
        return { role: "text", tag: lower(tag), name: text }
      }
      return null
    }

    function walk(root, out, pending, labels, redacted, counters) {
      var kids = (root && root.childNodes) || []
      for (var i = 0; i < kids.length; i++) {
        var el = kids[i]
        if (!el || el.nodeType !== 1) continue
        var tag = upper(el.tagName)
        if (includes(SKIP_TAGS, tag)) continue
        if (hasAttr(el, "data-neev-overlay")) continue
        if (hiddenByAttribute(el)) continue
        var redactedHere = redacted || hasAttr(el, "data-neev-redact")
        var node = classify(el, tag, labels, redactedHere, counters)
        if (node) {
          node.ref = "e" + epoch + ":" + out.length
          pending.set(node.ref, el)
          out.push(node)
          // One past the cap on purpose: `boundSnapshot` needs to see that
          // there was more than fits to report `truncated`.
          if (out.length > MAX_NODES) return
        }
        walk(el, out, pending, labels, redactedHere, counters)
        if (out.length > MAX_NODES) return
      }
    }

    function boundSnapshot(snapshot) {
      var nodes = snapshot.nodes
      var truncated = nodes.length > MAX_NODES
      var out = {
        epoch: snapshot.epoch,
        url: String(snapshot.url || "").slice(0, 512),
        title: clip(snapshot.title),
        nodes: nodes.slice(0, MAX_NODES),
        truncated: truncated,
      }
      while (
        out.nodes.length > 0 &&
        JSON.stringify(out).length > MAX_SNAPSHOT_BYTES
      ) {
        out.nodes.pop()
        out.truncated = true
      }
      return out
    }

    function buildSnapshot(nextEpoch) {
      epoch = typeof nextEpoch === "number" ? nextEpoch : epoch + 1
      var labels = collectLabels(doc.body || doc.documentElement, new Map())
      var nodes = []
      var pending = new Map()
      walk(doc.body || doc.documentElement, nodes, pending, labels, false, {
        text: 0,
      })
      var bounded = boundSnapshot({
        epoch: epoch,
        url: win.location ? win.location.href : "",
        title: doc.title,
        nodes: nodes,
      })
      // Keep the ref map in step with what actually shipped: a node dropped by
      // the byte budget must not remain clickable.
      refs = new Map()
      for (var i = 0; i < bounded.nodes.length; i++) {
        var ref = bounded.nodes[i].ref
        refs.set(ref, pending.get(ref))
      }
      return bounded
    }

    // -------------------------------------------------------------- actions

    function resolveRef(ref) {
      var el = refs.get(ref)
      if (!el) return { error: "stale_ref" }
      if (el.isConnected === false) return { error: "stale_ref" }
      return { el: el }
    }

    function makeEvent(type, init) {
      var Ctor = null
      if (/^pointer/.test(type) && win.PointerEvent) Ctor = win.PointerEvent
      else if (/^(mouse|click|dbl)/.test(type) && win.MouseEvent) {
        Ctor = win.MouseEvent
      } else if (type === "input" && win.InputEvent) Ctor = win.InputEvent
      if (!Ctor) Ctor = win.Event
      try {
        return new Ctor(type, init)
      } catch {
        return new win.Event(type, init)
      }
    }

    function dispatch(el, type, init) {
      el.dispatchEvent(makeEvent(type, init))
    }

    function clickAt(el, point) {
      if (el.focus) {
        try {
          el.focus()
        } catch {
          /* not focusable */
        }
      }
      var base = {
        bubbles: true,
        cancelable: true,
        composed: true,
        view: win,
        detail: 1,
        button: 0,
        clientX: Math.round(point.x),
        clientY: Math.round(point.y),
      }
      var down = Object.assign({}, base, {
        buttons: 1,
        pointerId: 1,
        isPrimary: true,
      })
      var up = Object.assign({}, base, {
        buttons: 0,
        pointerId: 1,
        isPrimary: true,
      })
      dispatch(el, "pointerdown", down)
      dispatch(el, "mousedown", down)
      dispatch(el, "pointerup", up)
      dispatch(el, "mouseup", up)
      dispatch(el, "click", base)
    }

    function typeInto(el, text) {
      if (el.focus) {
        try {
          el.focus()
        } catch {
          /* not focusable */
        }
      }
      var tag = upper(el.tagName)
      var Ctor =
        tag === "TEXTAREA" ? win.HTMLTextAreaElement : win.HTMLInputElement
      var setter = null
      try {
        var descriptor =
          Ctor && Object.getOwnPropertyDescriptor(Ctor.prototype, "value")
        setter = descriptor && descriptor.set
      } catch {
        setter = null
      }
      // React and friends install their own `value` setter on the element;
      // going through the prototype's is what makes the framework notice.
      if (setter) setter.call(el, text)
      else el.value = text
      dispatch(el, "input", { bubbles: true, composed: true })
      dispatch(el, "change", { bubbles: true })
      // Deliberately no Enter keydown and no form.submit(): submitting is the
      // user's decision, made through an approved click on a real control.
    }

    function actionable(el, kind) {
      if (el.disabled) return "not_actionable"
      if (!visible(el)) return "not_visible"
      var tag = upper(el.tagName)
      if (kind === "type") {
        if (tag !== "INPUT" && tag !== "TEXTAREA") return "not_actionable"
        if (tag === "INPUT") {
          var type = lower(attr(el, "type") || "text")
          if (!includes(TYPEABLE_INPUT_TYPES, type)) return "not_actionable"
        }
        if (isSensitive(el)) return "not_actionable"
      }
      if (kind === "click" && leavesOrigin(el)) return "not_actionable"
      return null
    }

    function act(kind, message) {
      var found = resolveRef(message.ref)
      if (found.error) return Promise.resolve(found)
      var el = found.el
      var refusal = actionable(el, kind)
      if (refusal) return Promise.resolve({ error: refusal })
      return movePointer(el).then(function (point) {
        if (kind === "click") clickAt(el, point)
        else typeInto(el, String(message.text == null ? "" : message.text))
        return { ok: true }
      })
    }

    function scroll(message) {
      if (message.ref) {
        var found = resolveRef(message.ref)
        if (found.error) return Promise.resolve(found)
        return movePointer(found.el).then(function () {
          return { ok: true }
        })
      }
      var amount = Math.min(10, Math.max(1, Number(message.amount) || 1))
      var viewport = Number(win.innerHeight) || 800
      var step = viewport * 0.8 * amount
      var top = message.direction === "up" ? -step : step
      if (win.scrollBy) {
        win.scrollBy({
          top: top,
          left: 0,
          behavior: reduced ? "auto" : "smooth",
        })
      }
      return Promise.resolve({ ok: true })
    }

    function describeRef(ref) {
      var found = resolveRef(ref)
      if (found.error) return found
      var el = found.el
      var tag = upper(el.tagName)
      var labels = collectLabels(doc.body || doc.documentElement, new Map())
      // The page's redaction hint has to hold here too. The snapshot already
      // strips these, but the approval card and the exported transcript are
      // fed from this call, and they are written to the person's disk.
      var redacted = !!ancestorWithAttribute(el, "data-neev-redact")
      var sensitive = redacted || isSensitive(el)
      // Same classifier the snapshot uses, so the approval card names the
      // element exactly as the model saw it.
      var classified = classify(el, tag, labels, redacted, { text: 0 }) || {}
      var role = classified.role || lower(tag)
      if (redacted) role = classified.role || "hidden"
      else if (sensitive) role = "password"
      return {
        ok: true,
        node: {
          ref: ref,
          tag: lower(tag),
          role: role,
          name: sensitive ? "" : classified.name || accessibleName(el, labels),
          consequential:
            !!classified.consequential ||
            isConsequential(el, tag, lower(attr(el, "type") || "")),
          sensitive: sensitive,
          leavesOrigin: leavesOrigin(el),
          enabled: !el.disabled,
        },
      }
    }

    function detach() {
      refs = new Map()
      token = null
      hidePointer()
      teardownOverlay()
      if (win.removeEventListener) {
        win.removeEventListener("pagehide", onPageHide)
      }
    }

    function handle(message) {
      if (!message || message.neev !== 1) {
        return Promise.resolve({ error: "not_actionable" })
      }
      if (message.kind === "attach") {
        token = message.token
        ensureOverlay()
        return Promise.resolve({ ok: true })
      }
      if (!token || message.token !== token) {
        return Promise.resolve({ error: "wrong_run" })
      }
      switch (message.kind) {
        case "snapshot":
          return Promise.resolve({
            ok: true,
            snapshot: buildSnapshot(message.epoch),
          })
        case "describe":
          return Promise.resolve(describeRef(message.ref))
        case "click":
          return act("click", message)
        case "type":
          return act("type", message)
        case "scroll":
          return scroll(message)
        case "reset":
          refs = new Map()
          hidePointer()
          return Promise.resolve({ ok: true })
        case "detach":
          detach()
          return Promise.resolve({ ok: true })
        default:
          return Promise.resolve({ error: "not_actionable" })
      }
    }

    return {
      handle: handle,
      detach: detach,
      // Read-only views used by the tests; nothing writes through these.
      _state: function () {
        return { epoch: epoch, refs: refs, overlay: overlay, reduced: reduced }
      },
    }
  }

  // Exposed so the Node tests can build an agent over a fake window. In the
  // page this is just a property on the injected script's own global.
  if (typeof globalThis !== "undefined") {
    globalThis.__neevBrowserCreateAgent = createAgent
  }

  var chromeApi = typeof chrome !== "undefined" ? chrome : null
  if (chromeApi && chromeApi.runtime && chromeApi.runtime.onMessage) {
    if (!window.__neevBrowserAgent) {
      window.__neevBrowserAgent = createAgent(window)
      chromeApi.runtime.onMessage.addListener(
        function (message, sender, sendResponse) {
          // Only this extension can reach here — `externally_connectable` is
          // not declared — but the sender check costs nothing and says so.
          if (!sender || sender.id !== chromeApi.runtime.id) return false
          if (!message || message.neev !== 1) return false
          window.__neevBrowserAgent
            .handle(message)
            .then(sendResponse, function () {
              sendResponse({ error: "action_failed" })
            })
          return true
        }
      )
    }
  }
})()
