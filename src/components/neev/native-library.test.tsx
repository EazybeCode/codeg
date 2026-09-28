import { configure, render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, expect, it, vi } from "vitest"
import { NativeLibrary } from "./native-library"
import { NativeSuspendedSurface } from "./native-suspended-surface"
import {
  json,
  mockNativeSession,
  NativeTestRoot,
  orgSession,
} from "./test-helpers"
vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))
configure({ asyncUtilTimeout: 5000 })
afterEach(() => vi.unstubAllGlobals())
const skill = {
  id: "d230e18b-8102-4560-a358-6d8aac05c712",
  name: "Brand writing",
  description: "Team tone",
  kind: "skill",
  enabled: false,
  revision: "1",
  files: ["SKILL.md"],
  setupNote: null,
}
it("lets members see team skills but reserves management for admins", async () => {
  const mock = mockNativeSession(
    { ...orgSession, role: "member" },
    { "GET /api/neev/library": () => json({ items: [skill] }) }
  )
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  expect(await screen.findByText("Brand writing")).toBeVisible()
  expect(screen.queryByLabelText("Import files")).not.toBeInTheDocument()
  expect(screen.queryByRole("switch")).not.toBeInTheDocument()
  expect(mock.unknown).toEqual([])
})
it("enables a skill with its current revision and confirms before deleting", async () => {
  const changes: unknown[] = []
  let deleted = false
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: deleted ? [] : [skill] }),
    [`PATCH /api/neev/library/${skill.id}`]: (init) => {
      changes.push(JSON.parse(init.body as string))
      return json({ ...skill, enabled: true, revision: "2" })
    },
    [`DELETE /api/neev/library/${skill.id}`]: (init) => {
      changes.push(JSON.parse(init.body as string))
      deleted = true
      return json({ ok: true })
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  await user.click(
    await screen.findByRole("switch", { name: "Enable Brand writing" })
  )
  expect(changes[0]).toEqual({ expectedRevision: "1", enabled: true })
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Delete Brand writing" })
    ).toBeEnabled()
  )
  await user.click(screen.getByRole("button", { name: "Delete Brand writing" }))
  expect(changes).toHaveLength(1)
  await user.click(
    screen.getByRole("button", { name: "Delete from workspace" })
  )
  await waitFor(() =>
    expect(screen.queryByText("Brand writing")).not.toBeInTheDocument()
  )
  expect(changes[1]).toEqual({ expectedRevision: "2" })
  expect(mock.unknown).toEqual([])
})
it("imports files disabled for review and shows safe import errors", async () => {
  let imported = false
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: imported ? [skill] : [] }),
    "POST /api/neev/library/import": (init) => {
      const body = JSON.parse(init.body as string)
      expect(body.fileName).toBe("SKILL.md")
      expect(atob(body.data)).toContain("Brand writing")
      imported = true
      return json({ items: [skill] })
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  await user.upload(
    await screen.findByLabelText("Import files"),
    new File(["# Brand writing"], "SKILL.md", { type: "text/markdown" })
  )
  expect(await screen.findByText("Brand writing")).toBeVisible()
  expect(screen.getByRole("switch")).not.toBeChecked()
  expect(mock.unknown).toEqual([])
})

it("keeps successful bulk imports and identifies rejected files", async () => {
  const mock = mockNativeSession(orgSession, {
    "GET /api/neev/library": () => json({ items: [] }),
    "POST /api/neev/library/import": (init) => {
      const body = JSON.parse(init.body as string)
      return body.fileName === "bad.json"
        ? json({ code: "invalid" }, 400)
        : json({ items: [skill] })
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  await user.upload(await screen.findByLabelText("Import files"), [
    new File(["{}"], "bad.json", { type: "application/json" }),
    new File(["# Brand writing"], "SKILL.md", { type: "text/markdown" }),
  ])
  expect(await screen.findByText("Brand writing")).toBeVisible()
  expect(await screen.findByRole("alert")).toHaveTextContent("bad.json")
  expect(screen.getByRole("switch")).not.toBeChecked()
  expect(mock.unknown).toEqual([])
})

it("shows an unavailable library with retry instead of presenting an empty workspace", async () => {
  let calls = 0
  mockNativeSession(orgSession, {
    "GET /api/neev/library": () =>
      ++calls === 1
        ? json({ code: "unavailable" }, 503)
        : json({ items: [skill] }),
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  await user.click(await screen.findByRole("button", { name: "Retry" }))
  expect(await screen.findByText("Brand writing")).toBeVisible()
})

it("reloads a teammate's changes after a revision conflict and explains the result", async () => {
  let changed = false
  mockNativeSession(orgSession, {
    "GET /api/neev/library": () =>
      json({
        items: [{ ...skill, revision: changed ? "2" : "1", enabled: changed }],
      }),
    [`PATCH /api/neev/library/${skill.id}`]: () => {
      changed = true
      return json({ code: "conflict" }, 409)
    },
  })
  const user = userEvent.setup()
  render(
    <NativeTestRoot>
      <NativeSuspendedSurface hidden={false}>
        <NativeLibrary />
      </NativeSuspendedSurface>
    </NativeTestRoot>
  )
  await user.click(
    await screen.findByRole("switch", { name: "Enable Brand writing" })
  )
  expect(await screen.findByText(/This item changed/)).toBeVisible()
  await waitFor(() => expect(screen.getByRole("switch")).toBeChecked())
})
