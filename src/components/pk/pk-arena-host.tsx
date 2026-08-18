"use client"

import { useEffect, useRef } from "react"
import { PkArenaDialog } from "@/components/pk/pk-arena-dialog"
import { PkLauncherDialog } from "@/components/pk/pk-launcher-dialog"
import { PkMinimizedPill } from "@/components/pk/pk-minimized-pill"
import { usePkRound } from "@/hooks/use-pk-round"
import { usePkArenaStore } from "@/stores/pk-arena-store"

/**
 * Arena mount point — renders the launcher and arena dialogs and drives the
 * orchestrator for rounds created by the launcher. Must live inside
 * `AcpConnectionsProvider` (the workspace layout provides it): the
 * orchestrator calls `connect`/`sendPrompt` and subscribes to `acp://event`.
 *
 * The launcher only writes the round into the store; this host picks it up,
 * so round creation works from anywhere (composer menu, future entries)
 * without prop-drilling.
 */
export function PkArenaHost() {
  const { startRound } = usePkRound()
  const rounds = usePkArenaStore((s) => s.rounds)

  // Drive any round that still has contestants in "preparing" — exactly the
  // state the launcher leaves behind. Restarted (interrupted) rounds come
  // back with settled statuses, so they are never re-driven.
  const drivenRef = useRef(new Set<string>())
  useEffect(() => {
    for (const round of rounds) {
      if (drivenRef.current.has(round.id)) continue
      if (!round.contestants.some((c) => c.status === "preparing")) continue
      drivenRef.current.add(round.id)
      void startRound(round)
    }
  }, [rounds, startRound])

  return (
    <>
      <PkLauncherDialog />
      <PkArenaDialog />
      <PkMinimizedPill />
    </>
  )
}
