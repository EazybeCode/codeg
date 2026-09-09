import { render, screen } from "@testing-library/react"
import { describe, expect, it } from "vitest"

import { AppBootLoading } from "./app-boot-loading"

describe("AppBootLoading", () => {
  it("shows the Neev product wordmark", () => {
    render(<AppBootLoading />)
    expect(screen.getByText("Neev")).toBeVisible()
    expect(screen.queryByText("Eazybe WorkOS")).toBeNull()
  })
})
