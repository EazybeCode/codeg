import { render, screen, within } from "@testing-library/react"
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

it("renders a GitHub-flavoured table as a table, not as its own source", () => {
  const { container } = render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <NativeMarkdown
        markdown={
          "| Section | Proposed Eazybe version |\n|---|---|\n| Hero | One line |\n| Pricing | Two lines |"
        }
      />
    </NextIntlClientProvider>
  )
  const table = screen.getByRole("table")
  expect(
    within(table).getByRole("columnheader", {
      name: "Proposed Eazybe version",
    })
  ).toBeVisible()
  expect(within(table).getAllByRole("row")).toHaveLength(3)
  expect(within(table).getByRole("cell", { name: "Two lines" })).toBeVisible()
  // The pipes are gone because they were read, not because they were dropped.
  expect(container).not.toHaveTextContent("|---|")
  // A table wider than the reply scrolls itself rather than widening the column
  // it sits in.
  expect(table.parentElement).toHaveClass("overflow-x-auto")
})

it("reads the rest of GitHub-flavoured Markdown an agent actually writes", () => {
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <NativeMarkdown
        markdown={
          "~~dropped~~ kept\n\n- [x] shipped\n- [ ] pending\n\nSee https://example.com/docs"
        }
      />
    </NextIntlClientProvider>
  )
  expect(screen.getByText("dropped").tagName).toBe("DEL")
  const boxes = screen.getAllByRole("checkbox")
  expect(boxes).toHaveLength(2)
  expect(boxes[0]).toBeChecked()
  expect(boxes[1]).not.toBeChecked()
  // Nothing in a reply is the reader's to tick: the list reports state.
  for (const box of boxes) expect(box).toBeDisabled()
  expect(
    screen.getByRole("link", { name: "https://example.com/docs" })
  ).toHaveAttribute("href", "https://example.com/docs")
})

it("holds GitHub-flavoured Markdown to the same link and HTML refusals", () => {
  const { container } = render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <NativeMarkdown
        markdown={
          '| Link |\n|---|\n| [bad](javascript:alert) |\n| <img src="https://example.com/a"> |'
        }
      />
    </NextIntlClientProvider>
  )
  expect(screen.getByRole("table")).toBeVisible()
  expect(screen.queryByRole("link", { name: "bad" })).toBeNull()
  expect(container.querySelector("img,iframe,script,style")).toBeNull()
})
