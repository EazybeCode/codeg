import { useEffect } from "react"
import { render, screen } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

const mocks = vi.hoisted(() => ({
  mode: "unknown" as "unknown" | "team" | "operator",
  legacyEffect: vi.fn(),
}))

vi.mock("@/components/team/team-mode-boundary", () => ({
  useTeamMode: () => mocks.mode,
}))
vi.mock("@/components/team/team-organization-settings", () => ({
  TeamOrganizationSettingsPage: () => <p>supported organization settings</p>,
}))
vi.mock("@/components/settings/settings-shell", () => ({
  SettingsShell: ({ children }: { children: React.ReactNode }) => (
    <div>operator settings {children}</div>
  ),
}))
vi.mock("@/contexts/remote-connection-context", () => ({
  RemoteConnectionGate: ({ children }: { children: React.ReactNode }) =>
    children,
}))
vi.mock("@/components/providers/update-provider", () => ({
  UpdateProvider: ({ children }: { children: React.ReactNode }) => children,
}))

import SettingsLayout from "./layout"

function LegacyChild() {
  useEffect(() => {
    mocks.legacyEffect()
  }, [])
  return <p>unsupported agents settings</p>
}

beforeEach(() => {
  vi.clearAllMocks()
  mocks.mode = "unknown"
})

describe("settings layout mode boundary", () => {
  it("mounts nothing and starts no legacy child effects while mode is unknown", () => {
    const { container } = render(
      <SettingsLayout>
        <LegacyChild />
      </SettingsLayout>
    )
    expect(container).toBeEmptyDOMElement()
    expect(mocks.legacyEffect).not.toHaveBeenCalled()
  })

  it("renders only the supported organization settings view in team mode", () => {
    mocks.mode = "team"
    render(
      <SettingsLayout>
        <LegacyChild />
      </SettingsLayout>
    )
    expect(screen.getByText("supported organization settings")).toBeVisible()
    expect(screen.queryByText("unsupported agents settings")).toBeNull()
    expect(mocks.legacyEffect).not.toHaveBeenCalled()
  })

  it("preserves the operator settings layout and child", () => {
    mocks.mode = "operator"
    render(
      <SettingsLayout>
        <LegacyChild />
      </SettingsLayout>
    )
    expect(screen.getByText(/operator settings/)).toBeVisible()
    expect(screen.getByText("unsupported agents settings")).toBeVisible()
    expect(mocks.legacyEffect).toHaveBeenCalledOnce()
  })
})
