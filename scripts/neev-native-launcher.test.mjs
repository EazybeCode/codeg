import assert from "node:assert/strict"
import { readFile } from "node:fs/promises"
import { test } from "node:test"
import { runInNewContext } from "node:vm"

const issuer = "http://localhost:47001"
const source = (
  await readFile(
    new URL("../src-tauri/src/neev/browser_fixture_google.js", import.meta.url),
    "utf8"
  )
).replaceAll("__NEEV_SYNTHETIC_ISSUER__", issuer)
function fixture(target) {
  const buttons = Object.fromEntries(
    ["begin-login", "begin-link", "status"].map((id) => [
      id,
      {
        textContent: "",
        addEventListener(_type, callback) {
          this.click = callback
        },
      },
    ])
  )
  const calls = []
  let assigned
  runInNewContext(source, {
    URL,
    document: { getElementById: (id) => buttons[id] },
    window: {
      location: {
        assign: (value) => {
          assigned = value
        },
      },
    },
    fetch: async (path, options) => {
      calls.push({ path, options })
      const body =
        path === "/api/neev/preauth"
          ? { csrf: "preauth-fixture" }
          : path === "/api/neev/session"
            ? { csrf: "session-fixture" }
            : { authorizationUrl: target }
      return { ok: true, json: async () => body }
    },
  })
  return { buttons, calls, assigned: () => assigned }
}

test("synthetic login uses preauth CSRF and only the owned issuer", async () => {
  const f = fixture(`${issuer}/authorize?state=synthetic`)
  await f.buttons["begin-login"].click()
  assert.equal(f.assigned(), `${issuer}/authorize?state=synthetic`)
  assert.deepEqual(
    f.calls.map((c) => c.path),
    ["/api/neev/preauth", "/api/neev/auth/google/login"]
  )
  assert.equal(f.calls[1].options.headers["x-neev-csrf"], "preauth-fixture")
  assert.equal(f.calls[1].options.body, "{}")
  assert.equal(f.calls[1].options.credentials, "same-origin")
  assert.equal(f.calls[1].options.redirect, "error")
})
test("synthetic linking uses current session CSRF after browser preauth", async () => {
  const f = fixture(`${issuer}/authorize`)
  await f.buttons["begin-link"].click()
  assert.equal(f.assigned(), `${issuer}/authorize`)
  assert.deepEqual(
    f.calls.map((c) => c.path),
    ["/api/neev/preauth", "/api/neev/session", "/api/neev/auth/google/link"]
  )
  assert.equal(f.calls[2].options.headers["x-neev-csrf"], "session-fixture")
})
test("untrusted authorization targets never navigate or appear in errors", async () => {
  for (const target of [
    "https://accounts.google.com/o/oauth2/v2/auth",
    `${issuer}/other`,
    "http://localhost:47002/authorize",
    `${issuer}/authorize#secret`,
    "http://credential@localhost:47001/authorize",
  ]) {
    const f = fixture(target)
    await f.buttons["begin-login"].click()
    assert.equal(f.assigned(), undefined)
    assert.equal(
      f.buttons.status.textContent,
      "Synthetic sign-in could not start."
    )
  }
})
