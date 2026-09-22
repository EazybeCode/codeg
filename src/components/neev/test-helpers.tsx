import { NextIntlClientProvider } from "next-intl"
import { vi } from "vitest"
import messages from "@/i18n/messages/en.json"
import { NativeAuthProvider } from "./native-auth-provider"
import type { NativeSession } from "@/lib/neev-client"

export const nativeCapabilities = {
  version: 1,
  identityMode: "native",
  runtimeEnabled: false,
  providers: { google: true, email: true, github: false },
  catalog: {
    nodeKinds: ["project", "folder"],
    repositoryDefaults: false,
    chats: true,
    documents: true,
  },
} as const
export const orgSession: Extract<NativeSession, { scope: "organization" }> = {
  user: { id: 1, displayName: "Sagar", avatarUrl: null },
  organization: { id: 3, name: "Neev demo", avatarUrl: null },
  role: "admin",
  scope: "organization",
  csrf: "synthetic-csrf",
}
export const identitySession: NativeSession = {
  ...orgSession,
  organization: null,
  role: null,
  scope: "identity",
}
/**
 * `GET /api/neev/agent/capabilities` exactly as the deployed runner answers it
 * (Claude Code 2.1.267, codex-cli 0.155.1), field for field.
 *
 * Copied from the backend contract rather than simplified: the controls refuse
 * a payload that does not match the documented shape, so a fixture that took
 * liberties would pass tests the real response fails. It carries the two cases
 * that matter — a model whose `efforts` narrow the provider's, and a provider
 * where only one model supports speed.
 */
export const agentCatalog = {
  providers: [
    {
      provider: "claude",
      label: "Claude Code",
      models: [
        {
          id: "default",
          label: "Default",
          default: true,
          supportsSpeed: false,
        },
        { id: "haiku", label: "Haiku", default: false, supportsSpeed: false },
        {
          id: "sonnet",
          label: "Sonnet",
          default: false,
          supportsSpeed: false,
          efforts: ["low", "medium", "high"],
        },
        {
          id: "opus",
          label: "Opus",
          default: false,
          supportsSpeed: true,
          efforts: null,
        },
      ],
      efforts: ["low", "medium", "high", "xhigh", "max"],
      defaultEffort: null,
      supportsSpeed: true,
      speedUnavailableReason:
        "Fast mode runs on Opus. Choose Opus to enable it.",
    },
    {
      provider: "codex",
      label: "Codex",
      models: [
        { id: "default", label: "Default", default: true, supportsSpeed: true },
      ],
      efforts: ["minimal", "low", "medium", "high", "xhigh"],
      defaultEffort: null,
      supportsSpeed: true,
      speedUnavailableReason: "Fast mode is not available for this model.",
    },
  ],
} as const

export type Handler = (init: RequestInit) => Response | Promise<Response>
export function json(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  })
}
export function mockNativeSession(
  session: NativeSession | null = orgSession,
  routes: Record<string, Handler> = {}
) {
  const unknown: string[] = []
  const fetcher = vi.fn(async (path: string, init: RequestInit = {}) => {
    const key = `${init.method ?? "GET"} ${path}`
    if (routes[key]) return routes[key](init)
    if (key === "GET /api/workos/capabilities") return json(nativeCapabilities)
    if (key === "GET /api/neev/session")
      return json(session ?? { code: "unauthorized" }, session ? 200 : 401)
    // The Settings page always mounts the join-code panel; default to "no code
    // yet" so unrelated Settings tests need not mock it (tests can still override).
    if (key === "GET /api/neev/organization/join-code")
      return json({ code: null, disabled: false })
    // Settings always mounts the per-user agent connection panel; default to
    // "no runtime bound" so unrelated Settings tests need not mock it.
    if (key === "GET /api/neev/agent/connections")
      return json({
        runtimeEnabled: false,
        connections: [
          {
            provider: "claude",
            state: "disabled",
            accountLabel: null,
            checkedAt: null,
            errorCode: null,
          },
        ],
      })
    // Every agent composer reads the catalogue to render its controls. Default
    // to the real deployed shape so unrelated tests need not mock it; a test
    // about the controls themselves overrides it with the case it is about.
    if (key === "GET /api/neev/agent/capabilities") return json(agentCatalog)
    unknown.push(key)
    throw new Error(`Unexpected fetch: ${key}`)
  })
  vi.stubGlobal("fetch", fetcher)
  return { fetcher, unknown }
}
export function NativeTestRoot({ children }: { children: React.ReactNode }) {
  return (
    <NextIntlClientProvider locale="en" messages={messages}>
      <NativeAuthProvider>{children}</NativeAuthProvider>
    </NextIntlClientProvider>
  )
}
