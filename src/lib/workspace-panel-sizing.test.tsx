import { render, cleanup } from "@testing-library/react"
import { Panel, PanelGroup } from "react-resizable-panels"
import { workspacePanelSizing } from "./workspace-panel-sizing"

afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
})

describe("workspace panel initial sizing", () => {
  it.each([
    ["closed terminal", 0, 180, 800, 900, 0, 0],
    ["closed sidebar", 0, 300, 900, 1440, 0, 0],
    ["saved sidebar", 25, 300, 900, 1440, (300 / 1440) * 100, 62.5],
    ["narrow sidebar", 20, 300, 900, 800, 20, 100],
  ])(
    "mounts %s with a valid initial range",
    (_, desired, min, max, total, wantMin, wantMax) => {
      // A fixed default or unscaled minimum causes the real panel library to
      // complain before the shell's later imperative resize can run.
      const error = vi.spyOn(console, "error")
      const props = workspacePanelSizing(desired, min, max, total)
      render(
        <PanelGroup direction="horizontal">
          <Panel {...props}>Side</Panel>
          <Panel defaultSize={100 - desired}>Center</Panel>
        </PanelGroup>
      )
      expect(props).toEqual({
        defaultSize: desired,
        minSize: wantMin,
        maxSize: wantMax,
      })
      expect(error.mock.calls.flat().join(" ")).not.toMatch(
        /default size|size constraint/i
      )
      error.mockRestore()
    }
  )
})
