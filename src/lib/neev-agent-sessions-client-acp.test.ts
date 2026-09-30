// Cursor, Gemini CLI and OpenCode as the client reads them. Knowing an id is
// not offering it: every assertion here is about accepting what the server
// actually sends for them, and nothing it could not have sent.
import { describe, expect, it } from "vitest"
import {
  parseAgentCapabilities,
  parseAgentConnections,
  parseApproval,
  type NeevAgentProvider,
} from "./neev-agent-sessions-client"
import { NativeClientError } from "./neev-client"
import { viewAgentControls } from "@/components/neev/native-agent-controls"

const CURSOR_URL = "https://cursor.com/loginDeepControl?challenge=x"
const GOOGLE_URL = "https://accounts.google.com/o/oauth2/v2/auth?client_id=x"
const DEVICE_URL = "https://auth.openai.com/codex/device"

const browser = {
  kind: "browser",
  verificationUrl: CURSOR_URL,
  userCode: null,
  needsCode: false,
}
const pasted = {
  kind: "authorizationCode",
  verificationUrl: GOOGLE_URL,
  userCode: null,
  needsCode: true,
}
const device = {
  kind: "device",
  verificationUrl: DEVICE_URL,
  userCode: "WXYZ-12345",
  needsCode: false,
}

describe("ACP sign-in approvals", () => {
  it("reads each provider's own flow", () => {
    expect(parseApproval(browser, "cursor")).toEqual({
      kind: "browser",
      verificationUrl: CURSOR_URL,
    })
    expect(parseApproval(pasted, "gemini")).toEqual({
      kind: "authorization_code",
      verificationUrl: GOOGLE_URL,
      needsCode: true,
    })
    expect(parseApproval(device, "opencode")).toEqual({
      kind: "device",
      verificationUrl: DEVICE_URL,
      userCode: "WXYZ-12345",
    })
  })

  it("refuses a flow or a host that belongs to another provider", () => {
    const refused: [unknown, NeevAgentProvider][] = [
      // Host allowed server-side, but for a different provider.
      [{ ...browser, verificationUrl: GOOGLE_URL }, "cursor"],
      [{ ...pasted, verificationUrl: CURSOR_URL }, "gemini"],
      [
        {
          ...pasted,
          verificationUrl: "https://claude.com/cai/oauth/authorize",
        },
        "gemini",
      ],
      // Right host, wrong flow: the supervisor never produces these.
      [{ ...device, verificationUrl: CURSOR_URL }, "cursor"],
      [browser, "claude"],
      [{ ...browser, verificationUrl: DEVICE_URL }, "codex"],
      [{ ...browser, verificationUrl: DEVICE_URL }, "opencode"],
      [{ ...device, verificationUrl: GOOGLE_URL }, "gemini"],
      // A browser approval has nothing to show and nothing to paste.
      [{ ...browser, userCode: "ABCD" }, "cursor"],
      [{ ...browser, needsCode: true }, "cursor"],
      // Lookalike host.
      [
        { ...browser, verificationUrl: "https://cursor.com.evil.test/" },
        "cursor",
      ],
    ]
    for (const [value, provider] of refused)
      expect(() => parseApproval(value, provider)).toThrow(NativeClientError)
  })
})

describe("ACP providers in the server's lists", () => {
  it("accepts a gated-on provider's connection row", () => {
    const wiring = parseAgentConnections({
      runtimeEnabled: true,
      connections: ["claude", "codex", "cursor", "gemini", "opencode"].map(
        (provider) => ({
          provider,
          state: "not_signed_in",
          accountLabel: null,
          checkedAt: null,
          errorCode: null,
        })
      ),
    })
    expect(wiring.connections.map((row) => row.provider)).toEqual([
      "claude",
      "codex",
      "cursor",
      "gemini",
      "opencode",
    ])
  })

  it("renders exactly the default-only catalogue the server sends", () => {
    // Verbatim `provider_capabilities("gemini")`: the provider default, no
    // levels, no speed. Nothing here may add a model, level or Fast.
    const capabilities = parseAgentCapabilities({
      providers: [
        {
          provider: "gemini",
          label: "Gemini CLI",
          models: [
            {
              id: "default",
              label: "Default",
              default: true,
              supportsSpeed: false,
              efforts: null,
            },
          ],
          efforts: [],
          defaultEffort: null,
          supportsSpeed: false,
          speedUnavailableReason:
            "Fast mode is not available for this provider.",
        },
      ],
    })
    const view = viewAgentControls(capabilities, "gemini", {
      model: "default",
      effort: null,
      fast: false,
    })
    expect(view.catalog?.models.map((model) => model.id)).toEqual(["default"])
    // Only "the provider's own default" — the reasoning menu is disabled.
    expect(view.efforts).toEqual([null])
    expect(view.speed).toBe("provider")
  })
})
