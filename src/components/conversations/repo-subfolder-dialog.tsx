"use client"

import { useEffect, useState } from "react"
import { Loader2, Search } from "lucide-react"
import { toast } from "sonner"
import { listGithubBranches } from "@/lib/api"
import { getCodegToken } from "@/lib/transport/web-auth"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Button } from "@/components/ui/button"
import { Label } from "@/components/ui/label"
import { Switch } from "@/components/ui/switch"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"

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
 * "New subfolder from a GitHub repo": pick a repo → pick (or create) a branch →
 * name the folder → the server clones it into a fresh dir beneath `parentId` and
 * records the repo + branch on the subfolder row. Two-step: repo list first,
 * then a small config step for the picked repo. `parentId === null` closes it.
 */
export function RepoSubfolderDialog({
  parentId,
  onOpenChange,
}: {
  parentId: number | null
  onOpenChange: (open: boolean) => void
}) {
  const createRepoSubfolder = useAppWorkspaceStore((s) => s.createRepoSubfolder)
  const open = parentId !== null

  const [repos, setRepos] = useState<Repo[]>([])
  const [q, setQ] = useState("")
  const [loadingRepos, setLoadingRepos] = useState(false)

  // The picked repo (step 2), or null while still choosing (step 1).
  const [picked, setPicked] = useState<Repo | null>(null)
  const [branches, setBranches] = useState<string[]>([])
  const [loadingBranches, setLoadingBranches] = useState(false)
  const [branch, setBranch] = useState("")
  const [createBranch, setCreateBranch] = useState(false)
  const [newBranch, setNewBranch] = useState("")
  const [name, setName] = useState("")
  const [submitting, setSubmitting] = useState(false)

  // Reset all step state whenever the dialog opens or closes.
  useEffect(() => {
    if (!open) return
    setPicked(null)
    setQ("")
    setLoadingRepos(true)
    apiPost<Repo[]>("list_github_repos")
      .then(setRepos)
      .catch((e) =>
        toast.error("Couldn't load your GitHub repos", {
          description: String(e),
        })
      )
      .finally(() => setLoadingRepos(false))
  }, [open])

  // On repo pick, load its branches and seed the name (default = repo name).
  useEffect(() => {
    if (!picked) return
    setName(picked.name)
    setBranch(picked.default_branch)
    setCreateBranch(false)
    setNewBranch("")
    setLoadingBranches(true)
    listGithubBranches(picked.full_name)
      .then((bs) => {
        const names = bs.map((b) => b.name)
        setBranches(names)
        // Keep the default branch selected if present, else the first branch.
        setBranch(
          names.includes(picked.default_branch)
            ? picked.default_branch
            : (names[0] ?? picked.default_branch)
        )
      })
      .catch((e) =>
        toast.error("Couldn't load branches", { description: String(e) })
      )
      .finally(() => setLoadingBranches(false))
  }, [picked])

  const filtered = repos.filter((r) =>
    r.full_name.toLowerCase().includes(q.trim().toLowerCase())
  )

  async function submit() {
    if (parentId === null || !picked) return
    const folderName = name.trim() || picked.name
    const chosenBranch = createBranch ? newBranch.trim() : branch.trim()
    if (chosenBranch === "") {
      toast.error("A branch name is required")
      return
    }
    setSubmitting(true)
    try {
      await createRepoSubfolder(
        parentId,
        folderName,
        picked.full_name,
        chosenBranch,
        createBranch
      )
      onOpenChange(false)
    } catch (e) {
      toast.error("Couldn't create the repo subfolder", {
        description: String(e),
      })
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {picked ? `Clone ${picked.full_name}` : "New subfolder from GitHub"}
          </DialogTitle>
        </DialogHeader>

        {!picked ? (
          <>
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
              {loadingRepos ? (
                <div className="flex items-center justify-center gap-2 py-10 text-sm text-muted-foreground">
                  <Loader2 className="h-4 w-4 animate-spin" /> Loading your
                  repos…
                </div>
              ) : filtered.length === 0 ? (
                <div className="py-10 text-center text-sm text-muted-foreground">
                  {repos.length === 0
                    ? "No repositories found."
                    : "No matches."}
                </div>
              ) : (
                filtered.map((r) => (
                  <button
                    key={r.full_name}
                    onClick={() => setPicked(r)}
                    className="flex w-full items-center justify-between gap-3 rounded-lg px-3 py-2 text-left hover:bg-accent"
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
                    </div>
                  </button>
                ))
              )}
            </div>
          </>
        ) : (
          <div className="flex flex-col gap-4 py-1">
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="repo-subfolder-name">Folder name</Label>
              <Input
                id="repo-subfolder-name"
                autoFocus
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder={picked.name}
              />
            </div>

            <div className="flex items-center justify-between gap-3">
              <div className="flex flex-col">
                <Label htmlFor="repo-subfolder-create-branch">
                  Create a new branch
                </Label>
                <span className="text-xs text-muted-foreground">
                  Branch off the default and check it out.
                </span>
              </div>
              <Switch
                id="repo-subfolder-create-branch"
                checked={createBranch}
                onCheckedChange={setCreateBranch}
              />
            </div>

            {createBranch ? (
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="repo-subfolder-new-branch">
                  New branch name
                </Label>
                <Input
                  id="repo-subfolder-new-branch"
                  value={newBranch}
                  onChange={(e) => setNewBranch(e.target.value)}
                  placeholder="feature/my-branch"
                />
              </div>
            ) : (
              <div className="flex flex-col gap-1.5">
                <Label>Branch</Label>
                {loadingBranches ? (
                  <div className="flex items-center gap-2 py-2 text-sm text-muted-foreground">
                    <Loader2 className="h-4 w-4 animate-spin" /> Loading
                    branches…
                  </div>
                ) : (
                  <Select value={branch} onValueChange={setBranch}>
                    <SelectTrigger>
                      <SelectValue placeholder="Select a branch" />
                    </SelectTrigger>
                    <SelectContent>
                      {branches.map((b) => (
                        <SelectItem key={b} value={b}>
                          {b}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                )}
              </div>
            )}
          </div>
        )}

        {picked ? (
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setPicked(null)}
              disabled={submitting}
            >
              Back
            </Button>
            <Button onClick={submit} disabled={submitting}>
              {submitting ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : null}
              Create
            </Button>
          </DialogFooter>
        ) : null}
      </DialogContent>
    </Dialog>
  )
}
