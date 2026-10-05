import { vi } from "vitest"
import { json } from "./test-helpers"

export type TestMember = {
  userId: number
  displayName: string
  avatarUrl: string | null
  role: "admin" | "member"
}

/**
 * Serve `GET /api/neev/organization/directory` from `members`, whatever query
 * string the client builds: `q` filters by name, `limit` pages, `after` is an
 * opaque cursor (the next index). Install AFTER `mockNativeSession`.
 */
export function serveDirectory(members: TestMember[]) {
  const fetcher = globalThis.fetch as unknown as ReturnType<typeof vi.fn>
  const original = fetcher.getMockImplementation()!
  const queries: Record<string, string | null>[] = []
  fetcher.mockImplementation(async (path: string, init: RequestInit = {}) => {
    if (
      (init.method ?? "GET") === "GET" &&
      path.startsWith("/api/neev/organization/directory")
    ) {
      const url = new URL(path, "http://neev.test")
      const q = url.searchParams.get("q")
      const limit = Number(url.searchParams.get("limit") ?? 50)
      const after = url.searchParams.get("after")
      queries.push({ q, after })
      const matching = members.filter(
        (member) =>
          !q || member.displayName.toLowerCase().includes(q.toLowerCase())
      )
      const start = after ? Number(after.replace("c", "")) : 0
      const page = matching.slice(start, start + limit)
      const next = start + limit < matching.length ? `c${start + limit}` : null
      return json({ members: page, nextAfter: next })
    }
    return original(path, init)
  })
  return queries
}

export const ANA: TestMember = {
  userId: 2,
  displayName: "Ana Lima",
  avatarUrl: null,
  role: "member",
}
export const BEN: TestMember = {
  userId: 3,
  displayName: "Ben Okafor",
  avatarUrl: null,
  role: "admin",
}

export const DESIGN_TEAM = {
  id: "aaaaaaaa-1111-4111-8111-111111111111",
  name: "Design",
  memberCount: 2,
  revision: "1",
  canManage: true,
}
