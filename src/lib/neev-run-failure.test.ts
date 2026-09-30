import { describe, expect, it } from "vitest"
import type { NeevActivityItem } from "./neev-agent-sessions-client"
import { providerName, runFailure } from "./neev-run-failure"

const errorStep = (category: string, detail: string | null) =>
  ({
    seq: 2,
    offset: 0,
    id: "error",
    kind: "status",
    state: "error",
    detail,
    category,
  }) as NeevActivityItem

const failed = (
  errorCode: string | null,
  activity: NeevActivityItem[] = []
) => ({
  status: "failed" as const,
  errorCode,
  activity,
})

describe("runFailure", () => {
  it("is null for a turn that did not fail", () => {
    for (const status of ["completed", "running", "canceled"] as const)
      expect(runFailure({ status, errorCode: null, activity: [] })).toBeNull()
  })

  it("uses the provider's own class and words when the server kept them", () => {
    expect(
      runFailure(
        failed("provider", [errorStep("quota", "You've hit your usage limit.")])
      )
    ).toEqual({
      category: "quota",
      message: "You've hit your usage limit.",
      retryable: false,
      reconnect: false,
      errorCode: null,
    })
  })

  it("offers reconnect, never retry, for the sign-in classes", () => {
    for (const category of ["auth_expired", "signin_required"])
      expect(
        runFailure(failed("unauthorized", [errorStep(category, "401")]))
      ).toMatchObject({
        category,
        reconnect: true,
        retryable: false,
      })
  })

  it("offers retry for classes a second attempt can fix", () => {
    for (const category of [
      "rate_limit",
      "network",
      "timeout",
      "runtime_unavailable",
      "provider",
    ])
      expect(
        runFailure(failed("provider", [errorStep(category, "x")]))?.retryable
      ).toBe(true)
    expect(
      runFailure(failed("provider", [errorStep("provider_refusal", "x")]))
        ?.retryable
    ).toBe(false)
  })

  it("falls back to the stored code for turns with no error step", () => {
    expect(runFailure(failed("unauthorized"))).toMatchObject({
      category: "signin_required",
      reconnect: true,
      message: null,
    })
    expect(runFailure(failed("unavailable"))).toMatchObject({
      category: "runtime_unavailable",
      retryable: true,
    })
    // The server said no and will say it again.
    expect(runFailure(failed("disabled"))).toMatchObject({ retryable: false })
    expect(runFailure(failed("unsupported"))).toMatchObject({
      category: "provider_refusal",
      retryable: false,
    })
    expect(runFailure(failed("provider"))).toMatchObject({
      category: "provider",
      retryable: true,
    })
  })

  it("reads the LAST error step", () => {
    expect(
      runFailure(
        failed("provider", [
          errorStep("provider", "first"),
          { ...errorStep("timeout", "second"), seq: 3 },
        ])
      )
    ).toMatchObject({ category: "timeout", message: "second" })
  })
})

describe("providerName", () => {
  it("names known providers and shows unknown ids as-is", () => {
    expect(providerName("codex")).toBe("Codex")
    expect(providerName("claude")).toBe("Claude")
    expect(providerName("mystery")).toBe("mystery")
  })
})
