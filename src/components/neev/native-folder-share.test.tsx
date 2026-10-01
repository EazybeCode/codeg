import { act, render, screen, waitFor } from "@testing-library/react"
import { afterEach, expect, it, vi } from "vitest"
import { useFolderSharing } from "./native-folder-share"
import { json, mockNativeSession, NativeTestRoot } from "./test-helpers"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))
afterEach(() => vi.unstubAllGlobals())

function Probe({ nodeId }: { nodeId: number }) {
  const folder = useFolderSharing(nodeId)
  return (
    <p data-testid="probe">
      {folder.status}:{folder.sharing?.nodeId ?? "-"}:
      {folder.sharing?.effectiveVisibility ?? "-"}
    </p>
  )
}

const policy = (nodeId: number, effective: "private" | "organization") => ({
  nodeId,
  revision: "1",
  defaultVisibility: effective === "organization" ? "organization" : "inherit",
  effectiveVisibility: effective,
  inheritedFrom: null,
  canManage: false,
})

/**
 * The same panel moving from one folder to another must never show the first
 * folder's answer for the second, however late it arrives — and must say
 * "loading" rather than the previous folder's value while it waits.
 */
it("keys every answer to the folder it was asked for", async () => {
  let releaseFirst: (() => void) | undefined
  mockNativeSession(undefined, {
    "GET /api/neev/nodes/4/sharing": () =>
      new Promise<Response>((resolve) => {
        releaseFirst = () => resolve(json(policy(4, "organization")))
      }) as unknown as Response,
    "GET /api/neev/nodes/6/sharing": () => json(policy(6, "private")),
  })
  const view = render(
    <NativeTestRoot>
      <Probe nodeId={4} />
    </NativeTestRoot>
  )
  await waitFor(() => expect(releaseFirst).toBeDefined())
  expect(screen.getByTestId("probe")).toHaveTextContent("loading:-:-")

  view.rerender(
    <NativeTestRoot>
      <Probe nodeId={6} />
    </NativeTestRoot>
  )
  await waitFor(() =>
    expect(screen.getByTestId("probe")).toHaveTextContent("ready:6:private")
  )
  await act(async () => {
    releaseFirst?.()
  })
  await act(async () => {
    await Promise.resolve()
  })
  expect(screen.getByTestId("probe")).toHaveTextContent("ready:6:private")
})

it("reports a failed read as unavailable, never as a value", async () => {
  mockNativeSession(undefined, {
    "GET /api/neev/nodes/4/sharing": () => json({ code: "unavailable" }, 503),
  })
  render(
    <NativeTestRoot>
      <Probe nodeId={4} />
    </NativeTestRoot>
  )
  await waitFor(() =>
    expect(screen.getByTestId("probe")).toHaveTextContent("unavailable:-:-")
  )
})
