import { expect, it } from "vitest"
import type { TabItem } from "@/stores/tab-store"
import {
  projectWorkspacePresentationGroups,
  selectWorkspacePresentationTabs,
} from "./use-workspace-presentation-tabs"

const tab = (id: string, folderId: number): TabItem => ({
  id,
  kind: "conversation",
  folderId,
  conversationId: Number(id.replace(/\D/g, "")) || 1,
  agentType: "codex",
  title: id,
  isPinned: true,
})

it("projects hidden-only groups and hidden selections out of catalog layout", () => {
  const legacy = tab("legacy-1", 1)
  const managedOne = tab("managed-2", 2)
  const managedTwo = tab("managed-3", 2)
  const visible = selectWorkspacePresentationTabs(
    [legacy, managedOne, managedTwo],
    [
      { id: 1, kind: "regular" },
      { id: 2, kind: "workspace_chat" },
    ] as never,
    true
  )
  const projected = projectWorkspacePresentationGroups(
    visible,
    {
      type: "split",
      id: "split",
      orientation: "horizontal",
      children: [
        { type: "group", id: "legacy-only" },
        { type: "group", id: "mixed" },
      ],
      ratios: [0.5, 0.5],
    },
    {
      "legacy-1": "legacy-only",
      "managed-2": "mixed",
      "managed-3": "mixed",
    },
    { "legacy-only": "legacy-1", mixed: "legacy-1" }
  )

  expect(projected.layout).toEqual({ type: "group", id: "mixed" })
  expect(projected.selection).toEqual({ mixed: "managed-2" })
  expect(projected.sourceLayoutUnchanged).toBe(false)
})
