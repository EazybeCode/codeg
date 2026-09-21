import { expect, it } from "vitest"
import {
  describeToolInput,
  mergeActivity,
  splitByActivity,
  summariseActivity,
} from "./neev-agent-activity"
import { type NeevActivityItem } from "./neev-agent-sessions-client"

const call = (seq: number, offset = 0): NeevActivityItem => ({
  seq,
  offset,
  kind: "tool_call",
  id: `toolu_${seq}`,
  name: "Bash",
  input: '{"command":"ls -la"}',
  inputTruncated: false,
})
const result = (
  seq: number,
  offset = 0,
  isError = false
): NeevActivityItem => ({
  seq,
  offset,
  kind: "tool_result",
  id: `res_${seq}`,
  callId: `toolu_${seq - 1}`,
  output: "total 42",
  outputTruncated: false,
  isError,
})

it("upserts by seq so a replayed prefix cannot double the trace", () => {
  // Reattaching replays what is already stored before streaming live, so the
  // same items arrive twice. Appending would show every tool call twice.
  const first = mergeActivity([], [call(1), result(2)])
  const again = mergeActivity(first, [call(1), result(2), call(3)])
  expect(again.map((item) => item.seq)).toEqual([1, 2, 3])
  // Out-of-order delivery still ends up in order.
  expect(mergeActivity(again, [call(5), call(4)]).map((i) => i.seq)).toEqual([
    1, 2, 3, 4, 5,
  ])
  // The same seq carrying a later shape replaces, never duplicates.
  const revised = mergeActivity(again, [
    { ...call(1), name: "Read" } as NeevActivityItem,
  ])
  expect(revised).toHaveLength(3)
  expect(revised[0]).toMatchObject({ seq: 1, name: "Read" })
})

it("splits the reply by scalar offsets, not UTF-16 units", () => {
  // The offset counts what Rust's chars() counts. An emoji is one scalar there
  // and two units here, so a raw slice drifts one character left per emoji —
  // this is the trap the contract calls out.
  const text = "a🙂bc"
  const segments = splitByActivity(text, [call(1, 3)])
  expect(segments[0].text).toBe("a🙂b")
  expect(segments[0].items).toEqual([])
  expect(segments[1].items.map((item) => item.seq)).toEqual([1])
  expect(segments[1].text).toBe("c")
  // Proof it is not the naive slice, which would cut after "a🙂" alone.
  expect(text.slice(0, 3)).not.toBe(segments[0].text)
})

it("keeps activity that lands past the end of the text", () => {
  // A tool call can be recorded after the last character streamed.
  const segments = splitByActivity("hi", [call(1, 99)])
  expect(segments[segments.length - 1].items.map((i) => i.seq)).toEqual([1])
  expect(segments.map((segment) => segment.text).join("")).toBe("hi")
})

it("never loses or reorders the prose it wraps", () => {
  const text = "one two three"
  const segments = splitByActivity(text, [call(2, 8), call(1, 4)])
  expect(segments.map((segment) => segment.text).join("")).toBe(text)
})

it("summarises a group without opening it", () => {
  const summary = summariseActivity([
    call(1),
    result(2),
    result(3, 0, true),
    {
      seq: 4,
      offset: 0,
      kind: "status",
      id: "s",
      state: "truncated",
      detail: null,
    },
  ])
  expect(summary).toEqual({
    calls: 1,
    results: 2,
    errors: 1,
    truncated: true,
    unsupported: false,
  })
})

it("describes a tool input without trusting it to be valid JSON", () => {
  expect(describeToolInput('{"command":"ls -la /tmp"}')).toBe("ls -la /tmp")
  expect(describeToolInput('{"file_path":"/etc/hosts"}')).toBe("/etc/hosts")
  // A streamed input can end mid-object; show what arrived rather than throw.
  expect(describeToolInput('{"command":"ls -l')).toBe('{"command":"ls -l')
  // Whitespace is flattened and long values are cut to one readable line.
  expect(describeToolInput('{"command":"a\\n\\nb"}')).toBe("a b")
  expect([
    ...describeToolInput(JSON.stringify({ command: "x".repeat(200) })),
  ]).toHaveLength(80)
})
