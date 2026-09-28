"use client"

import { useEffect, useRef, useState } from "react"
import { BookOpen, Plug, Trash2, Upload } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Dialog, DialogDescription, DialogTitle } from "@/components/ui/dialog"
import { NativeDialogContent as DialogContent } from "./native-suspended-surface"
import { NativeClientError } from "@/lib/neev-client"
import {
  deleteLibraryItem,
  enableLibraryItem,
  importLibrary,
  loadLibrary,
  type LibraryItem,
} from "@/lib/neev-library"
import { useNativeAuth } from "./native-auth-provider"
import { NativeError } from "./native-boundary"
import { cn } from "@/lib/utils"

export function NativeLibrary() {
  const auth = useNativeAuth()
  const { handleError } = auth
  const admin = auth.session?.role === "admin"
  const csrf = auth.session?.csrf ?? ""
  const orgId = auth.session?.organization?.id
  const [items, setItems] = useState<LibraryItem[]>([])
  const [kind, setKind] = useState<"skill" | "mcp">("skill")
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>(null)
  const [notice, setNotice] = useState("")
  const [failures, setFailures] = useState<string[]>([])
  const [remove, setRemove] = useState<LibraryItem | null>(null)
  const [refresh, setRefresh] = useState(0)
  const alive = useRef(false)
  const locked = useRef(false)

  useEffect(() => {
    if (!orgId) return
    alive.current = true
    const controller = new AbortController()
    setLoading(true)
    void loadLibrary(controller.signal)
      .then((rows) => {
        if (!controller.signal.aborted) {
          setItems(rows)
          setError(null)
        }
      })
      .catch((e) => {
        if (!controller.signal.aborted) {
          setError(e)
          handleError(e)
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false)
      })
    return () => {
      alive.current = false
      controller.abort()
    }
  }, [orgId, handleError, refresh])

  async function mutate(action: () => Promise<void>) {
    if (locked.current || !csrf || !admin) return
    locked.current = true
    setBusy(true)
    setError(null)
    setNotice("")
    try {
      await action()
    } catch (e) {
      if (alive.current && !auth.signal?.aborted) {
        setError(e)
        handleError(e)
        if (e instanceof NativeClientError && e.kind === "conflict") {
          setRemove(null)
          setNotice(
            "This item changed. The latest workspace library has been loaded; please review it and try again."
          )
          setRefresh((n) => n + 1)
        }
      }
    } finally {
      locked.current = false
      if (alive.current) setBusy(false)
    }
  }
  async function upload(files: File[]) {
    setFailures([])
    await mutate(async () => {
      let count = 0
      const failed: string[] = []
      for (const file of files) {
        if (!alive.current || auth.signal?.aborted) break
        try {
          const added = await importLibrary(file, csrf, auth.signal)
          if (!alive.current || auth.signal?.aborted) return
          count += added.length
          if (added[0]) setKind(added[0].kind)
          setItems((rows) => [
            ...rows.filter((row) => !added.some((item) => item.id === row.id)),
            ...added,
          ])
        } catch (e) {
          if (
            e instanceof NativeClientError &&
            ["unauthorized", "forbidden", "aborted"].includes(e.kind)
          )
            throw e
          failed.push(file.name)
        }
      }
      if (alive.current && !auth.signal?.aborted) {
        setFailures(failed)
        if (count)
          setNotice(
            `${count} ${count === 1 ? "item imported" : "items imported"}. Review and enable for your team.`
          )
      }
    })
  }
  const visible = items.filter((item) => item.kind === kind)
  return (
    <section className="space-y-5" aria-label="Shared library">
      <header className="space-y-2">
        <h2 className="text-base font-medium">Shared library</h2>
        <p className="text-sm text-muted-foreground">
          Skills and integrations for everyone in{" "}
          {auth.session?.organization?.name ?? "your workspace"}.
        </p>
        <p className="text-xs text-muted-foreground">
          Enabled items are available in new agent turns. Personal AI sign-ins
          stay private.
        </p>
      </header>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div
          className="flex gap-1 rounded-lg bg-muted p-1"
          role="group"
          aria-label="Library type"
        >
          {(["skill", "mcp"] as const).map((value) => (
            <button
              key={value}
              type="button"
              aria-pressed={kind === value}
              onClick={() => setKind(value)}
              className={cn(
                "rounded-md px-3 py-1.5 text-sm transition-colors",
                kind === value
                  ? "bg-background text-foreground shadow-sm"
                  : "text-muted-foreground hover:text-foreground"
              )}
            >
              {value === "skill" ? "Skills" : "Integrations"}
            </button>
          ))}
        </div>
        {admin && (
          <label
            className={cn(
              "relative inline-flex cursor-pointer items-center gap-2 rounded-md border px-3 py-2 text-sm hover:bg-muted focus-within:ring-2 focus-within:ring-ring",
              busy && "pointer-events-none opacity-50"
            )}
          >
            <Upload className="size-3.5" aria-hidden />
            {busy ? "Working…" : "Import"}
            <input
              type="file"
              aria-label="Import files"
              accept=".zip,.md,.json"
              multiple
              disabled={busy}
              className="absolute inset-0 w-full cursor-pointer opacity-0"
              onChange={(event) => {
                const files = Array.from(event.target.files ?? [])
                event.target.value = ""
                if (files.length) void upload(files)
              }}
            />
          </label>
        )}
      </div>
      <NativeError error={error} />
      {notice && (
        <p role="status" className="text-sm text-muted-foreground">
          {notice}
        </p>
      )}
      {failures.length > 0 && (
        <div role="alert" className="space-y-1 text-sm text-destructive">
          <p>Could not import: {failures.join(", ")}.</p>
          <p>
            Use a skill ZIP containing SKILL.md, a SKILL.md file, or MCP JSON
            without credentials. Check archive paths and supported
            configuration.
          </p>
        </div>
      )}
      {loading ? (
        <p role="status" className="py-8 text-sm text-muted-foreground">
          Loading library…
        </p>
      ) : error && !items.length ? (
        <Button variant="outline" onClick={() => setRefresh((n) => n + 1)}>
          Retry
        </Button>
      ) : visible.length === 0 ? (
        <div className="rounded-xl border border-dashed px-5 py-8 text-center">
          {kind === "skill" ? (
            <BookOpen
              className="mx-auto mb-3 size-5 text-muted-foreground"
              aria-hidden
            />
          ) : (
            <Plug
              className="mx-auto mb-3 size-5 text-muted-foreground"
              aria-hidden
            />
          )}
          <p className="text-sm">
            {kind === "skill"
              ? "A shared toolkit for your team"
              : "Connect your team’s tools"}
          </p>
          <p className="mx-auto mt-2 max-w-sm text-xs text-muted-foreground">
            {admin
              ? kind === "skill"
                ? "Import skill ZIPs or SKILL.md files, then enable them for your workspace."
                : "Import MCP JSON configurations. Remote services may need server setup before they can run."
              : "Your workspace admin can add and enable shared skills and integrations here."}
          </p>
        </div>
      ) : (
        <ul className="divide-y rounded-xl border">
          {visible.map((item) => (
            <li key={item.id} className="flex items-start gap-3 p-4">
              {item.kind === "skill" ? (
                <BookOpen
                  className="mt-0.5 size-4 shrink-0 text-muted-foreground"
                  aria-hidden
                />
              ) : (
                <Plug
                  className="mt-0.5 size-4 shrink-0 text-muted-foreground"
                  aria-hidden
                />
              )}
              <div className="min-w-0 flex-1 space-y-1">
                <p className="break-words text-sm font-medium">{item.name}</p>
                {item.description && (
                  <p className="break-words text-xs text-muted-foreground">
                    {item.description}
                  </p>
                )}
                {item.setupNote && (
                  <p className="break-words text-xs text-muted-foreground">
                    {item.setupNote}
                  </p>
                )}
                {item.files.length > 0 && (
                  <details className="text-xs text-muted-foreground">
                    <summary className="cursor-pointer">
                      {item.files.length}{" "}
                      {item.files.length === 1 ? "file" : "files"}
                    </summary>
                    <ul className="mt-1 space-y-1">
                      {item.files.map((file) => (
                        <li key={file} className="break-all">
                          {file}
                        </li>
                      ))}
                    </ul>
                  </details>
                )}
                {!admin && (
                  <p className="text-xs text-muted-foreground">
                    {item.enabled ? "Enabled for workspace" : "Not enabled"}
                  </p>
                )}
              </div>
              {admin && (
                <div className="flex shrink-0 items-center gap-3">
                  <Switch
                    aria-label={`Enable ${item.name}`}
                    checked={item.enabled}
                    disabled={busy}
                    onCheckedChange={(enabled) =>
                      void mutate(async () => {
                        const updated = await enableLibraryItem(
                          item,
                          enabled,
                          csrf,
                          auth.signal
                        )
                        if (alive.current && !auth.signal?.aborted)
                          setItems((rows) =>
                            rows.map((row) =>
                              row.id === updated.id ? updated : row
                            )
                          )
                      })
                    }
                  />
                  <Button
                    size="icon"
                    variant="ghost"
                    aria-label={`Delete ${item.name}`}
                    disabled={busy}
                    onClick={() => setRemove(item)}
                    className="size-7 text-muted-foreground"
                  >
                    <Trash2 className="size-3.5" aria-hidden />
                  </Button>
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
      {admin && (
        <p className="text-xs text-muted-foreground">
          Only enable skills and integrations you trust. They can guide agent
          actions for everyone in this workspace.
        </p>
      )}
      <Dialog
        open={remove !== null}
        onOpenChange={(open) => {
          if (!open && !busy) setRemove(null)
        }}
      >
        <DialogContent>
          <DialogTitle>Delete from workspace?</DialogTitle>
          <DialogDescription>
            {remove?.name} will no longer be available for new agent turns.
            Existing conversations are kept.
          </DialogDescription>
          <div className="flex justify-end gap-2">
            <Button
              variant="outline"
              disabled={busy}
              onClick={() => setRemove(null)}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              disabled={busy}
              onClick={() => {
                if (remove) {
                  const item = remove
                  void mutate(async () => {
                    await deleteLibraryItem(item, csrf, auth.signal)
                    if (alive.current && !auth.signal?.aborted) {
                      setItems((rows) =>
                        rows.filter((row) => row.id !== item.id)
                      )
                      setRemove(null)
                    }
                  })
                }
              }}
            >
              Delete from workspace
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </section>
  )
}
