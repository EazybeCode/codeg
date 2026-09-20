"use client"

import dynamic from "next/dynamic"

/**
 * Loaded only when it is about to render.
 *
 * This page is the operator surface, and `layout.tsx` only renders `children`
 * on the operator branch — native and team return their own shells first. So
 * the mode gate already exists one level up; what was missing is that the page
 * MODULE still shipped in the route's initial script regardless. Deferring it
 * here is what actually keeps it out of a native start.
 */
const OperatorWorkspacePage = dynamic(
  () => import("./operator-workspace-page"),
  { ssr: false, loading: () => null }
)

export default function WorkspacePage() {
  return <OperatorWorkspacePage />
}
