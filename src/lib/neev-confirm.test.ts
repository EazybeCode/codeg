import { readFileSync } from "node:fs"
import { join } from "node:path"
import { afterEach, expect, it, vi } from "vitest"

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
  history.replaceState(null, "", "/")
})

it("clears the magic fragment before POST and blocks repeated submissions", async () => {
  document.body.innerHTML = `
    <form><button type="submit">Continue</button></form>
    <p role="alert"></p>
  `
  history.replaceState(
    null,
    "",
    "/auth/email/confirm#token=n1.magic.synthetic-secret"
  )
  const fetchMock = vi
    .fn()
    .mockResolvedValueOnce({
      ok: true,
      headers: { get: () => "application/json" },
      json: async () => ({ csrf: "n1.csrf.synthetic" }),
    })
    .mockResolvedValueOnce({
      ok: false,
      headers: { get: () => "application/json" },
      json: async () => ({ code: "unauthorized" }),
    })
  vi.stubGlobal("fetch", fetchMock)

  const source = readFileSync(
    join(process.cwd(), "public/neev-auth/confirm.js"),
    "utf8"
  )
  window.eval(source)

  expect(location.pathname).toBe("/auth/email/confirm")
  expect(location.hash).toBe("")
  expect(location.href).not.toContain("synthetic-secret")

  const form = document.querySelector("form")!
  form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }))
  form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }))
  await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2))

  expect(fetchMock.mock.calls[0][0]).toBe("/api/neev/preauth")
  expect(fetchMock.mock.calls[1][0]).toBe("/api/neev/email/confirm")
  const confirmation = fetchMock.mock.calls[1][1]
  expect(JSON.parse(confirmation.body)).toEqual({
    token: "n1.magic.synthetic-secret",
  })
  expect(document.querySelector("button")).not.toBeDisabled()
  expect(document.querySelector('[role="alert"]')).toHaveTextContent(
    "original sign-in method"
  )
  expect(document.querySelector('[role="alert"]')).toHaveTextContent("Settings")
})
