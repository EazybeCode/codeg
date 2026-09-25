"use client"

/**
 * Pairing a shared thread's agent replies with the reader's own stored turns.
 *
 * The rendering of a trace is NOT here: that is `TranscriptBody` (prose and
 * tool rows in recorded order) and `ActivityTimeline` (the progress footer),
 * which the agent-chat surface owns and this module re-exports so both
 * surfaces show a trace the same way. What is here is the one question the
 * team thread has and the private one does not — which turn, if any, produced
 * this message.
 */

import type { NeevAgentTurn } from "@/lib/neev-agent-sessions-client"
import type { NativeMessage } from "@/lib/neev-content-client"

export { ActivityTimeline, TranscriptBody } from "./native-agent-chats"

/**
 * Pair the reader's own stored turns with the agent replies in a shared thread.
 *
 * A message carries no turn id, so the pairing is made from what the two sides
 * actually agree on: a completed turn's `response` IS the reply that was
 * published from it, and its `provider` is the label that reply carries. A
 * reply is matched only when exactly ONE unconsumed turn agrees on both.
 *
 * Anything less certain is left undecorated. Two identical replies from the
 * same provider, a turn that failed and published nothing, a reply attributed
 * to someone else — each of those is skipped rather than guessed at, because a
 * trace attached to the wrong message is worse than no trace at all: it would
 * claim a teammate's answer came from commands it never ran.
 *
 * Position is deliberately not used. An earlier version matched a reply to the
 * ask that preceded it, which breaks the moment a teammate says anything in
 * between, and cannot tell two providers answering the same question apart.
 */
export function attachTurns(
  messages: readonly NativeMessage[],
  turns: readonly NeevAgentTurn[],
  ownerId: number
): Map<string, NeevAgentTurn> {
  const attached = new Map<string, NeevAgentTurn>()
  const used = new Set<string>()
  for (const message of messages) {
    // Only this reader's own invocations, and only replies: a human message
    // never has a trace, and someone else's reply is never ours to explain.
    if (message.authorKind !== "agent" || message.authorId !== ownerId) continue
    const candidates = turns.filter(
      (turn) =>
        !used.has(turn.id) &&
        // A turn that did not complete published no reply, so it can be the
        // source of none.
        turn.status === "completed" &&
        turn.response === message.body &&
        turn.provider === message.agentLabel
    )
    if (candidates.length !== 1) continue
    used.add(candidates[0].id)
    attached.set(message.id, candidates[0])
  }
  return attached
}
