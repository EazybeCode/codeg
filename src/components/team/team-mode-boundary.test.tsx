import { act, render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { hydrateRoot } from "react-dom/client"
import { renderToString } from "react-dom/server"
import { beforeEach, describe, expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"

const mocks = vi.hoisted(() => ({
  environment: "web" as "web" | "tauri",
  loadCapabilities: vi.fn(),
  setBrowserServerMode: vi.fn(),
  getShellTransport: vi.fn(),
}))

vi.mock("@/lib/neev-client", () => ({
  loadServerCapabilities: mocks.loadCapabilities,
}))
vi.mock("@/lib/transport", () => ({
  setBrowserServerMode: mocks.setBrowserServerMode,
  getShellTransport: mocks.getShellTransport,
}))
vi.mock("@/lib/transport/detect", () => ({
  detectEnvironment: () => mocks.environment,
}))

import { TeamModeBoundary } from "./team-mode-boundary"

function showBoundary() {
  return render(boundary())
}

function boundary() {
  return (
    <NextIntlClientProvider locale="en" messages={messages}>
      <TeamModeBoundary
        operator={<p>legacy operator tree</p>}
        team={<ImmediateTeamChild />}
        native={<p>safe native identity tree</p>}
      />
    </NextIntlClientProvider>
  )
}

function ImmediateTeamChild() {
  return <p>safe team tree</p>
}

beforeEach(() => {
  mocks.environment = "web"
  localStorage.clear()
  vi.clearAllMocks()
})

describe("TeamModeBoundary", () => {
  it("publishes native identity mode without mounting legacy branches", async () => {
    mocks.loadCapabilities.mockResolvedValueOnce({
      version: 1,
      mode: "native",
      identityMode: "native",
      runtimeEnabled: false,
    })
    showBoundary()
    expect(await screen.findByText("safe native identity tree")).toBeVisible()
    expect(screen.queryByText("legacy operator tree")).toBeNull()
    expect(screen.queryByText("safe team tree")).toBeNull()
    expect(mocks.setBrowserServerMode).toHaveBeenCalledWith("native")
    expect(mocks.getShellTransport).not.toHaveBeenCalled()
  })
  it("mounts neither branch and starts no legacy transport while capability is unknown", () => {
    mocks.loadCapabilities.mockReturnValue(new Promise(() => {}))
    showBoundary()
    expect(screen.queryByText("legacy operator tree")).toBeNull()
    expect(screen.queryByText("safe team tree")).toBeNull()
    expect(mocks.getShellTransport).not.toHaveBeenCalled()
  })

  it("keeps both branches denied after a failed capability response and offers retry", async () => {
    mocks.loadCapabilities.mockRejectedValueOnce(new Error("offline"))
    showBoundary()
    expect(await screen.findByRole("button", { name: /retry/i })).toBeVisible()
    expect(screen.queryByText("legacy operator tree")).toBeNull()
    expect(screen.queryByText("safe team tree")).toBeNull()
    expect(mocks.setBrowserServerMode).toHaveBeenCalledWith("unknown")
    expect(mocks.getShellTransport).not.toHaveBeenCalled()
  })

  it("publishes team mode and clears the legacy token before mounting its child", async () => {
    localStorage.setItem("codeg_token", "old-shared-token")
    mocks.loadCapabilities.mockResolvedValueOnce({
      version: 1,
      mode: "team",
      runtimeEnabled: false,
    })
    expect(() => showBoundary()).not.toThrow()
    expect(await screen.findByText("safe team tree")).toBeVisible()
    expect(mocks.setBrowserServerMode).toHaveBeenCalledWith("team")
    expect(localStorage.getItem("codeg_token")).toBeNull()
    expect(mocks.getShellTransport).not.toHaveBeenCalled()
  })

  it("publishes operator mode before mounting an immediate transport consumer", async () => {
    mocks.loadCapabilities.mockResolvedValueOnce({
      version: 1,
      mode: "operator",
      runtimeEnabled: true,
    })
    function ImmediateOperatorChild() {
      mocks.getShellTransport()
      return <p>immediate operator</p>
    }
    render(
      <NextIntlClientProvider locale="en" messages={messages}>
        <TeamModeBoundary
          operator={<ImmediateOperatorChild />}
          team={<p>team</p>}
        />
      </NextIntlClientProvider>
    )
    expect(await screen.findByText("immediate operator")).toBeVisible()
    const operatorPublication = mocks.setBrowserServerMode.mock.calls.findIndex(
      ([mode]) => mode === "operator"
    )
    expect(operatorPublication).toBeGreaterThanOrEqual(0)
    expect(
      mocks.setBrowserServerMode.mock.invocationCallOrder[operatorPublication]
    ).toBeLessThan(mocks.getShellTransport.mock.invocationCallOrder[0])
  })

  it("retains the native tree without loading browser capabilities", async () => {
    mocks.environment = "tauri"
    showBoundary()
    expect(await screen.findByText("legacy operator tree")).toBeVisible()
    expect(mocks.loadCapabilities).not.toHaveBeenCalled()
  })

  it("hydrates the exported unknown shell before selecting the native tree", async () => {
    mocks.loadCapabilities.mockReturnValue(new Promise(() => {}))
    mocks.environment = "web"
    const serverHtml = renderToString(boundary())
    const container = document.createElement("div")
    container.innerHTML = serverHtml
    document.body.appendChild(container)
    const recoverableErrors: unknown[] = []

    mocks.environment = "tauri"
    let root: ReturnType<typeof hydrateRoot>
    await act(async () => {
      root = hydrateRoot(container, boundary(), {
        onRecoverableError: (error) => recoverableErrors.push(error),
      })
    })

    expect(recoverableErrors).toEqual([])
    expect(container).toHaveTextContent("legacy operator tree")
    expect(mocks.loadCapabilities).not.toHaveBeenCalled()
    await act(async () => root.unmount())
    container.remove()
  })
})
