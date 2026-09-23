"use client"

import {
  Activity,
  createContext,
  useCallback,
  useContext,
  useLayoutEffect,
  useState,
  type ComponentProps,
  type ReactNode,
} from "react"
import { DialogContent } from "@/components/ui/dialog"
import {
  DropdownMenuContent,
  DropdownMenuSubContent,
} from "@/components/ui/dropdown-menu"

const PortalContainer = createContext<HTMLElement | null>(null)

/**
 * A panel that stays MOUNTED while hidden, and takes its overlays with it.
 *
 * Unlike [`NativeSuspendedSurface`] this does not use `Activity`: hiding must
 * not pause the panel's effects, because a hidden tab still has to hold its
 * unsaved draft and keep the navigation guard it registered. What it does share
 * is the portal container, so a menu or dialog opened in one tab renders INSIDE
 * that tab rather than in the shell — otherwise it would still be on screen,
 * and still clickable, after switching to another tab.
 *
 * `inert` covers what `hidden` cannot: a portalled overlay that has not
 * finished animating out is inside an inert subtree and cannot take focus or
 * intercept a click meant for the tab in front.
 */
export function NativeTabPanel({
  hidden,
  children,
  className,
}: {
  hidden: boolean
  children: ReactNode
  className?: string
}) {
  const [container, setContainer] = useState<HTMLElement | null>(null)
  const capture = useCallback((node: HTMLDivElement | null) => {
    if (node) setContainer(node)
  }, [])
  useLayoutEffect(() => {
    const focused = document.activeElement
    // Focus must not stay inside a tab the reader has left.
    if (
      hidden &&
      focused instanceof HTMLElement &&
      container?.contains(focused)
    )
      focused.blur()
  }, [container, hidden])
  return (
    <div ref={capture} inert={hidden} hidden={hidden} className={className}>
      {container && (
        <PortalContainer.Provider value={container}>
          {children}
        </PortalContainer.Provider>
      )}
    </div>
  )
}

/** Owns DOM containment, not drafts. Hidden portals share the inert hidden root. */
export function NativeSuspendedSurface({
  hidden,
  children,
  className,
}: {
  hidden: boolean
  children: ReactNode
  className?: string
}) {
  const [container, setContainer] = useState<HTMLElement | null>(null)
  const capture = useCallback((node: HTMLDivElement | null) => {
    // Activity detaches refs while hiding. Keep the connected host so this
    // does not unmount private component state or fall back to document.body.
    if (node) setContainer(node)
  }, [])
  useLayoutEffect(() => {
    const focused = document.activeElement
    if (
      hidden &&
      focused instanceof HTMLElement &&
      container?.contains(focused)
    )
      focused.blur()
  }, [container, hidden])
  return (
    <Activity mode={hidden ? "hidden" : "visible"}>
      <div ref={capture} inert={hidden} className={className}>
        {container && (
          <PortalContainer.Provider value={container}>
            {children}
          </PortalContainer.Provider>
        )}
      </div>
    </Activity>
  )
}

export function NativeDialogContent(
  props: ComponentProps<typeof DialogContent>
) {
  const container = useContext(PortalContainer)
  return container ? (
    <DialogContent {...props} portalContainer={container} />
  ) : null
}

export function NativeDropdownMenuContent(
  props: ComponentProps<typeof DropdownMenuContent>
) {
  const container = useContext(PortalContainer)
  return container ? (
    <DropdownMenuContent {...props} portalContainer={container} />
  ) : null
}

/**
 * A submenu that stays inside its tab, like the menu that opened it.
 *
 * Radix portals a submenu independently of its parent content, so a submenu
 * left to the default container escapes the tab panel: switching tabs would
 * leave it on screen and clickable over the tab in front, which is the exact
 * failure `NativeDropdownMenuContent` exists to prevent one level up.
 */
export function NativeDropdownMenuSubContent(
  props: ComponentProps<typeof DropdownMenuSubContent>
) {
  const container = useContext(PortalContainer)
  return container ? (
    <DropdownMenuSubContent {...props} portalContainer={container} />
  ) : null
}
