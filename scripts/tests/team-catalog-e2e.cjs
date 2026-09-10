/* eslint-disable @typescript-eslint/no-require-imports */
// Exported Next UI -> isolated Axum router -> synthetic loopback GitHub proof.
// This is fixture evidence only and does not replace hosted/provider acceptance.

const assert = require("node:assert/strict")
const fs = require("node:fs")
const path = require("node:path")
const { spawn } = require("node:child_process")
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || "playwright")

const repo = path.resolve(__dirname, "../..")
const binary = process.env.WORKOS_E2E_TEST_BINARY
const artifactRoot = process.env.WORKOS_TEAM_E2E_ARTIFACTS
assert(
  binary && fs.statSync(binary).isFile(),
  "WORKOS_E2E_TEST_BINARY must be an emitted lib test executable"
)
assert(
  artifactRoot && path.isAbsolute(artifactRoot),
  "Set an absolute WORKOS_TEAM_E2E_ARTIFACTS"
)
if (fs.existsSync(artifactRoot)) {
  assert(
    fs.statSync(artifactRoot).isDirectory(),
    "artifact path must be a directory"
  )
  assert.equal(
    fs.readdirSync(artifactRoot).length,
    0,
    "artifact path must be empty"
  )
} else {
  fs.mkdirSync(artifactRoot, { recursive: true })
}
fs.writeFileSync(
  path.join(artifactRoot, "fixture-owned"),
  "team catalog fixture\n",
  { flag: "wx" }
)
const syntheticRuntime = path.join(artifactRoot, "synthetic-runtime")
fs.mkdirSync(syntheticRuntime)
fs.writeFileSync(
  path.join(syntheticRuntime, "fixture-owned"),
  "synthetic runtime\n",
  { flag: "wx" }
)

let fixture
let fixtureLog = ""
let browser

function wait(ms, label) {
  return new Promise((_, reject) =>
    setTimeout(() => reject(new Error(`Timed out waiting for ${label}`)), ms)
  )
}

async function startFixture() {
  fixture = spawn(
    binary,
    [
      "team_server::browser_fixture::team_browser_fixture_server",
      "--exact",
      "--ignored",
      "--nocapture",
    ],
    {
      cwd: path.join(repo, "src-tauri"),
      env: {
        ...process.env,
        WORKOS_TEAM_E2E_ROOT: artifactRoot,
        CODEG_STATIC_DIR: path.join(repo, "out"),
      },
      stdio: ["pipe", "pipe", "pipe"],
    }
  )
  return Promise.race([
    new Promise((resolve, reject) => {
      const receive = (chunk) => {
        fixtureLog += chunk.toString()
        const marker = fixtureLog
          .split(/\r?\n/)
          .find((line) => line.startsWith("TEAM_BROWSER_E2E_READY "))
        if (marker) {
          try {
            resolve(JSON.parse(marker.slice("TEAM_BROWSER_E2E_READY ".length)))
          } catch (error) {
            reject(error)
          }
        }
      }
      fixture.stdout.on("data", receive)
      fixture.stderr.on("data", receive)
      fixture.once("error", reject)
      fixture.once("exit", (code) =>
        reject(new Error(`Fixture exited ${code}\n${fixtureLog}`))
      )
    }),
    wait(30_000, "fixture startup"),
  ])
}

async function stopFixture() {
  if (!fixture || fixture.exitCode != null) return
  const exited = new Promise((resolve) => fixture.once("exit", resolve))
  fixture.stdin.end("shutdown\n")
  await Promise.race([exited, wait(10_000, "fixture shutdown")])
}

async function signIn(page, appOrigin, persona, organization) {
  await page.goto(`${appOrigin}/login`)
  await page.getByRole("link", { name: /GitHub/i }).click()
  await page.getByRole("heading", { name: "Fake GitHub consent" }).waitFor()
  await page.getByRole("button", { name: persona, exact: true }).click()
  await page.waitForURL(`${appOrigin}/onboarding`)
  const radio = page.getByRole("radio", {
    name: new RegExp(`^${organization}\\b`, "i"),
  })
  await radio.check()
  await page.getByRole("button", { name: "Continue", exact: true }).click()
  await page.waitForURL(`${appOrigin}/workspace`)
}

async function openActions(page, name) {
  await page
    .getByRole("button", { name: `Actions for ${name}`, exact: true })
    .click()
}

async function openMobileNavigation(page) {
  await page
    .getByRole("button", {
      name: "Open workspace navigation",
      exact: true,
    })
    .click()
}

async function createProject(page) {
  await page
    .getByRole("button", { name: "New project", exact: true })
    .first()
    .click()
  await page.getByLabel("Project name", { exact: true }).fill("Launch")
  await page
    .getByRole("button", { name: "Create project", exact: true })
    .click()
  await page.getByText("Launch", { exact: true }).first().waitFor()
}

async function createNestedCatalog(page) {
  await openActions(page, "Launch")
  await page.getByRole("menuitem", { name: "New folder", exact: true }).click()
  await page.getByLabel("Folder name", { exact: true }).fill("Planning")
  await page.getByRole("button", { name: "Create folder", exact: true }).click()
  await page.getByRole("button", { name: "Expand Launch", exact: true }).click()
  await page.getByText("Planning", { exact: true }).first().waitFor()

  await openActions(page, "Planning")
  await page
    .getByRole("menuitem", { name: "New working directory", exact: true })
    .click()
  await page
    .getByLabel("Working directory name", { exact: true })
    .fill("Web app")
  await page.getByLabel("Repository", { exact: true }).fill("atlas/repo")
  await page.getByLabel("Starting branch", { exact: true }).fill("main")
  await page
    .getByRole("button", { name: "Create working directory", exact: true })
    .click()
  await page
    .getByRole("button", { name: "Expand Planning", exact: true })
    .click()
  await page.getByText("Web app", { exact: true }).first().waitFor()

  await openActions(page, "Web app")
  await page.getByRole("menuitem", { name: "New chat", exact: true }).click()
  await page
    .getByText("New chats are private to you unless you share them later.", {
      exact: true,
    })
    .waitFor()
  await page
    .getByLabel("Chat title", { exact: true })
    .fill("Private launch plan")
  await page.getByRole("button", { name: "Create chat", exact: true }).click()
  await page
    .getByRole("heading", { name: "Private launch plan", exact: true })
    .waitFor()
}

async function reloadCatalog(page) {
  await page.reload()
  await openMobileNavigation(page)
  await page.getByText("Launch", { exact: true }).first().waitFor()
}

async function run() {
  const fixtureInfo = await startFixture()
  assert.equal(fixtureInfo.evidence, "fake-loopback-github")
  const { appOrigin } = fixtureInfo
  browser = await chromium.launch({ headless: true })
  const aliceContext = await browser.newContext({
    viewport: { width: 1440, height: 960 },
  })
  const bobContext = await browser.newContext({
    viewport: { width: 375, height: 812 },
  })
  const alice = await aliceContext.newPage()
  const bob = await bobContext.newPage()
  const diagnostics = []
  for (const [owner, page] of [
    ["alice", alice],
    ["bob", bob],
  ]) {
    page.on("pageerror", (error) =>
      diagnostics.push({ owner, type: "pageerror", text: error.message })
    )
    page.on("console", (entry) => {
      if (entry.type() === "error" || entry.type() === "warning")
        diagnostics.push({ owner, type: entry.type(), text: entry.text() })
    })
  }

  await signIn(alice, appOrigin, "Continue as Alice Atlas", "atlas-labs")
  await createProject(alice)
  await createNestedCatalog(alice)
  assert.equal(
    await alice.getByText("Private to you", { exact: true }).count(),
    1
  )
  await alice.screenshot({
    path: path.join(artifactRoot, "alice-private-desktop.png"),
    fullPage: true,
  })

  await signIn(bob, appOrigin, "Continue as Bob Birch", "atlas-labs")
  await openMobileNavigation(bob)
  await bob.getByText("Launch", { exact: true }).first().waitFor()
  assert.equal(
    await bob.getByText("Private launch plan", { exact: true }).count(),
    0
  )
  await bob.screenshot({
    path: path.join(artifactRoot, "bob-private-absent-mobile.png"),
    fullPage: true,
  })

  await alice.getByRole("button", { name: "Share", exact: true }).click()
  await alice.getByRole("radio", { name: "Organization", exact: true }).check()
  await alice.getByRole("button", { name: "Share", exact: true }).last().click()
  await reloadCatalog(bob)
  await openActions(bob, "Launch")
  await bob.keyboard.press("Escape")
  await bob.getByRole("button", { name: "Expand Launch", exact: true }).click()
  await bob
    .getByRole("button", { name: "Expand Planning", exact: true })
    .click()
  await bob.getByRole("button", { name: "Expand Web app", exact: true }).click()
  await bob.getByText("Private launch plan", { exact: true }).waitFor()
  await bob.getByText("Private launch plan", { exact: true }).click()
  assert.equal(
    await bob.getByRole("button", { name: "Share", exact: true }).count(),
    0
  )
  await bob.screenshot({
    path: path.join(artifactRoot, "bob-shared-mobile.png"),
    fullPage: true,
  })

  await alice.getByRole("button", { name: "Share", exact: true }).click()
  await alice
    .getByRole("radio", { name: "Private to you", exact: true })
    .check()
  await alice.getByRole("button", { name: "Share", exact: true }).last().click()
  await alice.getByRole("dialog").waitFor({ state: "hidden" })
  await alice.getByText("Private to you", { exact: true }).waitFor()
  await alice.screenshot({
    path: path.join(artifactRoot, "alice-private-restored-desktop.png"),
    fullPage: true,
  })
  await bob.goto(`${appOrigin}/workspace`)
  await openMobileNavigation(bob)
  await bob.getByText("Launch", { exact: true }).first().waitFor()
  assert.equal(
    await bob.getByText("Private launch plan", { exact: true }).count(),
    0
  )

  await alice.getByRole("link", { name: "Settings", exact: true }).click()
  await alice.waitForURL(`${appOrigin}/settings`)
  await alice
    .getByRole("heading", { name: "Organization settings", exact: true })
    .waitFor()
  await alice
    .getByRole("link", { name: "Back to workspace", exact: true })
    .click()
  await alice.waitForURL(`${appOrigin}/workspace`)
  await alice.getByText("Launch", { exact: true }).first().waitFor()
  await alice
    .getByText("Loading workspace…", { exact: true })
    .waitFor({ state: "hidden" })
  await alice.screenshot({
    path: path.join(artifactRoot, "alice-settings-return-desktop.png"),
    fullPage: true,
  })

  const result = diagnostics.length === 0 ? "PASS" : "FAIL"
  fs.writeFileSync(
    path.join(artifactRoot, "evidence.json"),
    JSON.stringify(
      {
        result,
        evidence: "synthetic loopback GitHub only",
        journey: [
          "nested hierarchy",
          "private absent for colleague",
          "organization visible",
          "private revoked",
          "settings return",
        ],
        screenshots: [
          "alice-private-desktop.png",
          "bob-private-absent-mobile.png",
          "bob-shared-mobile.png",
          "alice-private-restored-desktop.png",
          "alice-settings-return-desktop.png",
        ],
        browserConsoleErrors: diagnostics,
      },
      null,
      2
    ) + "\n"
  )
  assert.deepEqual(diagnostics, [], "browser console must be clean")
}

run()
  .then(async () => {
    await browser?.close()
    await stopFixture()
    console.log(`Team catalog browser journey PASS: ${artifactRoot}`)
  })
  .catch(async (error) => {
    console.error(error)
    console.error(fixtureLog)
    try {
      await browser?.close()
    } catch {}
    try {
      await stopFixture()
    } catch {}
    process.exitCode = 1
  })
