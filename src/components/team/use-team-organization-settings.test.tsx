import { act, renderHook, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

const client = vi.hoisted(() => ({
  loadTeamOrganizationSettings: vi.fn(),
  updateTeamOrganizationSettings: vi.fn(),
}))
const auth = vi.hoisted(() => ({
  value: {} as Record<string, unknown>,
  notifyTeamAuthChanged: vi.fn(),
}))

vi.mock("@/lib/team-client", () => {
  class TeamClientError extends Error {
    constructor(
      public readonly kind: string,
      public readonly status?: number
    ) {
      super("Team request failed")
    }
  }
  return { ...client, TeamClientError }
})
vi.mock("./team-auth-provider", () => ({
  useTeamAuth: () => auth.value,
  notifyTeamAuthChanged: auth.notifyTeamAuthChanged,
}))

import { TeamClientError } from "@/lib/team-client"
import { useTeamOrganizationSettings } from "./use-team-organization-settings"

const organizationA = {
  id: 21,
  login: "org-a",
  name: "Organization A",
  avatarUrl: null,
  canEdit: true,
}
const organizationB = {
  id: 22,
  login: "org-b",
  name: "Organization B",
  avatarUrl: null,
  canEdit: true,
}
const sessionA = {
  user: { id: 11, login: "person-a", name: "Person A", avatarUrl: null },
  organization: organizationA,
  role: "admin" as const,
  scope: "fingerprint-a",
  csrf: "csrf-a",
}
const sessionA2 = {
  ...sessionA,
  user: { id: 12, login: "person-b", name: "Person B", avatarUrl: null },
  scope: "fingerprint-b",
  csrf: "csrf-b",
}
const sessionB = {
  user: { id: 13, login: "person-c", name: "Person C", avatarUrl: null },
  organization: organizationB,
  role: "admin" as const,
  scope: "fingerprint-c",
  csrf: "csrf-c",
}

function setAuthenticated(
  session = sessionA,
  scopeKey = `${session.scope}:${session.organization.id}:${session.user.id}`
) {
  auth.value = {
    status: "authenticated",
    session,
    scopeKey,
    refresh: vi.fn().mockResolvedValue(undefined),
  }
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

beforeEach(() => {
  vi.clearAllMocks()
  setAuthenticated()
})

describe("useTeamOrganizationSettings", () => {
  it("refuses to display an organization outside the current verified scope", async () => {
    client.loadTeamOrganizationSettings.mockResolvedValueOnce(organizationB)
    const { result } = renderHook(() => useTeamOrganizationSettings())

    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.organization).toBeNull()
    expect(result.current.error).toBe("load")
  })

  it("preserves the same-scope draft through revalidation and a retryable load failure", async () => {
    client.loadTeamOrganizationSettings.mockResolvedValueOnce(organizationA)
    const { result, rerender } = renderHook(() => useTeamOrganizationSettings())
    await waitFor(() =>
      expect(result.current.organization).toEqual(organizationA)
    )
    act(() => result.current.setName("Unsent Atlas"))

    auth.value = {
      ...auth.value,
      status: "loading",
      session: null,
      scopeKey: null,
    }
    rerender()
    expect(result.current.name).toBe("")

    client.loadTeamOrganizationSettings.mockRejectedValueOnce(
      new TeamClientError("unavailable", 503)
    )
    setAuthenticated()
    rerender()
    await waitFor(() => expect(result.current.error).toBe("load"))
    expect(result.current.name).toBe("Unsent Atlas")
    expect(result.current.organization).toEqual(organizationA)
  })

  it("clears a draft for a different session even when the organization is unchanged", async () => {
    client.loadTeamOrganizationSettings
      .mockResolvedValueOnce(organizationA)
      .mockResolvedValueOnce(organizationA)
    const { result, rerender } = renderHook(() => useTeamOrganizationSettings())
    await waitFor(() =>
      expect(result.current.organization).toEqual(organizationA)
    )
    act(() => result.current.setName("Private draft"))

    setAuthenticated(
      sessionA2,
      `${sessionA2.scope}:${sessionA2.organization.id}:${sessionA2.user.id}`
    )
    rerender()

    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.name).toBe("Organization A")
  })

  it("ignores a late save after authority changes to another organization", async () => {
    const save = deferred<typeof organizationA>()
    client.loadTeamOrganizationSettings
      .mockResolvedValueOnce(organizationA)
      .mockResolvedValueOnce(organizationB)
    client.updateTeamOrganizationSettings.mockReturnValueOnce(save.promise)
    const { result, rerender } = renderHook(() => useTeamOrganizationSettings())
    await waitFor(() =>
      expect(result.current.organization).toEqual(organizationA)
    )
    act(() => result.current.setName("Saved too late"))
    act(() => void result.current.save())

    setAuthenticated(sessionB)
    rerender()
    await waitFor(() =>
      expect(result.current.organization).toEqual(organizationB)
    )
    await act(async () =>
      save.resolve({ ...organizationA, name: "Saved too late" })
    )

    expect(result.current.organization).toEqual(organizationB)
    expect(result.current.name).toBe("Organization B")
    expect(auth.notifyTeamAuthChanged).not.toHaveBeenCalled()
  })

  it("aborts and ignores stale file readers on replacement, save, removal, logout, and unmount", async () => {
    const readers: DeferredFileReader[] = []
    class DeferredFileReader {
      result: string | ArrayBuffer | null = null
      onload: null | (() => void) = null
      onerror: null | (() => void) = null
      onabort: null | (() => void) = null
      abort = vi.fn()
      readAsDataURL(file: File) {
        this.result = `data:${file.type};base64,${file.name}`
        readers.push(this)
      }
    }
    vi.stubGlobal("FileReader", DeferredFileReader)
    client.loadTeamOrganizationSettings.mockResolvedValueOnce(organizationA)
    const { result, rerender, unmount } = renderHook(() =>
      useTeamOrganizationSettings()
    )
    await waitFor(() =>
      expect(result.current.organization).toEqual(organizationA)
    )

    act(() =>
      result.current.uploadLogo(new File(["1"], "first", { type: "image/png" }))
    )
    act(() =>
      result.current.uploadLogo(
        new File(["2"], "second", { type: "image/webp" })
      )
    )
    expect(readers[0].abort).toHaveBeenCalledOnce()
    act(() => readers[0].onload?.())
    act(() => readers[1].onload?.())
    expect(result.current.logo).toEqual({
      action: "upload",
      data: "data:image/webp;base64,second",
    })

    act(() =>
      result.current.uploadLogo(new File(["3"], "third", { type: "image/png" }))
    )
    await act(async () => result.current.save())
    expect(readers[2].abort).toHaveBeenCalledOnce()
    act(() => readers[2].onload?.())
    expect(result.current.logo).toEqual({
      action: "upload",
      data: "data:image/webp;base64,second",
    })

    act(() =>
      result.current.uploadLogo(
        new File(["4"], "fourth", { type: "image/png" })
      )
    )
    act(() => result.current.removeLogo())
    expect(readers[3].abort).toHaveBeenCalledOnce()
    act(() => readers[3].onload?.())
    expect(result.current.logo).toEqual({ action: "remove" })

    act(() =>
      result.current.uploadLogo(new File(["5"], "fifth", { type: "image/png" }))
    )
    auth.value = {
      ...auth.value,
      status: "unauthenticated",
      session: null,
      scopeKey: null,
    }
    rerender()
    expect(readers[4].abort).toHaveBeenCalledOnce()
    act(() => readers[4].onload?.())
    expect(result.current.organization).toBeNull()
    expect(result.current.logo).toEqual({ action: "keep" })

    client.loadTeamOrganizationSettings.mockResolvedValueOnce(organizationA)
    setAuthenticated()
    rerender()
    await waitFor(() =>
      expect(result.current.organization).toEqual(organizationA)
    )
    act(() =>
      result.current.uploadLogo(new File(["6"], "sixth", { type: "image/png" }))
    )
    unmount()
    expect(readers[5].abort).toHaveBeenCalledOnce()
    act(() => readers[5].onload?.())
    vi.unstubAllGlobals()
  })

  it("aborts an in-flight organization load when verified scope disappears", () => {
    client.loadTeamOrganizationSettings.mockReturnValueOnce(
      new Promise(() => {})
    )
    const { rerender } = renderHook(() => useTeamOrganizationSettings())
    const signal = client.loadTeamOrganizationSettings.mock
      .calls[0][0] as AbortSignal
    expect(signal.aborted).toBe(false)

    auth.value = {
      ...auth.value,
      status: "loading",
      session: null,
      scopeKey: null,
    }
    rerender()
    expect(signal.aborted).toBe(true)
  })

  it("clears a rejected 401 draft and refreshes authentication without resubmitting", async () => {
    client.loadTeamOrganizationSettings.mockResolvedValueOnce(organizationA)
    client.updateTeamOrganizationSettings.mockRejectedValueOnce(
      new TeamClientError("unauthorized", 401)
    )
    const { result } = renderHook(() => useTeamOrganizationSettings())
    await waitFor(() =>
      expect(result.current.organization).toEqual(organizationA)
    )
    act(() => result.current.setName("Do not resubmit"))
    await act(async () => result.current.save())

    expect(result.current.organization).toBeNull()
    expect(result.current.name).toBe("")
    expect(auth.value.refresh).toHaveBeenCalledOnce()
    expect(client.updateTeamOrganizationSettings).toHaveBeenCalledOnce()
  })

  it("retains scoped save success through a real refresh transition", async () => {
    const savedOrganization = { ...organizationA, name: "Saved Atlas" }
    client.loadTeamOrganizationSettings
      .mockResolvedValueOnce(organizationA)
      .mockResolvedValueOnce(savedOrganization)
    client.updateTeamOrganizationSettings.mockResolvedValueOnce(
      savedOrganization
    )
    const control = { rerender: () => {} }
    const refresh = vi.fn(async () => {
      auth.value = {
        ...auth.value,
        status: "loading",
        session: null,
        scopeKey: null,
        refresh,
      }
      control.rerender()
    })
    auth.value = { ...auth.value, refresh }
    const rendered = renderHook(() => useTeamOrganizationSettings())
    control.rerender = rendered.rerender
    await waitFor(() =>
      expect(rendered.result.current.organization).toEqual(organizationA)
    )
    act(() => rendered.result.current.setName("Saved Atlas"))

    await act(async () => rendered.result.current.save())
    expect(refresh).toHaveBeenCalledOnce()
    expect(auth.value.status).toBe("loading")

    auth.value = {
      ...auth.value,
      status: "authenticated",
      session: sessionA,
      scopeKey: "fingerprint-a:21:11",
      refresh,
    }
    control.rerender()
    await waitFor(() => expect(rendered.result.current.loading).toBe(false))

    expect(rendered.result.current.organization).toEqual(savedOrganization)
    expect(rendered.result.current.saved).toBe(true)
  })
})
