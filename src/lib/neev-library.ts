import { NativeClientError, nativeMutation, nativeRequest } from "./neev-client"

export interface LibraryItem {
  id: string
  name: string
  description: string
  kind: "skill" | "mcp"
  enabled: boolean
  revision: string
  files: string[]
  setupNote: string | null
}
function invalid(): never {
  throw new NativeClientError("invalid_response")
}
export function parseLibraryItem(value: unknown): LibraryItem {
  if (!value || typeof value !== "object") return invalid()
  const o = value as Record<string, unknown>
  if (
    typeof o.id !== "string" ||
    !/^(?:[a-f0-9]{32}|[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12})$/i.test(
      o.id
    ) ||
    typeof o.name !== "string" ||
    !o.name ||
    typeof o.description !== "string" ||
    (o.kind !== "skill" && o.kind !== "mcp") ||
    typeof o.enabled !== "boolean" ||
    typeof o.revision !== "string" ||
    !/^[1-9]\d{0,18}$/.test(o.revision) ||
    !Array.isArray(o.files) ||
    !o.files.every((f) => typeof f === "string") ||
    !(o.setupNote === null || typeof o.setupNote === "string")
  )
    return invalid()
  return {
    id: o.id,
    name: o.name,
    description: o.description,
    kind: o.kind,
    enabled: o.enabled,
    revision: o.revision,
    files: o.files,
    setupNote: o.setupNote,
  }
}
export function parseLibraryItems(value: unknown): LibraryItem[] {
  if (
    !value ||
    typeof value !== "object" ||
    !("items" in value) ||
    !Array.isArray(value.items)
  )
    return invalid()
  return value.items.map(parseLibraryItem)
}
export async function loadLibrary(signal?: AbortSignal) {
  return parseLibraryItems(await nativeRequest("/api/neev/library", {}, signal))
}
function fileBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onerror = () => reject(new NativeClientError("invalid_request"))
    reader.onload = () => {
      const value = reader.result
      if (typeof value !== "string" || !value.includes(","))
        reject(new NativeClientError("invalid_request"))
      else resolve(value.slice(value.indexOf(",") + 1))
    }
    reader.readAsDataURL(file)
  })
}
export async function importLibrary(
  file: File,
  csrf: string,
  signal?: AbortSignal
) {
  const data = await fileBase64(file)
  return parseLibraryItems(
    await nativeMutation(
      "/api/neev/library/import",
      { fileName: file.name, data },
      csrf,
      signal
    )
  )
}
export async function enableLibraryItem(
  item: LibraryItem,
  enabled: boolean,
  csrf: string,
  signal?: AbortSignal
) {
  return parseLibraryItem(
    await nativeMutation(
      `/api/neev/library/${item.id}`,
      { expectedRevision: item.revision, enabled },
      csrf,
      signal,
      "PATCH"
    )
  )
}
export async function deleteLibraryItem(
  item: LibraryItem,
  csrf: string,
  signal?: AbortSignal
) {
  await nativeMutation(
    `/api/neev/library/${item.id}`,
    { expectedRevision: item.revision },
    csrf,
    signal,
    "DELETE"
  )
}
