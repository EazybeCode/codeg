import { readFileSync } from "node:fs"
import { join } from "node:path"
import { afterEach, expect, it, vi } from "vitest"

afterEach(() => {
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  document.body.replaceChildren()
  history.replaceState(null, "", "/")
})

it("clears the invitation fragment before preview and keeps the token in memory", async () => {
  document.body.innerHTML = `
    <p data-company></p>
    <form hidden><button type="submit">Join</button></form>
    <p role="alert"></p>
  `
  history.replaceState(
    null,
    "",
    "/auth/invite#token=n1.invite.synthetic-secret"
  )
  const storageWrite = vi.spyOn(Storage.prototype, "setItem")
  const hashesAtFetch: string[] = []
  const fetchMock = vi.fn().mockImplementation(async (url: string) => {
    hashesAtFetch.push(location.hash)
    if (url === "/api/neev/session") {
      return {
        ok: true,
        headers: { get: () => "application/json" },
        json: async () => ({ csrf: "n1.csrf.synthetic" }),
      }
    }
    if (url === "/api/neev/invitations/preview") {
      return {
        ok: true,
        headers: { get: () => "application/json" },
        json: async () => ({ organization: { id: 17, name: "Exact Company" } }),
      }
    }
    return {
      ok: false,
      headers: { get: () => "application/json" },
      json: async () => ({ code: "forbidden" }),
    }
  })
  vi.stubGlobal("fetch", fetchMock)

  const source = readFileSync(
    join(process.cwd(), "public/neev-auth/invite.js"),
    "utf8"
  )
  window.eval(source)

  await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2))
  expect(location.pathname).toBe("/auth/invite")
  expect(location.hash).toBe("")
  expect(location.href).not.toContain("synthetic-secret")
  expect(hashesAtFetch).toEqual(["", ""])
  expect(document.querySelector("[data-company]")).toHaveTextContent(
    "Join Exact Company?"
  )
  expect(document.querySelector("form")).not.toHaveAttribute("hidden")
  expect(storageWrite).not.toHaveBeenCalled()

  const form = document.querySelector("form")!
  form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }))
  form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }))
  await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(3))
  expect(fetchMock.mock.calls[2][0]).toBe("/api/neev/invitations/accept")
  expect(JSON.parse(fetchMock.mock.calls[2][1].body)).toEqual({
    token: "n1.invite.synthetic-secret",
    confirmedOrgId: 17,
  })
  expect(storageWrite).not.toHaveBeenCalled()
})
