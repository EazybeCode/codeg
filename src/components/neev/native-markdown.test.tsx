import { render, screen, waitFor, within } from "@testing-library/react"
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

/** Render a reply the way the chat does, with a `streaming` flag. */
function show(markdown: string, streaming = false) {
  return render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <NativeMarkdown markdown={markdown} streaming={streaming} />
    </NextIntlClientProvider>
  )
}

it("reads a half-arrived table, emphasis and code fence as what they are becoming", async () => {
  // Mid-stream the closing syntax has not been typed yet. Read literally, each
  // of these renders as its own punctuation and then snaps into place a chunk
  // later — which is the flicker the reply was showing. Each is tested at the
  // END of the text, because that is the only place a stream can be cut off.
  const bold = show("A **strong sta", true)
  expect(screen.getByText(/strong sta/).tagName).toBe("STRONG")
  expect(bold.container).not.toHaveTextContent("**")
  bold.unmount()

  const table = show("| Section | Proposed |\n|---|---|\n| Hero | One li", true)
  await waitFor(() => expect(screen.queryByRole("table")).not.toBeNull())
  const grid = screen.getByRole("table")
  expect(
    within(grid).getByRole("columnheader", { name: "Proposed" })
  ).toBeVisible()
  expect(within(grid).getByRole("cell", { name: "Hero" })).toBeVisible()
  expect(table.container).not.toHaveTextContent("|---|")
  table.unmount()

  const code = show("```ts\nconst a = 1", true)
  await waitFor(() =>
    expect(code.container.querySelector("pre")).toHaveTextContent("const a = 1")
  )
  expect(code.container).not.toHaveTextContent("```ts")
})

it("adds nothing to a finished reply that a half-arrived one would have closed", async () => {
  // The repair appends closers. On text that is already complete it can close
  // a span that was never open — a glob inside code, an identifier with an
  // underscore — and write a character the author did not. Stored replies are
  // rendered static precisely so that cannot happen.
  const finished = "Ignore `foo/*` and _meta and `_blank`.\n\n```sh\nls\n```"
  const { container } = show(finished)
  await waitFor(() =>
    expect(container.querySelector("pre")).toHaveTextContent("ls")
  )
  expect(screen.getByText(/Ignore/)).toHaveTextContent(
    "Ignore foo/* and _meta and _blank."
  )
  // The fence closed, so its content is code and not three loose backticks.
  expect(container).not.toHaveTextContent("```")
  expect(container.querySelectorAll("em")).toHaveLength(0)
})

it("leaves the blocks it has already drawn alone while the last one grows", async () => {
  // What makes a long reply readable while it streams: a finished paragraph is
  // not re-rendered — and so not re-laid-out under the reader — every time a
  // token lands at the end of the one below it.
  const { container, rerender } = show("First settled.\n\nSecond gro", true)
  await waitFor(() =>
    expect(container.querySelectorAll("p").length).toBeGreaterThan(1)
  )
  const first = container.querySelectorAll("p")[0]
  expect(first).toHaveTextContent("First settled.")
  rerender(
    <NextIntlClientProvider locale="en" messages={messages}>
      <NativeMarkdown
        markdown={"First settled.\n\nSecond growing longer."}
        streaming
      />
    </NextIntlClientProvider>
  )
  await waitFor(() =>
    expect(container).toHaveTextContent("Second growing longer.")
  )
  // The same node, not an equal one: it was never torn down and rebuilt.
  expect(container.querySelectorAll("p")[0]).toBe(first)
})
