import assert from "node:assert/strict"
import { createServer } from "node:http"
import { test } from "node:test"
import { preflight } from "./neev-identity-preflight.mjs"

test("refuses external or ambiguous origins before any request", async () => {
  for (const origin of [
    undefined,
    "https://app.neev.so",
    "http://localhost:1234",
    "http://127.0.0.1",
    "http://user@127.0.0.1:1234",
    "http://127.0.0.1:1234/path",
    "http://[::]:1234",
    "http://2130706433:1234",
    "http://127.0.0.1:1234/#",
  ]) {
    await assert.rejects(preflight(origin), /isolated/)
  }
})

test("actual HTTP boundary rejects wrong mode, runtime, redirects and legacy routes", async () => {
  let mode = "native"
  let runtime = false
  let legacy = 404
  let redirect = false
  let malformed = false
  const requests = []
  const server = createServer((req, res) => {
    requests.push(req.url)
    assert.equal(req.headers.cookie, undefined)
    assert.equal(req.headers.authorization, undefined)
    if (redirect) {
      res.writeHead(302, { location: "https://app.neev.so" })
      res.end()
      return
    }
    if (req.url === "/api/workos/capabilities") {
      res.setHeader("content-type", "application/json")
      res.end(
        malformed
          ? "untrusted-body-must-not-echo"
          : JSON.stringify({ identityMode: mode, runtimeEnabled: runtime })
      )
    } else {
      res.writeHead(legacy)
      res.end()
    }
  })
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve))
  const origin = `http://127.0.0.1:${server.address().port}`
  try {
    mode = "team"
    await assert.rejects(preflight(origin), /identity\/runtime/)
    mode = "native"
    runtime = true
    await assert.rejects(preflight(origin), /identity\/runtime/)
    runtime = false
    legacy = 200
    await assert.rejects(preflight(origin), /Legacy route/)
    legacy = 404
    redirect = true
    await assert.rejects(preflight(origin), /HTTP boundary/)
    redirect = false
    malformed = true
    await assert.rejects(preflight(origin), {
      message: "HTTP boundary invalid capabilities",
    })
    malformed = false
    const result = await preflight(origin)
    assert.equal(result.runtimeEnabled, false)
    assert.ok(requests.includes("/api/chat/send"))
    assert.ok(requests.includes("/api/team/session"))
    assert.ok(requests.includes("/api/settings"))
  } finally {
    await new Promise((resolve) => server.close(resolve))
  }
})
