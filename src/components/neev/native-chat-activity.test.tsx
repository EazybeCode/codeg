import { expect, it } from "vitest"
import { attachTurns } from "./native-chat-activity"
import type { NeevAgentTurn } from "@/lib/neev-agent-sessions-client"
import type { NativeMessage } from "@/lib/neev-content-client"

const OWNER = 1

function human(
  id: string,
  sequence: string,
  body: string,
  authorId = OWNER
): NativeMessage {
  return {
    id,
    sequence,
    authorId,
    authorName: authorId === OWNER ? "Sagar" : "Riya",
    body,
    createdAt: 1_700_000_000,
    authorKind: "human",
    agentLabel: null,
    attachments: [],
  }
}

function reply(
  id: string,
  sequence: string,
  body: string,
  agentLabel: string,
  authorId = OWNER
): NativeMessage {
  return {
    ...human(id, sequence, body, authorId),
    authorKind: "agent",
    agentLabel,
  }
}

function turn(
  id: string,
  sequence: string,
  fields: Partial<NeevAgentTurn> = {}
): NeevAgentTurn {
  return {
    id,
    sessionId: "77777777-7777-4777-8777-777777777777",
    sequence,
    runId: "0123456789abcdef0123456789abcdef",
    prompt: "help",
    response: "do X",
    activity: [],
    inputs: [],
    supportsSteering: false,
    status: "completed",
    errorCode: null,
    provider: "claude",
    model: "default",
    attachments: [],
    createdAt: 1_700_000_000,
    updatedAt: 1_700_000_050,
    completedAt: 1_700_000_050,
    ...fields,
  }
}

it("matches a reply to the turn that published it, whatever was said in between", () => {
  // A teammate speaking between the ask and the reply is ordinary, and used to
  // be enough to mis-pair the trace when pairing went by position.
  const messages = [
    human("m1", "1", "help"),
    human("m2", "2", "one sec", 2),
    reply("m3", "3", "do X", "claude"),
  ]
  const attached = attachTurns(messages, [turn("t1", "1")], OWNER)
  expect([...attached.keys()]).toEqual(["m3"])
  expect(attached.get("m3")?.id).toBe("t1")
})

it("tells two providers answering the same prompt apart by their label", () => {
  const messages = [
    human("m1", "1", "help"),
    reply("m2", "2", "claude says", "claude"),
    reply("m3", "3", "codex says", "codex"),
  ]
  const turns = [
    turn("t1", "1", { response: "claude says", provider: "claude" }),
    turn("t2", "2", { response: "codex says", provider: "codex" }),
  ]
  const attached = attachTurns(messages, turns, OWNER)
  expect(attached.get("m2")?.id).toBe("t1")
  expect(attached.get("m3")?.id).toBe("t2")
})

it("attaches nothing when the same provider's answer could have come from either of two turns", () => {
  // Guessing here would claim one reply ran the other's commands. Silence is
  // the only honest answer.
  const messages = [
    reply("m1", "1", "done", "claude"),
    reply("m2", "2", "done", "claude"),
  ]
  const turns = [
    turn("t1", "1", { response: "done" }),
    turn("t2", "2", { response: "done" }),
  ]
  expect(attachTurns(messages, turns, OWNER).size).toBe(0)
})

it("never sources a reply from a turn that failed, was canceled, or is still running", () => {
  const messages = [reply("m1", "1", "do X", "claude")]
  for (const status of ["failed", "canceled"] as const)
    expect(
      attachTurns(
        messages,
        [turn("t1", "1", { status, errorCode: "provider" })],
        OWNER
      ).size
    ).toBe(0)
  expect(
    attachTurns(
      messages,
      [turn("t1", "1", { status: "running", response: "", completedAt: null })],
      OWNER
    ).size
  ).toBe(0)
})

it("leaves a teammate's reply undecorated even when a turn of ours would fit it", () => {
  const messages = [reply("m1", "1", "do X", "claude", 2)]
  expect(attachTurns(messages, [turn("t1", "1")], OWNER).size).toBe(0)
})

it("consumes each turn once, so a repeated answer cannot borrow an earlier trace", () => {
  const messages = [
    reply("m1", "1", "do X", "claude"),
    reply("m2", "2", "do X", "claude"),
  ]
  // Only one turn produced "do X"; the second reply is not it.
  const attached = attachTurns(messages, [turn("t1", "1")], OWNER)
  expect(attached.get("m1")?.id).toBe("t1")
  expect(attached.has("m2")).toBe(false)
})
