import { afterEach, expect, it } from "vitest"
import {
  closeTab,
  closeTabByKey,
  MAX_TABS,
  openTab,
  openTabInPlace,
  readTabs,
  tabKey,
  writeTabs,
  type WorkspaceTab,
} from "./neev-workspace-tabs"

const A: WorkspaceTab = { kind: "project", nodeId: 1 }
const B: WorkspaceTab = {
  kind: "resource",
  resourceId: "11111111-1111-4111-8111-111111111111",
  nodeId: 1,
}
const C: WorkspaceTab = {
  kind: "agent",
  sessionId: "22222222-2222-4222-8222-222222222222",
}
const DRAFT: WorkspaceTab = { kind: "draft", provider: "claude", nodeId: 1 }
const EMPTY: WorkspaceTab = { kind: "empty" }

afterEach(() => localStorage.clear())

it("treats a second open of the same thing as the same tab", () => {
  const first = openTab([], A)
  const again = openTab(first.tabs, { kind: "project", nodeId: 1 })
  expect(again.tabs).toHaveLength(1)
  expect(again.active).toBe(0)
  expect(again.refused).toBe(false)
  // And a draft for the same provider and project is that same draft, so a
  // second New Claude chat returns to what is already being written.
  const drafted = openTab(first.tabs, DRAFT)
  expect(openTab(drafted.tabs, { ...DRAFT }).tabs).toHaveLength(2)
})

it("refuses at the limit rather than evicting unsaved work", () => {
  let tabs: WorkspaceTab[] = []
  for (let index = 0; index < MAX_TABS; index += 1)
    tabs = openTab(tabs, { kind: "project", nodeId: index + 1 }).tabs
  expect(tabs).toHaveLength(MAX_TABS)

  const refused = openTab(tabs, { kind: "project", nodeId: 999 })
  // The oldest tab may hold a half-written document. Dropping it to make room
  // would discard that without ever asking.
  expect(refused.refused).toBe(true)
  expect(refused.tabs).toHaveLength(MAX_TABS)
  expect(refused.tabs[0]).toEqual({ kind: "project", nodeId: 1 })
})

it("replaces what is in front rather than piling tabs up", () => {
  // Browsing leaves no trail: the strip stays one tab long however many
  // things are looked at.
  const first = openTabInPlace([], A, -1)
  expect(first).toEqual({ tabs: [A], active: 0, refused: false })
  const second = openTabInPlace(first.tabs, B, 0)
  expect(second).toEqual({ tabs: [B], active: 0, refused: false })
  // Something already open is gone to, not opened a second time.
  const both = openTab(second.tabs, C)
  expect(openTabInPlace(both.tabs, B, 1)).toEqual({
    tabs: [B, C],
    active: 0,
    refused: false,
  })
})

it("asks for one blank tab at a time, and fills it with the next choice", () => {
  const asked = openTab([A], EMPTY)
  expect(asked).toEqual({ tabs: [A, EMPTY], active: 1, refused: false })
  // A second New tab returns to the blank already waiting rather than
  // stacking another one behind it.
  expect(openTab(asked.tabs, { kind: "empty" })).toEqual({
    tabs: [A, EMPTY],
    active: 1,
    refused: false,
  })
  // And the next thing chosen fills it.
  expect(openTabInPlace(asked.tabs, B, 1)).toEqual({
    tabs: [A, B],
    active: 1,
    refused: false,
  })
})

it("takes the blank back when the thing chosen is already open", () => {
  // Otherwise: ask for somewhere to put something, choose something already
  // open, and be left with an orphan blank nobody asked to keep.
  expect(openTabInPlace([EMPTY, B, C], C, 0)).toEqual({
    tabs: [B, C],
    active: 1,
    refused: false,
  })
  expect(openTabInPlace([A, B, EMPTY], A, 2)).toEqual({
    tabs: [A, B],
    active: 0,
    refused: false,
  })
  // A blank that is NOT the one in front is left alone: it is still waiting
  // for a choice of its own.
  expect(openTabInPlace([EMPTY, B, C], C, 1)).toEqual({
    tabs: [EMPTY, B, C],
    active: 2,
    refused: false,
  })
})

it("never persists a blank tab, and restores onto something real", () => {
  writeTabs(1, 3, [A, EMPTY], 1)
  const restored = readTabs(1, 3)
  // An empty tab holds nothing; restoring one would restore the absence of a
  // decision. And quitting on it lands on a real tab rather than nowhere.
  expect(restored.tabs).toEqual([A])
  expect(restored.active).toBe(0)
  // Injected by another build or an extension, it is refused on the way in
  // too — the stored list is hostile input.
  localStorage.setItem(
    "neev.ws.tabs.1:3",
    JSON.stringify({ tabs: [{ kind: "empty" }, A], active: "empty" })
  )
  expect(readTabs(1, 3)).toEqual({ tabs: [A], active: 0 })
})

it("lands on a neighbour when the active tab closes", () => {
  const tabs = [A, B, C]
  // The one to the right takes its place …
  expect(closeTab(tabs, 1, 1)).toEqual({ tabs: [A, C], active: 1 })
  // … unless it was the last, in which case the one to the left does.
  expect(closeTab(tabs, 2, 2)).toEqual({ tabs: [A, B], active: 1 })
  // Closing something else does not move the selection off what is in front.
  expect(closeTab(tabs, 0, 2)).toEqual({ tabs: [B, C], active: 1 })
  expect(closeTab(tabs, 2, 0)).toEqual({ tabs: [A, B], active: 0 })
  // Nothing left to select.
  expect(closeTab([A], 0, 0)).toEqual({ tabs: [], active: -1 })
})

it("closes by identity, for a panel that found its own subject gone", () => {
  expect(closeTabByKey([A, B, C], tabKey(B), 2)).toEqual({
    tabs: [A, C],
    active: 1,
  })
  // An identity that is not open changes nothing.
  expect(closeTabByKey([A], "resource:missing", 0)).toEqual({
    tabs: [A],
    active: 0,
  })
})

it("restores the tab that was in front, not the one opened last", () => {
  writeTabs(1, 3, [A, B, C], 0)
  const restored = readTabs(1, 3)
  expect(restored.tabs).toEqual([A, B, C])
  // The reader had switched back to the first one; that is where they return.
  expect(restored.active).toBe(0)
})

it("never restores a draft, and lands somewhere sensible without it", () => {
  writeTabs(1, 3, [A, DRAFT], 1)
  const restored = readTabs(1, 3)
  // A draft has no server-side identity; restoring one would promise work
  // that does not exist.
  expect(restored.tabs).toEqual([A])
  expect(restored.active).toBe(0)
})

it("keeps one organization's tabs out of another's", () => {
  writeTabs(1, 3, [A, B], 1)
  writeTabs(1, 4, [C], 0)
  expect(readTabs(1, 3).tabs).toEqual([A, B])
  expect(readTabs(1, 4).tabs).toEqual([C])
  expect(readTabs(2, 3).tabs).toEqual([])
})

it("reads a stored value as hostile input", () => {
  const key = "neev.ws.tabs.1:3"
  for (const bad of [
    "not json",
    '{"tabs":"nope"}',
    '[{"kind":"project"}]',
    '[{"kind":"project","nodeId":0}]',
    '[{"kind":"project","nodeId":-1}]',
    '[{"kind":"resource","resourceId":"nope","nodeId":1}]',
    '[{"kind":"agent","sessionId":"nope"}]',
    '[{"kind":"elsewhere","nodeId":1}]',
  ]) {
    localStorage.setItem(key, bad)
    expect(readTabs(1, 3)).toEqual({ tabs: [], active: -1 })
  }
  // Duplicates collapse, and the list stays bounded.
  localStorage.setItem(
    key,
    JSON.stringify(
      Array.from({ length: MAX_TABS + 8 }, () => ({
        kind: "project",
        nodeId: 1,
      }))
    )
  )
  expect(readTabs(1, 3).tabs).toEqual([A])
})

it("still reads the older bare-array shape", () => {
  // Written before the active tab was remembered: the tabs survive, and the
  // first one is shown rather than none.
  localStorage.setItem("neev.ws.tabs.1:3", JSON.stringify([A, B]))
  expect(readTabs(1, 3)).toEqual({ tabs: [A, B], active: 0 })
})
