import { getCodegToken } from "@/lib/transport/web-auth"

export type GithubRepository = {
  full_name: string
  name: string
  owner: string
  private: boolean
  default_branch: string
  description?: string | null
}

export class GithubProjectError extends Error {
  constructor(
    message: string,
    readonly status: number
  ) {
    super(message)
  }
}

export async function githubProjectPost<T>(
  path: string,
  body?: unknown,
  signal?: AbortSignal
): Promise<T> {
  const response = await fetch(`/api/${path}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${getCodegToken()}`,
    },
    body: JSON.stringify(body ?? {}),
    signal,
  })
  const data = await response.json().catch(() => ({}))
  if (!response.ok || data?.error) {
    throw new GithubProjectError(
      typeof data?.error === "string"
        ? data.error
        : response.statusText || "Request failed",
      response.status
    )
  }
  return data as T
}
