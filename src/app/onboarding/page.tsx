"use client"

import { TeamOnboarding } from "@/components/team/team-onboarding"
import { useTeamMode } from "@/components/team/team-mode-boundary"

export default function OnboardingPage() {
  const mode = useTeamMode()
  return mode === "team" ? <TeamOnboarding /> : null
}
