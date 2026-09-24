import { test } from "node:test"
import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { fileURLToPath } from "node:url"
import vm from "node:vm"

import { el, makeWindow, find } from "./helpers/fake-dom.mjs"
import { makeRef, refEpoch } from "../extension/lib/protocol.js"

const source = readFileSync(
  fileURLToPath(new URL("../extension/content/agent.js", import.meta.url)),
  "utf8"
)
// The file guards its own Chrome bootstrap behind `typeof chrome`, so running
// it here only publishes the factory.
vm.runInThisContext(source, { filename: "content/agent.js" })
const createAgent = globalThis.__neevBrowserCreateAgent

const PAGE = [
  el("h1", { text: "Orders" }),
  el("p", { text: "Three orders are waiting for payment this morning." }),
  el("form", { action: "/pay" }, [
    el("label", { for: "cust", text: "Customer" }),
    el("input", { id: "cust", type: "text", value: "Ada Lovelace" }),
    el("label", { for: "pw", text: "Password" }),
    el("input", { id: "pw", type: "password", value: "hunter2" }),
    el("input", { type: "hidden", name: "csrf", value: "tok-abc123" }),
    el("input", { id: "card", type: "text", name: "cardNumber", value: "" }),
    el("textarea", { id: "note", placeholder: "Notes" }),
    el("button", { type: "submit", text: "Save order" }),
  ]),
  el("a", { href: "/orders/9", text: "Order 9" }),
  el("a", { href: "https://evil.test/pay", text: "Pay elsewhere" }),
  el("button", { id: "plain", text: "Refresh" }),
]

function agentOn(options = {}) {
  const win = makeWindow(Object.assign({ body: PAGE }, options))
  const agent = createAgent(win)
  return { win, agent }
}

async function attached(options) {
  const { win, agent } = agentOn(options)
  await agent.handle({ neev: 1, kind: "attach", token: "tok" })
  return { win, agent }
}

async function snapshot(agent, epoch) {
  const reply = await agent.handle({
    neev: 1,
    token: "tok",
    kind: "snapshot",
    epoch,
  })
  return reply.snapshot
}

function nodeNamed(snap, name) {
  return snap.nodes.find((node) => node.name === name)
}

function byId(win, id) {
  return find(win.document.body, (node) => node.getAttribute("id") === id)
}

// ------------------------------------------------------------------ reading

test("no field value ever leaves the page", async () => {
  const { agent } = await attached()
  const snap = await snapshot(agent, 1)
  const blob = JSON.stringify(snap)

  for (const secret of ["hunter2", "Ada Lovelace", "tok-abc123"]) {
    assert.ok(!blob.includes(secret), secret + " must not be in a snapshot")
  }
  for (const node of snap.nodes) {
    assert.ok(!("value" in node), "no node may carry a value key")
  }
})

test("password, hidden and payment fields are reported with a role and no name", async () => {
  const { agent } = await attached()
  const snap = await snapshot(agent, 1)

  const password = snap.nodes.find((node) => node.role === "password")
  assert.ok(password)
  assert.ok(!("name" in password), "not even the label")
  assert.equal(password.tag, "input")

  const hidden = snap.nodes.find((node) => node.role === "hidden")
  assert.ok(hidden)
  assert.ok(!("name" in hidden))

  // Three redacted fields: the password, the hidden csrf token and the card
  // number, which is only recognisable by its name.
  const redacted = snap.nodes.filter(
    (node) => node.role === "password" || node.role === "hidden"
  )
  assert.equal(redacted.length, 3)
})

test("a text field reports whether it is filled, and takes its name from its label", async () => {
  const { agent } = await attached()
  const snap = await snapshot(agent, 1)
  const customer = nodeNamed(snap, "Customer")
  assert.equal(customer.role, "textbox")
  assert.equal(customer.filled, true)
  assert.equal(customer.enabled, true)

  const note = snap.nodes.find((node) => node.tag === "textarea")
  assert.equal(note.filled, false)
  assert.equal(note.placeholder, "Notes")
})

test("consequential elements are flagged so the model can see what needs a human", async () => {
  const { agent } = await attached()
  const snap = await snapshot(agent, 1)

  assert.equal(nodeNamed(snap, "Save order").consequential, true)
  assert.equal(nodeNamed(snap, "Order 9").consequential, false)
  assert.equal(nodeNamed(snap, "Pay elsewhere").consequential, true)
  assert.equal(nodeNamed(snap, "Refresh").consequential, false)
})

test("headings and body text come through as bounded plain strings", async () => {
  const { agent } = await attached()
  const snap = await snapshot(agent, 1)
  assert.equal(nodeNamed(snap, "Orders").role, "heading")
  const text = snap.nodes.find((node) => node.role === "text")
  assert.match(text.name, /Three orders/)
})

test("strings are clipped and the node count is capped, and truncation is reported", async () => {
  const many = []
  for (let i = 0; i < 260; i += 1) {
    many.push(el("button", { text: "Button " + i + " " + "x".repeat(400) }))
  }
  const { agent } = await attached({ body: many })
  const snap = await snapshot(agent, 1)

  assert.equal(snap.nodes.length, 200)
  assert.equal(snap.truncated, true)
  for (const node of snap.nodes) {
    assert.ok(node.name.length <= 120, "names are clipped to 120 chars")
  }
  assert.ok(JSON.stringify(snap).length <= 49152)
})

test("the overlay never appears in its own snapshot", async () => {
  const { win, agent } = await attached()
  const snap = await snapshot(agent, 1)
  const overlay = find(win.document.body, (node) =>
    node.hasAttribute("data-neev-overlay")
  )
  assert.ok(overlay, "the badge is on the page")
  assert.ok(
    !JSON.stringify(snap).includes("controlling this tab"),
    "but it is not page content"
  )
})

test("anything inside data-neev-redact is reported without its content", async () => {
  const { agent } = await attached({
    body: [
      el("div", { "data-neev-redact": "" }, [
        el("input", { id: "secret", type: "text", value: "classified" }),
        el("p", { text: "classified narrative" }),
      ]),
    ],
  })
  const snap = await snapshot(agent, 1)
  const blob = JSON.stringify(snap)
  assert.ok(!blob.includes("classified"))
  assert.equal(snap.nodes[0].role, "password")
})

// ------------------------------------------------------------------- refs

test("refs carry the epoch the worker handed down, in the shared format", async () => {
  const { agent } = await attached()
  const snap = await snapshot(agent, 4)
  assert.equal(snap.epoch, 4)
  // The page mints refs and the worker parses them; this is the one place
  // both halves of that agreement are checked against each other.
  snap.nodes.forEach((node, index) => {
    assert.equal(node.ref, makeRef(4, index))
    assert.equal(refEpoch(node.ref), 4)
  })
})

test("a ref from the previous reading is stale, never re-resolved", async () => {
  const { agent } = await attached()
  const first = await snapshot(agent, 1)
  const refresh = nodeNamed(first, "Refresh").ref

  await snapshot(agent, 2)
  const reply = await agent.handle({
    neev: 1,
    token: "tok",
    kind: "click",
    ref: refresh,
  })
  assert.deepEqual(reply, { error: "stale_ref" })
})

test("a ref for an element that left the document is stale", async () => {
  const { win, agent } = await attached()
  const snap = await snapshot(agent, 1)
  byId(win, "plain").remove()

  const reply = await agent.handle({
    neev: 1,
    token: "tok",
    kind: "click",
    ref: nodeNamed(snap, "Refresh").ref,
  })
  assert.deepEqual(reply, { error: "stale_ref" })
})

test("a message without this run's token is refused", async () => {
  const { agent } = await attached()
  const reply = await agent.handle({
    neev: 1,
    token: "some-other-run",
    kind: "snapshot",
    epoch: 1,
  })
  assert.deepEqual(reply, { error: "wrong_run" })
})

// ---------------------------------------------------------------- actions

test("a click dispatches a real pointer sequence at the element's centre", async () => {
  const { win, agent } = await attached()
  const snap = await snapshot(agent, 1)
  const button = byId(win, "plain")
  button.rect = { left: 100, top: 200, width: 60, height: 20 }

  const reply = await agent.handle({
    neev: 1,
    token: "tok",
    kind: "click",
    ref: nodeNamed(snap, "Refresh").ref,
  })

  assert.deepEqual(reply, { ok: true })
  assert.deepEqual(
    button.dispatched.map((event) => event.type),
    ["pointerdown", "mousedown", "pointerup", "mouseup", "click"]
  )
  assert.equal(button.focused, true)
  const click = button.dispatched[4]
  assert.equal(click.clientX, 130)
  assert.equal(click.clientY, 210)
  assert.equal(click.bubbles, true)
  assert.equal(click.composed, true)
})

test("typing goes through the prototype setter and fires input and change", async () => {
  const { win, agent } = await attached()
  const snap = await snapshot(agent, 1)
  const input = byId(win, "cust")
  const before = input.valueWrites

  const reply = await agent.handle({
    neev: 1,
    token: "tok",
    kind: "type",
    ref: nodeNamed(snap, "Customer").ref,
    text: "Grace Hopper",
  })

  assert.deepEqual(reply, { ok: true })
  assert.equal(input.value, "Grace Hopper")
  assert.equal(input.valueWrites, before + 1)
  assert.deepEqual(
    input.dispatched.map((event) => event.type),
    ["input", "change"]
  )
  assert.ok(
    !input.dispatched.some((event) => /^key/.test(event.type)),
    "no synthetic Enter: submitting is the user's decision"
  )
})

test("typing into a password or payment field is refused in the page too", async () => {
  const { win, agent } = await attached()
  const snap = await snapshot(agent, 1)
  // The model cannot get a ref for these from a snapshot, so drive it the
  // hard way: take a ref that does exist and point it at the password field.
  const ref = nodeNamed(snap, "Customer").ref
  const state = agent._state()
  state.refs.set(ref, byId(win, "pw"))

  assert.deepEqual(
    await agent.handle({ neev: 1, token: "tok", kind: "type", ref, text: "x" }),
    { error: "not_actionable" }
  )

  state.refs.set(ref, byId(win, "card"))
  assert.deepEqual(
    await agent.handle({
      neev: 1,
      token: "tok",
      kind: "type",
      ref,
      text: "4111",
    }),
    { error: "not_actionable" }
  )
  assert.equal(byId(win, "pw").value, "hunter2", "untouched")
})

test("a link off this origin is refused in the page too", async () => {
  const { agent } = await attached()
  const snap = await snapshot(agent, 1)
  const reply = await agent.handle({
    neev: 1,
    token: "tok",
    kind: "click",
    ref: nodeNamed(snap, "Pay elsewhere").ref,
  })
  assert.deepEqual(reply, { error: "not_actionable" })
})

test("a disabled control is not actionable", async () => {
  const { win, agent } = await attached()
  const snap = await snapshot(agent, 1)
  byId(win, "plain").disabled = true
  const reply = await agent.handle({
    neev: 1,
    token: "tok",
    kind: "click",
    ref: nodeNamed(snap, "Refresh").ref,
  })
  assert.deepEqual(reply, { error: "not_actionable" })
})

test("scrolling moves the viewport by whole screens, clamped", async () => {
  const { win, agent } = await attached()
  await snapshot(agent, 1)
  await agent.handle({
    neev: 1,
    token: "tok",
    kind: "scroll",
    direction: "down",
    amount: 2,
  })
  assert.equal(win.scrolls[0].top, 800 * 0.8 * 2)

  await agent.handle({
    neev: 1,
    token: "tok",
    kind: "scroll",
    direction: "up",
    amount: 99,
  })
  assert.equal(win.scrolls[1].top, -(800 * 0.8 * 10), "amount is clamped to 10")
})

// ----------------------------------------------------------------- pointer

function pointerOf(agent) {
  return agent._state().overlay.pointer
}

test("the pointer does not move for a reading of the page", async () => {
  const { agent } = await attached()
  await snapshot(agent, 1)
  assert.equal(pointerOf(agent).style.transform, undefined)
  assert.equal(pointerOf(agent).style.opacity, undefined)
})

test("the pointer moves to the element only as the action is dispatched", async () => {
  const { win, agent } = await attached()
  const snap = await snapshot(agent, 1)
  byId(win, "plain").rect = { left: 40, top: 60, width: 20, height: 10 }

  await agent.handle({
    neev: 1,
    token: "tok",
    kind: "click",
    ref: nodeNamed(snap, "Refresh").ref,
  })

  const pointer = pointerOf(agent)
  assert.equal(pointer.style.transform, "translate(50px, 65px)")
  assert.equal(pointer.style.opacity, "1")
  assert.ok(pointer.classList.contains("pulse"))
  assert.equal(byId(win, "plain").scrolledIntoView, 1)
})

test("the pointer does not move for a refused action", async () => {
  const { agent } = await attached()
  const snap = await snapshot(agent, 1)
  await agent.handle({
    neev: 1,
    token: "tok",
    kind: "click",
    ref: nodeNamed(snap, "Pay elsewhere").ref,
  })
  assert.equal(pointerOf(agent).style.transform, undefined)
})

test("the overlay cannot take a click that belonged to the page", async () => {
  const { agent } = await attached()
  const host = agent._state().overlay.host
  assert.equal(host.style.pointerEvents, "none")
  assert.equal(host.style.position, "fixed")
  assert.equal(host.style.width, "0")
  assert.equal(host.style.height, "0")
  const css = host.shadowRoot.children[0].textContent
  for (const selector of [".layer", ".pointer", ".badge"]) {
    assert.ok(css.includes(selector), selector + " is styled")
  }
  assert.ok(
    (css.match(/pointer-events:none/g) || []).length >= 3,
    "every painted layer is click-through"
  )
})

test("the badge says who is driving, for exactly as long as the run lasts", async () => {
  const { win, agent } = await attached()
  assert.match(agent._state().overlay.badge.textContent, /controlling this tab/)

  await agent.handle({ neev: 1, token: "tok", kind: "detach" })
  assert.equal(agent._state().overlay, null)
  assert.equal(
    find(win.document.body, (node) => node.hasAttribute("data-neev-overlay")),
    null
  )
})

test("the overlay goes away when the document does", async () => {
  const { win, agent } = await attached()
  win.fire("pagehide")
  assert.equal(agent._state().overlay, null)
  assert.equal(
    find(win.document.body, (node) => node.hasAttribute("data-neev-overlay")),
    null
  )
})

test("detaching drops every ref and the run token with it", async () => {
  const { agent } = await attached()
  const snap = await snapshot(agent, 1)
  await agent.handle({ neev: 1, token: "tok", kind: "detach" })

  assert.equal(agent._state().refs.size, 0)
  assert.deepEqual(
    await agent.handle({
      neev: 1,
      token: "tok",
      kind: "click",
      ref: snap.nodes[0].ref,
    }),
    { error: "wrong_run" },
    "the token is gone, so nothing acts until the panel attaches again"
  )
})

test("a reset keeps the badge but drops the refs and hides the pointer", async () => {
  const { agent } = await attached()
  const snap = await snapshot(agent, 1)
  await agent.handle({ neev: 1, token: "tok", kind: "reset" })

  assert.ok(agent._state().overlay, "the run is still live")
  assert.equal(pointerOf(agent).style.opacity, "0")
  assert.deepEqual(
    await agent.handle({
      neev: 1,
      token: "tok",
      kind: "click",
      ref: snap.nodes[0].ref,
    }),
    { error: "stale_ref" }
  )
})

test("reduced motion means the pointer teleports instead of gliding", async () => {
  const { agent } = await attached({ reducedMotion: true })
  const snap = await snapshot(agent, 1)
  assert.ok(pointerOf(agent).classList.contains("reduced"))

  await agent.handle({
    neev: 1,
    token: "tok",
    kind: "click",
    ref: nodeNamed(snap, "Refresh").ref,
  })
  assert.ok(!pointerOf(agent).classList.contains("pulse"))
})

// ----------------------------------------------------------------- describe

test("describe names the element for the approval card and flags what it is", async () => {
  const { agent } = await attached()
  const snap = await snapshot(agent, 1)

  const save = await agent.handle({
    neev: 1,
    token: "tok",
    kind: "describe",
    ref: nodeNamed(snap, "Save order").ref,
  })
  assert.equal(save.node.name, "Save order")
  assert.equal(save.node.role, "button")
  assert.equal(save.node.consequential, true)
  assert.equal(save.node.sensitive, false)

  const link = await agent.handle({
    neev: 1,
    token: "tok",
    kind: "describe",
    ref: nodeNamed(snap, "Pay elsewhere").ref,
  })
  assert.equal(link.node.leavesOrigin, true)

  assert.deepEqual(
    await agent.handle({
      neev: 1,
      token: "tok",
      kind: "describe",
      ref: "e9:1",
    }),
    { error: "stale_ref" }
  )
})

test("an unknown instruction is refused rather than guessed at", async () => {
  const { agent } = await attached()
  assert.deepEqual(
    await agent.handle({ neev: 1, token: "tok", kind: "evaluate", code: "1" }),
    { error: "not_actionable" }
  )
  assert.deepEqual(await agent.handle({ kind: "snapshot" }), {
    error: "not_actionable",
  })
})

test("a redacted region stays redacted in the approval card and the export", async () => {
  const { agent } = await attached({
    body: [
      el("div", { "data-neev-redact": "" }, [
        el("button", { id: "wire", text: "Wire £40,000 to Acme Ltd" }),
        el("a", { href: "/case/8812", text: "Case 8812 — Jane Roe" }),
      ]),
      el("button", { id: "ok", text: "Cancel" }),
    ],
  })
  const snap = await snapshot(agent, 1)

  // The model already sees nothing; this is about what `describe` feeds into
  // the approval card and the exported transcript on disk.
  for (const ref of snap.nodes.map((node) => node.ref)) {
    const described = await agent.handle({
      neev: 1,
      token: "tok",
      kind: "describe",
      ref,
    })
    if (described.node.name === "Cancel") continue
    assert.equal(described.node.name, "", "no name escapes a redacted region")
    assert.equal(described.node.role, "hidden")
    assert.equal(described.node.sensitive, true)
  }
})
