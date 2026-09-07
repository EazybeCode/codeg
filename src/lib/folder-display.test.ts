import { describe, expect, it } from "vitest"
import {
  excludeChatFolders,
  filterTopLevelFolders,
  formatFolderLabelWithAlias,
  resolveFolderBreadcrumb,
  resolveFolderDisplayName,
  resolveKnownRepoFolder,
  resolvePickerSelectedFolderId,
} from "@/lib/folder-display"

const folders = [
  { id: 1, name: "myproject" },
  { id: 2, name: "myproject-feature-x" },
]

describe("resolveFolderDisplayName", () => {
  it("returns the folder's own name for a top-level (non-worktree) folder", () => {
    expect(
      resolveFolderDisplayName(
        { name: "myproject", parent_id: null, kind: "regular" },
        folders
      )
    ).toBe("myproject")
  })

  it("returns the parent (root repo) name for a worktree folder", () => {
    expect(
      resolveFolderDisplayName(
        { name: "myproject-feature-x", parent_id: 1, kind: "regular" },
        folders
      )
    ).toBe("myproject")
  })

  it("falls back to the folder's own name when the parent is absent", () => {
    expect(
      resolveFolderDisplayName(
        { name: "myproject-feature-x", parent_id: 99, kind: "regular" },
        folders
      )
    ).toBe("myproject-feature-x")
  })

  it("falls back when the folder list is empty", () => {
    expect(
      resolveFolderDisplayName(
        { name: "wt", parent_id: 1, kind: "regular" },
        []
      )
    ).toBe("wt")
  })

  it("keeps an organizational subfolder's own identity", () => {
    expect(
      resolveFolderDisplayName(
        { name: "Analytics", parent_id: 3, kind: "subfolder" },
        [{ id: 3, name: "docs" }]
      )
    ).toBe("Analytics")
  })
})

describe("resolveFolderBreadcrumb", () => {
  const tree = [
    { id: 1, name: "workos", alias: "Product", parent_id: null },
    { id: 2, name: "docs", alias: null, parent_id: 1 },
    { id: 3, name: "api", alias: "API", parent_id: 2 },
  ]

  it("returns the complete logical ancestor path with aliases", () => {
    expect(resolveFolderBreadcrumb(3, tree)).toEqual([
      { id: 1, label: "Product", name: "workos" },
      { id: 2, label: "docs", name: "docs" },
      { id: 3, label: "API", name: "api" },
    ])
  })

  it("stops at a missing ancestor without losing known descendants", () => {
    expect(
      resolveFolderBreadcrumb(3, [
        { id: 2, name: "docs", alias: null, parent_id: 99 },
        { id: 3, name: "api", alias: null, parent_id: 2 },
      ])
    ).toEqual([
      { id: 2, label: "docs", name: "docs" },
      { id: 3, label: "api", name: "api" },
    ])
  })

  it("stops before repeating a folder when the hierarchy contains a cycle", () => {
    expect(
      resolveFolderBreadcrumb(2, [
        { id: 1, name: "one", alias: null, parent_id: 2 },
        { id: 2, name: "two", alias: null, parent_id: 1 },
      ])
    ).toEqual([
      { id: 1, label: "one", name: "one" },
      { id: 2, label: "two", name: "two" },
    ])
  })
})

describe("resolveKnownRepoFolder", () => {
  const tree = [
    { id: 1, parent_id: null, kind: "regular" as const, repo_full_name: null },
    { id: 2, parent_id: 1, kind: "subfolder" as const, repo_full_name: null },
    { id: 3, parent_id: 2, kind: "subfolder" as const, repo_full_name: null },
    {
      id: 4,
      parent_id: 2,
      kind: "subfolder" as const,
      repo_full_name: "acme/override",
    },
    { id: 5, parent_id: 1, kind: "regular" as const, repo_full_name: null },
  ]

  it("uses the enclosing repo for an inherited organizational subfolder", () => {
    expect(resolveKnownRepoFolder(tree[2], tree)?.id).toBe(1)
  })

  it("uses an explicit nested repo override as its own Git root", () => {
    expect(resolveKnownRepoFolder(tree[3], tree)?.id).toBe(4)
  })

  it("keeps a runtime worktree on its own execution root", () => {
    expect(resolveKnownRepoFolder(tree[4], tree)?.id).toBe(5)
  })
})

describe("filterTopLevelFolders", () => {
  it("keeps only folders without a parent_id (drops worktrees)", () => {
    const list = [
      { id: 1, parent_id: null },
      { id: 2, parent_id: 1 },
      { id: 3, parent_id: null },
      { id: 4, parent_id: 3 },
    ]
    expect(filterTopLevelFolders(list).map((f) => f.id)).toEqual([1, 3])
  })

  it("returns all folders when none are worktrees", () => {
    const list = [
      { id: 1, parent_id: null },
      { id: 2, parent_id: null },
    ]
    expect(filterTopLevelFolders(list)).toHaveLength(2)
  })
})

describe("excludeChatFolders", () => {
  it("drops hidden chat folders, keeping real ones", () => {
    const list = [
      { id: 1, kind: "regular" as const },
      { id: 2, kind: "chat" as const },
      { id: 3, kind: "regular" as const },
    ]
    expect(excludeChatFolders(list).map((f) => f.id)).toEqual([1, 3])
  })

  it("returns all folders when none are chat folders", () => {
    const list = [
      { id: 1, kind: "regular" as const },
      { id: 2, kind: "regular" as const },
    ]
    expect(excludeChatFolders(list)).toHaveLength(2)
  })
})

describe("resolvePickerSelectedFolderId", () => {
  it("returns the folder's own id for a top-level folder", () => {
    expect(resolvePickerSelectedFolderId({ id: 5, parent_id: null })).toBe(5)
  })

  it("returns the parent id for a worktree folder", () => {
    expect(resolvePickerSelectedFolderId({ id: 7, parent_id: 3 })).toBe(3)
  })
})

describe("formatFolderLabelWithAlias", () => {
  it("renders `alias [ name ]` (spaced) when an alias is set", () => {
    expect(
      formatFolderLabelWithAlias({ name: "codeg", alias: "My Project" })
    ).toBe("My Project [ codeg ]")
  })

  it("falls back to the bare name when alias is null", () => {
    expect(formatFolderLabelWithAlias({ name: "codeg", alias: null })).toBe(
      "codeg"
    )
  })

  it("treats an empty / whitespace-only alias as unset", () => {
    expect(formatFolderLabelWithAlias({ name: "codeg", alias: "   " })).toBe(
      "codeg"
    )
  })

  it("trims surrounding whitespace from the alias", () => {
    expect(
      formatFolderLabelWithAlias({ name: "codeg", alias: "  Work  " })
    ).toBe("Work [ codeg ]")
  })
})
