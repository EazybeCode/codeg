import { beforeEach, describe, expect, it } from "vitest"
import {
  effectivePins,
  NEEV_SHORTLIST_MAX,
  readShortlist,
  resolveShortlist,
  resetShortlistCache,
  shortlistSnapshot,
  writeShortlist,
} from "./neev-model-shortlist"

const CATALOG = ["default", "haiku", "sonnet", "opus", "gpt-5", "gpt-6"]

beforeEach(() => {
  localStorage.clear()
  resetShortlistCache()
})

describe("what the menu shows", () => {
  it("falls back to the catalogue's own order when nothing was pinned", () => {
    expect(
      resolveShortlist({ available: CATALOG, saved: undefined, selected: null })
    ).toEqual(["default", "haiku", "sonnet"])
  })

  it("shows every explicit pin, past the size the default is capped at", () => {
    // The bug this covers: `size` was applied to saved pins as well, so
    // pinning a fourth model saved it, ticked it in the editor, and then never
    // showed it in the menu it had just been pinned to.
    const saved = ["opus", "gpt-5", "gpt-6", "sonnet"]
    expect(
      resolveShortlist({ available: CATALOG, saved, selected: null })
    ).toEqual(saved)
  })

  it("never lets a stale pin use up the cap", () => {
    // Half of these name models the catalogue no longer lists. They are
    // dropped BEFORE the ceiling applies, so real pins are not crowded out by
    // ones that could never be chosen.
    const saved = [
      ...Array.from({ length: NEEV_SHORTLIST_MAX }, (_, at) => `gone-${at}`),
      "opus",
    ]
    expect(
      resolveShortlist({ available: CATALOG, saved, selected: null })
    ).toEqual(["opus"])
  })

  it("keeps an empty pin list empty rather than reverting to the default", () => {
    // `[]` is "I cleared this" and `undefined` is "I never touched it". Merging
    // the two would re-tick three boxes the moment they were all cleared.
    expect(
      resolveShortlist({ available: CATALOG, saved: [], selected: null })
    ).toEqual([])
  })

  it("always includes the model in force, pinned or not", () => {
    expect(
      resolveShortlist({
        available: CATALOG,
        saved: ["opus"],
        selected: "gpt-6",
      })
    ).toEqual(["opus", "gpt-6"])
    // But never a model the catalogue does not list: a menu may not offer a
    // run the server has not said it would accept.
    expect(
      resolveShortlist({
        available: CATALOG,
        saved: ["opus"],
        selected: "invented",
      })
    ).toEqual(["opus"])
  })
})

describe("what the editor ticks", () => {
  it("ticks exactly the rows the menu is showing, with nothing saved", () => {
    // The other bug: ticks came from storage (empty) while a toggle started
    // from what the menu showed (three rows). The first click on an unticked
    // row therefore REMOVED it, and the shortlist lost a model instead of
    // gaining one.
    const saved = undefined
    const baseline = effectivePins({ available: CATALOG, saved })
    expect(baseline).toEqual(
      resolveShortlist({ available: CATALOG, saved, selected: null })
    )
    // And so a first toggle adds.
    const clicked = "opus"
    expect(baseline.includes(clicked)).toBe(false)
    expect([...baseline, clicked]).toEqual([
      "default",
      "haiku",
      "sonnet",
      "opus",
    ])
  })

  it("drops stale ids from the baseline a toggle writes back", () => {
    expect(
      effectivePins({ available: CATALOG, saved: ["opus", "gone", "gpt-5"] })
    ).toEqual(["opus", "gpt-5"])
  })
})

describe("persistence", () => {
  it("keeps one provider's list without touching another's", () => {
    writeShortlist("claude", ["opus", "sonnet"])
    writeShortlist("codex", ["gpt-6"])
    expect(readShortlist("claude")).toEqual(["opus", "sonnet"])
    expect(readShortlist("codex")).toEqual(["gpt-6"])
  })

  it("caps what it stores, so a list cannot grow without end", () => {
    writeShortlist(
      "claude",
      Array.from({ length: NEEV_SHORTLIST_MAX + 5 }, (_, at) => `m-${at}`)
    )
    expect(readShortlist("claude")).toHaveLength(NEEV_SHORTLIST_MAX)
  })

  it("treats a corrupt value as no preference rather than an error", () => {
    localStorage.setItem("neev.model-shortlist.v1", "{not json")
    resetShortlistCache()
    expect(shortlistSnapshot()).toEqual({})
    expect(readShortlist("claude")).toEqual([])
  })

  it("survives storage that refuses to write", () => {
    const original = Storage.prototype.setItem
    Storage.prototype.setItem = () => {
      throw new Error("quota")
    }
    try {
      expect(writeShortlist("claude", ["opus"])).toBe(false)
      // Still in force for this session: the choice was made, and only the
      // remembering failed.
      expect(shortlistSnapshot().claude).toEqual(["opus"])
    } finally {
      Storage.prototype.setItem = original
    }
  })
})
