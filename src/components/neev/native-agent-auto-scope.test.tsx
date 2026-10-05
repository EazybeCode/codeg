import { act, renderHook } from "@testing-library/react"
import { afterEach, expect, it, vi } from "vitest"
import { useAutoRouting } from "./native-agent-auto"

const READY = {
  state: "ready",
  router: "jev",
  maxTaskChars: 6000,
  maxPreviousChars: 2000,
  supervision: "off",
  usage: [],
}

afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
})

it("never shows one account's router status while another's is read", async () => {
  const answers: ((response: Response) => void)[] = []
  vi.stubGlobal(
    "fetch",
    vi.fn(
      () =>
        new Promise<Response>((resolve) => {
          answers.push(resolve)
        })
    )
  )
  const { result, rerender } = renderHook(
    ({ scope, account }) => useAutoRouting(scope, account),
    { initialProps: { scope: "1:3:0", account: "1:3" } }
  )
  await act(async () =>
    answers[0](
      new Response(JSON.stringify(READY), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      })
    )
  )
  expect(result.current.active).toBe(true)
  // Another account: until its own status arrives, Auto is not offered.
  rerender({ scope: "2:4:0", account: "2:4" })
  expect(result.current.status).toBeNull()
  expect(result.current.active).toBe(false)
})
