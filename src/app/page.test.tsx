import { render, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

const router = vi.hoisted(() => ({ replace: vi.fn() }))
vi.mock("next/navigation", () => ({ useRouter: () => router }))
const mode = vi.hoisted(() => ({ value: "operator" }))
vi.mock("@/components/team/team-mode-boundary", () => ({
  useTeamMode: () => mode.value,
}))

import Page from "./page"

const reply = (status: number, challenge?: string) => ({
  ok: status >= 200 && status < 300,
  status,
  headers: new Headers(challenge ? { "WWW-Authenticate": challenge } : {}),
})

let fetchMock: ReturnType<typeof vi.fn>

beforeEach(() => {
  mode.value = "operator"
  router.replace.mockClear()
  localStorage.setItem("codeg_token", "tok")
  fetchMock = vi.fn()
  vi.stubGlobal("fetch", fetchMock)
})

afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
})

describe("root page token check", () => {
  it("asks a proxy for a 401 instead of a redirect", async () => {
    fetchMock.mockResolvedValue(reply(200))
    render(<Page />)
    await waitFor(() => expect(router.replace).toHaveBeenCalled())
    expect(fetchMock.mock.calls[0][1].headers).toMatchObject({
      Authorization: "Bearer tok",
      "X-Requested-With": "XMLHttpRequest",
    })
  })

  it("clears the token and goes to /login when Codeg rejects it", async () => {
    fetchMock.mockResolvedValue(reply(401, 'Bearer realm="codeg"'))
    render(<Page />)
    await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
    expect(localStorage.getItem("codeg_token")).toBeNull()
  })

  it("keeps the token when the 401 came from a proxy in front of Codeg", async () => {
    fetchMock.mockResolvedValue(
      reply(401, 'Cloudflare-Access resource_metadata="https://x.test/m"')
    )
    render(<Page />)
    // The workspace's connection guard offers the reload from there.
    await waitFor(() =>
      expect(router.replace).toHaveBeenCalledWith("/workspace")
    )
    expect(localStorage.getItem("codeg_token")).toBe("tok")
  })
})

describe("root page without a token", () => {
  beforeEach(() => localStorage.removeItem("codeg_token"))

  it("enters the workspace when a proxy in front of Codeg admits the browser", async () => {
    fetchMock.mockResolvedValue(reply(200))
    render(<Page />)
    await waitFor(() =>
      expect(router.replace).toHaveBeenCalledWith("/workspace")
    )
    expect(localStorage.getItem("codeg_proxy_auth")).toBe("1")
  })

  it("asks for the token when the server wants one", async () => {
    fetchMock.mockResolvedValue(reply(401, 'Bearer realm="codeg"'))
    render(<Page />)
    await waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login"))
  })
})

describe("root page mode entry", () => {
  it("mounts no legacy effect while mode is unknown", async () => {
    mode.value = "unknown"
    localStorage.setItem("codeg_token", "legacy-token")
    const fetchSpy = vi.spyOn(globalThis, "fetch")
    render(<Page />)
    await waitFor(() => expect(router.replace).not.toHaveBeenCalled())
    expect(fetchSpy).not.toHaveBeenCalled()
    expect(localStorage.getItem("codeg_token")).toBe("legacy-token")
    fetchSpy.mockRestore()
  })

  it("redirects team entry without legacy token validation", async () => {
    mode.value = "team"
    localStorage.setItem("codeg_token", "legacy-token")
    const fetchSpy = vi.spyOn(globalThis, "fetch")
    render(<Page />)
    await waitFor(() =>
      expect(router.replace).toHaveBeenCalledWith("/workspace")
    )
    expect(fetchSpy).not.toHaveBeenCalled()
    expect(localStorage.getItem("codeg_token")).toBe("legacy-token")
    fetchSpy.mockRestore()
  })

  it("preserves explicit operator token validation", async () => {
    mode.value = "operator"
    localStorage.setItem("codeg_token", "operator-token")
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(new Response("{}", { status: 200 }))
    render(<Page />)
    await waitFor(() =>
      expect(router.replace).toHaveBeenCalledWith("/workspace")
    )
    expect(fetchSpy).toHaveBeenCalledWith(
      "/api/health",
      expect.objectContaining({ method: "POST" })
    )
    fetchSpy.mockRestore()
  })

  it("preserves the native redirect without reading a browser token", async () => {
    mode.value = "operator"
    ;(
      window as Window & { __TAURI_INTERNALS__?: unknown }
    ).__TAURI_INTERNALS__ = {}
    const storageSpy = vi.spyOn(Storage.prototype, "getItem")
    const fetchSpy = vi.spyOn(globalThis, "fetch")
    render(<Page />)
    await waitFor(() =>
      expect(router.replace).toHaveBeenCalledWith("/workspace")
    )
    expect(storageSpy).not.toHaveBeenCalledWith("codeg_token")
    expect(fetchSpy).not.toHaveBeenCalled()
    storageSpy.mockRestore()
    fetchSpy.mockRestore()
    delete (window as Window & { __TAURI_INTERNALS__?: unknown })
      .__TAURI_INTERNALS__
  })
})
