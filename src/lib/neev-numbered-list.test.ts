import { Schema } from "@tiptap/pm/model"
import { EditorState, TextSelection } from "@tiptap/pm/state"
import { describe, expect, it } from "vitest"
import { continueNumberedList } from "./neev-numbered-list"

const schema = new Schema({
  nodes: {
    doc: { content: "paragraph+" },
    paragraph: { content: "inline*", group: "block" },
    text: { group: "inline" },
    attachment: { inline: true, group: "inline", atom: true },
  },
})
function setup(text: string, attachment = false) {
  const nodes = [schema.text(text)]
  if (attachment) nodes.push(schema.nodes.attachment.create())
  const doc = schema.nodes.doc.create(
    null,
    schema.nodes.paragraph.create(null, nodes)
  )
  let state = EditorState.create({ doc, selection: TextSelection.atEnd(doc) })
  return {
    enter: () =>
      continueNumberedList(state, (tr) => {
        state = state.apply(tr)
      }),
    type: (value: string) => {
      state = state.apply(state.tr.insertText(value))
    },
    select: (at: number) => {
      state = state.apply(
        state.tr.setSelection(TextSelection.create(state.doc, at))
      )
    },
    state: () => state,
    lines: () => {
      const lines: string[] = []
      state.doc.forEach((p) => lines.push(p.textContent))
      return lines
    },
  }
}
describe("numbered list Enter", () => {
  it("continues 1 to 2 to 3, then exits an empty item", () => {
    const e = setup("1. first")
    expect(e.enter()).toBe(true)
    expect(e.lines()).toEqual(["1. first", "2. "])
    e.type("second")
    expect(e.enter()).toBe(true)
    expect(e.lines()).toEqual(["1. first", "2. second", "3. "])
    expect(e.enter()).toBe(true)
    expect(e.lines()).toEqual(["1. first", "2. second", ""])
    expect(e.enter()).toBe(false)
  })
  it("splits at the caret and leaves the trailing text in the next item", () => {
    const e = setup("1. first second")
    e.select(10)
    expect(e.enter()).toBe(true)
    expect(e.lines()).toEqual(["1. first ", "2. second"])
  })
  it("does not consider an attachment-only item empty", () => {
    const e = setup("1. ", true)
    expect(e.enter()).toBe(true)
    expect(e.lines()).toEqual(["1. ", "2. "])
    expect(e.state().doc.firstChild?.lastChild?.type.name).toBe("attachment")
  })
  it("leaves ordinary text and oversized numbers unchanged", () => {
    for (const value of ["hello", "999999999999999999999. item"]) {
      const e = setup(value)
      expect(e.enter()).toBe(false)
      expect(e.lines()).toEqual([value])
    }
  })
})
