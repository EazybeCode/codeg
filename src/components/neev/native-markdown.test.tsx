import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"
import { NativeMarkdown } from "./native-markdown"

it("renders readable Markdown without mounting remote images or active HTML", () => {
  const fetcher = vi.spyOn(globalThis, "fetch")
  const { container } = render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <NativeMarkdown
        markdown={
          '# Notes\n\n**Strong** [safe](https://example.com) [anchor](#notes) [bad](javascript:alert) [data](data:text/html,hi) [local](/private)\n\n![Tracking pixel](https://example.com/pixel.png)\n\n<script>alert(1)</script><iframe src="https://example.com"></iframe><img src="https://example.com/a"><style>body{display:none}</style>'
        }
      />
    </NextIntlClientProvider>
  )
  expect(screen.getByRole("heading", { name: "Notes" })).toBeVisible()
  expect(screen.getByRole("link", { name: "safe" })).toHaveAttribute(
    "rel",
    "noopener noreferrer"
  )
  expect(screen.getByRole("link", { name: "anchor" })).toHaveAttribute(
    "href",
    "#notes"
  )
  for (const name of ["bad", "data", "local"])
    expect(screen.queryByRole("link", { name })).toBeNull()
  expect(screen.getByRole("link", { name: /Tracking pixel/ })).toHaveAttribute(
    "href",
    "https://example.com/pixel.png"
  )
  expect(
    container.querySelector("img,iframe,object,embed,style,script,link")
  ).toBeNull()
  expect(fetcher).not.toHaveBeenCalled()
  fetcher.mockRestore()
})
