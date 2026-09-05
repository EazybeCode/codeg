"use client"

import { useEffect, useState } from "react"
import { Loader2, Search } from "lucide-react"
import { toast } from "sonner"
import { getCodegToken } from "@/lib/transport/web-auth"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"

type Repo = {
  full_name: string
  name: string
  owner: string
  private: boolean
  default_branch: string
  description?: string | null
}

async function apiPost<T>(path: string, body?: unknown): Promise<T> {
  const res = await fetch(`/api/${path}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${getCodegToken()}`,
    },
    body: JSON.stringify(body ?? {}),
  })
  const data = await res.json().catch(() => ({}))
  if (!res.ok || (data && (data as { error?: string }).error)) {
    throw new Error((data as { error?: string }).error || res.statusText)
  }
  return data as T
}

/**
 * "Open GitHub project" — the cloud-native, no-local-clone flow. Lists the
 * signed-in user's GitHub repos (server-side, via their token), and on select
 * the server clones the repo into the org's space and we openFolder() it.
 */
export function GithubProjectDialog({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const openFolder = useAppWorkspaceStore((s) => s.openFolder)
  const [repos, setRepos] = useState<Repo[]>([])
  const [q, setQ] = useState("")
  const [loading, setLoading] = useState(false)
  const [busy, setBusy] = useState<string | null>(null)

  useEffect(() => {
    if (!open) return
    setLoading(true)
    apiPost<Repo[]>("list_github_repos")
      .then(setRepos)
      .catch((e) =>
        toast.error("Couldn't load your GitHub repos", {
          description: String(e),
        })
      )
      .finally(() => setLoading(false))
  }, [open])

  const filtered = repos.filter((r) =>
    r.full_name.toLowerCase().includes(q.trim().toLowerCase())
  )

  async function pick(r: Repo) {
    setBusy(r.full_name)
    try {
      const { path } = await apiPost<{ path: string }>("clone_github_repo", {
        full_name: r.full_name,
      })
      await openFolder(path)
      onOpenChange(false)
    } catch (e) {
      toast.error("Couldn't open repository", { description: String(e) })
    } finally {
      setBusy(null)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Open a GitHub project</DialogTitle>
        </DialogHeader>
        <div className="relative">
          <Search className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-muted-foreground" />
          <Input
            autoFocus
            placeholder="Search your repositories…"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            className="pl-9"
          />
        </div>
        <div className="mt-2 max-h-80 overflow-y-auto">
          {loading ? (
            <div className="flex items-center justify-center gap-2 py-10 text-sm text-muted-foreground">
              <Loader2 className="h-4 w-4 animate-spin" /> Loading your repos…
            </div>
          ) : filtered.length === 0 ? (
            <div className="py-10 text-center text-sm text-muted-foreground">
              {repos.length === 0 ? "No repositories found." : "No matches."}
            </div>
          ) : (
            filtered.map((r) => (
              <button
                key={r.full_name}
                disabled={busy !== null}
                onClick={() => pick(r)}
                className="flex w-full items-center justify-between gap-3 rounded-lg px-3 py-2 text-left hover:bg-accent disabled:opacity-50"
              >
                <div className="min-w-0">
                  <div className="truncate text-sm font-medium">
                    {r.full_name}
                  </div>
                  {r.description ? (
                    <div className="truncate text-xs text-muted-foreground">
                      {r.description}
                    </div>
                  ) : null}
                </div>
                <div className="flex shrink-0 items-center gap-2 text-xs text-muted-foreground">
                  <span>{r.default_branch}</span>
                  <span className="rounded-full border px-1.5 py-0.5">
                    {r.private ? "private" : "public"}
                  </span>
                  {busy === r.full_name ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  ) : null}
                </div>
              </button>
            ))
          )}
        </div>
      </DialogContent>
    </Dialog>
  )
}
