import { render, screen } from "@testing-library/react"
import Ansi from "ansi-to-react"
import { describe, expect, it, vi } from "vitest"
import { Terminal } from "./terminal"

vi.mock("next-intl", () => ({
  useTranslations: () => (key: string) => key,
}))

describe("terminal linkification compatibility", () => {
  it("renders HTTP and mailto links through ansi-to-react's optional path", () => {
    render(
      <Ansi linkify="fuzzy">
        {"Docs: https://example.com/guide Email: mailto:help@example.com"}
      </Ansi>
    )

    expect(screen.getByRole("link", { name: "https://example.com/guide" }))
      .toHaveAttribute("href", "https://example.com/guide")
    expect(screen.getByRole("link", { name: "mailto:help@example.com" }))
      .toHaveAttribute("href", "mailto:help@example.com")
  })

  it("keeps links disabled in the application's terminal output", () => {
    render(<Terminal output="Logs: https://example.com/guide" />)

    expect(screen.queryByRole("link")).not.toBeInTheDocument()
    expect(screen.getByText(/https:\/\/example\.com\/guide/)).toBeInTheDocument()
  })
})
