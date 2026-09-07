import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import userEvent from "@testing-library/user-event"
import { beforeEach, describe, expect, it, vi } from "vitest"

// MUST mock the hook BEFORE importing the component under test.
vi.mock("@/hooks/use-acp-agents", () => ({
  useAcpAgents: vi.fn(),
}))

import { AgentSelector } from "./agent-selector"
import { useAcpAgents } from "@/hooks/use-acp-agents"
import enMessages from "@/i18n/messages/en.json"
import { getAgentLabel } from "@/lib/custom-agents"
import type { AcpAgentInfo, AgentType } from "@/lib/types"

const mockUseAcpAgents = vi.mocked(useAcpAgents)

function agent(
  agentType: AgentType,
  overrides: Partial<AcpAgentInfo> = {}
): AcpAgentInfo {
  return {
    agent_type: agentType,
    skills_capable: true,
    registry_id: `${agentType}-registry`,
    registry_version: null,
    supports_custom_version: false,
    name: agentType,
    description: "",
    available: true,
    distribution_type: "system",
    is_acp_adapter: false,
    custom_source: null,
    enabled: true,
    sort_order: 0,
    installed_version: null,
    host_tools_agent_mode: false,
    env: {},
    config_json: null,
    config_file_path: null,
    opencode_auth_json: null,
    codex_auth_json: null,
    codex_config_toml: null,
    codex_model_catalog: null,
    codex_sandbox_settings: null,
    grok_config_toml: null,
    grok_settings: null,
    cline_secrets_json: null,
    hermes_config_yaml: null,
    cursor_cli_config_json: null,
    cursor_settings: null,
    model_provider_id: null,
    icon_url: null,
    ...overrides,
  }
}

function renderWithIntl(ui: React.ReactElement) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      {ui}
    </NextIntlClientProvider>
  )
}

const MANY_AGENTS: AgentType[] = [
  "claude_code",
  "codex",
  "open_code",
  "gemini",
  "open_claw",
  "cline",
  "hermes",
  "code_buddy",
  "kimi_code",
  "pi",
  "grok",
  "cursor",
  "deepseek",
]

beforeEach(() => {
  mockUseAcpAgents.mockReset()
  vi.restoreAllMocks()
})

describe("AgentSelector", () => {
  it("shows the empty state when no enabled agents are returned", () => {
    mockUseAcpAgents.mockReturnValue({
      agents: [],
      fresh: true,
      refresh: async () => {},
    })
    const onOpenSettings = vi.fn()
    renderWithIntl(
      <AgentSelector
        onSelect={() => {}}
        onOpenAgentsSettings={onOpenSettings}
      />
    )
    expect(screen.getByText("No enabled agents")).toBeInTheDocument()
    fireEvent.click(
      screen.getByRole("button", { name: "Open Agents settings" })
    )
    expect(onOpenSettings).toHaveBeenCalledTimes(1)
  })

  it("clicking an available agent invokes onSelect with its agent_type", async () => {
    mockUseAcpAgents.mockReturnValue({
      agents: [agent("claude_code"), agent("codex")],
      fresh: true,
      refresh: async () => {},
    })
    const onSelect = vi.fn()
    renderWithIntl(
      <AgentSelector
        defaultAgentType="claude_code"
        onSelect={onSelect}
        onFallback={vi.fn()}
      />
    )
    await userEvent.click(screen.getByRole("button"))
    await userEvent.click(
      await screen.findByRole("menuitem", { name: /Codex/ })
    )
    expect(onSelect).toHaveBeenCalledTimes(1)
    expect(onSelect).toHaveBeenCalledWith("codex")
  })

  it("fires onFallback (not onSelect) when the preferred agent is unavailable", async () => {
    // Regression guard: this is the fix path for stale-default resolution.
    // If onFallback is supplied it must receive the substitute; onSelect
    // must NOT fire, because the caller treats onSelect as a confirmed user
    // choice (which would mask a downstream correction effect).
    mockUseAcpAgents.mockReturnValue({
      agents: [
        agent("claude_code", { available: false }),
        agent("codex"),
        agent("gemini"),
      ],
      fresh: true,
      refresh: async () => {},
    })
    const onSelect = vi.fn()
    const onFallback = vi.fn()
    renderWithIntl(
      <AgentSelector
        defaultAgentType="claude_code"
        onSelect={onSelect}
        onFallback={onFallback}
      />
    )
    await waitFor(() => {
      expect(onFallback).toHaveBeenCalledTimes(1)
    })
    expect(onFallback).toHaveBeenCalledWith("codex")
    expect(onSelect).not.toHaveBeenCalled()
  })

  it("falls back through onSelect when onFallback is not provided (legacy)", async () => {
    // Without onFallback, the auto-pick must still surface as onSelect for
    // backwards compatibility with older call sites.
    mockUseAcpAgents.mockReturnValue({
      agents: [agent("codex"), agent("gemini")],
      fresh: true,
      refresh: async () => {},
    })
    const onSelect = vi.fn()
    renderWithIntl(
      <AgentSelector defaultAgentType={undefined} onSelect={onSelect} />
    )
    await waitFor(() => {
      expect(onSelect).toHaveBeenCalled()
    })
    expect(onSelect).toHaveBeenCalledWith("codex")
  })

  it("keeps the icon wrapper a flex box so the mark stays vertically centered", () => {
    // jsdom has no layout engine, so this guards the cause rather than the
    // symptom: as a bare inline box the wrapper lays the icon out on a text
    // baseline and grows by the strut's descender (20px around a 16px mark),
    // which pushes every icon ~2px above the pill's centerline while the
    // label sits dead center. Measured in a real engine; assert the class
    // here so a future markup cleanup can't silently drop it.
    mockUseAcpAgents.mockReturnValue({
      agents: [agent("codex")],
      fresh: true,
      refresh: async () => {},
    })
    renderWithIntl(
      <AgentSelector defaultAgentType="codex" onSelect={() => {}} />
    )
    const wrapper = screen.getByRole("button").firstElementChild
    expect(wrapper).toHaveClass("inline-flex")
  })

  it("uses one trigger for many agents and exposes every enabled choice", async () => {
    mockUseAcpAgents.mockReturnValue({
      agents: MANY_AGENTS.map((type) => agent(type)),
      fresh: true,
      refresh: async () => {},
    })
    const onSelect = vi.fn()
    renderWithIntl(
      <AgentSelector defaultAgentType="cursor" onSelect={onSelect} />
    )
    expect(screen.getAllByRole("button")).toHaveLength(1)
    expect(screen.getByRole("button")).toHaveTextContent(
      getAgentLabel("cursor")
    )
    await userEvent.click(screen.getByRole("button"))
    for (const type of MANY_AGENTS)
      expect(
        await screen.findByRole("menuitem", {
          name: new RegExp(getAgentLabel(type)),
        })
      ).toBeVisible()
    await userEvent.click(
      screen.getByRole("menuitem", {
        name: new RegExp(getAgentLabel("deepseek")),
      })
    )
    expect(onSelect).toHaveBeenCalledWith("deepseek")
  })

  it("keeps unavailable choices disabled and respects a disabled selector", async () => {
    mockUseAcpAgents.mockReturnValue({
      agents: [agent("claude_code"), { ...agent("codex"), available: false }],
      fresh: true,
      refresh: async () => {},
    })
    const onSelect = vi.fn()
    const view = renderWithIntl(
      <AgentSelector defaultAgentType="claude_code" onSelect={onSelect} />
    )
    await userEvent.click(screen.getByRole("button"))
    const unavailable = await screen.findByRole("menuitem", { name: /Codex/ })
    expect(unavailable).toHaveAttribute("data-disabled")
    await userEvent.click(unavailable)
    expect(onSelect).not.toHaveBeenCalled()
    view.unmount()
    renderWithIntl(
      <AgentSelector
        disabled
        defaultAgentType="claude_code"
        onSelect={onSelect}
      />
    )
    expect(screen.getByRole("button")).toBeDisabled()
  })

  it("notifies onAgentsLoaded with the current agent list", async () => {
    const codex = agent("codex")
    mockUseAcpAgents.mockReturnValue({
      agents: [codex],
      fresh: true,
      refresh: async () => {},
    })
    const onAgentsLoaded = vi.fn()
    renderWithIntl(
      <AgentSelector
        defaultAgentType="codex"
        onSelect={() => {}}
        onAgentsLoaded={onAgentsLoaded}
      />
    )
    await waitFor(() => {
      expect(onAgentsLoaded).toHaveBeenCalled()
    })
    const calls = onAgentsLoaded.mock.calls
    const lastCall = calls[calls.length - 1]?.[0]
    expect(lastCall).toEqual([codex])
  })
})
