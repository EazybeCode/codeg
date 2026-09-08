import type { ReactNode } from "react"
import { NextIntlClientProvider } from "next-intl"
import {
  act,
  cleanup,
  fireEvent,
  render as rtlRender,
  screen,
  waitFor,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it, vi } from "vitest"

import messages from "@/i18n/messages/en.json"
import { OrganizationSettings } from "./organization-settings"

const render = (ui: ReactNode) =>
  rtlRender(
    <NextIntlClientProvider locale="en" messages={messages}>
      {ui}
    </NextIntlClientProvider>
  )

function response(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  })
}

const organization = {
  id: 4,
  login: "acme-github",
  name: "Acme Product",
  avatar_url: "data:image/png;base64,b2xk",
  can_edit: true,
}

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe("organization settings", () => {
  it("renders members as read-only while still showing GitHub identity", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(response({ ...organization, can_edit: false }))
    )

    render(<OrganizationSettings />)

    expect(await screen.findByDisplayValue("Acme Product")).toHaveAttribute(
      "readonly"
    )
    expect(screen.getByDisplayValue("acme-github")).toHaveAttribute("readonly")
    expect(
      screen.getByText("Only organization owners and admins can make changes.")
    ).toBeVisible()
    expect(screen.queryByRole("button", { name: "Save changes" })).toBeNull()
    expect(screen.queryByLabelText("Upload organization logo")).toBeNull()
  })

  it("saves a trimmed name without deleting the existing logo and refreshes identity", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(response(organization))
      .mockResolvedValueOnce(response({ ...organization, name: "New name" }))
    vi.stubGlobal("fetch", fetch)
    const identityChanged = vi.fn()
    window.addEventListener("workos:identity-changed", identityChanged)
    const user = userEvent.setup()
    render(<OrganizationSettings />)

    const name = await screen.findByLabelText("Display name")
    await user.clear(name)
    await user.type(name, "  New name  ")
    await user.click(screen.getByRole("button", { name: "Save changes" }))

    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(2))
    expect(fetch).toHaveBeenLastCalledWith(
      "/auth/organization",
      expect.objectContaining({
        method: "PATCH",
        credentials: "same-origin",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ name: "New name", logo: { action: "keep" } }),
      })
    )
    expect(identityChanged).toHaveBeenCalledOnce()
    expect(name).toHaveValue("New name")
    expect(screen.getByRole("button", { name: "Save changes" })).toBeDisabled()
    window.removeEventListener("workos:identity-changed", identityChanged)
  })

  it("retains dirty edits and reports a failed save", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(response(organization))
        .mockResolvedValueOnce(response({ error: "unavailable" }, 503))
    )
    const user = userEvent.setup()
    render(<OrganizationSettings />)

    const name = await screen.findByLabelText("Display name")
    await user.clear(name)
    await user.type(name, "Unsaved name")
    await user.click(screen.getByRole("button", { name: "Save changes" }))

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not save organization settings."
    )
    expect(name).toHaveValue("Unsaved name")
    expect(screen.getByRole("button", { name: "Save changes" })).toBeEnabled()
  })

  it("locks editable drafts while a save is pending so success cannot overwrite newer input", async () => {
    let finishSave!: (value: Response) => void
    const pendingSave = new Promise<Response>((resolve) => {
      finishSave = resolve
    })
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(response(organization))
        .mockReturnValueOnce(pendingSave)
    )
    const user = userEvent.setup()
    render(<OrganizationSettings />)

    const name = await screen.findByLabelText("Display name")
    await user.clear(name)
    await user.type(name, "Pending name")
    await user.click(screen.getByRole("button", { name: "Save changes" }))

    expect(screen.getByRole("button", { name: "Saving…" })).toBeDisabled()
    expect(name).toBeDisabled()
    expect(screen.getByLabelText("Upload organization logo")).toBeDisabled()
    expect(screen.getByRole("button", { name: "Remove logo" })).toBeDisabled()
    expect(screen.getByText("Choose image")).toHaveAttribute(
      "aria-disabled",
      "true"
    )

    finishSave(response({ ...organization, name: "Pending name" }))
    expect(await screen.findByDisplayValue("Pending name")).toBeEnabled()
  })

  it("previews an uploaded logo and sends an explicit upload action", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(response({ ...organization, avatar_url: null }))
      .mockResolvedValueOnce(
        response({
          ...organization,
          avatar_url: "data:image/png;base64,c2F2ZWQ=",
        })
      )
    vi.stubGlobal("fetch", fetch)
    const user = userEvent.setup()
    render(<OrganizationSettings />)
    const file = new File([new Uint8Array([137, 80, 78, 71])], "logo.png", {
      type: "image/png",
    })

    const input = await screen.findByLabelText("Upload organization logo")
    await user.upload(input, file)

    expect(
      await screen.findByRole("img", { name: "Organization logo preview" })
    ).toHaveAttribute("src", expect.stringMatching(/^data:image\/png;base64,/))
    await user.click(screen.getByRole("button", { name: "Save changes" }))
    const request = fetch.mock.calls[1]?.[1] as RequestInit
    expect(JSON.parse(String(request.body))).toEqual({
      name: "Acme Product",
      logo: {
        action: "upload",
        data: expect.stringMatching(/^data:image\/png;base64,/),
      },
    })
  })

  it("keeps removal explicit until save", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(response(organization))
      .mockResolvedValueOnce(response({ ...organization, avatar_url: null }))
    vi.stubGlobal("fetch", fetch)
    const user = userEvent.setup()
    render(<OrganizationSettings />)

    await user.click(await screen.findByRole("button", { name: "Remove logo" }))
    expect(
      screen.queryByRole("img", { name: "Organization logo preview" })
    ).toBeNull()
    expect(fetch).toHaveBeenCalledOnce()
    await user.click(screen.getByRole("button", { name: "Save changes" }))
    const request = fetch.mock.calls[1]?.[1] as RequestInit
    expect(JSON.parse(String(request.body))).toEqual({
      name: "Acme Product",
      logo: { action: "remove" },
    })
  })

  it("ignores a stale file read that finishes after a newer logo choice", async () => {
    const readers: Array<{
      result: string
      onload: null | (() => void)
      onerror: null | (() => void)
    }> = []
    class DeferredFileReader {
      result = ""
      onload: null | (() => void) = null
      onerror: null | (() => void) = null
      readAsDataURL(file: File) {
        this.result = `data:${file.type};base64,${file.name}`
        readers.push(this)
      }
    }
    vi.stubGlobal("FileReader", DeferredFileReader)
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(response({ ...organization, avatar_url: null }))
    )
    render(<OrganizationSettings />)
    const input = await screen.findByLabelText("Upload organization logo")
    const first = new File(["first"], "first", { type: "image/png" })
    const second = new File(["second"], "second", { type: "image/webp" })

    fireEvent.change(input, { target: { files: [first] } })
    fireEvent.change(input, { target: { files: [second] } })
    act(() => readers[1]?.onload?.())
    act(() => readers[0]?.onload?.())

    expect(
      await screen.findByRole("img", { name: "Organization logo preview" })
    ).toHaveAttribute("src", "data:image/webp;base64,second")
  })

  it("does not allow saving a name while the selected logo is still being read", async () => {
    let finishRead: (() => void) | null = null
    class DeferredFileReader {
      result = "data:image/png;base64,cGVuZGluZw=="
      onload: null | (() => void) = null
      onerror: null | (() => void) = null
      readAsDataURL() {
        finishRead = () => this.onload?.()
      }
    }
    vi.stubGlobal("FileReader", DeferredFileReader)
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(response({ ...organization, avatar_url: null }))
    )
    const user = userEvent.setup()
    render(<OrganizationSettings />)
    const name = await screen.findByLabelText("Display name")
    await user.clear(name)
    await user.type(name, "New name")

    const input = screen.getByLabelText("Upload organization logo")
    await user.upload(
      input,
      new File(["logo"], "logo.png", { type: "image/png" })
    )

    expect(screen.getByRole("button", { name: "Save changes" })).toBeDisabled()
    act(() => finishRead?.())
    expect(screen.getByRole("button", { name: "Save changes" })).toBeEnabled()
  })

  it("allows the same file to be chosen again after removing its preview", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(response({ ...organization, avatar_url: null }))
    )
    const user = userEvent.setup()
    render(<OrganizationSettings />)
    const input = await screen.findByLabelText("Upload organization logo")
    const file = new File(["logo"], "logo.png", { type: "image/png" })

    await user.upload(input, file)
    await screen.findByRole("img", { name: "Organization logo preview" })
    await user.click(screen.getByRole("button", { name: "Remove logo" }))
    expect(
      screen.queryByRole("img", { name: "Organization logo preview" })
    ).toBeNull()
    await user.upload(input, file)

    expect(
      await screen.findByRole("img", { name: "Organization logo preview" })
    ).toHaveAttribute("src", expect.stringMatching(/^data:image\/png;base64,/))
  })

  it("accepts 80 non-BMP characters and rejects 81 by Unicode code point", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValueOnce(response(organization))
    )
    const user = userEvent.setup()
    render(<OrganizationSettings />)
    const name = await screen.findByLabelText("Display name")

    await user.clear(name)
    await user.type(name, "🚀".repeat(80))

    expect(name).toHaveValue("🚀".repeat(80))
    expect(screen.getByRole("button", { name: "Save changes" })).toBeEnabled()

    fireEvent.change(name, { target: { value: "🚀".repeat(81) } })
    expect(screen.getByRole("button", { name: "Save changes" })).toBeDisabled()
  })

  it("shows load errors with a retry action", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(response({ error: "unavailable" }, 503))
      .mockResolvedValueOnce(response(organization))
    vi.stubGlobal("fetch", fetch)
    render(<OrganizationSettings />)

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not load organization settings."
    )
    fireEvent.click(screen.getByRole("button", { name: "Try again" }))
    expect(await screen.findByDisplayValue("Acme Product")).toBeVisible()
  })
})
