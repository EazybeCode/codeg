"use client"

import { useEffect, useRef } from "react"
import { PkArenaDialog } from "@/components/pk/pk-arena-dialog"
import { PkLauncherDialog } from "@/components/pk/pk-launcher-dialog"
import { PkMinimizedPill } from "@/components/pk/pk-minimized-pill"
import { usePkRound, fetchUsage } from "@/hooks/use-pk-round"
import { usePkArenaStore, dbRoundToStoreRound } from "@/stores/pk-arena-store"
import { pkRoundList } from "@/lib/api"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"

/**
 * Arena mount point — renders the launcher and arena dialogs and drives the
 * orchestrator for rounds created by the launcher. Must live inside
 * `AcpConnectionsProvider` (the workspace layout provides it): the
 * orchestrator calls `connect`/`sendPrompt` and subscribes to `acp://event`.
 *
 * The launcher only writes the round into the store; this host picks it up,
 * so round creation works from anywhere (composer menu, future entries)
 * without prop-drilling.
 *
 * On mount, hydrates the store from the DB so finished rounds' scoreboards and
 * diffs remain viewable after a restart. The folder's path is needed to map
 * each DB round's folderId to its workingDir.
 */
export function PkArenaHost() {
  const { startRound } = usePkRound()
  const rounds = usePkArenaStore((s) => s.rounds)
  const hydrating = usePkArenaStore((s) => s.hydrating)
  const hydrateFromDb = usePkArenaStore((s) => s.hydrateFromDb)
  const folders = useAppWorkspaceStore((s) => s.allFolders)

  // Hydrate from DB on mount (once).
  const hydratedRef = useRef(false)
  useEffect(() => {
    if (hydratedRef.current || folders.length === 0) return
    hydratedRef.current = true
    void (async () => {
      try {
        const dbRounds = await pkRoundList()
        const storeRounds = dbRounds
          .map((info) => {
            const folder = folders.find((f) => f.id === info.folder_id)
            const workingDir = folder?.path ?? ""
            return dbRoundToStoreRound(info, workingDir)
          })
          .filter((r) => r.workingDir !== "")
        hydrateFromDb(storeRounds)
        // Backfill usage for finished contestants — usage is live-only in
        // the store (issue #4 / #16), so after a restart it's null. Fetch
        // it from the conversation turns for any contestant that has a
        // conversationId and is done/error/canceled.
        for (const round of storeRounds) {
          for (const c of round.contestants) {
            if (
              c.conversationId != null &&
              (c.status === "done" ||
                c.status === "error" ||
                c.status === "canceled")
            ) {
              const usage = await fetchUsage(c.conversationId)
              if (usage) {
                usePkArenaStore
                  .getState()
                  .updateContestant(round.id, c.slot, { usage })
              }
            }
          }
        }
      } catch {
        hydrateFromDb([])
      }
    })()
  }, [folders, hydrateFromDb])

  // Drive any round that still has contestants in "preparing" — exactly the
  // state the launcher leaves behind. Restarted (interrupted) rounds come
  // back with settled statuses, so they are never re-driven.
  const drivenRef = useRef(new Set<string>())
  useEffect(() => {
    if (hydrating) return
    for (const round of rounds) {
      if (drivenRef.current.has(round.id)) continue
      if (!round.contestants.some((c) => c.status === "preparing")) continue
      drivenRef.current.add(round.id)
      void startRound(round)
    }
  }, [rounds, startRound, hydrating])

  return (
    <>
      <PkLauncherDialog />
      <PkArenaDialog />
      <PkMinimizedPill />
    </>
  )
}
