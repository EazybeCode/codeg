import { expect, it } from "vitest"
import {
  describeToolInput,
  elapsedDisplay,
  elapsedMs,
  isNoise,
  mergeActivity,
  pairResults,
  runningTool,
  splitByActivity,
  summariseActivity,
  visibleActivity,
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

it("folds a result into the call it answers, at the call's position", () => {
  // One invocation is one row. The result is attached to the CALL's seq, which
  // is why a running row becomes a finished one in place rather than a second
  // nameless row appearing further down the reply.
  const { resultOf, absorbed } = pairResults([call(1, 4), result(2, 30)])
  expect(resultOf.get(1)?.seq).toBe(2)
  expect(absorbed.has(2)).toBe(true)
})

it("leaves a result no call in this trace claims on its own row", () => {
  // The contract says a result's `callId` may match none. Hiding it would drop
  // recorded output to make the list shorter.
  const orphan = { ...result(9), callId: "toolu_missing" } as NeevActivityItem
  const { resultOf, absorbed } = pairResults([call(1), orphan])
  expect(resultOf.size).toBe(0)
  expect(absorbed.size).toBe(0)
})

it("keeps a second result for the same call rather than swallowing it", () => {
  const first = result(2)
  const second = { ...result(3), callId: "toolu_1" } as NeevActivityItem
  const { resultOf, absorbed } = pairResults([call(1), first, second])
  expect(resultOf.get(1)?.seq).toBe(2)
  expect(absorbed.has(3)).toBe(false)
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

const status = (
  seq: number,
  state: "model" | "stop" | "result" | "truncated" | "unsupported",
  detail: string | null = null
): NeevActivityItem => ({
  seq,
  offset: 0,
  kind: "status",
  id: `s${seq}`,
  state,
  detail,
})

it("drops the provider's bookkeeping and keeps what changes the meaning", () => {
  // `end_turn` and `success` repeat what the transcript already shows; a cap
  // being hit, or a stream we cannot read, does not.
  expect(isNoise(status(1, "model"))).toBe(true)
  expect(isNoise(status(2, "stop"))).toBe(true)
  expect(isNoise(status(3, "result"))).toBe(true)
  expect(isNoise(status(4, "truncated"))).toBe(false)
  expect(isNoise(status(5, "unsupported", "codex"))).toBe(false)
  expect(isNoise(call(6))).toBe(false)
  expect(
    visibleActivity([call(1), status(2, "result"), status(3, "truncated")]).map(
      (item) => item.seq
    )
  ).toEqual([1, 3])
})

it("names the tool still in flight, and nothing once it answers", () => {
  // The last call with no result is the one running.
  expect(runningTool([call(1)])?.name).toBe("Bash")
  expect(runningTool([call(1), result(2)])).toBeNull()
  expect(runningTool([call(1), result(2), call(3)])?.name).toBe("Bash")
  expect(runningTool([])).toBeNull()
  expect(runningTool([status(1, "truncated")])).toBeNull()
})

it("measures elapsed time and never measures backwards", () => {
  expect(elapsedMs(1_000, 4_500)).toBe(3_500)
  expect(elapsedMs(1_000, 1_000)).toBe(0)
  // A clock that disagrees with itself must not show a negative age.
  expect(elapsedMs(5_000, 1_000)).toBe(0)
})

it("shows tenths only where a tenth was actually measured", () => {
  // A live clock, under a minute: the tenth is the whole point.
  expect(elapsedDisplay(37_240, true)).toBe(37.2)
  expect(elapsedDisplay(999, true)).toBe(0.9)
  expect(elapsedDisplay(0, true)).toBe(0)
  // Truncated, not rounded: the turn has not reached 37.3s yet.
  expect(elapsedDisplay(37_290, true)).toBe(37.2)
  // A long run is where a still number reads as a hung one: the tenth stays.
  expect(elapsedDisplay(61_400, true)).toBe(61.4)
  expect(elapsedDisplay(754_000, true)).toBe(754)
  // Read off stored whole-second timestamps: no invented precision.
  expect(elapsedDisplay(37_000, false)).toBe(37)
  expect(elapsedDisplay(37_240, false)).toBe(37)
  expect(elapsedDisplay(-5, true)).toBe(0)
})
