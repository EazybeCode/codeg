import { readFileSync } from "node:fs"
import { resolve } from "node:path"

const read = (rel: string) => readFileSync(resolve(process.cwd(), rel), "utf8")

const statusBarSource = read("src/components/layout/status-bar.tsx")
const contextBarSource = read(
  "src/components/chat/conversation-context-bar.tsx"
)
const branchDropdownSource = read("src/components/layout/branch-dropdown.tsx")

// The rich git BranchDropdown lives in the document-like conversation header,
// not beside the composer. Logical subfolders resolve the physical repo root
// before handing a folder to Git controls.
describe("branch selector placement", () => {
  it("removes the branch selector from the bottom status bar", () => {
    expect(statusBarSource).not.toContain("BranchDropdown")
    expect(statusBarSource).not.toContain("branch-dropdown")
  })

  it("hosts the rich BranchDropdown in the conversation header", () => {
    expect(contextBarSource).toContain(
      'import { BranchDropdown } from "@/components/layout/branch-dropdown"'
    )
    expect(contextBarSource).toContain(
      "<BranchDropdown folder={branchFolder} isChatMode={isChatMode} />"
    )
  })

  it("retires the basic BranchPicker and the status-bar variant", () => {
    // The basic popover BranchPicker is gone (replaced by BranchDropdown).
    expect(contextBarSource).not.toContain("const BranchPicker")
    expect(contextBarSource).not.toContain("interface BranchPickerProps")
    // BranchDropdown now renders a single composer-chip trigger — the
    // status-bar `showFolderName` split/pill variant is removed.
    expect(branchDropdownSource).not.toContain("showFolderName")
    expect(branchDropdownSource).not.toContain("isStatusBar")
  })
})
