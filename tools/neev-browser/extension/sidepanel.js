/**
 * The side panel.
 *
 * Everything rendered here — agent output, element names in approval prompts,
 * tab titles — is page- or model-derived, so every single insertion goes
 * through `textContent`. There is no `innerHTML` in this file and there must
 * never be one: a page that can put markup in its own `<h1>` would otherwise
 * be putting markup in the panel that approves actions against it.
 */

import { report } from "./lib/boot.js"
import { PanelConnection } from "./lib/connection.js"

const $ = (id) => document.getElementById(id)

const els = {
  statusPill: $("status-pill"),
  error: $("error"),
  setup: $("setup"),
  provider: $("provider"),
  model: $("model"),
  tabs: $("tabs"),
  tabsEmpty: $("tabs-empty"),
  loadTabs: $("load-tabs"),
  task: $("task"),
  attach: $("attach"),
  runCard: $("run-card"),
  runTitle: $("run-title"),
  runOrigin: $("run-origin"),
  runProvider: $("run-provider"),
  runCli: $("run-cli"),
  runDir: $("run-dir"),
  stop: $("stop"),
  approvalCard: $("approval-card"),
  approvalTool: $("approval-tool"),
  approvalRole: $("approval-role"),
  approvalName: $("approval-name"),
  approvalText: $("approval-text"),
  approvalReason: $("approval-reason"),
  approve: $("approve"),
  deny: $("deny"),
  handoffCard: $("handoff-card"),
  handoffInstruction: $("handoff-instruction"),
  handoffReason: $("handoff-reason"),
  handoffDone: $("handoff-done"),
  handoffSkip: $("handoff-skip"),
  stream: $("stream"),
  exportTranscript: $("export-transcript"),
  exportResults: $("export-results"),
}

let state = { status: "idle", run: null, approval: null, handoff: null }
let entries = []
let candidates = []
let selectedTabId = null

/** True while the worker holds a run that has not ended yet. */
let liveRun = false

const LOST_RUN_TEXT =
  "Chrome shut down the extension's background service worker, and the run " +
  "was running inside it, so the run is over. Nothing was replayed: no " +
  "further action was taken on the page, and the agent process ended when " +
  "its connection to the Neev companion closed. The run badge stays on that " +
  "tab until you reload it."

const WORKER_GONE_TEXT =
  "Chrome had already shut down the extension's background service worker, " +
  "so this run is no longer attached. Nothing was sent and nothing further " +
  "was done to the page."

const connection = new PanelConnection({
  connect: () => chrome.runtime.connect({ name: "neev-panel" }),
  handlers: {
    onMessage: onWorkerMessage,
    onDrop: onWorkerDrop,
    onGaveUp: onWorkerGaveUp,
  },
})

// Connecting is the first thing in this file that can fail, and a throw here
// would abort the module before a single listener below is registered. The
// failure comes back as a value instead, so the panel stays rendered and says
// what went wrong.
const openFailure = connection.open()
if (openFailure) {
  report(
    "The panel could not reach the extension's background service worker (" +
      openFailure +
      "). Reload the extension."
  )
}

/**
 * Post to the service worker, or say plainly why it did not go.
 *
 * `reconnect` is for messages that start something new: Chrome shutting an
 * idle worker down costs nothing, and waking it is the normal way back. It is
 * deliberately *not* set for stop, approval or handoff — those name state
 * that lived in the worker that went away, so sending them to a fresh one
 * would be answering a question nobody is asking any more.
 */
function send(message, { reconnect = false } = {}) {
  if (!connection.connected && reconnect) {
    const failure = connection.ensure()
    if (failure) {
      showError(
        "Could not reach the extension's background service worker (" +
          failure +
          "). Reload the extension."
      )
      return false
    }
  }
  if (!connection.connected) {
    showError(WORKER_GONE_TEXT)
    return false
  }
  const failure = connection.post(message)
  if (failure) {
    showError("The background service worker went away: " + failure)
    return false
  }
  return true
}

function onWorkerMessage(message) {
  if (!message) return
  if (message.kind === "sync") {
    // A snapshot with no run, when this panel had one, is the worker telling
    // us it restarted empty. Keep what the panel already has so the run that
    // died is still readable and exportable, and mark it ended for the reason
    // it actually ended for.
    if (liveRun && !(message.state && message.state.run)) {
      announceLostRun()
      return
    }
    state = message.state
    entries = message.log || []
    liveRun = isLive(state)
    renderStream()
    renderState()
    return
  }
  if (message.kind === "state") {
    state = message.state
    liveRun = isLive(state)
    renderState()
    return
  }
  if (message.kind === "log") {
    entries.push(message.entry)
    appendEntry(message.entry)
  }
}

function isLive(next) {
  return !!(next && next.run && next.status && next.status !== "idle")
}

function announceLostRun() {
  liveRun = false
  state = {
    ...state,
    status: "idle",
    approval: null,
    handoff: null,
    ended: {
      state: "revoked",
      reason: "service_worker_terminated",
      at: Date.now(),
    },
    error: LOST_RUN_TEXT,
  }
  const entry = { at: Date.now(), type: "error", text: LOST_RUN_TEXT }
  entries.push(entry)
  appendEntry(entry)
  renderState()
}

function onWorkerDrop() {
  if (liveRun) {
    // Something was riding on that port. Reconnect — bounded — so the panel
    // can say what became of the run instead of guessing.
    connection.recover()
    return
  }
  // Idle. Chrome terminates an idle service worker on purpose; nothing is
  // wrong and nothing is lost, so the panel says nothing and the next thing
  // the person does opens a fresh port.
}

// Hand the port back when the panel goes away, so the worker ends the run on
// the panel's own teardown rather than on Chrome noticing later, and so a
// scheduled retry cannot fire into a dying document.
window.addEventListener("pagehide", () => connection.close())

function onWorkerGaveUp() {
  report(
    "The extension's background service worker disconnected and would not " +
      "start again. Reload the extension, then reopen the panel."
  )
}

// ------------------------------------------------------------------ render

const STATUS_LABEL = {
  idle: "Idle",
  starting: "Starting",
  running: "Running",
  stopping: "Stopping",
}

function renderState() {
  const status = state.status || "idle"
  els.statusPill.textContent = STATUS_LABEL[status] || status
  els.statusPill.dataset.state = status

  if (state.error) {
    els.error.textContent = state.error
    els.error.hidden = false
  } else {
    els.error.hidden = true
  }

  const run = state.run
  els.setup.hidden = status !== "idle"
  els.runCard.hidden = !run
  if (run) {
    els.runTitle.textContent = run.title || "(untitled tab)"
    els.runOrigin.textContent = run.url || run.origin
    els.runProvider.textContent =
      (run.provider === "codex" ? "Codex CLI" : "Claude Code") +
      (run.model ? " · " + run.model : "")
    els.runCli.textContent = run.cli || "resolving…"
    els.runDir.textContent = run.runDir || "—"
    els.stop.hidden = status === "idle"
  }

  const approval = state.approval
  els.approvalCard.hidden = !approval
  if (approval) {
    els.approvalTool.textContent =
      approval.tool === "browser_type" ? "type" : "click"
    els.approvalRole.textContent =
      approval.role + (approval.tag ? " · <" + approval.tag + ">" : "")
    els.approvalName.textContent = approval.name || "(no accessible name)"
    if (approval.text === null || approval.text === undefined) {
      els.approvalText.hidden = true
      els.approvalText.textContent = ""
    } else {
      els.approvalText.hidden = false
      els.approvalText.textContent = approval.text
    }
    els.approvalReason.textContent = approval.reason || ""
  }

  const handoff = state.handoff
  els.handoffCard.hidden = !handoff
  if (handoff) {
    els.handoffInstruction.textContent = handoff.instruction
    els.handoffReason.textContent = handoff.reason
  }

  els.attach.disabled = selectedTabId === null || status !== "idle"
  const finished = status === "idle" && state.ended
  els.exportTranscript.disabled = !run
  els.exportResults.disabled = !run
  if (finished && run) {
    els.attach.textContent = "Grant access & run again"
  }
}

function entryLabel(entry) {
  if (entry.type === "output") return entry.channel
  if (entry.type === "action") return entry.approval
  return entry.type
}

function entryBody(entry) {
  if (entry.type === "action") {
    const name = entry.name ? " — " + entry.name : ""
    return entry.tool + name
  }
  return entry.text || ""
}

function appendEntry(entry) {
  const item = document.createElement("li")
  item.className = "entry"
  item.dataset.type = entry.type
  if (entry.channel) item.dataset.channel = entry.channel

  const when = document.createElement("span")
  when.className = "when"
  when.textContent = formatTime(entry.at) + " · " + entryLabel(entry)

  const body = document.createElement("pre")
  body.textContent = entryBody(entry)

  item.appendChild(when)
  item.appendChild(body)
  els.stream.appendChild(item)
  item.scrollIntoView({ block: "nearest" })
}

function renderStream() {
  els.stream.replaceChildren()
  for (const entry of entries) appendEntry(entry)
}

function formatTime(at) {
  try {
    return new Date(at).toLocaleTimeString()
  } catch {
    return ""
  }
}

function showError(text) {
  els.error.textContent = text
  els.error.hidden = false
}

// -------------------------------------------------------------- tab picker

async function listTabs() {
  const all = await chrome.tabs.query({})
  candidates = all.filter(
    (tab) =>
      typeof tab.url === "string" && /^https?:\/\//.test(tab.url) && tab.id >= 0
  )
  renderTabs()
}

function renderTabs() {
  els.tabs.replaceChildren()
  els.tabsEmpty.hidden = candidates.length > 0
  if (!candidates.length) {
    els.tabsEmpty.textContent =
      "No http(s) tabs found. Open the page you want Neev to work on, then " +
      "list the tabs again."
    return
  }
  for (const tab of candidates) {
    const item = document.createElement("li")
    const label = document.createElement("label")
    const radio = document.createElement("input")
    radio.type = "radio"
    radio.name = "tab"
    radio.value = String(tab.id)
    radio.checked = tab.id === selectedTabId
    radio.addEventListener("change", () => {
      selectedTabId = tab.id
      renderState()
    })
    const text = document.createElement("span")
    text.className = "tab-text"
    const title = document.createElement("span")
    title.className = "tab-title"
    title.textContent = tab.title || "(untitled)"
    const origin = document.createElement("span")
    origin.className = "tab-origin"
    origin.textContent = safeOrigin(tab.url)
    text.appendChild(title)
    text.appendChild(origin)
    label.appendChild(radio)
    label.appendChild(text)
    item.appendChild(label)
    els.tabs.appendChild(item)
  }
}

function safeOrigin(url) {
  try {
    return new URL(url).origin
  } catch {
    return url || ""
  }
}

// ------------------------------------------------------------------ actions

els.loadTabs.addEventListener("click", () => {
  // First statement in the handler, deliberately: Chrome only grants an
  // optional permission during a user gesture, and an `await` before this
  // point spends the gesture. `request` resolves true straight away when the
  // permission is already held, so there is no `contains` pre-check.
  chrome.permissions
    .request({ permissions: ["tabs"] })
    .then((granted) => {
      if (!granted) {
        showError(
          "Chrome needs the tabs permission to show you a list to pick from. " +
            "Without it Neev cannot tell you which tab is which."
        )
        return undefined
      }
      els.error.hidden = true
      return listTabs()
    })
    .catch((error) => showError(String(error && error.message)))
})

els.attach.addEventListener("click", () => {
  const tab = candidates.find((candidate) => candidate.id === selectedTabId)
  if (!tab) {
    showError("Pick a tab first.")
    return
  }
  const origin = safeOrigin(tab.url)
  const task = els.task.value.trim()
  if (!task) {
    showError("Type a task first.")
    return
  }
  els.error.hidden = true
  // Asked for here, inside the click, because Chrome only grants an optional
  // permission on a real user gesture — and because this is the moment the
  // user is agreeing to it.
  chrome.permissions
    .request({ origins: [origin + "/*"] })
    .then((granted) => {
      if (!granted) {
        showError("Chrome did not grant access to " + origin + ".")
        return
      }
      // The one message that may wake a shut-down worker: it starts something
      // new rather than answering for something the old worker held.
      send(
        {
          kind: "panel.start",
          provider: selectedProvider(),
          model: els.model.value.trim() || null,
          task,
          tabId: tab.id,
        },
        { reconnect: true }
      )
    })
    .catch((error) => showError(String(error && error.message)))
})

function selectedProvider() {
  const checked = els.provider.querySelector("input:checked")
  return checked ? checked.value : "claude"
}

els.stop.addEventListener("click", () => {
  send({ kind: "panel.stop" })
})

els.approve.addEventListener("click", () => decide("approve"))
els.deny.addEventListener("click", () => decide("deny"))

function decide(decision) {
  if (!state.approval) return
  // Only dismissed once the decision is actually on its way: a swallowed
  // send would otherwise take the prompt away with the answer unrecorded.
  const sent = send({
    kind: "panel.approval",
    id: state.approval.id,
    decision,
  })
  if (sent) els.approvalCard.hidden = true
}

els.handoffDone.addEventListener("click", () => handoff("done"))
els.handoffSkip.addEventListener("click", () => handoff("skipped"))

function handoff(outcome) {
  if (!state.handoff) return
  const sent = send({ kind: "panel.handoff", id: state.handoff.id, outcome })
  if (sent) els.handoffCard.hidden = true
}

// ------------------------------------------------------------------ export

function download(name, text, mime) {
  const blob = new Blob([text], { type: mime })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement("a")
  anchor.href = url
  anchor.download = name
  anchor.click()
  setTimeout(() => URL.revokeObjectURL(url), 30000)
}

function transcriptMarkdown() {
  const run = state.run || {}
  const lines = [
    "# Neev Browser run " + (run.runId || ""),
    "",
    "- Agent: " +
      (run.provider || "") +
      (run.model ? " (" + run.model + ")" : ""),
    "- CLI: " + (run.cli || "unknown"),
    "- Target: " + (run.url || run.origin || "") + " — tab " + run.tabId,
    "- Started: " +
      (run.startedAt ? new Date(run.startedAt).toISOString() : ""),
    "- Run directory: " + (run.runDir || "unknown"),
    "- Ended: " +
      (state.ended
        ? state.ended.state + " (" + state.ended.reason + ")"
        : "still running"),
    "",
    "Nothing in this run was saved to a Neev server. This file is the copy.",
    "",
    "## Activity",
    "",
  ]
  for (const entry of entries) {
    lines.push(
      "- `" +
        formatTime(entry.at) +
        "` **" +
        entryLabel(entry) +
        "** — " +
        entryBody(entry).replace(/\n/g, "\n  ")
    )
  }
  return lines.join("\n") + "\n"
}

function resultsJson() {
  return JSON.stringify(
    {
      tool: "neev-browser",
      savedTo: "local file only",
      run: state.run,
      ended: state.ended || null,
      entries,
    },
    null,
    2
  )
}

els.exportTranscript.addEventListener("click", () => {
  const run = state.run || {}
  download(
    "neev-browser-" + (run.runId || "run") + ".md",
    transcriptMarkdown(),
    "text/markdown"
  )
})

els.exportResults.addEventListener("click", () => {
  const run = state.run || {}
  download(
    "neev-browser-" + (run.runId || "run") + ".json",
    resultsJson(),
    "application/json"
  )
})

// If the tabs permission is already held, fill the picker straight away. This
// only reads — it never asks — so there is no gesture to lose.
chrome.permissions.contains({ permissions: ["tabs"] }).then((has) => {
  if (has) listTabs().catch(() => {})
})

renderState()
