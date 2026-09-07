/** Scrubbed catalog/runner errors arrive as parsed transport objects. */
export function workspaceErrorCode(error: unknown): string {
  if (error && typeof error === "object") {
    if ("error" in error && typeof error.error === "string") return error.error
    if ("code" in error && typeof error.code === "string") return error.code
    if ("status" in error && error.status === 401) return "reauth_required"
  }
  return "unavailable"
}

export function workspaceErrorKey(
  error: unknown
):
  | "authError"
  | "repoError"
  | "busyError"
  | "disabledError"
  | "unavailableError" {
  switch (workspaceErrorCode(error)) {
    case "unauthorized":
    case "reauth_required":
      return "authError"
    case "repository_required":
    case "repository_unavailable":
      return "repoError"
    case "busy":
    case "conflict":
      return "busyError"
    case "execution_disabled":
      return "disabledError"
    default:
      return "unavailableError"
  }
}
