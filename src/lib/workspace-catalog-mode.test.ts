import { expect, it } from "vitest"
import { canRestoreCatalogConversation } from "./workspace-catalog-mode"

it("restores only canonical managed conversations in catalog mode", () => {
  expect(canRestoreCatalogConversation(false, false, null)).toBe(true)
  expect(canRestoreCatalogConversation(true, true, 42)).toBe(true)
  expect(canRestoreCatalogConversation(true, false, 42)).toBe(false)
  expect(canRestoreCatalogConversation(true, true, null)).toBe(false)
})
