"use client"

import dynamic from "next/dynamic"
import { NativeWorkspace } from "@/components/neev/native-workspace"
import { TeamSignedInShell } from "@/components/team/team-signed-in-shell"
import { useTeamMode } from "@/components/team/team-mode-boundary"

/**
 * The operator shell is fetched only when operator mode is the one that renders.
 *
 * It is by far the largest thing on this route — sidebar, tab bar, terminal,
 * file workspace, workbench and their contexts — and native and team modes
 * render none of it. Keeping it in this module meant every native `/workspace`
 * start downloaded and parsed all of it first.
 *
 * `loading: null` matches what this layout already renders while the mode is
 * still unknown, so no new visual state is introduced.
 */
const OperatorWorkspaceLayout = dynamic(
  () => import("./operator-workspace-layout"),
  { ssr: false, loading: () => null }
)

export default function WorkspaceLayout({
  children,
}: {
  children: React.ReactNode
}) {
  const mode = useTeamMode()
  if (mode === "unknown") return null
  if (mode === "team") return <TeamSignedInShell />
  if (mode === "native") return <NativeWorkspace />
  return <OperatorWorkspaceLayout>{children}</OperatorWorkspaceLayout>
}
