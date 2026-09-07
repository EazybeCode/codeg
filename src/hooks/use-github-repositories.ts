import { useEffect, useState } from "react"
import {
  githubProjectPost,
  type GithubRepository,
} from "@/lib/github-project-api"

export function useGithubRepositories(open: boolean) {
  const [repos, setRepos] = useState<GithubRepository[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<unknown>(null)
  const [attempt, setAttempt] = useState(0)
  useEffect(() => {
    if (!open) return
    const controller = new AbortController()
    // A newly opened dialog must not show results from the previous request.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setRepos([])
    setError(null)
    setLoading(true)
    githubProjectPost<GithubRepository[]>(
      "list_github_repos",
      undefined,
      controller.signal
    )
      .then((result) => {
        if (!controller.signal.aborted) setRepos(result)
      })
      .catch((cause) => {
        if (!controller.signal.aborted) setError(cause)
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false)
      })
    return () => controller.abort()
  }, [open, attempt])
  return {
    repos,
    loading,
    error,
    retry: () => setAttempt((value) => value + 1),
  }
}
