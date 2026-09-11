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
import { DropdownMenuContent } from "@/components/ui/dropdown-menu"

const PortalContainer = createContext<HTMLElement | null>(null)

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
