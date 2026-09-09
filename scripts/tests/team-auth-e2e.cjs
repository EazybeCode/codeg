/* eslint-disable @typescript-eslint/no-require-imports */
// Actual browser -> exported Next shell -> isolated Axum team router acceptance.
// All GitHub evidence is invented and served from a controlled loopback origin.

const assert = require("node:assert/strict")
const fs = require("node:fs")
const http = require("node:http")
const path = require("node:path")
const { spawn } = require("node:child_process")
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || "playwright")

const repo = path.resolve(__dirname, "../..")
const planWorkspace = path.join(
  repo,
  ".superpowers/sdd/2026-09-08-team-http-onboarding"
)
const binary = process.env.WORKOS_E2E_TEST_BINARY
assert(binary, "Set WORKOS_E2E_TEST_BINARY to the emitted lib test executable")
assert(fs.statSync(binary).isFile(), "WORKOS_E2E_TEST_BINARY must be a file")

function createArtifactRoot() {
  const explicit = process.env.WORKOS_TEAM_E2E_ARTIFACTS
  if (explicit) {
    assert(path.isAbsolute(explicit), "artifact path must be absolute")
    if (fs.existsSync(explicit)) {
      assert(fs.statSync(explicit).isDirectory(), "artifact path must be a directory")
      assert.equal(fs.readdirSync(explicit).length, 0, "artifact path must be empty")
    } else {
      fs.mkdirSync(explicit, { recursive: true })
    }
    return explicit
  }
  const parent = path.join(planWorkspace, "artifacts")
  fs.mkdirSync(parent, { recursive: true })
  return fs.mkdtempSync(path.join(parent, "team-auth-e2e-"))
}

const artifactRoot = createArtifactRoot()
fs.writeFileSync(path.join(artifactRoot, "fixture-owned"), "team browser artifacts\n", {
  flag: "wx",
})
const syntheticRuntime = path.join(artifactRoot, "synthetic-runtime")
fs.mkdirSync(syntheticRuntime)
fs.writeFileSync(
  path.join(syntheticRuntime, "fixture-owned"),
  "team browser synthetic runtime\n",
  { flag: "wx" }
)

let fixture
let browser
let fixtureLog = ""
let fixtureExited = false
let ready
const allDiagnostics = []
const allContexts = []
const screenshots = []
const teamProjectionChecks = []
const observedTeamCookieValues = new Set()

function timeout(ms, label) {
  return new Promise((_, reject) =>
    setTimeout(() => reject(new Error(`Timed out waiting for ${label}`)), ms)
  )
}

function startFixture() {
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
  let readyResolve
  let readyReject
  ready = new Promise((resolve, reject) => {
    readyResolve = resolve
    readyReject = reject
  })
  const receive = (buffer) => {
    fixtureLog += buffer.toString()
    const marker = "TEAM_BROWSER_E2E_READY "
    const line = fixtureLog
      .split(/\r?\n/)
      .find((candidate) => candidate.startsWith(marker))
    if (!line) return
    try {
      readyResolve(JSON.parse(line.slice(marker.length)))
    } catch (error) {
      readyReject(error)
    }
  }
  fixture.stdout.on("data", receive)
  fixture.stderr.on("data", receive)
  fixture.once("error", readyReject)
  fixture.once("exit", (code, signal) => {
    fixtureExited = true
    if (!fixtureLog.includes("TEAM_BROWSER_E2E_READY ")) {
      readyReject(
        new Error(`Fixture exited before ready (${code}/${signal})\n${fixtureLog}`)
      )
    }
  })
}

async function stopFixture() {
  if (!fixture || fixtureExited) return
  const exited = new Promise((resolve) => fixture.once("exit", resolve))
  fixture.stdin.end("shutdown\n")
  try {
    await Promise.race([exited, timeout(10_000, "fixture shutdown")])
  } catch (error) {
    fixture.kill("SIGTERM")
    try {
      await Promise.race([exited, timeout(5_000, "fixture SIGTERM")])
    } catch {
      fixture.kill("SIGKILL")
      await exited
    }
    throw error
  }
}

async function control(providerOrigin, route) {
  const response = await fetch(`${providerOrigin}${route}`, { method: "POST" })
  assert.equal(response.status, 204, `control ${route}`)
}

async function status(origin) {
  const response = await fetch(`${origin}/fixture/status`, { cache: "no-store" })
  assert.equal(response.status, 200)
  return response.json()
}

async function poll(predicate, label, attempts = 100) {
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    const value = await predicate()
    if (value) return value
    await new Promise((resolve) => setTimeout(resolve, 50))
  }
  throw new Error(`Timed out polling ${label}`)
}

async function newTrackedContext(label, appOrigin, canaryOrigin, viewport) {
  const context = await browser.newContext({ viewport })
  const diagnostics = {
    label,
    pageErrors: [],
    consoleDiagnostics: [],
    failedRequests: [],
    websockets: [],
    apiResponses: [],
    httpErrors: [],
    remoteDescriptorProbes: 0,
    remoteLookupAttempts: 0,
  }
  const remoteDescriptor = {
    id: 777,
    name: "Controlled legacy remote",
    base_url: canaryOrigin,
    token: "remote-operator-canary-secret",
    headers: [],
    sort_order: 0,
    created_at: "2026-09-09T00:00:00Z",
    updated_at: "2026-09-09T00:00:00Z",
  }
  await context.route("**/api/get_remote_workspace_connection", async (route) => {
    const request = route.request()
    const positiveProbe =
      request.headers()["x-workos-e2e-remote-probe"] === "descriptor"
    if (positiveProbe) {
      diagnostics.remoteDescriptorProbes += 1
    } else {
      diagnostics.remoteLookupAttempts += 1
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      headers: { "Cache-Control": "no-store" },
      body: JSON.stringify(remoteDescriptor),
    })
  })
  await context.addInitScript(
    ({ canary }) => {
      localStorage.setItem("codeg_token", "operator-canary-secret")
      localStorage.setItem("codeg-custom-css-enabled", "1")
      localStorage.setItem(
        "codeg-custom-css",
        `body{background-image:url("${canary}/custom-css-canary")}`
      )
      window.__teamBroadcastMessages = []
      window.__teamWebSocketAttempts = []
      if (typeof BroadcastChannel !== "undefined") {
        const NativeChannel = BroadcastChannel
        window.BroadcastChannel = class TrackingBroadcastChannel extends NativeChannel {
          postMessage(value) {
            window.__teamBroadcastMessages.push(value)
            return super.postMessage(value)
          }
        }
      }
      const NativeWebSocket = WebSocket
      window.WebSocket = class TrackingWebSocket extends NativeWebSocket {
        constructor(url, protocols) {
          window.__teamWebSocketAttempts.push(String(url))
          super(url, protocols)
        }
      }
    },
    { canary: canaryOrigin }
  )
  allContexts.push({ context, diagnostics })
  allDiagnostics.push(diagnostics)
  return { context, diagnostics, appOrigin, label }
}

function trackPage(owner, page, name) {
  const diagnostics = owner.diagnostics
  let phase = `${owner.label}:${name}:initial`
  page.on("pageerror", (error) =>
    diagnostics.pageErrors.push({ phase, message: error.message })
  )
  page.on("console", (entry) => {
    if (entry.type() === "error" || entry.type() === "warning") {
      diagnostics.consoleDiagnostics.push({
        phase,
        type: entry.type(),
        text: entry.text(),
        location: safePath(entry.location().url),
      })
    }
  })
  page.on("requestfailed", (request) =>
    diagnostics.failedRequests.push({
      phase,
      path: safePath(request.url()),
      method: request.method(),
      error: request.failure()?.errorText || "unknown",
    })
  )
  page.on("websocket", (socket) => diagnostics.websockets.push(safePath(socket.url())))
  page.on("response", (response) => {
    const url = new URL(response.url())
    if (response.status() >= 400) {
      diagnostics.httpErrors.push({
        phase,
        path: `${url.origin}${url.pathname}`,
        method: response.request().method(),
        status: response.status(),
      })
    }
    if (url.origin === owner.appOrigin && url.pathname.startsWith("/api/")) {
      diagnostics.apiResponses.push({
        phase,
        path: url.pathname,
        method: response.request().method(),
        status: response.status(),
        cacheControl: response.headers()["cache-control"] || null,
      })
    }
  })
  return {
    page,
    contextLabel: `${owner.label}:${name}`,
    setPhase(value) {
      phase = `${owner.label}:${name}:${value}`
    },
  }
}

function safePath(raw) {
  try {
    const url = new URL(raw)
    return `${url.origin}${url.pathname}`
  } catch {
    return "invalid-url"
  }
}

async function validateTeamProjection(
  context,
  appOrigin,
  payload,
  kind,
  contextLabel
) {
  assert(
    typeof payload === "object" && payload !== null && !Array.isArray(payload),
    "team JSON must be an object"
  )
  assert(kind === "onboarding" || kind === "session", "known team JSON kind")
  const cookies = await context.cookies(appOrigin)
  const csrfCookieName =
    kind === "onboarding" ? "workos_team_pending_csrf" : "workos_team_csrf"
  const csrfCookie = cookies.find((cookie) => cookie.name === csrfCookieName)
  assert(csrfCookie, "team JSON must have its bound CSRF cookie")
  assert.equal(typeof payload.csrf, "string", "team JSON CSRF must be a string")
  assert(
    payload.csrf === csrfCookie.value,
    "team JSON CSRF must match its bound session cookie"
  )
  const scrubbed = { ...payload }
  scrubbed.csrf = "[verified-bound-csrf]"

  const serialized = JSON.stringify(scrubbed)
  for (const cookie of cookies) {
    if (cookie.value) observedTeamCookieValues.add(cookie.value)
  }
  for (const cookieValue of observedTeamCookieValues) {
    assert(!serialized.includes(cookieValue), "team JSON contains raw cookie material")
  }
  for (const marker of [
    "fixture-access-",
    "fixture-refresh-",
    "operator-canary-secret",
    "7878787878787878",
  ]) {
    assert(!serialized.includes(marker), "team JSON contains forbidden secret material")
  }
  teamProjectionChecks.push({
    context: contextLabel,
    kind,
    verifiedCsrf: true,
  })
}

async function assertNoPriorWorkspaceContent(page, priorSession) {
  if (!priorSession) return
  const bodyText = await page.locator("body").innerText()
  if (priorSession.organization.name) {
    assert(
      !bodyText.includes(priorSession.organization.name),
      "prior stored organization branding reached login/onboarding"
    )
  }
  assert(
    !bodyText.includes(priorSession.scope),
    "prior session scope reached login/onboarding"
  )
  assert.equal(
    await page.getByText("Signed in as", { exact: true }).count(),
    0,
    "signed-in identity region reached login/onboarding"
  )
  assert.equal(
    await page.getByText("Organization", { exact: true }).count(),
    0,
    "signed-in organization region reached login/onboarding"
  )
  assert.equal(
    await page
      .getByRole("heading", {
        name: "Your Neev workspace is being prepared",
        exact: true,
      })
      .count(),
    0,
    "signed-in workspace region reached login/onboarding"
  )
  if (priorSession.organization.avatarUrl) {
    assert.equal(
      await page
        .locator("img")
        .evaluateAll(
          (images, source) => images.filter((image) => image.src === source).length,
          priorSession.organization.avatarUrl
        ),
      0,
      "prior stored organization image reached login/onboarding"
    )
  }
}

async function startSignIn(tracked, appOrigin, priorSession) {
  const { page } = tracked
  await page.goto(`${appOrigin}/login`)
  await page.getByRole("heading", { name: "Sign in to Neev", exact: true }).waitFor()
  assert.equal(await page.title(), "Sign in - Neev")
  assert(!(await page.locator("body").innerText()).includes("Legacy workspace content"))
  await assertNoPriorWorkspaceContent(page, priorSession)
  await page.getByRole("link", { name: /GitHub/i }).click()
  await page.getByRole("heading", { name: "Fake GitHub consent", exact: true }).waitFor()
}

async function chooseProviderPersona(tracked, personaLabel) {
  await tracked.page.getByRole("button", { name: personaLabel, exact: true }).click()
}

async function reachOnboarding(tracked, appOrigin, personaLabel, priorSession) {
  await startSignIn(tracked, appOrigin, priorSession)
  const choicesResponse = tracked.page.waitForResponse(
    (response) =>
      new URL(response.url()).origin === appOrigin &&
      new URL(response.url()).pathname === "/api/team/onboarding"
  )
  await chooseProviderPersona(tracked, personaLabel)
  await tracked.page.waitForURL(`${appOrigin}/onboarding`)
  const response = await choicesResponse
  assert.equal(response.status(), 200)
  assert.equal(response.headers()["cache-control"], "no-store")
  const payload = await response.json()
  await validateTeamProjection(
    tracked.page.context(),
    appOrigin,
    payload,
    "onboarding",
    tracked.contextLabel
  )
  assert.equal(typeof payload.csrf, "string", "checked pending CSRF is authorized")
  assert.equal(payload.csrf.length, 64)
  assert(!(await tracked.page.locator("body").innerText()).includes("Legacy workspace content"))
  await assertNoPriorWorkspaceContent(tracked.page, priorSession)
  return payload
}

async function selectOrganization(tracked, appOrigin, login) {
  const { page } = tracked
  const radio = page.getByRole("radio", { name: new RegExp(`^${login}\\b`, "i") })
  await radio.waitFor()
  const continueButton = page.getByRole("button", {
    name: "Continue",
    exact: true,
  })
  assert.equal(await continueButton.isDisabled(), true, "organization is never auto-selected")
  await radio.check()
  const selected = page.waitForResponse(
    (response) =>
      new URL(response.url()).origin === appOrigin &&
      new URL(response.url()).pathname === "/api/team/onboarding/select"
  )
  await continueButton.click()
  const response = await selected
  assert.equal(response.status(), 200)
  assert.equal(response.headers()["cache-control"], "no-store")
  await page.waitForURL(`${appOrigin}/workspace`)
}

async function sessionJson(page, appOrigin, contextLabel) {
  const result = await page.evaluate(async (origin) => {
    const response = await fetch(`${origin}/api/team/session`, {
      credentials: "same-origin",
      cache: "no-store",
    })
    return {
      status: response.status,
      cacheControl: response.headers.get("cache-control"),
      body: await response.json(),
    }
  }, appOrigin)
  assert.equal(result.status, 200)
  assert.equal(result.cacheControl, "no-store")
  await validateTeamProjection(
    page.context(),
    appOrigin,
    result.body,
    "session",
    contextLabel
  )
  return result.body
}

async function assertSessionCookies(context, appOrigin, projectedJson) {
  const cookies = await context.cookies(appOrigin)
  const session = cookies.find((cookie) => cookie.name === "workos_team_session")
  const csrf = cookies.find((cookie) => cookie.name === "workos_team_csrf")
  assert(session && csrf, "session and CSRF cookies must exist")
  for (const cookie of [session, csrf]) {
    assert.equal(cookie.httpOnly, true)
    assert.equal(cookie.sameSite, "Lax")
    assert.equal(cookie.secure, false, "explicit loopback HTTP preview is not Secure")
  }
  const serialized = JSON.stringify(projectedJson)
  assert(!serialized.includes(session.value), "raw session cookie must not be projected")
  assert(!serialized.includes("fixture-access-"), "OAuth access token must not be projected")
  assert(!serialized.includes("fixture-refresh-"), "OAuth refresh token must not be projected")
  assert(!serialized.includes("operator-canary-secret"), "operator token must not be projected")
  assert(!serialized.includes("7878787878787878"), "encryption material must not be projected")
  return { session, csrf }
}

async function assertBrowserSecretAbsence(page, cookieValues) {
  const snapshot = await page.evaluate(() => ({
    storage: Object.fromEntries(
      Array.from({ length: localStorage.length }, (_, index) => localStorage.key(index))
        .filter(Boolean)
        .map((key) => [key, localStorage.getItem(key)])
    ),
    documentCookie: document.cookie,
    messages: window.__teamBroadcastMessages || [],
    sockets: window.__teamWebSocketAttempts || [],
    customStylePresent: Boolean(document.getElementById("codeg-custom-css")),
  }))
  assert.equal(snapshot.storage.codeg_token, undefined)
  assert.equal(snapshot.documentCookie, "")
  assert.equal(snapshot.customStylePresent, false)
  assert.deepEqual(snapshot.sockets, [])
  const serialized = JSON.stringify(snapshot)
  for (const cookie of cookieValues) {
    assert(!serialized.includes(cookie.value), `${cookie.name} leaked into browser state`)
  }
  for (const forbidden of [
    "fixture-access-",
    "fixture-refresh-",
    "7878787878787878",
  ]) {
    assert(!serialized.includes(forbidden), `${forbidden} leaked into browser state`)
  }
  for (const message of snapshot.messages) {
    assert.deepEqual(message, { type: "invalidate" })
  }
}

async function assertLegacyWebSocketAbsent(appOrigin) {
  const url = new URL(appOrigin)
  const statusCode = await new Promise((resolve, reject) => {
    const request = http.request({
      host: url.hostname,
      port: url.port,
      path: "/ws",
      headers: {
        Authorization: "Bearer operator-canary-secret",
        Connection: "Upgrade",
        Upgrade: "websocket",
        "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
        "Sec-WebSocket-Version": "13",
      },
    })
    request.once("response", (response) => {
      response.resume()
      resolve(response.statusCode)
    })
    request.once("upgrade", () => resolve(101))
    request.once("error", reject)
    request.setTimeout(5_000, () => request.destroy(new Error("upgrade timed out")))
    request.end()
  })
  assert.equal(statusCode, 404, "legacy WebSocket route must remain physically absent")
}

function screenshot(page, name) {
  const target = path.join(artifactRoot, name)
  screenshots.push(name)
  return page.screenshot({ path: target, fullPage: true })
}

async function run() {
  console.log(`Fixture artifacts: ${artifactRoot}`)
  startFixture()
  const fixtureInfo = await Promise.race([ready, timeout(30_000, "fixture startup")])
  assert.equal(fixtureInfo.evidence, "fake-loopback-github")
  const { appOrigin, providerOrigin, canaryOrigin } = fixtureInfo
  assert.notEqual(new URL(providerOrigin).origin, new URL(canaryOrigin).origin)
  assert.notEqual(new URL(appOrigin).origin, new URL(canaryOrigin).origin)

  browser = await chromium.launch({ headless: true })

  const negative = await newTrackedContext(
    "negative",
    appOrigin,
    canaryOrigin,
    { width: 1440, height: 960 }
  )
  const negativePage = trackPage(negative, await negative.context.newPage(), "boundary")
  negativePage.setPhase("root-redirect")
  await negativePage.page.goto(`${appOrigin}/`)
  await negativePage.page.waitForURL(`${appOrigin}/workspace`)
  assert.equal(await negativePage.page.title(), "Neev")
  await negativePage.page
    .getByRole("heading", { name: "Sign in required", exact: true })
    .waitFor()
  const denials = await negativePage.page.evaluate(async () => {
    const headers = { Authorization: "Bearer operator-canary-secret" }
    const session = await fetch("/api/team/session", { headers })
    const health = await fetch("/api/health", {
      method: "POST",
      headers: { ...headers, "Content-Type": "application/json" },
      body: "{}",
    })
    const settings = await fetch("/api/get_settings", {
      method: "POST",
      headers: { ...headers, "Content-Type": "application/json" },
      body: "{}",
    })
    return [session.status, health.status, settings.status]
  })
  assert.deepEqual(denials, [401, 404, 404])
  await assertLegacyWebSocketAbsent(appOrigin)

  const alice = await newTrackedContext("alice-switch", appOrigin, canaryOrigin, {
    width: 1440,
    height: 960,
  })
  const alicePage = trackPage(alice, await alice.context.newPage(), "primary")
  alicePage.setPhase("denied-consent")
  await startSignIn(alicePage, appOrigin)
  await chooseProviderPersona(alicePage, "Deny access")
  await alicePage.page.waitForURL(`${appOrigin}/login?reason=access_denied`)
  await alicePage.page.getByRole("alert").getByText(/permission was not granted/i).waitFor()

  alicePage.setPhase("provider-outage")
  await alicePage.page.getByRole("link", { name: "Try again with GitHub" }).click()
  await alicePage.page.getByRole("heading", { name: "Fake GitHub consent" }).waitFor()
  await chooseProviderPersona(alicePage, "Simulate provider outage")
  await alicePage.page.waitForURL(`${appOrigin}/login?reason=unavailable`)
  await alicePage.page.getByRole("alert").getByText(/temporarily unavailable/i).waitFor()

  alicePage.setPhase("alice-multiple-choice")
  await alicePage.page.getByRole("link", { name: "Try again with GitHub" }).click()
  await alicePage.page.getByRole("heading", { name: "Fake GitHub consent" }).waitFor()
  const aliceChoicesResponse = alicePage.page.waitForResponse((response) =>
    response.url().startsWith(`${appOrigin}/api/team/onboarding`)
  )
  await chooseProviderPersona(alicePage, "Continue as Alice Atlas")
  const aliceChoices = await (await aliceChoicesResponse).json()
  await validateTeamProjection(
    alice.context,
    appOrigin,
    aliceChoices,
    "onboarding",
    "alice-switch:primary"
  )
  assert.equal(aliceChoices.organizations.length, 2)
  assert.deepEqual(
    aliceChoices.organizations.map((organization) => organization.login),
    ["atlas-labs", "beacon-works"]
  )
  await alicePage.page.getByText("Alice Atlas", { exact: true }).waitFor()
  const forgedOrigin = await alice.context.request.post(
    `${appOrigin}/api/team/onboarding/select`,
    {
      headers: {
        Origin: "http://127.0.0.1:9",
        "Content-Type": "application/json",
        "X-WorkOS-CSRF": aliceChoices.csrf,
      },
      data: { githubOrgId: "61001" },
    }
  )
  assert.equal(forgedOrigin.status(), 403)
  const forgedCsrf = await alicePage.page.evaluate(async () => {
    const response = await fetch("/api/team/onboarding/select", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-WorkOS-CSRF": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
      },
      body: JSON.stringify({ githubOrgId: "61001" }),
    })
    return response.status
  })
  assert.equal(forgedCsrf, 401)
  await selectOrganization(alicePage, appOrigin, "atlas-labs")
  await alicePage.page.getByText("Alice Atlas", { exact: true }).waitFor()
  await alicePage.page.getByText("Atlas Operations", { exact: true }).waitFor()
  const brandedLogo = alicePage.page.locator('img[src^="data:image/png;base64,"]')
  await brandedLogo.waitFor()
  await brandedLogo.evaluate((image) => image.decode())
  const aliceSessionA = await sessionJson(
    alicePage.page,
    appOrigin,
    "alice-switch:org-a"
  )
  assert.equal(aliceSessionA.organization.login, "atlas-labs")
  assert.equal(aliceSessionA.organization.name, "Atlas Operations")
  assert.equal(aliceSessionA.role, "admin")
  const aliceCookiesA = await assertSessionCookies(
    alice.context,
    appOrigin,
    aliceSessionA
  )
  await assertBrowserSecretAbsence(alicePage.page, [aliceCookiesA.session, aliceCookiesA.csrf])
  await screenshot(alicePage.page, "alice-org-a-desktop-1440.png")

  const aliceStale = trackPage(alice, await alice.context.newPage(), "stale-tab")
  aliceStale.setPhase("org-a")
  await aliceStale.page.goto(`${appOrigin}/workspace`)
  await aliceStale.page.getByText("Atlas Operations", { exact: true }).waitFor()
  await control(providerOrigin, "/fixture/session-delay/arm")
  aliceStale.setPhase("delayed-org-a-refresh")
  const delayedRequestStarted = aliceStale.page.waitForRequest(
    (request) =>
      request.method() === "GET" &&
      request.url() === `${appOrigin}/api/team/session`
  )
  await aliceStale.page.evaluate(() => window.dispatchEvent(new Event("focus")))
  const delayedRequest = await delayedRequestStarted
  await poll(
    async () => (await status(providerOrigin)).sessionDelayStarted,
    "delayed prior-scope response"
  )
  const delayedRequestTerminal = Promise.race([
    aliceStale.page
      .waitForResponse((response) => response.request() === delayedRequest)
      .then(async (response) => {
        const finishError = await response.finished()
        return finishError
          ? { outcome: "canceled", error: finishError.message }
          : { outcome: "completed", status: response.status() }
      }),
    aliceStale.page
      .waitForEvent("requestfailed", (request) => request === delayedRequest)
      .then((request) => ({
        outcome: "canceled",
        error: request.failure()?.errorText || "unknown",
      })),
  ])

  alicePage.setPhase("switch-to-org-b")
  const switchedChoices = await reachOnboarding(
    alicePage,
    appOrigin,
    "Continue as Alice Atlas",
    aliceSessionA
  )
  assert.equal(switchedChoices.organizations.length, 2)
  await selectOrganization(alicePage, appOrigin, "beacon-works")
  await alicePage.page.getByText("beacon-works", { exact: true }).waitFor()
  await aliceStale.page.getByText("beacon-works", { exact: true }).waitFor()
  await aliceStale.page.evaluate(
    ({ priorBranding, priorLogin, priorScope }) => {
      const staleValues = [priorBranding, `@${priorLogin}`, priorScope]
      window.__staleScopePaintedAfterSwitch = false
      window.__staleScopeObserver = new MutationObserver(() => {
        const text = document.body.innerText
        if (staleValues.some((value) => value && text.includes(value))) {
          window.__staleScopePaintedAfterSwitch = true
        }
      })
      window.__staleScopeObserver.observe(document.body, {
        childList: true,
        characterData: true,
        subtree: true,
      })
    },
    {
      priorBranding: aliceSessionA.organization.name,
      priorLogin: aliceSessionA.organization.login,
      priorScope: aliceSessionA.scope,
    }
  )
  await control(providerOrigin, "/fixture/session-delay/release")
  const staleRequestLifecycle = await delayedRequestTerminal
  assert(
    (staleRequestLifecycle.outcome === "completed" &&
      staleRequestLifecycle.status === 200) ||
      (staleRequestLifecycle.outcome === "canceled" &&
        staleRequestLifecycle.error.includes("ERR_ABORTED")),
    "held prior-scope request must complete successfully or cancel fail-closed"
  )
  await aliceStale.page.evaluate(
    () =>
      new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve))
      )
  )
  assert.equal(await aliceStale.page.getByText("Atlas Operations", { exact: true }).count(), 0)
  assert.equal(await aliceStale.page.getByText("@atlas-labs", { exact: true }).count(), 0)
  assert.equal(await aliceStale.page.getByText("beacon-works", { exact: true }).count(), 1)
  assert.equal(
    await aliceStale.page.evaluate(() => window.__staleScopePaintedAfterSwitch),
    false,
    "held prior-scope response repainted organization A after the switch"
  )
  const aliceSessionB = await sessionJson(
    alicePage.page,
    appOrigin,
    "alice-switch:org-b"
  )
  assert.equal(aliceSessionB.organization.login, "beacon-works")
  assert.notEqual(aliceSessionA.scope, aliceSessionB.scope)
  await screenshot(alicePage.page, "alice-org-b-after-stale-desktop-1440.png")

  const aliceIndependent = await newTrackedContext(
    "alice-independent-b",
    appOrigin,
    canaryOrigin,
    { width: 1440, height: 960 }
  )
  const aliceIndependentPage = trackPage(
    aliceIndependent,
    await aliceIndependent.context.newPage(),
    "primary"
  )
  aliceIndependentPage.setPhase("sign-in")
  const independentChoices = await reachOnboarding(
    aliceIndependentPage,
    appOrigin,
    "Continue as Alice Atlas"
  )
  assert.equal(independentChoices.organizations.length, 2)
  await selectOrganization(aliceIndependentPage, appOrigin, "beacon-works")
  await aliceIndependentPage.page.getByText("Alice Atlas", { exact: true }).waitFor()

  alicePage.setPhase("stale-proof-provider-outage")
  await control(providerOrigin, "/fixture/advance-clock?seconds=61")
  await control(providerOrigin, "/fixture/fail-next-proof")
  await alicePage.page.evaluate(() => window.dispatchEvent(new Event("focus")))
  await alicePage.page
    .getByRole("heading", { name: "Session verification is unavailable" })
    .waitFor()
  assert.equal(await alicePage.page.getByText("Alice Atlas", { exact: true }).count(), 0)
  await alicePage.page.getByRole("button", { name: "Sign out", exact: true }).click()
  await alicePage.page.waitForURL(`${appOrigin}/login`)
  await aliceIndependentPage.page.evaluate(() =>
    document.dispatchEvent(new Event("visibilitychange"))
  )
  await aliceIndependentPage.page.getByText("Alice Atlas", { exact: true }).waitFor()

  const bob = await newTrackedContext("bob-a", appOrigin, canaryOrigin, {
    width: 1440,
    height: 960,
  })
  const bobPage = trackPage(bob, await bob.context.newPage(), "primary")
  bobPage.setPhase("single-choice")
  const bobChoices = await reachOnboarding(bobPage, appOrigin, "Continue as Bob Birch")
  assert.equal(bobChoices.organizations.length, 1)
  await selectOrganization(bobPage, appOrigin, "atlas-labs")
  await bobPage.page.getByText("Bob Birch", { exact: true }).waitFor()
  const bobSession = await sessionJson(bobPage.page, appOrigin, "bob-a")
  assert.equal(bobSession.organization.login, "atlas-labs")
  assert.equal(bobSession.role, "member", "Bob must not gain admin authority")

  const carol = await newTrackedContext("carol-b", appOrigin, canaryOrigin, {
    width: 375,
    height: 812,
  })
  const carolPage = trackPage(carol, await carol.context.newPage(), "primary")
  await control(providerOrigin, "/fixture/zero-next")
  carolPage.setPhase("zero-choice")
  const zeroChoices = await reachOnboarding(
    carolPage,
    appOrigin,
    "Continue as Carol Coast"
  )
  assert.deepEqual(zeroChoices.organizations, [])
  await carolPage.page
    .getByRole("heading", { name: "No eligible organizations found" })
    .waitFor()
  const install = carolPage.page.getByRole("link", {
    name: "Open GitHub App installation",
    exact: true,
  })
  assert.equal(
    await install.getAttribute("href"),
    "https://github.com/apps/eazyworkos/installations/new"
  )
  await carolPage.page.getByRole("button", { name: "Retry", exact: true }).waitFor()
  assert.equal(
    await carolPage.page
      .getByRole("link", { name: "Restart sign-in", exact: true })
      .getAttribute("href"),
    "/auth/github/login"
  )
  const retryChoices = carolPage.page.waitForResponse(
    (response) => new URL(response.url()).pathname === "/api/team/onboarding"
  )
  await carolPage.page.getByRole("button", { name: "Retry", exact: true }).click()
  const carolRetryChoices = await (await retryChoices).json()
  await validateTeamProjection(
    carol.context,
    appOrigin,
    carolRetryChoices,
    "onboarding",
    "carol-b:retry"
  )
  assert.equal(carolRetryChoices.organizations.length, 1)
  await selectOrganization(carolPage, appOrigin, "beacon-works")
  await carolPage.page.getByText("Carol Coast", { exact: true }).waitFor()
  const carolSession = await sessionJson(carolPage.page, appOrigin, "carol-b")
  assert.equal(carolSession.organization.login, "beacon-works")
  assert.equal(carolSession.role, "admin")
  const carolRefresh = carolPage.page.waitForResponse(
    (response) => new URL(response.url()).pathname === "/api/team/session"
  )
  await carolPage.page.evaluate(() =>
    document.dispatchEvent(new Event("visibilitychange"))
  )
  const carolRefreshResponse = await carolRefresh
  assert.equal(carolRefreshResponse.status(), 200)
  const carolRefreshSession = await carolRefreshResponse.json()
  await validateTeamProjection(
    carol.context,
    appOrigin,
    carolRefreshSession,
    "session",
    "carol-b:visibility-refresh"
  )
  assert(
    await carolPage.page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth
    )
  )
  await screenshot(carolPage.page, "carol-org-b-mobile-375.png")

  carolPage.setPhase("settings-fixed-return")
  await carolPage.page.setViewportSize({ width: 1440, height: 960 })
  await carolPage.page.goto(
    `${appOrigin}/settings?returnChatId=42&remoteConnectionId=777&remoteWindowId=legacy`
  )
  const back = carolPage.page.getByRole("link", {
    name: "Back to workspace",
    exact: true,
  })
  await back.waitFor()
  assert.equal(await back.getAttribute("href"), "/workspace")
  assert.equal(
    carol.diagnostics.remoteLookupAttempts,
    0,
    "team settings must not resolve the requested legacy remote connection"
  )
  const remoteDescriptorProbe = await carolPage.page.evaluate(async () => {
    const response = await fetch("/api/get_remote_workspace_connection", {
      method: "POST",
      headers: {
        Authorization: "Bearer operator-canary-secret",
        "Content-Type": "application/json",
        "X-WorkOS-E2E-Remote-Probe": "descriptor",
      },
      body: JSON.stringify({ id: 777 }),
    })
    return {
      status: response.status,
      cacheControl: response.headers.get("cache-control"),
      body: await response.json(),
    }
  })
  assert.equal(remoteDescriptorProbe.status, 200)
  assert.equal(remoteDescriptorProbe.cacheControl, "no-store")
  assert.equal(remoteDescriptorProbe.body.id, 777)
  assert.equal(remoteDescriptorProbe.body.base_url, canaryOrigin)
  assert(
    remoteDescriptorProbe.body.token === "remote-operator-canary-secret",
    "controlled remote descriptor must include its invented canary credential"
  )
  assert.equal(
    carol.diagnostics.remoteDescriptorProbes,
    1,
    "controlled remote descriptor probe must exercise the real lookup shape"
  )
  assert.equal(
    carol.diagnostics.remoteLookupAttempts,
    0,
    "team settings must remain unable to resolve legacy remote connections"
  )
  await back.click()
  await carolPage.page.waitForURL(`${appOrigin}/workspace`)
  await screenshot(carolPage.page, "carol-settings-return-desktop-1440.png")

  const canaries = await status(canaryOrigin)
  assert.deepEqual(canaries, { httpRequests: 0, websocketUpgrades: 0 })

  const allCookieValues = []
  for (const { context } of allContexts) {
    allCookieValues.push(...(await context.cookies(appOrigin)))
  }
  for (const { context } of allContexts) {
    for (const page of context.pages()) {
      if (page.url().startsWith(appOrigin)) {
        await assertBrowserSecretAbsence(page, allCookieValues)
      }
    }
  }

  assert.deepEqual(
    {
      onboarding: teamProjectionChecks.filter(
        (check) => check.kind === "onboarding"
      ).length,
      session: teamProjectionChecks.filter((check) => check.kind === "session")
        .length,
    },
    { onboarding: 6, session: 5 },
    "every explicitly awaited successful onboarding/session projection must be checked"
  )
  assert.equal(
    allDiagnostics.reduce(
      (total, diagnostics) => total + diagnostics.remoteDescriptorProbes,
      0
    ),
    1,
    "exactly one controlled remote descriptor positive probe must run"
  )
  assert.equal(
    allDiagnostics.reduce(
      (total, diagnostics) => total + diagnostics.remoteLookupAttempts,
      0
    ),
    0,
    "team mode must not attempt a legacy remote descriptor lookup"
  )
  assert.equal(
    await aliceStale.page.evaluate(() => {
      const painted = window.__staleScopePaintedAfterSwitch
      window.__staleScopeObserver?.disconnect()
      return painted
    }),
    false,
    "organization A must remain absent after the held request reaches terminal state"
  )

  const successfulSelectionPhases = new Set([
    "alice-switch:primary:alice-multiple-choice",
    "alice-switch:primary:switch-to-org-b",
    "alice-independent-b:primary:sign-in",
    "bob-a:primary:single-choice",
    "carol-b:primary:zero-choice",
  ])
  const expectedAbortedRequest = (entry) =>
    entry.error.includes("ERR_ABORTED") &&
    ((entry.path === `${appOrigin}/api/team/onboarding/select` &&
      entry.method === "POST" &&
      successfulSelectionPhases.has(entry.phase)) ||
      (entry.path === `${appOrigin}/api/team/session` &&
        entry.method === "GET" &&
        (entry.phase.includes("delayed-org-a-refresh") ||
          entry.phase.includes("stale-proof-provider-outage"))) ||
      (entry.path === `${appOrigin}/api/team/logout` &&
        entry.method === "POST" &&
        entry.phase.includes("stale-proof-provider-outage")))
  const allowedFailedRequests = allDiagnostics.flatMap((diagnostics) =>
    diagnostics.failedRequests.filter(expectedAbortedRequest)
  )
  const unexpectedFailedRequests = allDiagnostics.flatMap((diagnostics) =>
    diagnostics.failedRequests.filter((entry) => !allowedFailedRequests.includes(entry))
  )
  assert.deepEqual(unexpectedFailedRequests, [])
  const expectedHttpError = (entry) =>
    (entry.path === `${appOrigin}/api/team/session` &&
      entry.method === "GET" &&
      ((entry.status === 401 &&
        (entry.phase === "negative:boundary:root-redirect" ||
          entry.phase === "alice-switch:stale-tab:delayed-org-a-refresh")) ||
        (entry.status === 503 &&
          entry.phase ===
            "alice-switch:primary:stale-proof-provider-outage"))) ||
    (entry.path === `${appOrigin}/api/health` &&
      entry.method === "POST" &&
      entry.status === 404 &&
      entry.phase === "negative:boundary:root-redirect") ||
    (entry.path === `${appOrigin}/api/get_settings` &&
      entry.method === "POST" &&
      entry.status === 404 &&
      entry.phase === "negative:boundary:root-redirect") ||
    (entry.path === `${appOrigin}/api/team/onboarding/select` &&
      entry.method === "POST" &&
      entry.status === 401 &&
      entry.phase === "alice-switch:primary:alice-multiple-choice")
  const expectedHttpErrors = allDiagnostics.flatMap((diagnostics) =>
    diagnostics.httpErrors.filter(expectedHttpError)
  )
  const unexpectedHttpErrors = allDiagnostics.flatMap((diagnostics) =>
    diagnostics.httpErrors.filter((entry) => !expectedHttpError(entry))
  )
  assert.deepEqual(unexpectedHttpErrors, [])
  assert.deepEqual(
    allDiagnostics.flatMap((diagnostics) => diagnostics.pageErrors),
    []
  )
  const expectedConsoleControls = [
    ["negative:boundary:root-redirect", "/api/team/session", 401],
    ["negative:boundary:root-redirect", "/api/health", 404],
    ["negative:boundary:root-redirect", "/api/get_settings", 404],
    ["alice-switch:primary:alice-multiple-choice", "/api/team/onboarding/select", 401],
    [
      "alice-switch:primary:stale-proof-provider-outage",
      "/api/team/session",
      503,
    ],
    [
      "alice-switch:stale-tab:delayed-org-a-refresh",
      "/api/team/session",
      401,
    ],
  ]
  const statusReason = {
    401: "Unauthorized",
    404: "Not Found",
    503: "Service Unavailable",
  }
  const expectedConsoleDiagnostic = (entry) =>
    entry.type === "error" &&
    expectedConsoleControls.some(
      ([phase, path, statusCode]) =>
        entry.phase === phase &&
        entry.location === `${appOrigin}${path}` &&
        entry.text ===
          `Failed to load resource: the server responded with a status of ${statusCode} (${statusReason[statusCode]})`
    )
  const expectedConsoleDiagnostics = allDiagnostics.flatMap((diagnostics) =>
    diagnostics.consoleDiagnostics.filter(expectedConsoleDiagnostic)
  )
  assert.deepEqual(
    allDiagnostics.flatMap((diagnostics) =>
      diagnostics.consoleDiagnostics.filter(
        (entry) => !expectedConsoleDiagnostic(entry)
      )
    ),
    []
  )
  assert.deepEqual(
    allDiagnostics.flatMap((diagnostics) => diagnostics.websockets),
    []
  )
  for (const diagnostics of allDiagnostics) {
    for (const response of diagnostics.apiResponses) {
      if (response.path.startsWith("/api/team/")) {
        assert.equal(response.cacheControl, "no-store")
      }
    }
  }

  const providerCounts = await status(providerOrigin)
  assert(providerCounts.authorize >= 7)
  assert(providerCounts.exchange >= 6)
  assert(providerCounts.identity >= 5)
  assert(providerCounts.listing >= 10)
  assert(providerCounts.proof >= 6)
  const evidence = {
    result: "PASS",
    externalEvidence: "fake loopback GitHub; real consent remains a release gate",
    viewports: ["desktop-1440", "mobile-375"],
    contexts: ["alice-switch-a-to-b", "alice-independent-b", "bob-a", "carol-b"],
    screenshots,
    providerCounts,
    canaries,
    expectedHttpDenials: {
      operatorSession: 401,
      legacyRoutes: [404, 404],
      forgedOrigin: 403,
      forgedCsrf: 401,
      legacyWebSocket: 404,
    },
    expectedBrowserAborts: allowedFailedRequests,
    expectedBrowserHttpErrors: expectedHttpErrors,
    expectedConsoleDiagnostics,
    teamJsonProjectionChecks: teamProjectionChecks,
    remoteActivationControls: {
      descriptorProbes: 1,
      unexpectedLookups: 0,
    },
    staleRequestLifecycle,
  }
  fs.writeFileSync(
    path.join(artifactRoot, "evidence.json"),
    JSON.stringify(evidence, null, 2) + "\n",
    { flag: "wx" }
  )
  console.log("PASS actual browser onboarding, organization isolation, and negative controls")
  console.log("PASS fake external evidence only; real GitHub App consent remains a release gate")
}

;(async () => {
  let failure
  try {
    await run()
  } catch (error) {
    failure = error
  }
  try {
    if (browser) await browser.close()
  } catch (error) {
    failure ||= error
  }
  try {
    await stopFixture()
  } catch (error) {
    failure ||= error
  }
  try {
    assert.equal(fs.existsSync(syntheticRuntime), false, "fixture cleaned synthetic runtime")
  } catch (error) {
    failure ||= error
  }
  if (failure) throw failure
  assert.equal(browser.isConnected(), false, "browser process released")
  console.log(`PASS browser and fixture processes released; evidence: ${artifactRoot}`)
})().catch((error) => {
  console.error(error)
  if (fixtureLog && !fixtureLog.includes("TEAM_BROWSER_E2E_READY ")) {
    console.error(fixtureLog)
  }
  process.exitCode = 1
})
