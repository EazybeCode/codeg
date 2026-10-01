import { expect, it } from "vitest"
import {
  keepWarm,
  warmExpired,
  warmUsable,
  WARM_LIMIT,
  WARM_TTL_MS,
  type WarmEntry,
} from "./neev-warm-panels"

const entry = (
  key: string,
  hiddenAt = 0,
  scope = "1:3:0"
): WarmEntry<string> => ({ key, tab: key, scope, hiddenAt })

it("keeps the most recent two and names the one that fell off", () => {
  expect(WARM_LIMIT).toBe(2)
  let list: WarmEntry<string>[] = []
  list = keepWarm(list, entry("a")).list
  list = keepWarm(list, entry("b")).list
  const third = keepWarm(list, entry("c"))
  expect(third.list.map((item) => item.key)).toEqual(["c", "b"])
  expect(third.evicted).toEqual(["a"])
})

it("moves a kept key to the front instead of keeping it twice", () => {
  const list = [entry("b", 5), entry("a", 1)]
  const next = keepWarm(list, entry("a", 9))
  expect(next.list.map((item) => [item.key, item.hiddenAt])).toEqual([
    ["a", 9],
    ["b", 5],
  ])
  expect(next.evicted).toEqual([])
})

it("lets an entry go after ten minutes unseen", () => {
  expect(WARM_TTL_MS).toBe(600_000)
  const kept = entry("a", 1_000)
  expect(warmUsable(kept, "1:3:0", 1_000 + WARM_TTL_MS)).toBe(true)
  expect(warmUsable(kept, "1:3:0", 1_000 + WARM_TTL_MS + 1)).toBe(false)
  expect(warmExpired([kept], "1:3:0", 1_000 + WARM_TTL_MS + 1)).toEqual(["a"])
})

it("never reveals across a scope, or with no session at all", () => {
  const kept = entry("a", 0, "1:3:0")
  // Another organization, or the same one after a re-bootstrap.
  expect(warmUsable(kept, "1:4:0", 0)).toBe(false)
  expect(warmUsable(kept, "1:3:1", 0)).toBe(false)
  expect(warmUsable(kept, null, 0)).toBe(false)
  expect(warmExpired([kept, entry("b", 0, "1:4:0")], "1:4:0", 0)).toEqual(["a"])
})
