"use client"

// The actions on a completed reply: Copy, and a small menu holding "Fork to
// new tab". Shared by the resource chat and the saved Agent Chat.
//
// A fork is a server-side copy up through one reply into a NEW private
// conversation (see `@/lib/neev-fork`). Nothing runs: no model is asked
// anything. The original stays open where it is; the fork opens in a new tab.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react"
import { useRouter } from "next/navigation"
import { GitFork, MoreHorizontal } from "lucide-react"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { cn } from "@/lib/utils"
import { forkRefusal } from "@/lib/neev-fork"
import { loadNodes, type NativeNode } from "@/lib/neev-client"
import { loadNativeResource } from "@/lib/neev-content-client"
import { loadAgentSession } from "@/lib/neev-agent-sessions-client"
import {
  openTab,
  readTabs,
  writeTabs,
  type WorkspaceTab,
} from "@/lib/neev-workspace-tabs"
import { useNativeAuth } from "./native-auth-provider"
import { NativeError } from "./native-boundary"
import { CopyMessageButton } from "./native-copy"
import { NativeDropdownMenuContent as DropdownMenuContent } from "./native-suspended-surface"

/** What a fork produced, by the id the existing APIs load it with. */
export type ForkTarget =
  | { kind: "resource"; resourceId: string }
  | { kind: "agent"; sessionId: string }

/**
 * Open a fork in a NEW tab, never in place of the one in front: the original
 * must stay open. `tab_limit` means the strip is full; nothing was replaced.
 */
export type OpenFork = (target: ForkTarget) => Promise<"opened" | "tab_limit">

/** Provided by the workspace, which owns the tab strip. */
export const ForkTabsContext = createContext<OpenFork | null>(null)

/**
 * Where a fork goes outside the workspace (a standalone view): the saved
 * strip gains a tab, and the workspace opens on it. Same rule — added, never
 * replacing, and refused at the limit.
 */
function useStoredForkTabs(): OpenFork {
  const auth = useNativeAuth()
  const router = useRouter()
  const user = auth.session?.user.id
  const org = auth.session?.organization?.id
  return useCallback(
    async (target) => {
      if (user === undefined || org === undefined) return "tab_limit"
      let tab: WorkspaceTab
      if (target.kind === "resource") {
        // Read through the ordinary API first: it is what says this reader
        // can open it, and it names the folder the tab belongs to.
        const resource = await loadNativeResource(org, target.resourceId)
        tab = {
          kind: "resource",
          resourceId: resource.id,
          nodeId: resource.nodeId,
        }
      } else {
        const session = await loadAgentSession(target.sessionId)
        tab = { kind: "agent", sessionId: session.id }
      }
      const saved = readTabs(user, org)
      const next = openTab(saved.tabs, tab)
      if (next.refused) return "tab_limit"
      writeTabs(user, org, next.tabs, next.active)
      router.push("/workspace")
      return "opened"
    },
    [user, org, router]
  )
}

export type ForkOutcome =
  | { key: string; kind: "opened"; archived: number; skipped: number }
  | { key: string; kind: "tab_limit" }
  | { key: string; kind: "error"; error: unknown }

/** A started fork, as the server described it. */
export type ForkMade = { target: ForkTarget; archived: number; skipped: number }

/**
 * One fork at a time per conversation view.
 *
 * The request id is per selected reply and survives every failure — a lost
 * response, a refusal, a full tab strip — so pressing Fork again resolves to
 * the fork already made rather than making a second. It is spent only once
 * the fork is open. A change of account or org forgets all of it, and an
 * answer for the old scope is dropped.
 */
export function useForkAction(scopeKey: unknown, openFork: OpenFork) {
  const ids = useRef(new Map<string, string>())
  const busy = useRef<string | null>(null)
  const scope = useRef(scopeKey)
  const [forking, setForking] = useState<string | null>(null)
  const [outcome, setOutcome] = useState<ForkOutcome | null>(null)
  useEffect(() => {
    if (scope.current === scopeKey) return
    scope.current = scopeKey
    ids.current.clear()
    busy.current = null
    setForking(null)
    setOutcome(null)
  }, [scopeKey])
  const fork = useCallback(
    async (key: string, make: (requestId: string) => Promise<ForkMade>) => {
      // A second press while one is on its way does nothing.
      if (busy.current !== null) return
      const started = scope.current
      busy.current = key
      setForking(key)
      setOutcome(null)
      let requestId = ids.current.get(key)
      if (!requestId) {
        requestId = crypto.randomUUID()
        ids.current.set(key, requestId)
      }
      try {
        const made = await make(requestId)
        if (scope.current !== started) return
        const opened = await openFork(made.target)
        if (scope.current !== started) return
        if (opened === "tab_limit") {
          setOutcome({ key, kind: "tab_limit" })
          return
        }
        ids.current.delete(key)
        setOutcome({
          key,
          kind: "opened",
          archived: made.archived,
          skipped: made.skipped,
        })
      } catch (error) {
        if (scope.current !== started) return
        setOutcome({ key, kind: "error", error })
      } finally {
        if (scope.current === started) {
          busy.current = null
          setForking(null)
        }
      }
    },
    [openFork]
  )
  return { fork, forking, outcome }
}

/** The workspace's tab opener, or the stored-strip fallback outside it. */
export function useOpenFork(): OpenFork {
  const provided = useContext(ForkTabsContext)
  const stored = useStoredForkTabs()
  return provided ?? stored
}

/**
 * Copy and the actions menu, after a completed reply, plus what the last fork
 * of THIS reply came to. Hidden until the reply is hovered or focused, like
 * Copy; always there on touch.
 */
export function ReplyActions({
  markdown,
  forkKey,
  onFork,
  onForkInto,
  forking,
  outcome,
  className,
}: {
  markdown: () => string
  forkKey: string
  onFork: () => void
  /**
   * After `fork_no_location`: fork into a folder the reader picks. The SAME
   * request id is used (the hook keeps it until the fork opens), so a retry
   * can never make two forks.
   */
  onForkInto?: (nodeId: number) => void
  /** The reply a fork is on its way for, if any. */
  forking: string | null
  outcome: ForkOutcome | null
  className?: string
}) {
  const t = useTranslations("NativeContent")
  const mine = outcome?.key === forkKey ? outcome : null
  const busy = forking === forkKey
  return (
    <div className={cn("space-y-1", className)}>
      <div className="flex items-center gap-0.5">
        {markdown().trim() && <CopyMessageButton markdown={markdown} />}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              aria-label={t("replyActions")}
              title={t("replyActions")}
              className={cn(
                "size-7 text-muted-foreground",
                !busy &&
                  "opacity-0 group-hover/message:opacity-100 group-focus-within/message:opacity-100 data-[state=open]:opacity-100 [@media(hover:none)]:opacity-100"
              )}
            >
              <MoreHorizontal aria-hidden className="size-4" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start">
            <DropdownMenuItem
              disabled={forking !== null}
              title={t("forkExplain")}
              onSelect={() => onFork()}
            >
              <GitFork aria-hidden className="size-3.5 opacity-60" />
              <span>{t("forkToNewTab")}</span>
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      <ForkStatus busy={busy} outcome={mine} onForkInto={onForkInto} />
    </div>
  )
}

function ForkStatus({
  busy,
  outcome,
  onForkInto,
}: {
  busy: boolean
  outcome: ForkOutcome | null
  onForkInto?: (nodeId: number) => void
}) {
  const t = useTranslations("NativeContent")
  if (busy)
    return (
      <p role="status" className="text-xs text-muted-foreground">
        {t("forking")}
      </p>
    )
  if (!outcome) return null
  if (outcome.kind === "error") {
    const refusal = forkRefusal(outcome.error)
    if (refusal === "fork_no_location" && onForkInto)
      return <ForkDestination onPick={onForkInto} />
    return refusal ? (
      <p role="alert" className="text-xs text-destructive">
        {t(`forkError_${refusal}`)}
      </p>
    ) : (
      <NativeError error={outcome.error} />
    )
  }
  if (outcome.kind === "tab_limit")
    return (
      <p role="status" className="text-xs text-muted-foreground">
        {t("forkTabLimit")}
      </p>
    )
  const notes = [
    t("forkOpened"),
    outcome.archived > 0
      ? t("forkArchivedOmitted", { number: outcome.archived })
      : null,
    outcome.skipped > 0
      ? t("forkTurnsSkipped", { number: outcome.skipped })
      : null,
  ].filter(Boolean)
  return (
    <p role="status" className="text-xs text-muted-foreground">
      {notes.join(" ")}
    </p>
  )
}

/**
 * Where to put a fork when its source's folder is hidden from the reader:
 * the folders the reader can see (every page, including empty ones the
 * server returns while hidden rows shorten pages), and "Fork here". A folder
 * they can only view refuses, and they choose again.
 */
function ForkDestination({ onPick }: { onPick: (nodeId: number) => void }) {
  const t = useTranslations("NativeContent")
  const sh = useTranslations("Sharing")
  const auth = useNativeAuth()
  const orgId = auth.session?.organization?.id
  const [folders, setFolders] = useState<NativeNode[] | null>(null)
  const [chosen, setChosen] = useState<number | null>(null)
  const [error, setError] = useState<unknown>(null)
  useEffect(() => {
    if (orgId === undefined) return
    const controller = new AbortController()
    void (async () => {
      try {
        let after: number | null = null
        const all: NativeNode[] = []
        do {
          const page = await loadNodes(orgId, after, controller.signal)
          if (controller.signal.aborted) return
          all.push(...page.nodes)
          after = page.nextAfter
        } while (after !== null)
        setFolders(all)
        setChosen(all[0]?.id ?? null)
      } catch (cause) {
        if (!controller.signal.aborted) setError(cause)
      }
    })()
    return () => controller.abort()
  }, [orgId])
  return (
    <div
      role="group"
      aria-label={sh("forkChooseFolder")}
      className="space-y-1.5"
    >
      <p role="alert" className="text-xs text-muted-foreground">
        {t("forkError_fork_no_location")}
      </p>
      {folders && folders.length > 0 && (
        <form
          className="flex flex-wrap items-center gap-2"
          onSubmit={(event) => {
            event.preventDefault()
            if (chosen !== null) onPick(chosen)
          }}
        >
          <select
            aria-label={sh("forkChooseFolder")}
            value={chosen ?? ""}
            onChange={(event) => setChosen(Number(event.target.value))}
            className="h-8 max-w-64 rounded-md border bg-background px-2 text-xs"
          >
            {folders.map((folder) => (
              <option key={folder.id} value={folder.id}>
                {folder.name}
              </option>
            ))}
          </select>
          <Button type="submit" size="sm" variant="secondary">
            {sh("forkHere")}
          </Button>
        </form>
      )}
      <NativeError error={error} />
    </div>
  )
}
