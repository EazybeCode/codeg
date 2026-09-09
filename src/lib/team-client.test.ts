import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

import {
  TeamClientError,
  loadCapabilities,
  loadPendingChoices,
  loadTeamOrganizationSettings,
  loadTeamSession,
  logoutTeamSession,
  selectOrganization,
  updateTeamOrganizationSettings,
} from "./team-client"

const fetchMock = vi.fn<typeof fetch>()

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  })
}

function stalledJsonResponse(
  signal: AbortSignal,
  onBodyStart: () => void
): Response {
  return {
    ok: true,
    status: 200,
    json: vi.fn(() => {
      onBodyStart()
      return new Promise((_resolve, reject) => {
        signal.addEventListener(
          "abort",
          () => reject(new DOMException("Aborted", "AbortError")),
          { once: true }
        )
      })
    }),
  } as unknown as Response
}

beforeEach(() => {
  vi.stubGlobal("fetch", fetchMock)
  fetchMock.mockReset()
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

describe("team client", () => {
  it("loads and validates an explicit operator capability without credentials from storage", async () => {
    localStorage.setItem("codeg_token", "legacy-secret")
    fetchMock.mockResolvedValueOnce(
      jsonResponse({ version: 1, mode: "operator", runtimeEnabled: true })
    )

    await expect(loadCapabilities()).resolves.toEqual({
      version: 1,
      mode: "operator",
      runtimeEnabled: true,
    })
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/workos/capabilities",
      expect.objectContaining({
        credentials: "same-origin",
        cache: "no-store",
      })
    )
    const init = fetchMock.mock.calls[0][1] as RequestInit
    expect(init.headers).not.toEqual(
      expect.objectContaining({ Authorization: expect.anything() })
    )
  })

  it.each([
    null,
    {},
    { version: 2, mode: "team", runtimeEnabled: false },
    { version: 1, mode: "legacy", runtimeEnabled: false },
    { version: 1, mode: "team", runtimeEnabled: "false" },
  ])("denies malformed capabilities %#", async (payload) => {
    fetchMock.mockResolvedValueOnce(jsonResponse(payload))
    await expect(loadCapabilities()).rejects.toMatchObject({
      name: "TeamClientError",
      kind: "invalid_response",
    })
  })

  it("keeps GitHub IDs as decimal strings and sends explicit pending CSRF", async () => {
    fetchMock.mockResolvedValueOnce(
      jsonResponse({
        identity: {
          githubUserId: "9007199254740992",
          login: "octo-person",
          name: "Octo Person",
          avatarUrl: null,
        },
        organizations: [
          {
            githubOrgId: "9223372036854775807",
            login: "octo-org",
            avatarUrl: null,
            role: "admin",
          },
        ],
        csrf: "pending-csrf",
      })
    )

    const pending = await loadPendingChoices()
    expect(pending.identity.githubUserId).toBe("9007199254740992")
    expect(pending.organizations[0].githubOrgId).toBe("9223372036854775807")

    fetchMock.mockResolvedValueOnce(jsonResponse({ redirect: "/workspace" }))
    await selectOrganization("9223372036854775807", "pending-csrf")
    expect(fetchMock).toHaveBeenLastCalledWith(
      "/api/team/onboarding/select",
      expect.objectContaining({
        method: "POST",
        credentials: "same-origin",
        cache: "no-store",
        headers: {
          "Content-Type": "application/json",
          "X-WorkOS-CSRF": "pending-csrf",
        },
        body: JSON.stringify({ githubOrgId: "9223372036854775807" }),
      })
    )
  })

  it.each(["", "0", "01", "1.2", "+1", "9223372036854775808"])(
    "rejects invalid GitHub organization ID %s before fetch",
    async (githubOrgId) => {
      await expect(
        selectOrganization(githubOrgId, "csrf")
      ).rejects.toMatchObject({ kind: "invalid_request" })
      expect(fetchMock).not.toHaveBeenCalled()
    }
  )

  it("loads the exact selected session scope contract", async () => {
    const storedLogo = "data:image/png;base64,aW52ZW50ZWQ="
    fetchMock.mockResolvedValueOnce(
      jsonResponse({
        user: {
          id: 31,
          login: "octo-person",
          name: "Octo Person",
          avatarUrl: null,
        },
        organization: {
          id: 41,
          login: "octo-org",
          name: "Octo Operations",
          avatarUrl: storedLogo,
        },
        role: "member",
        scope: "opaque-fingerprint",
        csrf: "session-csrf",
      })
    )

    await expect(loadTeamSession()).resolves.toMatchObject({
      user: { id: 31, login: "octo-person" },
      organization: { id: 41, login: "octo-org", avatarUrl: storedLogo },
      scope: "opaque-fingerprint",
    })
  })

  it("aborts a team request after the 65 second client budget", async () => {
    vi.useFakeTimers()
    fetchMock.mockImplementationOnce((_input, init) => {
      return new Promise((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => {
          reject(new DOMException("Aborted", "AbortError"))
        })
      })
    })

    const pending = loadCapabilities()
    const rejection = expect(pending).rejects.toMatchObject({
      kind: "unavailable",
    })
    await vi.advanceTimersByTimeAsync(64_999)
    expect(fetchMock.mock.calls[0][1]?.signal).not.toHaveProperty(
      "aborted",
      true
    )
    await vi.advanceTimersByTimeAsync(1)
    await rejection
  })

  it("keeps the 65 second budget active while the JSON body stalls", async () => {
    vi.useFakeTimers()
    let requestSignal: AbortSignal | undefined
    let bodyStarted!: () => void
    const started = new Promise<void>((resolve) => {
      bodyStarted = resolve
    })
    fetchMock.mockImplementationOnce((_input, init) => {
      requestSignal = init?.signal ?? undefined
      return Promise.resolve(stalledJsonResponse(requestSignal!, bodyStarted))
    })

    const pending = loadCapabilities()
    const rejection = pending.catch((error: unknown) => error)
    await started
    await vi.advanceTimersByTimeAsync(64_999)
    expect(requestSignal?.aborted).toBe(false)
    await vi.advanceTimersByTimeAsync(1)

    expect(requestSignal?.aborted).toBe(true)
    await expect(rejection).resolves.toMatchObject({ kind: "unavailable" })
  })

  it("keeps external cancellation active while consuming the JSON body", async () => {
    const external = new AbortController()
    let requestSignal: AbortSignal | undefined
    let bodyStarted!: () => void
    const started = new Promise<void>((resolve) => {
      bodyStarted = resolve
    })
    fetchMock.mockImplementationOnce((_input, init) => {
      requestSignal = init?.signal ?? undefined
      return Promise.resolve(stalledJsonResponse(requestSignal!, bodyStarted))
    })

    const pending = loadTeamSession(external.signal)
    const rejection = pending.catch((error: unknown) => error)
    await started
    external.abort()
    await Promise.resolve()

    expect(requestSignal?.aborted).toBe(true)
    await expect(rejection).resolves.toMatchObject({ kind: "aborted" })
  })

  it("rejects a pre-aborted external signal without issuing fetch", async () => {
    const external = new AbortController()
    external.abort()
    fetchMock.mockResolvedValueOnce(
      jsonResponse({ version: 1, mode: "team", runtimeEnabled: false })
    )

    await expect(loadCapabilities(external.signal)).rejects.toMatchObject({
      kind: "aborted",
    })
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it("uses static client errors instead of reflecting server messages", async () => {
    fetchMock.mockResolvedValueOnce(
      jsonResponse(
        { code: "unavailable", message: "secret upstream detail" },
        503
      )
    )

    const error = await loadTeamSession().catch((value) => value)
    expect(error).toBeInstanceOf(TeamClientError)
    expect(error).toMatchObject({ status: 503, kind: "unavailable" })
    expect(String(error)).not.toContain("secret upstream detail")
  })

  it("posts an empty JSON object with the session-bound CSRF on logout", async () => {
    const response = new Response(null, { status: 204 })
    const json = vi.spyOn(response, "json")
    fetchMock.mockResolvedValueOnce(response)
    await logoutTeamSession("session-csrf")
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/team/logout",
      expect.objectContaining({
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "X-WorkOS-CSRF": "session-csrf",
        },
        body: "{}",
      })
    )
    expect(json).not.toHaveBeenCalled()
  })

  it("loads the current organization with the private request lifetime", async () => {
    const organization = {
      id: 41,
      login: "octo-org",
      name: "Octo Operations",
      avatarUrl: "data:image/png;base64,aW52ZW50ZWQ=",
      canEdit: true,
    }
    fetchMock.mockResolvedValueOnce(jsonResponse(organization))

    await expect(loadTeamOrganizationSettings()).resolves.toEqual(organization)
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/team/organization",
      expect.objectContaining({
        credentials: "same-origin",
        cache: "no-store",
      })
    )
  })

  it.each([
    null,
    {},
    { id: 0, login: "octo-org", name: "Octo", avatarUrl: null, canEdit: true },
    { id: 41, login: 7, name: "Octo", avatarUrl: null, canEdit: true },
    { id: 41, login: "octo-org", name: null, avatarUrl: null, canEdit: true },
    { id: 41, login: "octo-org", name: "Octo", avatarUrl: 7, canEdit: true },
    {
      id: 41,
      login: "octo-org",
      name: "Octo",
      avatarUrl: null,
      canEdit: "yes",
    },
  ])("rejects malformed organization settings %#", async (payload) => {
    fetchMock.mockResolvedValueOnce(jsonResponse(payload))
    await expect(loadTeamOrganizationSettings()).rejects.toMatchObject({
      kind: "invalid_response",
    })
  })

  it("patches only branding with the current session CSRF", async () => {
    const updated = {
      id: 41,
      login: "octo-org",
      name: "Atlas Team",
      avatarUrl: null,
      canEdit: true,
    }
    fetchMock.mockResolvedValueOnce(jsonResponse(updated))

    await expect(
      updateTeamOrganizationSettings(
        { name: "Atlas Team", logo: { action: "remove" } },
        "checked-csrf"
      )
    ).resolves.toEqual(updated)
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/team/organization",
      expect.objectContaining({
        method: "PATCH",
        credentials: "same-origin",
        cache: "no-store",
        headers: {
          "Content-Type": "application/json",
          "X-WorkOS-CSRF": "checked-csrf",
        },
        body: JSON.stringify({
          name: "Atlas Team",
          logo: { action: "remove" },
        }),
      })
    )
  })

  it("rejects an empty mutation CSRF before issuing fetch", async () => {
    await expect(
      updateTeamOrganizationSettings(
        { name: "Atlas Team", logo: { action: "keep" } },
        ""
      )
    ).rejects.toMatchObject({ kind: "invalid_request" })
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it("keeps organization cancellation active while its response body stalls", async () => {
    const external = new AbortController()
    let requestSignal: AbortSignal | undefined
    let bodyStarted!: () => void
    const started = new Promise<void>((resolve) => {
      bodyStarted = resolve
    })
    fetchMock.mockImplementationOnce((_input, init) => {
      requestSignal = init?.signal ?? undefined
      return Promise.resolve(stalledJsonResponse(requestSignal!, bodyStarted))
    })

    const pending = loadTeamOrganizationSettings(external.signal)
    const rejection = pending.catch((error: unknown) => error)
    await started
    external.abort()
    await Promise.resolve()

    expect(requestSignal?.aborted).toBe(true)
    await expect(rejection).resolves.toMatchObject({ kind: "aborted" })
  })
})
