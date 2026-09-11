import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { it, expect } from "vitest"
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import {
  NativeSuspendedSurface,
  NativeDialogContent,
  NativeDropdownMenuContent,
} from "./native-suspended-surface"

it.each(["dialog", "menu"] as const)(
  "contains the actual native %s portal inside both auth and resource suspension roots",
  async (kind) => {
    function Surface({
      authPending = false,
      resourcePending = false,
    }: {
      authPending?: boolean
      resourcePending?: boolean
    }) {
      return (
        <>
          <button>Outside control</button>
          <NativeSuspendedSurface hidden={authPending}>
            <NativeSuspendedSurface hidden={resourcePending}>
              {kind === "dialog" ? (
                <Dialog open>
                  <NativeDialogContent>
                    <DialogTitle>Private dialog</DialogTitle>
                    <DialogDescription>Private details</DialogDescription>
                    <button>Private action</button>
                  </NativeDialogContent>
                </Dialog>
              ) : (
                <DropdownMenu open>
                  <DropdownMenuTrigger>Open actions</DropdownMenuTrigger>
                  <NativeDropdownMenuContent>
                    <DropdownMenuItem>Private action</DropdownMenuItem>
                  </NativeDropdownMenuContent>
                </DropdownMenu>
              )}
            </NativeSuspendedSurface>
          </NativeSuspendedSurface>
        </>
      )
    }
    const view = render(<Surface />)
    const portal = await screen.findByRole(kind)
    expect(view.container).toContainElement(portal)
    expect(portal).toBeVisible()
    const user = userEvent.setup()
    for (const pending of [{ authPending: true }, { resourcePending: true }]) {
      view.rerender(<Surface {...pending} />)
      expect(portal).not.toBeVisible()
      expect(portal.closest("[inert]")).not.toBeNull()
      expect(screen.queryByRole(kind)).toBeNull()
      await user.tab()
      expect(portal).not.toContainElement(document.activeElement as HTMLElement)
      view.rerender(<Surface />)
      expect(await screen.findByRole(kind)).toBeVisible()
    }
  }
)

it.each(["dialog", "menu"] as const)(
  "keeps the shared legacy %s default portal on document.body",
  async (kind) => {
    const view = render(
      kind === "dialog" ? (
        <Dialog open>
          <DialogContent>
            <DialogTitle>Legacy dialog</DialogTitle>
            <DialogDescription>Legacy details</DialogDescription>
          </DialogContent>
        </Dialog>
      ) : (
        <DropdownMenu open>
          <DropdownMenuTrigger>Legacy actions</DropdownMenuTrigger>
          <DropdownMenuContent>
            <DropdownMenuItem>Legacy action</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      )
    )
    const portal = await screen.findByRole(kind)
    expect(portal).toBeVisible()
    expect(document.body).toContainElement(portal)
    expect(view.container).not.toContainElement(portal)
  }
)
