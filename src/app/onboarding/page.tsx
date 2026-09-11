"use client"

import { TeamOnboarding } from "@/components/team/team-onboarding"
import { NativeOnboarding } from "@/components/neev/native-onboarding"
import { useTeamMode } from "@/components/team/team-mode-boundary"

export default function OnboardingPage() {
  const mode = useTeamMode()
  if (mode === "native") return <NativeOnboarding />
  return mode === "team" ? <TeamOnboarding /> : null
}
