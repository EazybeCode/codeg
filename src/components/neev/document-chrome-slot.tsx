"use client"

import { createContext, useContext } from "react"

/**
 * Where a tab's document controls go in the workspace's top chrome.
 *
 * The workspace already draws a row above every tab — the sidebar toggle and
 * the breadcrumb — with its right side empty. A document drawing its own band
 * of controls below the tab strip spent a second row on the same few buttons,
 * and that row is the blank strip between the tabs and the title. So the
 * workspace offers that empty right side, and a document portals its controls
 * into it instead.
 *
 * Three values, and the difference between the last two is the point:
 *
 * - `undefined` — no workspace around this at all (the standalone folder
 *   view). The document keeps drawing its own header there, Back included.
 * - `null` — inside the workspace, but this tab is not the one in front. It
 *   renders no controls anywhere: only the front tab owns the top row, and a
 *   hidden tab drawing a header of its own would be height nobody sees.
 * - an element — this is the front tab; its controls belong there.
 */
export const DocumentChromeSlot = createContext<HTMLElement | null | undefined>(
  undefined
)

export function useDocumentChromeSlot() {
  return useContext(DocumentChromeSlot)
}
