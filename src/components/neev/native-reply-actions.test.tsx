import { act, renderHook } from "@testing-library/react"
import { expect, it, vi } from "vitest"
import {
  useForkAction,
  type ForkMade,
  type OpenFork,
} from "./native-reply-actions"

const made: ForkMade = {
  target: {
    kind: "resource",
    resourceId: "77777777-7777-4777-8777-777777777777",
  },
  archived: 0,
  skipped: 0,
}

it("drops a fork's answer after an account or org change, and forgets its request id", async () => {
  const openFork = vi.fn<OpenFork>(async () => "opened")
  const ids: string[] = []
  let answer!: (value: ForkMade) => void
  const { result, rerender } = renderHook(
    ({ scope }) => useForkAction(scope, openFork),
    { initialProps: { scope: "1:3:0" } }
  )
  act(() => {
    void result.current.fork("reply", (id) => {
      ids.push(id)
      return new Promise<ForkMade>((resolve) => {
        answer = resolve
      })
    })
  })
  expect(result.current.forking).toBe("reply")
  rerender({ scope: "2:4:0" })
  await act(async () => answer(made))
  // Nothing opens for the old account, and nothing is reported.
  expect(openFork).not.toHaveBeenCalled()
  expect(result.current.outcome).toBeNull()
  expect(result.current.forking).toBeNull()
  // A fork under the new scope is a new request.
  await act(async () => {
    await result.current.fork("reply", async (id) => {
      ids.push(id)
      return made
    })
  })
  expect(ids[1]).not.toBe(ids[0])
  expect(openFork).toHaveBeenCalledTimes(1)
})

it("ignores a second fork while one is on its way", async () => {
  const make = vi.fn(() => new Promise<ForkMade>(() => {}))
  const { result } = renderHook(() =>
    useForkAction(
      "1:3:0",
      vi.fn<OpenFork>(async () => "opened")
    )
  )
  act(() => {
    void result.current.fork("a", make)
    void result.current.fork("a", make)
    void result.current.fork("b", make)
  })
  expect(make).toHaveBeenCalledTimes(1)
})
