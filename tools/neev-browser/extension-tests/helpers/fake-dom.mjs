/**
 * A hand-built DOM, small enough to read in one sitting.
 *
 * `content/agent.js` is deliberately written against a narrow slice of the DOM
 * — childNodes, attributes, rects, dispatchEvent, a value accessor on the
 * element prototypes — so that slice can be reproduced here without pulling in
 * a browser or a jsdom dependency. If the agent ever needs an API this file
 * does not have, that is a signal to reconsider the agent, not to grow this.
 */

export class FakeText {
  constructor(value) {
    this.nodeType = 3
    this.nodeValue = String(value)
    this.parentNode = null
  }

  get textContent() {
    return this.nodeValue
  }
}

export class FakeEvent {
  constructor(type, init) {
    this.type = type
    Object.assign(this, init || {})
  }
}

export class FakeMouseEvent extends FakeEvent {}
export class FakePointerEvent extends FakeMouseEvent {}
export class FakeInputEvent extends FakeEvent {}

export class FakeElement {
  constructor(ownerDocument, tagName) {
    this.nodeType = 1
    this.ownerDocument = ownerDocument
    this.tagName = String(tagName).toUpperCase()
    this.attributes = new Map()
    this.childNodes = []
    this.parentNode = null
    this.style = {}
    this.rect = { left: 0, top: 0, width: 120, height: 24 }
    this.disabled = false
    this.checked = false
    this.dispatched = []
    this.focused = false
    this.scrolledIntoView = 0
    this._class = ""
  }

  get children() {
    return this.childNodes.filter((node) => node.nodeType === 1)
  }

  get isConnected() {
    let node = this.parentNode
    if (!node) return false
    while (node.parentNode) node = node.parentNode
    return node === this.ownerDocument
  }

  get className() {
    return this._class
  }

  set className(value) {
    this._class = String(value)
  }

  get classList() {
    const parts = () => this._class.split(/\s+/).filter(Boolean)
    const write = (set) => {
      this._class = [...set].join(" ")
    }
    return {
      add: (...names) => {
        const set = new Set(parts())
        for (const name of names) set.add(name)
        write(set)
      },
      remove: (...names) => {
        const set = new Set(parts())
        for (const name of names) set.delete(name)
        write(set)
      },
      contains: (name) => parts().includes(name),
    }
  }

  getAttribute(name) {
    return this.attributes.has(name) ? this.attributes.get(name) : null
  }

  setAttribute(name, value) {
    this.attributes.set(name, String(value))
  }

  hasAttribute(name) {
    return this.attributes.has(name)
  }

  appendChild(child) {
    child.parentNode = this
    this.childNodes.push(child)
    return child
  }

  removeChild(child) {
    const index = this.childNodes.indexOf(child)
    if (index >= 0) this.childNodes.splice(index, 1)
    child.parentNode = null
    return child
  }

  remove() {
    if (this.parentNode) this.parentNode.removeChild(this)
  }

  get textContent() {
    return this.childNodes
      .map((node) => (node.nodeType === 3 ? node.nodeValue : node.textContent))
      .join("")
  }

  set textContent(value) {
    this.childNodes = []
    this.appendChild(new FakeText(value))
  }

  getBoundingClientRect() {
    return this.rect
  }

  dispatchEvent(event) {
    this.dispatched.push(event)
    return true
  }

  focus() {
    this.focused = true
    this.ownerDocument.activeElement = this
  }

  scrollIntoView() {
    this.scrolledIntoView += 1
  }

  attachShadow() {
    const root = new FakeElement(this.ownerDocument, "#shadow")
    root.parentNode = this
    this.shadowRoot = root
    return root
  }
}

/** Separate classes so the agent's native-value-setter path is the real one. */
export class FakeHTMLInputElement extends FakeElement {
  constructor(ownerDocument, tagName) {
    super(ownerDocument, tagName || "input")
    this._value = ""
    this.valueWrites = 0
  }

  get value() {
    return this._value
  }

  set value(next) {
    this._value = String(next)
    this.valueWrites += 1
  }
}

/**
 * Not a subclass of the input: the agent looks the `value` setter up on the
 * exact prototype for the tag, and inheriting it here would let a broken
 * lookup pass.
 */
export class FakeHTMLTextAreaElement extends FakeElement {
  constructor(ownerDocument) {
    super(ownerDocument, "textarea")
    this._value = ""
    this.valueWrites = 0
  }

  get value() {
    return this._value
  }

  set value(next) {
    this._value = String(next)
    this.valueWrites += 1
  }
}

export class FakeDocument {
  constructor() {
    this.title = "Test page"
    this.activeElement = null
    this.documentElement = new FakeElement(this, "html")
    this.documentElement.parentNode = this
    this.body = new FakeElement(this, "body")
    this.documentElement.appendChild(this.body)
    this.childNodes = [this.documentElement]
  }

  createElement(tagName) {
    const tag = String(tagName).toLowerCase()
    if (tag === "input") return new FakeHTMLInputElement(this, "input")
    if (tag === "textarea") return new FakeHTMLTextAreaElement(this)
    return new FakeElement(this, tag)
  }

  getElementById(id) {
    const search = (node) => {
      for (const child of node.children || []) {
        if (child.getAttribute && child.getAttribute("id") === id) return child
        const found = search(child)
        if (found) return found
      }
      return null
    }
    return search(this.documentElement)
  }
}

/**
 * Build an element tree from a compact literal:
 *   el("form", {}, [el("input", { type: "submit", value: "Pay" })])
 */
export function el(tag, attrs, children) {
  return { tag, attrs: attrs || {}, children: children || [] }
}

const SPECIAL = new Set(["text", "disabled", "checked", "rect"])

function materialise(doc, spec) {
  const node = doc.createElement(spec.tag)
  for (const [name, value] of Object.entries(spec.attrs || {})) {
    if (SPECIAL.has(name)) continue
    node.setAttribute(name, value)
  }
  if (spec.attrs.disabled) node.disabled = true
  if (spec.attrs.checked) node.checked = true
  if (spec.attrs.rect) node.rect = spec.attrs.rect
  if (typeof spec.attrs.value === "string") node._value = spec.attrs.value
  if (typeof spec.attrs.text === "string") {
    node.appendChild(new FakeText(spec.attrs.text))
  }
  for (const child of spec.children || []) {
    node.appendChild(materialise(doc, child))
  }
  return node
}

export function makeWindow(options = {}) {
  const doc = new FakeDocument()
  for (const spec of options.body || []) {
    doc.body.appendChild(materialise(doc, spec))
  }
  const listeners = new Map()
  const win = {
    document: doc,
    innerHeight: 800,
    location: { href: options.href || "https://app.test/orders" },
    scrolls: [],
    timeouts: 0,
    frames: 0,
    matchMedia(query) {
      return {
        matches: !!options.reducedMotion && /reduced-motion/.test(query),
        addEventListener() {},
      }
    },
    requestAnimationFrame(callback) {
      win.frames += 1
      callback(0)
      return win.frames
    },
    setTimeout(callback) {
      win.timeouts += 1
      callback()
      return win.timeouts
    },
    scrollBy(arg) {
      win.scrolls.push(arg)
    },
    addEventListener(type, handler) {
      if (!listeners.has(type)) listeners.set(type, [])
      listeners.get(type).push(handler)
    },
    removeEventListener(type, handler) {
      const list = listeners.get(type) || []
      const index = list.indexOf(handler)
      if (index >= 0) list.splice(index, 1)
    },
    fire(type) {
      for (const handler of (listeners.get(type) || []).slice())
        handler({ type })
    },
    listenerCount(type) {
      return (listeners.get(type) || []).length
    },
    Event: FakeEvent,
    MouseEvent: FakeMouseEvent,
    PointerEvent: FakePointerEvent,
    InputEvent: FakeInputEvent,
    HTMLInputElement: FakeHTMLInputElement,
    HTMLTextAreaElement: FakeHTMLTextAreaElement,
  }
  return win
}

/** Depth-first search of a built tree, for asserting against real elements. */
export function find(root, predicate) {
  for (const child of root.children) {
    if (predicate(child)) return child
    const found = find(child, predicate)
    if (found) return found
  }
  return null
}

export function findByTag(root, tagName) {
  return find(root, (node) => node.tagName === tagName.toUpperCase())
}
