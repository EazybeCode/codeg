import { render, screen, fireEvent, cleanup } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import ja from "@/i18n/messages/ja.json"
import { GithubLoadError } from "./github-load-error"
import { GithubProjectError } from "@/lib/github-project-api"

afterEach(cleanup)
it("renders Japanese recovery and retains working reconnect and retry actions", () => {
  const retry = vi.fn()
  render(
    <NextIntlClientProvider locale="ja" messages={ja}>
      <GithubLoadError
        error={new GithubProjectError("expired", 401)}
        retry={retry}
      />
    </NextIntlClientProvider>
  )
  expect(screen.getByRole("link", { name: "GitHub に再接続" })).toHaveAttribute(
    "href",
    "/auth/github/login"
  )
  fireEvent.click(screen.getByRole("button", { name: "再試行" }))
  expect(retry).toHaveBeenCalledOnce()
})
