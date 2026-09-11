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
        json: async () => ({
          user: { id: 1, displayName: "Recipient", avatarUrl: null },
          organization: null,
          role: null,
          scope: "identity",
          csrf: "n1.csrf.synthetic",
        }),
      }
    }
    if (url === "/api/neev/invitations/preview") {
      return {
        ok: true,
        headers: { get: () => "application/json" },
        json: async () => ({ organization: { id: 17, name: "Exact Company" } }),
      }
    }
    if (url !== "/api/neev/invitations/accept")
      throw new Error(`Unexpected fetch ${url}`)
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

it("confirms a different joined company without switching an existing scoped session", async () => {
  document.body.innerHTML = `<p data-company></p><form hidden><button type="submit">Join</button></form><p role="alert"></p><p class="hint"></p><a data-recovery href="/login">Sign in</a>`
  history.replaceState(
    null,
    "",
    "/auth/invite#token=n1.invite.synthetic-secret"
  )
  const writes = vi.spyOn(Storage.prototype, "setItem")
  const requests: string[] = []
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init: RequestInit = {}) => {
      requests.push(url)
      const values: Record<string, unknown> = {
        "/api/neev/session": {
          user: { id: 1, displayName: "Recipient", avatarUrl: null },
          organization: { id: 4, name: "Current company", avatarUrl: null },
          scope: "organization",
          role: "member",
          csrf: "synthetic-csrf",
        },
        "/api/neev/invitations/preview": {
          organization: { id: 17, name: "New exact company" },
        },
        "/api/neev/invitations/accept": { organizationId: 17 },
      }
      if (!(url in values)) throw new Error(`Unexpected fetch ${url}`)
      if (url.endsWith("accept"))
        expect(JSON.parse(init.body as string)).toEqual({
          token: "n1.invite.synthetic-secret",
          confirmedOrgId: 17,
        })
      return new Response(JSON.stringify(values[url]), {
        headers: { "Content-Type": "application/json" },
      })
    })
  )
  window.eval(
    readFileSync(join(process.cwd(), "public/neev-auth/invite.js"), "utf8")
  )
  await vi.waitFor(() =>
    expect(document.querySelector("form")).not.toHaveAttribute("hidden")
  )
  expect(requests).toHaveLength(2)
  document
    .querySelector("form")!
    .dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }))
  await vi.waitFor(() =>
    expect(document.querySelector("[data-company]")).toHaveTextContent(
      "You joined New exact company."
    )
  )
  expect(document.querySelector(".hint")).toHaveTextContent(
    /sign out.*sign in/i
  )
  expect(document.querySelector("[data-recovery]")).toHaveAttribute(
    "href",
    "/settings"
  )
  expect(location.pathname).toBe("/auth/invite")
  expect(location.hash).toBe("")
  expect(writes).not.toHaveBeenCalled()
})

it("guides a signed-out invite recipient to sign in and reopen the original email", async () => {
  document.body.innerHTML = `<p data-company></p><form hidden><button>Join</button></form><p role="alert"></p>`
  history.replaceState(null, "", "/auth/invite#token=n1.invite.synthetic")
  const fetcher = vi.fn(async (url: string) => {
    if (url !== "/api/neev/session") throw new Error(`Unexpected fetch ${url}`)
    return new Response(JSON.stringify({ code: "unauthorized" }), {
      status: 401,
      headers: { "Content-Type": "application/json" },
    })
  })
  vi.stubGlobal("fetch", fetcher)
  window.eval(
    readFileSync(join(process.cwd(), "public/neev-auth/invite.js"), "utf8")
  )
  await vi.waitFor(() =>
    expect(document.querySelector('[role="alert"]')).toHaveTextContent(
      /sign in.*reopen the original invitation/i
    )
  )
  expect(fetcher).toHaveBeenCalledTimes(1)
  expect(location.hash).toBe("")
})
