import assert from "node:assert/strict"
import { createServer } from "node:http"
import { test } from "node:test"
import { preflight } from "./neev-content-preflight.mjs"

const fixtureSession = `n1.session.${"A".repeat(43)}`

test("refuses an unsafe origin or absent explicit synthetic session before requests", async () => {
  for (const [origin, session] of [
    [undefined, fixtureSession],
    ["https://app.neev.so", fixtureSession],
    ["http://localhost:1234", fixtureSession],
    ["http://127.0.0.1:1234", undefined],
    ["http://127.0.0.1:1234", ""],
    ["http://127.0.0.1:1234", "bad\r\nsession"],
    ["http://127.0.0.1:1234", `n1.csrf.${"A".repeat(43)}`],
  ]) {
    await assert.rejects(preflight(origin, session), /isolated|session/)
  }
})

test("reads native content through only the supplied fixture session and returns sanitized counts", async () => {
  const requests = []
  const server = createServer((req, res) => {
    requests.push({
      method: req.method,
      url: req.url,
      cookie: req.headers.cookie,
      authorization: req.headers.authorization,
    })
    assert.equal(req.method, "GET")
    assert.equal(req.headers.authorization, undefined)
    assert.equal(req.headers.cookie, `neev_preview_session=${fixtureSession}`)
    res.setHeader("content-type", "application/json")
    if (req.url === "/api/workos/capabilities") {
      res.end(
        JSON.stringify({
          identityMode: "native",
          runtimeEnabled: false,
          catalog: { chats: true, documents: true },
        })
      )
    } else if (req.url === "/api/neev/nodes") {
      res.end(
        JSON.stringify({
          nodes: [
            { id: 1, kind: "project" },
            { id: 2, kind: "folder" },
          ],
          nextAfter: null,
        })
      )
    } else if (req.url === "/api/neev/nodes/1/chats") {
      res.end(JSON.stringify({ items: [{ id: "chat" }], nextAfter: null }))
    } else if (req.url === "/api/neev/nodes/1/documents") {
      res.end(JSON.stringify({ items: [], nextAfter: null }))
    } else if (req.url === "/api/neev/nodes/2/chats") {
      res.end(JSON.stringify({ items: [], nextAfter: null }))
    } else if (req.url === "/api/neev/nodes/2/documents") {
      res.end(JSON.stringify({ items: [{ id: "doc" }], nextAfter: null }))
    } else {
      res.writeHead(404)
      res.end(JSON.stringify({ code: "not_found" }))
    }
  })
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve))
  const origin = `http://127.0.0.1:${server.address().port}`
  try {
    const result = await preflight(origin, fixtureSession)
    assert.deepEqual(result, {
      identityMode: "native",
      runtimeEnabled: false,
      organizationScoped: true,
      nodes: 2,
      chats: 1,
      documents: 1,
    })
    assert.equal(JSON.stringify(result).includes("session"), false)
    assert.equal(requests.length, 6)
  } finally {
    await new Promise((resolve) => server.close(resolve))
  }
})

test("fails closed on redirects, wrong boundaries and malformed pagination", async () => {
  let response = "redirect"
  const server = createServer((req, res) => {
    if (response === "redirect") {
      res.writeHead(302, { location: "https://app.neev.so" })
      res.end()
      return
    }
    res.setHeader("content-type", "application/json")
    if (req.url === "/api/workos/capabilities") {
      res.end(
        JSON.stringify({
          identityMode: response === "wrong-mode" ? "team" : "native",
          runtimeEnabled: false,
          catalog: { chats: true, documents: true },
        })
      )
    } else if (req.url === "/api/neev/nodes") {
      res.end(JSON.stringify({ nodes: [], nextAfter: "unexpected-page" }))
    }
  })
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve))
  const origin = `http://127.0.0.1:${server.address().port}`
  try {
    await assert.rejects(
      preflight(origin, fixtureSession),
      /unavailable|redirect/
    )
    response = "wrong-mode"
    await assert.rejects(
      preflight(origin, fixtureSession),
      /identity\/runtime\/content/
    )
    response = "malformed-pagination"
    await assert.rejects(
      preflight(origin, fixtureSession),
      /bounded first page/
    )
  } finally {
    await new Promise((resolve) => server.close(resolve))
  }
})
