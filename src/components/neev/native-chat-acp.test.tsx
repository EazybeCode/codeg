// A team chat's agent replay asks only about providers the server offers. An
// ACP provider the operator has not gated on is refused like an unknown name
// (400), so asking about one would fail the replay for everyone.
import { screen, render, waitFor } from "@testing-library/react"
import { afterEach, beforeEach, expect, it, vi } from "vitest"
import { NativeChat } from "./native-chat"
import { NativeBoundary } from "./native-boundary"
import {
  json,
  mockNativeSession,
  nativeCapabilities,
  NativeTestRoot,
} from "./test-helpers"
import type { NativeResource } from "@/lib/neev-content-client"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

const resource: NativeResource = {
  id: "11111111-1111-4111-8111-111111111111",
  orgId: 3,
  nodeId: 4,
  kind: "chat",
  creatorId: 1,
  title: "Design chat",
  visibility: "private",
  originChatId: null,
  authRevision: "1",
  contentRevision: "0",
  archived: false,
  createdAt: 0,
  updatedAt: 0,
}
const lookup = (provider: string) =>
  `GET /api/neev/chats/${resource.id}/agent?provider=${provider}`

function row(provider: string) {
  return {
    provider,
    state: "connected",
    accountLabel: "test",
    checkedAt: 1_700_000_000,
    errorCode: null,
  }
}

function mountWith(listed: string[]) {
  const asked: string[] = []
  const routes: Record<string, () => Response> = {
    "GET /api/workos/capabilities": () =>
      json({ ...nativeCapabilities, runtimeEnabled: true }),
    [`GET /api/neev/resources/${resource.id}`]: () => json(resource),
    [`GET /api/neev/chats/${resource.id}/messages`]: () =>
      json({ items: [], nextAfter: null }),
    "GET /api/neev/agent/connections": () =>
      json({ runtimeEnabled: true, connections: listed.map(row) }),
  }
  for (const provider of ["claude", "codex", "cursor", "gemini", "opencode"])
    routes[lookup(provider)] = () => {
      asked.push(provider)
      return listed.includes(provider)
        ? json({ code: "not_found" }, 404)
        : json({ code: "invalid" }, 400)
    }
  const mock = mockNativeSession(undefined, routes)
  render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeChat
          resource={resource}
          onBack={() => {}}
          onChanged={() => {}}
        />
      </NativeBoundary>
    </NativeTestRoot>
  )
  return { asked, mock }
}

beforeEach(() => localStorage.clear())
afterEach(() => vi.unstubAllGlobals())

it("never asks about an ACP provider the server does not offer", async () => {
  const { asked, mock } = mountWith(["claude", "codex"])
  await screen.findByRole("textbox", { name: "Message" })
  await waitFor(() => expect(asked).toEqual(["claude", "codex"]))
  expect(mock.unknown).toEqual([])
})

it("replays a gated-on ACP provider's holder once the server lists it", async () => {
  const { asked } = mountWith(["claude", "codex", "gemini"])
  await screen.findByRole("textbox", { name: "Message" })
  await waitFor(() => expect(asked).toContain("gemini"))
  expect(asked).not.toContain("cursor")
  expect(asked).not.toContain("opencode")
})

it("drops a remembered provider the server no longer offers", async () => {
  // Remembered from when Gemini was gated on; now it is not, and nothing is
  // connected, so no "connected" provider exists to move to.
  localStorage.setItem(
    `neev:chat-composer:1:${resource.id}`,
    JSON.stringify({ mode: "agent", provider: "gemini" })
  )
  const { asked } = mountWith(["claude", "codex"])
  await screen.findByRole("textbox", { name: "Message" })
  await waitFor(() => expect(asked).toEqual(["claude", "codex"]))
  expect(asked).not.toContain("gemini")
})
