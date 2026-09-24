import { act, renderHook } from "@testing-library/react"
import { expect, it } from "vitest"
import type { NativeSession } from "./neev-client"
import {
  runningChatKey,
  usePublishRunningChat,
  useRunningChats,
} from "./neev-running-chats"
const session: NativeSession = {
  user: { id: 1, displayName: "Tester", avatarUrl: null },
  csrf: "test",
  scope: "organization",
  organization: { id: 1, name: "Test", avatarUrl: null },
  role: "member",
}
it("publishes active chats, clears on completion and unmount, and isolates identities", () => {
  const observer = renderHook(() => useRunningChats())
  const publisher = renderHook(
    ({ busy }) => usePublishRunningChat(session, "chat", "a", busy),
    { initialProps: { busy: true } }
  )
  const key = runningChatKey(session, "chat", "a")!
  expect(observer.result.current.has(key)).toBe(true)
  expect(
    observer.result.current.has(
      runningChatKey(
        { ...session, user: { ...session.user, id: 2 } },
        "chat",
        "a"
      )!
    )
  ).toBe(false)
  act(() => publisher.rerender({ busy: false }))
  expect(observer.result.current.has(key)).toBe(false)
  act(() => publisher.rerender({ busy: true }))
  act(() => publisher.unmount())
  expect(observer.result.current.has(key)).toBe(false)
  observer.unmount()
})
it("one unmount cannot clear another publisher's running state", () => {
  const observer = renderHook(() => useRunningChats())
  const first = renderHook(() =>
    usePublishRunningChat(session, "agent", "a", true)
  )
  const second = renderHook(() =>
    usePublishRunningChat(session, "agent", "a", true)
  )
  act(() => first.unmount())
  expect(
    observer.result.current.has(runningChatKey(session, "agent", "a")!)
  ).toBe(true)
  act(() => second.unmount())
  expect(observer.result.current.size).toBe(0)
  observer.unmount()
})
