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
