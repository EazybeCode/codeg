"use client"

import {
  captureSigninFragment,
  NativeSigninReturn,
} from "@/components/neev/native-signin-return"

// Scrub the fragment as soon as this route's code runs — before the app has
// finished deciding what to render — rather than when the page mounts.
captureSigninFragment()

export default function AgentSigninPage() {
  return <NativeSigninReturn />
}
