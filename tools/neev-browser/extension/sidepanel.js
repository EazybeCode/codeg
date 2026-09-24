/**
 * The side panel.
 *
 * Everything rendered here — agent output, element names in approval prompts,
 * tab titles — is page- or model-derived, so every single insertion goes
 * through `textContent`. There is no `innerHTML` in this file and there must
 * never be one: a page that can put markup in its own `<h1>` would otherwise
 * be putting markup in the panel that approves actions against it.
 */

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

const port = chrome.runtime.connect({ name: "neev-panel" })

port.onMessage.addListener((message) => {
  if (!message) return
  if (message.kind === "sync") {
    state = message.state
    entries = message.log || []
    renderStream()
    renderState()
    return
  }
  if (message.kind === "state") {
    state = message.state
    renderState()
    return
  }
  if (message.kind === "log") {
    entries.push(message.entry)
    appendEntry(message.entry)
  }
})

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

async function loadTabs() {
  const hasTabs = await chrome.permissions.contains({ permissions: ["tabs"] })
  if (!hasTabs) {
    const granted = await chrome.permissions.request({ permissions: ["tabs"] })
    if (!granted) {
      showError(
        "Chrome needs the tabs permission to show you a list to pick from. " +
          "Without it Neev cannot tell you which tab is which."
      )
      return
    }
  }
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
  loadTabs().catch((error) => showError(String(error && error.message)))
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
      port.postMessage({
        kind: "panel.start",
        provider: selectedProvider(),
        model: els.model.value.trim() || null,
        task,
        tabId: tab.id,
      })
    })
    .catch((error) => showError(String(error && error.message)))
})

function selectedProvider() {
  const checked = els.provider.querySelector("input:checked")
  return checked ? checked.value : "claude"
}

els.stop.addEventListener("click", () => {
  port.postMessage({ kind: "panel.stop" })
})

els.approve.addEventListener("click", () => decide("approve"))
els.deny.addEventListener("click", () => decide("deny"))

function decide(decision) {
  if (!state.approval) return
  port.postMessage({
    kind: "panel.approval",
    id: state.approval.id,
    decision,
  })
  els.approvalCard.hidden = true
}

els.handoffDone.addEventListener("click", () => handoff("done"))
els.handoffSkip.addEventListener("click", () => handoff("skipped"))

function handoff(outcome) {
  if (!state.handoff) return
  port.postMessage({ kind: "panel.handoff", id: state.handoff.id, outcome })
  els.handoffCard.hidden = true
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

// If the tabs permission is already granted, fill the picker straight away.
chrome.permissions.contains({ permissions: ["tabs"] }).then((has) => {
  if (has) loadTabs().catch(() => {})
})

renderState()
