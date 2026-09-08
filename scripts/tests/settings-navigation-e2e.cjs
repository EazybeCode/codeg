/* eslint-disable @typescript-eslint/no-require-imports */
// Run after the frontend build and server-mode lib test build.
// External GitHub/provider behavior is provided by the isolated test fixture.
// Uses the same port as workspace-e2e.cjs; do not run both concurrently.

const assert = require("node:assert/strict")
const path = require("node:path")
const { spawn } = require("node:child_process")
const { createFixtureRoot } = require("./workspace-e2e-support.cjs")
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || "playwright")
const repo = path.resolve(__dirname, "../..")
assert(
  process.env.WORKOS_E2E_TEST_BINARY,
  "Set WORKOS_E2E_TEST_BINARY to the built lib test executable"
)
const root = createFixtureRoot(
  path.join(repo, ".superpowers/sdd/2026-09-08-settings-return")
)
const server = spawn(
  process.env.WORKOS_E2E_TEST_BINARY,
  ["workspace_browser_fixture_server", "--ignored", "--nocapture"],
  {
    cwd: path.join(repo, "src-tauri"),
    env: {
      ...process.env,
      WORKOS_E2E_ROOT: root,
      CLAUDE_CONFIG_DIR: path.join(root, "provider"),
      CODEG_HOME: root,
      CODEG_DATA_DIR: path.join(root, "managed"),
      CODEG_STATIC_DIR: path.join(repo, "out"),
      WORKOS_ENABLE_CATALOG_RUNTIME: "true",
    },
    stdio: ["ignore", "pipe", "pipe"],
  }
)
let log = "",
  browser,
  spawnError
server.once("error", (error) => {
  spawnError = error
})
server.stdout.on("data", (b) => (log += b))
server.stderr.on("data", (b) => (log += b))
;(async () => {
  try {
    for (let n = 0; ; n++) {
      if (spawnError) throw spawnError
      if (n > 100 || server.exitCode !== null)
        throw Error("Fixture failed " + log)
      try {
        const r = await fetch("http://localhost:3097/api/health", {
          method: "POST",
          headers: { Authorization: "Bearer workspace-e2e-installation" },
        })
        if (r.ok) break
      } catch {
        // Wait for this fixture's HTTP listener to start.
      }
      await new Promise((r) => setTimeout(r, 100))
    }
    browser = await chromium.launch({ headless: true })
    const ctx = await browser.newContext({
      viewport: { width: 1440, height: 960 },
    })
    ctx.setDefaultTimeout(10000)
    await ctx.addCookies([
      {
        name: "workos_session",
        value: "workspace-e2e-session",
        domain: "localhost",
        path: "/",
        httpOnly: true,
      },
    ])
    await ctx.addInitScript(() =>
      localStorage.setItem("codeg_token", "workspace-e2e-installation")
    )
    const page = await ctx.newPage()
    const errors = []
    page.on("pageerror", (e) => errors.push(e.message))
    const routes = [
      "appearance",
      "general",
      "agents",
      "model-providers",
      "mcp",
      "skills",
      "skill-packs",
      "quick-messages",
      "shortcuts",
      "version-control",
      "chat-channels",
      "logs",
      "system",
      "web-service",
      "experts",
      "science",
      "office-tools",
    ]
    for (const route of routes) {
      const response = await page.goto(
        "http://localhost:3097/settings/" + route + "?returnChatId=42"
      )
      assert.equal(response.status(), 200)
      const back = page.getByRole("link", {
        name: "Back to workspace",
        exact: true,
      })
      await back.waitFor()
      assert.equal(await back.getAttribute("href"), "/workspace?chatId=42")
    }
    console.log("PASS 17 settings routes expose chat-preserving return link")
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 800 })
      await page.goto("http://localhost:3097/settings/general")
      const back = page.getByRole("link", {
        name: "Back to workspace",
        exact: true,
      })
      await back.waitFor()
      const terminal = page
        .getByRole("heading", { name: "Default Terminal", exact: true })
        .locator("xpath=ancestor::section[1]")
      const description = await terminal.locator("p").boundingBox()
      const picker = await page
        .getByLabel("Default Terminal", { exact: true })
        .boundingBox()
      assert(description && picker)
      if (width < 640) {
        assert(
          description.width > 200,
          "Terminal description should use mobile card width; got " +
            description.width
        )
        assert(
          picker.y >= description.y + description.height,
          "Wide picker must stack below description on mobile"
        )
      }
      const box = await back.boundingBox()
      assert(box && box.x >= 0 && box.x + box.width <= width)
      await page
        .getByLabel("Default Terminal", { exact: true })
        .getByText("System default", { exact: true })
        .waitFor()
      await page.screenshot({
        path: path.join(root, "settings-" + width + ".png"),
      })
      await back.click()
      await page.waitForURL("**/workspace")
      console.log("PASS return navigation viewport " + width)
    }
    assert.deepEqual(errors, [])
    console.log("PASS no page exceptions; screenshots: " + root)
  } finally {
    try {
      if (browser) await browser.close()
    } finally {
      if (server.pid && server.exitCode === null) {
        const stopped = new Promise((resolve) => server.once("exit", resolve))
        server.kill("SIGTERM")
        await stopped
      }
    }
  }
})().catch((e) => {
  console.error(e)
  process.exitCode = 1
})
