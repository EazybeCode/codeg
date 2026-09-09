import { render, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

const mode = vi.hoisted(() => ({ value: "unknown" }))
const router = vi.hoisted(() => ({ replace: vi.fn() }))
vi.mock("@/components/team/team-mode-boundary", () => ({
  useTeamMode: () => mode.value,
}))
vi.mock("next/navigation", () => ({ useRouter: () => router }))

import Page from "./page"

beforeEach(() => {
  vi.clearAllMocks()
  localStorage.clear()
  mode.value = "unknown"
})

describe("root page mode entry", () => {
  it("mounts no legacy effect while mode is unknown", async () => {
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
