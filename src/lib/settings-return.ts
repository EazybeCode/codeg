function positiveId(params: URLSearchParams, key: string): string | null {
  const values = params.getAll(key)
  if (values.length !== 1 || !/^[1-9]\d*$/.test(values[0])) return null
  return Number.isSafeInteger(Number(values[0])) ? values[0] : null
}

// A fixed route and a small allowlist avoid forwarding credentials, settings
// filters, or arbitrary return URLs into the workspace.
export function workspaceReturnHref(search: string): string {
  const source = new URLSearchParams(search)
  const target = new URLSearchParams()
  const chatId = positiveId(source, "returnChatId")
  if (chatId) target.set("chatId", chatId)
  const remoteId = positiveId(source, "remoteConnectionId")
  if (remoteId) {
    target.set("remoteConnectionId", remoteId)
    const windowIds = source.getAll("remoteWindowId")
    if (windowIds.length === 1 && /^[\w-]{1,128}$/.test(windowIds[0])) {
      target.set("remoteWindowId", windowIds[0])
    }
  }
  const query = target.toString()
  return query ? `/workspace?${query}` : "/workspace"
}

export function settingsReturnPath(
  path: string,
  workspaceSearch: string
): string {
  const chatId = positiveId(new URLSearchParams(workspaceSearch), "chatId")
  if (!chatId) return path
  const hashIndex = path.indexOf("#")
  const route = hashIndex < 0 ? path : path.slice(0, hashIndex)
  const hash = hashIndex < 0 ? "" : path.slice(hashIndex)
  return `${route}${route.includes("?") ? "&" : "?"}returnChatId=${chatId}${hash}`
}
