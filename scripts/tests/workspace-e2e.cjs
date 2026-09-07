/* eslint-disable @typescript-eslint/no-require-imports */
// Run after cargo test --release ... --lib workspace_browser_fixture_server --no-run.
// WORKOS_E2E_TEST_BINARY must name that cfg(test) executable. No browser routes
// are mocked: exported UI, HTTP, WS, DB, Git, Files and native parser are real.
const assert = require("node:assert/strict")
const fs = require("node:fs")
const path = require("node:path")
const { spawn } = require("node:child_process")
const {
  createFixtureRoot,
  assertDiagnostics,
} = require("./workspace-e2e-support.cjs")
let chromium
try {
  ;({ chromium } = require(process.env.PLAYWRIGHT_MODULE || "playwright"))
} catch {
  throw new Error(
    "Playwright is unavailable. Set PLAYWRIGHT_MODULE to an existing Playwright module path; this driver does not install dependencies."
  )
}
const repository = path.resolve(__dirname, "../..")
const scratch = path.join(
  repository,
  ".superpowers/sdd/2026-09-07-workspace-execution-ui"
)
const root = createFixtureRoot(scratch, process.env.WORKOS_E2E_ROOT)
const origin = "http://localhost:3097"
const token = "workspace-e2e-installation"
let server,
  browser,
  diagnostics = {},
  phase = "happy-path"
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
async function start() {
  assert(
    process.env.WORKOS_E2E_TEST_BINARY,
    "Set WORKOS_E2E_TEST_BINARY to the compiled lib test executable"
  )
  const log = fs.openSync(path.join(root, "server.log"), "a")
  server = spawn(
    process.env.WORKOS_E2E_TEST_BINARY,
    ["workspace_browser_fixture_server", "--ignored", "--nocapture"],
    {
      cwd: path.join(repository, "src-tauri"),
      stdio: ["ignore", log, log],
      env: {
        ...process.env,
        WORKOS_E2E_ROOT: root,
        CLAUDE_CONFIG_DIR: path.join(root, "provider"),
        CODEG_HOME: root,
        CODEG_DATA_DIR: path.join(root, "managed"),
        CODEG_STATIC_DIR: path.join(repository, "out"),
        WORKOS_ENABLE_CATALOG_RUNTIME: "true",
      },
    }
  )
  fs.closeSync(log)
  let spawnError
  server.once("error", (error) => {
    spawnError = error
  })
  for (let i = 0; i < 100; i++) {
    if (spawnError) throw spawnError
    if (server.exitCode !== null)
      throw Error(
        `Fixture exited: ${fs.readFileSync(path.join(root, "server.log"), "utf8")}`
      )
    try {
      const response = await fetch(`${origin}/api/health`, {
        method: "POST",
        headers: { Authorization: `Bearer ${token}` },
      })
      if (response.ok) return
    } catch {
      /* startup */
    }
    await pause(100)
  }
  throw Error("Fixture startup timed out")
}
async function stop() {
  if (!server?.pid || server.exitCode !== null) return
  const stopped = new Promise((resolve) => server.once("exit", resolve))
  server.kill("SIGTERM")
  await stopped
}
async function call(page, name, body = {}, expected = 200) {
  const result = await page.evaluate(
    async ({ origin, name, body, token }) => {
      const response = await fetch(`${origin}/api/${name}`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${token}`,
        },
        body: JSON.stringify(body),
      })
      return { status: response.status, data: await response.json() }
    },
    { origin, name, body, token }
  )
  assert.equal(
    result.status,
    expected,
    `${name}: ${JSON.stringify(result.data)}`
  )
  return result.data
}
async function action(page, name, menu) {
  await page
    .getByRole("button", { name: `Actions for ${name}`, exact: true })
    .click()
  await page.getByRole("menuitem", { name: menu, exact: true }).click()
}
async function createChat(page, title, testReauth = false) {
  for (const name of ["Website refresh", "Launch planning"]) {
    const expand = page.getByRole("button", {
      name: `Expand ${name}`,
      exact: true,
    })
    if (await expand.isVisible()) await expand.click()
  }
  await action(page, "Campaign site", "New chat")
  await page.getByLabel("Chat title").fill(title)
  if (testReauth) {
    fs.writeFileSync(path.join(root, "external-access"), "reauth")
    const denied = page.waitForResponse((r) =>
      r.url().endsWith("/api/create_workspace_chat_draft")
    )
    await page.getByRole("button", { name: "Create chat", exact: true }).click()
    const failedRequest = await denied
    assert.equal(failedRequest.status(), 401)
    await page
      .getByRole("link", { name: "Reconnect GitHub", exact: true })
      .waitFor()
    assert.equal(await page.getByLabel("Chat title").inputValue(), title)
    fs.unlinkSync(path.join(root, "external-access"))
    phase = "happy-path:github-reauth-retry"
    const accepted = page.waitForResponse((r) =>
      r.url().endsWith("/api/create_workspace_chat_draft")
    )
    await page.getByRole("button", { name: "Create chat", exact: true }).click()
    const retried = await accepted
    assert.equal(retried.status(), 200)
    assert.deepEqual(
      retried.request().postDataJSON(),
      failedRequest.request().postDataJSON(),
      "Unchanged form retry retains idempotency request ID"
    )
    console.log(
      "PASS real credential failure shows reconnect, retains form, retries identical request"
    )
  } else {
    await page.getByRole("button", { name: "Create chat", exact: true }).click()
  }
  await page.getByRole("dialog").waitFor({ state: "hidden" })
  await page.waitForFunction(() =>
    new URL(location.href).searchParams.has("chatId")
  )
  return Number(new URL(page.url()).searchParams.get("chatId"))
}
async function prepare(page, draftId, liveCount = 0) {
  await page.getByRole("button", { name: /Prepare/ }).click()
  await page
    .getByText("Unpublished", { exact: true })
    .filter({ visible: true })
    .waitFor()
  const runtime = await call(page, "get_workspace_chat_runtime", { draftId })
  assert.equal(runtime.state, "ready")
  assert.equal(runtime.conversation.folder_id, runtime.folder.id)
  assert.equal(runtime.branch_name, `workos/chat/${draftId}`)
  assert(runtime.folder.path.startsWith(path.join(root, "managed")))
  assert.equal(
    (await call(page, "acp_list_connections")).length,
    liveCount,
    "Viewing ready must not start a provider"
  )
  return runtime
}
async function panelGeometry(page) {
  await page.evaluate(() => {
    localStorage.setItem(
      "workspace:left-sidebar",
      JSON.stringify({ isOpen: true, width: 380 })
    )
    localStorage.setItem(
      "workspace:right-sidebar",
      JSON.stringify({ isOpen: true, width: 280 })
    )
  })
  await page.reload()
  const panel = (id) => page.locator(`[data-panel-id="${id}"]`)
  const width = (id) =>
    panel(id).evaluate((e) => e.getBoundingClientRect().width)
  async function near(id, expected) {
    await page.waitForFunction(
      ({ id, expected }) =>
        Math.abs(
          (document
            .querySelector(`[data-panel-id="${id}"]`)
            ?.getBoundingClientRect().width ?? NaN) - expected
        ) < 3,
      { id, expected }
    )
  }
  await near("folder-shell-left-panel", 380)
  await near("folder-shell-right-panel", 280)
  assert.equal(
    await panel("folder-main-terminal-panel").getAttribute("data-panel-size"),
    "0.0"
  )
  await page.getByTitle(/^Hide Sidebar/).click()
  await near("folder-shell-left-panel", 0)
  await page.getByTitle(/^Show Sidebar/).click()
  await near("folder-shell-left-panel", 380)
  await page.getByTitle(/^Toggle Auxiliary Panel/).click()
  await near("folder-shell-right-panel", 0)
  await page.getByTitle(/^Toggle Auxiliary Panel/).click()
  await near("folder-shell-right-panel", 280)
  await page.setViewportSize({ width: 800, height: 700 })
  await pause(350)
  assert((await width("folder-shell-main-panel")) >= 410)
  assert((await width("folder-shell-left-panel")) > 0)
  assert((await width("folder-shell-right-panel")) > 0)
  await page.setViewportSize({ width: 1440, height: 960 })
  await near("folder-shell-left-panel", 380)
  await near("folder-shell-right-panel", 280)
  const handle = page
    .locator(
      '[data-panel-group-id="folder-shell-group"][data-panel-resize-handle-id]'
    )
    .first()
  const box = await handle.boundingBox()
  assert(box)
  await page.mouse.move(box.x + box.width / 2, box.y + 160)
  await page.mouse.down()
  await page.mouse.move(box.x + 40, box.y + 160, { steps: 8 })
  await page.mouse.up()
  const dragged = await width("folder-shell-left-panel")
  assert(dragged > 400, "Dragging resizes the panel")
  await page.reload()
  await near("folder-shell-left-panel", dragged)
  const keyboardHandle = page
    .locator(
      '[data-panel-group-id="folder-shell-group"][data-panel-resize-handle-id]'
    )
    .first()
  await keyboardHandle.focus()
  await keyboardHandle.press("ArrowRight")
  await pause(150)
  const keyed = await width("folder-shell-left-panel")
  assert(keyed > dragged + 5, "Keyboard resizing changes the panel")
  await page.reload()
  await near("folder-shell-left-panel", keyed)
  console.log("PASS drag and keyboard resize preferences survive reload")
  console.log(
    "PASS saved panel widths, collapse/reopen, narrow viewport and closed terminal"
  )
}
async function run() {
  console.log("Fixture artifacts:", root)
  await start()
  browser = await chromium.launch({ headless: true })
  const context = await browser.newContext({
    viewport: { width: 1440, height: 960 },
  })
  await context.addCookies([
    {
      name: "workos_session",
      value: "workspace-e2e-session",
      domain: "localhost",
      path: "/",
      httpOnly: true,
    },
  ])
  const page = await context.newPage()
  const exceptions = [],
    consoleErrors = [],
    requests = [],
    frames = []
  diagnostics = { exceptions, consoleErrors, requests, frames }
  page.on("pageerror", (e) => exceptions.push(e.message))
  page.on("console", (e) => {
    if (["error", "warning"].includes(e.type()))
      consoleErrors.push({ phase, type: e.type(), text: e.text() })
  })
  page.on("response", (r) => {
    if (new URL(r.url()).pathname.startsWith("/api/"))
      requests.push({
        path: new URL(r.url()).pathname,
        status: r.status(),
        body: r.request().postDataJSON(),
      })
  })
  page.on("websocket", (ws) =>
    ws.on("framereceived", (event) => {
      try {
        frames.push(JSON.parse(event.payload))
      } catch {
        /* binary */
      }
    })
  )
  await page.goto(`${origin}/login.html`)
  await page.locator('input[type="password"]').fill(token)
  await page.getByRole("button", { name: /connect/i }).click()
  await page
    .getByText("Fixture Organization", { exact: true })
    .first()
    .waitFor()
  await page
    .getByRole("button", { name: "New project", exact: true })
    .first()
    .click()
  await page.getByLabel("Project name").fill("Website refresh")
  await page
    .getByRole("button", { name: "Create project", exact: true })
    .click()
  await action(page, "Website refresh", "New folder")
  await page.getByLabel("Folder name").fill("Launch planning")
  await page.getByRole("button", { name: "Create folder", exact: true }).click()
  await action(page, "Launch planning", "New working directory")
  await page.getByLabel("Working directory name").fill("Campaign site")
  await page.getByLabel("Repository", { exact: true }).fill("fixture-org/docs")
  await page.getByLabel("Starting branch").fill("main")
  await page
    .getByRole("button", { name: "Create working directory", exact: true })
    .click()
  // An unprepared draft keeps draft IDs distinct from legacy conversation IDs.
  await createChat(page, "Unprepared intent")
  const interrupted = await createChat(page, "Interrupted preparation")
  fs.writeFileSync(
    path.join(root, "interrupt-provision"),
    "pause after worktree allocation"
  )
  await page.getByRole("button", { name: /Prepare/ }).click()
  for (
    let i = 0;
    i < 100 && !fs.existsSync(path.join(root, "interrupted-allocation.json"));
    i++
  )
    await pause(100)
  const reserved = JSON.parse(
    fs.readFileSync(path.join(root, "interrupted-allocation.json"), "utf8")
  )
  await page.goto("about:blank")
  await stop()
  fs.unlinkSync(path.join(root, "interrupt-provision"))
  await start()
  await page.goto(`${origin}/workspace?chatId=${interrupted}`)
  const recovered = await prepare(page, interrupted)
  assert.equal(recovered.folder.path, reserved.path)
  assert.equal(recovered.reserved_agent_type, "claude_code")
  const recoveredAllocation = JSON.parse(
    fs.readFileSync(path.join(root, "last-allocation.json"), "utf8")
  )
  assert.equal(recoveredAllocation.allocation_id, reserved.allocation_id)
  console.log(
    "PASS crash/restart preparation resumes the original reservation and worktree through the launcher"
  )
  const a = await createChat(page, "Alpha draft")
  const first = await prepare(page, a)
  assert.notEqual(first.draft.id, first.conversation.id)
  fs.writeFileSync(
    path.join(root, "hold-turn"),
    "keep the fixture turn open across reload"
  )
  // This HTTP request remains open while the fixture produces a live turn.
  const liveEditor = page.locator('[contenteditable="true"]:visible').last()
  await liveEditor.fill("Alpha independent content")
  await liveEditor.press("Enter")
  await page
    .locator("#workspace-conversation-panel")
    .getByText("Generated Markdown", { exact: true })
    .waitFor()
  assert.equal(
    fs.readFileSync(path.join(first.folder.path, "generated.md"), "utf8"),
    "# Generated Markdown\n\nAlpha independent content\n"
  )
  await page.getByRole("button", { name: "Files", exact: true }).click()
  await page.getByText("generated.md", { exact: true }).first().click()
  await page
    .locator("#workspace-files-panel")
    .getByText("Alpha independent content", { exact: true })
    .waitFor()
  await page.screenshot({ path: path.join(root, "alpha-files.png") })
  fs.writeFileSync(
    path.join(root, "browser-progress.json"),
    JSON.stringify(
      { first, exceptions, consoleErrors, requests, frames },
      null,
      2
    )
  )
  assert.deepEqual(exceptions, [])
  assert.deepEqual(consoleErrors, [])
  console.log(
    "PASS initial UI -> HTTP -> Git -> WS prompt -> Markdown -> Files"
  )
  const firstConnection = (
    await call(page, "acp_find_connection_for_conversation", {
      conversationId: first.conversation.id,
      workspaceChatId: a,
      agentType: "claude_code",
    })
  ).connection_id
  assert(firstConnection)
  phase = "expected-failure:second-writer-busy"
  const busy = await call(
    page,
    "acp_connect",
    {
      workspaceChatId: a,
      conversationId: first.conversation.id,
      folderId: first.folder.id,
      workingDir: first.folder.path,
      agentType: "claude_code",
    },
    409
  )
  assert.equal(busy.error, "busy")
  console.log("PASS second writer Busy; existing writer retained")
  await pause(100) // Flush the browser's intentional HTTP409 diagnostic.
  phase = "happy-path:reload-and-resize"
  const processEvents = fs.readFileSync(
    path.join(root, "process-events.log"),
    "utf8"
  )
  const discoveryStart = requests.length
  const frameStart = frames.length
  await page.goto(`${origin}/workspace?chatId=${a}`)
  await page
    .locator("#workspace-conversation-panel")
    .getByText("Alpha independent content", { exact: true })
    .last()
    .waitFor()
  for (
    let i = 0;
    i < 100 &&
    !requests
      .slice(discoveryStart)
      .some(
        (r) =>
          r.path === "/api/acp_find_connection_for_conversation" &&
          r.body.workspaceChatId === a &&
          r.body.conversationId === first.conversation.id
      );
    i++
  )
    await pause(100)
  assert(
    requests
      .slice(discoveryStart)
      .some(
        (r) =>
          r.path === "/api/acp_find_connection_for_conversation" &&
          r.status === 200 &&
          r.body.workspaceChatId === a &&
          r.body.conversationId === first.conversation.id
      ),
    "Application must initiate scoped discovery on reload"
  )
  fs.unlinkSync(path.join(root, "hold-turn"))
  await page
    .locator("#workspace-conversation-panel")
    .getByText("Live output after reload", { exact: true })
    .waitFor()
  assert(
    frames
      .slice(frameStart)
      .some(
        (frame) =>
          frame.envelope?.type === "content_delta" &&
          frame.envelope.text.includes("Live output after reload")
      ),
    "New live WS content must reach the fresh client"
  )
  assert.equal(
    fs.readFileSync(path.join(root, "process-events.log"), "utf8"),
    processEvents,
    "Reload neither spawns nor stops a writer"
  )
  assert.equal((await call(page, "acp_list_connections")).length, 1)
  console.log(
    "PASS application reload attaches and receives new live output without Send, spawn or stop"
  )
  await panelGeometry(page)
  phase = "expected-failure:github-reauth-and-retry"
  const b = await createChat(page, "Beta draft", true)
  phase = "happy-path:second-runtime"
  const second = await prepare(page, b, 1)
  assert.notEqual(second.folder.path, first.folder.path)
  assert(!fs.existsSync(path.join(second.folder.path, "generated.md")))
  fs.writeFileSync(
    path.join(root, "provider-fail-start"),
    "fail external spawn"
  )
  phase = "expected-failure:provider-spawn-and-retry"
  const editor = page.locator('[contenteditable="true"]:visible').last()
  await editor.fill("Beta independent content")
  await editor.press("Enter")
  await page
    .getByRole("button", { name: "Retry retained draft", exact: true })
    .waitFor({ timeout: 15000 })
  assert.equal(await editor.innerText(), "Beta independent content")
  assert.equal((await call(page, "acp_list_connections")).length, 1)
  assert(!fs.existsSync(path.join(second.folder.path, "generated.md")))
  fs.unlinkSync(path.join(root, "provider-fail-start"))
  phase = "happy-path:provider-spawn-retry"
  const retryAccepted = page.waitForResponse((response) =>
    response.url().endsWith("/api/acp_prompt")
  )
  await page
    .getByRole("button", { name: "Retry retained draft", exact: true })
    .click()
  const retryResponse = await retryAccepted
  assert.equal(retryResponse.status(), 200)
  await retryResponse.finished()
  await page
    .locator("#workspace-conversation-panel")
    .getByRole("heading", { name: "Generated Markdown", exact: true })
    .first()
    .waitFor()
  for (
    let i = 0;
    i < 100 && !fs.existsSync(path.join(second.folder.path, "generated.md"));
    i++
  )
    await pause(50)
  assert.equal(
    fs.readFileSync(path.join(second.folder.path, "generated.md"), "utf8"),
    "# Generated Markdown\n\nBeta independent content\n"
  )
  assert.equal(
    fs.readFileSync(path.join(first.folder.path, "generated.md"), "utf8"),
    "# Generated Markdown\n\nAlpha independent content\n"
  )
  console.log(
    "PASS failed first connect retained draft; explicit retry; two isolated file contents"
  )
  phase = "happy-path:history"
  await page.goto(`${origin}/workspace?chatId=${a}`)
  await page
    .locator("#workspace-conversation-panel")
    .getByText("Alpha independent content", { exact: true })
    .last()
    .waitFor()
  assert.equal(
    (
      await call(page, "acp_find_connection_for_conversation", {
        conversationId: first.conversation.id,
        workspaceChatId: a,
        agentType: "claude_code",
      })
    ).connection_id,
    firstConnection
  )
  assert.equal((await call(page, "acp_list_connections")).length, 2)
  assert.equal(new URL(page.url()).searchParams.get("chatId"), String(a))
  const firstDetail = await call(page, "get_folder_conversation", {
    conversationId: first.conversation.id,
  })
  assert(
    JSON.stringify(firstDetail.turns).includes("Alpha independent content")
  )
  assert(
    !JSON.stringify(firstDetail.turns).includes("Beta independent content")
  )
  // A changed external GitHub credential produces the real scoped reconnect
  // contract while the actual composer keeps the rejected draft and history.
  fs.writeFileSync(path.join(root, "external-access"), "reauth")
  phase = "expected-failure:revoked-access"
  const activeEditor = page.locator('[contenteditable="true"]:visible').last()
  await activeEditor.fill("Retain this rejected follow-up")
  await activeEditor.press("Enter")
  await page
    .getByRole("button", { name: "Retry retained draft", exact: true })
    .waitFor()
  assert.equal(await activeEditor.innerText(), "Retain this rejected follow-up")
  assert.equal(
    fs.readFileSync(path.join(first.folder.path, "generated.md"), "utf8"),
    "# Generated Markdown\n\nAlpha independent content\n"
  )
  fs.unlinkSync(path.join(root, "external-access"))
  console.log(
    "PASS deep-link reload reuses writer; revoked access retains prompt and original history"
  )
  // Disconnect positively through the actual scoped handler before process
  // restart. Unknown surviving/foreign writer leases are intentionally Busy.
  for (const runtime of [first, second]) {
    const owner = await call(page, "acp_find_connection_for_conversation", {
      conversationId: runtime.conversation.id,
      workspaceChatId: runtime.draft.id,
      agentType: "claude_code",
    })
    await call(page, "acp_disconnect", {
      connectionId: owner.connection_id,
      workspaceChatId: runtime.draft.id,
    })
  }
  await page.goto("about:blank")
  await stop()
  await start()
  phase = "happy-path:backend-restart"
  await page.goto(`${origin}/workspace?chatId=${b}`)
  await page
    .locator("#workspace-conversation-panel")
    .getByText("Beta independent content", { exact: true })
    .last()
    .waitFor({ timeout: 15000 })
  for (const runtime of [first, second]) {
    const retained = await call(page, "get_workspace_chat_runtime", {
      draftId: runtime.draft.id,
    })
    assert.equal(retained.folder.id, runtime.folder.id)
    assert.equal(retained.conversation.id, runtime.conversation.id)
    assert.equal(retained.folder.path, runtime.folder.path)
    assert.equal(retained.starting_commit, runtime.starting_commit)
    assert.equal(retained.branch_name, runtime.branch_name)
    const inverse = await call(
      page,
      "get_workspace_chat_runtime_for_conversation",
      { conversationId: runtime.conversation.id }
    )
    assert.equal(inverse.draft.id, runtime.draft.id)
  }
  assert.equal(
    (await call(page, "acp_list_connections")).length,
    0,
    "Restarted view never spawns"
  )
  const detail = await call(page, "get_folder_conversation", {
    conversationId: second.conversation.id,
  })
  assert(JSON.stringify(detail.turns).includes("Beta independent content"))
  assert(!JSON.stringify(detail.turns).includes("Alpha independent content"))
  await page.getByRole("button", { name: "Files", exact: true }).click()
  await page.getByText("generated.md", { exact: true }).last().click()
  await page
    .locator("#workspace-files-panel")
    .getByText("Beta independent content", { exact: true })
    .waitFor()
  await page.screenshot({ path: path.join(root, "restart-beta-files.png") })
  assert.deepEqual(exceptions, [])
  assertDiagnostics(exceptions, consoleErrors, {
    "expected-failure:second-writer-busy": [
      "Failed to load resource: the server responded with a status of 409 (Conflict)",
    ],
    "expected-failure:github-reauth-and-retry": [
      "Failed to load resource: the server responded with a status of 401 (Unauthorized)",
    ],
    "expected-failure:provider-spawn-and-retry": [
      "Failed to load resource: the server responded with a status of 500 (Internal Server Error)",
      "[ConnLifecycle] sendPrompt: {error: failed}",
    ],
    "expected-failure:revoked-access": [
      "Failed to load resource: the server responded with a status of 401 (Unauthorized)",
      "[ConnLifecycle] sendPrompt: {error: reauth_required}",
    ],
  })
  assert(
    frames.some(
      (f) =>
        f.envelope?.type === "user_message" &&
        f.envelope.message_id.startsWith("optimistic-")
    )
  )
  assert(
    frames.some(
      (f) =>
        f.envelope?.type === "content_delta" &&
        f.envelope.text.includes("Alpha independent content")
    )
  )
  assert(frames.some((f) => f.envelope?.type === "turn_complete"))
  fs.writeFileSync(
    path.join(root, "acceptance.json"),
    JSON.stringify(
      {
        first,
        second,
        firstDetail,
        detail,
        assertions:
          "actual app/HTTP/WS/DB/Git/Files; isolated external fixtures",
      },
      null,
      2
    )
  )
  console.log(
    "PASS backend restart retains mappings, unpublished files and provider-native parsed history; no eager writer"
  )
}
run()
  .catch(async (e) => {
    console.error(e)
    if (browser) {
      const page = browser.contexts()[0]?.pages()[0]
      if (page) {
        console.error((await page.locator("body").innerText()).slice(-9000))
        await page.screenshot({ path: path.join(root, "failure.png") })
      }
    }
    process.exitCode = 1
  })
  .finally(async () => {
    fs.writeFileSync(
      path.join(root, "diagnostics.json"),
      JSON.stringify(diagnostics, null, 2)
    )
    if (browser) await browser.close()
    await stop()
  })
