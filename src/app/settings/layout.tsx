"use client"

import { Suspense } from "react"
import { SettingsShell } from "@/components/settings/settings-shell"
import { RemoteConnectionGate } from "@/contexts/remote-connection-context"
import { UpdateProvider } from "@/components/providers/update-provider"
import { TeamOrganizationSettingsPage } from "@/components/team/team-organization-settings"
import { useTeamMode } from "@/components/team/team-mode-boundary"

function OperatorSettingsLayout({ children }: { children: React.ReactNode }) {
  return (
    <Suspense>
      <RemoteConnectionGate>
        <UpdateProvider>
          <SettingsShell>{children}</SettingsShell>
        </UpdateProvider>
      </RemoteConnectionGate>
    </Suspense>
  )
}

export default function SettingsLayout({
  children,
}: {
  children: React.ReactNode
}) {
  const mode = useTeamMode()
  if (mode === "unknown") return null
  if (mode === "team") return <TeamOrganizationSettingsPage />
  return <OperatorSettingsLayout>{children}</OperatorSettingsLayout>
}
