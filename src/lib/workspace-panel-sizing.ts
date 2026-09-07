export function workspacePanelSizing(
  desired: number,
  minPixels: number,
  maxPixels: number,
  totalPixels: number
) {
  if (desired === 0) return { defaultSize: 0, minSize: 0, maxSize: 0 }
  // The shell has already shared the available space between its siblings.
  // At narrow widths that allocation can be below a panel's pixel minimum.
  const minSize = Math.min(desired, (minPixels / totalPixels) * 100)
  const maxSize = Math.max(
    desired,
    Math.min(100, (maxPixels / totalPixels) * 100)
  )
  return { defaultSize: desired, minSize, maxSize }
}
