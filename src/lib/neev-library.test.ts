import { expect, it } from "vitest"
import {
  credentialGuidance,
  githubAuthorization,
  githubConnectionFile,
  integrationStatusDetail,
  integrationStatusMessage,
  unusualGithubToken,
  parseLibraryItem,
  skippedReasonMessage,
  validGithubToken,
} from "./neev-library"

const base = {
  id: "d230e18b-8102-4560-a358-6d8aac05c712",
  name: "HubSpot",
  description: "",
  kind: "mcp",
  enabled: false,
  revision: "1",
  files: [],
  setupNote: null,
}
const integration = {
  transport: "remote",
  hosts: ["mcp.hubspot.com"],
  secretNames: ["HUBSPOT_ACCESS_TOKEN"],
  source: "import",
  notes: [],
  status: {
    state: "ready",
    checkedAt: "1700000000",
    toolCount: 3,
    detail: null,
    revision: "1",
  },
}

it("parses items with, without and null integration", () => {
  expect(parseLibraryItem(base).integration).toBeUndefined()
  expect(parseLibraryItem({ ...base, integration: null }).integration).toBe(
    undefined
  )
  const item = parseLibraryItem({ ...base, integration })
  expect(item.integration?.status?.toolCount).toBe(3)
  expect(
    parseLibraryItem({ ...base, integration: { ...integration, status: null } })
      .integration?.status
  ).toBeNull()
})

it("rejects malformed integrations", () => {
  for (const bad of [
    "x",
    { ...integration, transport: "stdio" },
    { ...integration, hosts: [1] },
    { ...integration, secretNames: "A" },
    { ...integration, status: { ...integration.status, checkedAt: 5 } },
    { ...integration, status: { ...integration.status, state: 1 } },
    { ...integration, status: { ...integration.status, revision: undefined } },
    { ...integration, status: { ...integration.status, revision: "0" } },
  ])
    expect(() => parseLibraryItem({ ...base, integration: bad })).toThrow()
})

it("maps status states to messages", () => {
  expect(integrationStatusMessage(null)).toBe("Not tested yet")
  expect(
    integrationStatusMessage({
      state: "auth_failed",
      checkedAt: "1",
      toolCount: null,
      detail: null,
      revision: "1",
    })
  ).toMatch(/rejected/)
  expect(
    integrationStatusMessage({
      state: "weird",
      checkedAt: "1",
      toolCount: null,
      detail: null,
      revision: "1",
    })
  ).toBe("weird")
})

it("explains the newer states and skipped reasons", () => {
  const status = (state: string) => ({
    state,
    checkedAt: "1",
    toolCount: null,
    detail: null,
    revision: "1",
  })
  expect(integrationStatusMessage(status("forbidden"))).toBe(
    "Credentials were accepted but access is not allowed (403)."
  )
  for (const state of ["session_expired", "busy", "broker_unavailable"])
    expect(integrationStatusMessage(status(state))).not.toBe(state)
  for (const reason of ["unsupported_field", "unsupported_env"])
    expect(skippedReasonMessage(reason)).not.toBe(reason)
})

it("explains the broker's lost-connection, egress and invalid states", () => {
  const status = (state: string) => ({
    state,
    checkedAt: "1",
    toolCount: null,
    detail: null,
    revision: "1",
  })
  expect(integrationStatusMessage(status("connection_lost"))).toMatch(
    /not retried/
  )
  expect(integrationStatusMessage(status("egress_not_configured"))).toMatch(
    /network path/
  )
  expect(integrationStatusMessage(status("invalid"))).toMatch(/Re-import/)
})

it("builds the GitHub remote MCP connection and validates the token shape", async () => {
  expect(validGithubToken(" github_pat_ABC123 ")).toBe("github_pat_ABC123")
  for (const bad of [
    "",
    "   ",
    "two words",
    "tab\there",
    "naïve",
    "x".repeat(8001),
  ])
    expect(validGithubToken(bad)).toBeNull()
  const file = githubConnectionFile("github_pat_ABC123")
  expect(file.name).toBe("github.json")
  const text = await new Promise<string>((resolve, reject) => {
    const reader = new FileReader()
    reader.onerror = () => reject(reader.error)
    reader.onload = () => resolve(String(reader.result))
    reader.readAsText(file)
  })
  expect(JSON.parse(text)).toEqual({
    mcpServers: {
      github: {
        type: "http",
        url: "https://api.githubcopilot.com/mcp/",
        headers: { Authorization: "Bearer github_pat_ABC123" },
      },
    },
  })
})

it("shows the status detail and points credential failures at Update credentials", () => {
  const status = (state: string, detail: string | null = null) => ({
    state,
    checkedAt: "1",
    toolCount: null,
    detail,
    revision: "1",
  })
  expect(integrationStatusDetail(status("auth_failed", "HTTP 400"))).toBe(
    "HTTP 400"
  )
  expect(integrationStatusDetail(status("ready", "  "))).toBeNull()
  expect(integrationStatusDetail(null)).toBeNull()
  for (const state of ["bad_request", "runtime_disabled"])
    expect(integrationStatusMessage(status(state))).not.toBe(state)
  expect(integrationStatusMessage(status("auth_failed"))).not.toMatch(/401/)

  const github = {
    transport: "remote" as const,
    hosts: ["api.githubcopilot.com"],
    secretNames: ["Authorization"],
    source: "import",
    notes: [],
    status: status("auth_failed", "HTTP 400"),
  }
  expect(credentialGuidance(github)).toMatch(/new GitHub token.*HTTP 400/)
  // A status recorded before the broker knew GitHub's 400.
  expect(
    credentialGuidance({
      ...github,
      status: status("protocol_error", "HTTP 400"),
    })
  ).toMatch(/Update credentials/)
  expect(
    credentialGuidance({
      ...github,
      status: status("protocol_error", "HTTP 502"),
    })
  ).toBeNull()
  expect(
    credentialGuidance({ ...github, status: status("forbidden", "HTTP 403") })
  ).toMatch(/refused this access/)
  expect(credentialGuidance({ ...github, status: status("ready") })).toBeNull()
  expect(credentialGuidance({ ...github, status: null })).toBeNull()
  const other = { ...github, hosts: ["mcp.example.com"] }
  expect(credentialGuidance(other)).toBe(
    "Use Update credentials to enter new credentials, then Test again."
  )
  expect(credentialGuidance({ ...other, secretNames: [] })).toBeNull()
})

it("normalises a pasted GitHub token and hints at unusual shapes", () => {
  expect(validGithubToken("Bearer ghp_ABC")).toBe("ghp_ABC")
  expect(validGithubToken("token ghp_ABC")).toBe("ghp_ABC")
  expect(validGithubToken("Bearer two words")).toBeNull()
  expect(githubAuthorization(" bearer github_pat_X ")).toBe(
    "Bearer github_pat_X"
  )
  expect(githubAuthorization("two words")).toBeNull()
  expect(unusualGithubToken("github_pat_X")).toBe(false)
  expect(unusualGithubToken("ghp_X")).toBe(false)
  expect(unusualGithubToken("sk-something")).toBe(true)
})
