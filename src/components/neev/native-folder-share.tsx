"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { Users } from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog"
import { NativeClientError, type NativeNode } from "@/lib/neev-client"
import type { NativeResource } from "@/lib/neev-content-client"
import {
  applyFolderShare,
  isAuthority,
  previewFolderShare,
  type FolderSharePreview,
  type FolderShareResult,
} from "@/lib/neev-folder-share"
import {
  loadFolderSharing,
  updateFolderSharing,
  type FolderDefault,
  type FolderSharing,
} from "@/lib/neev-folder-sharing"
import { useNativeAuth } from "./native-auth-provider"
import { NativeError } from "./native-boundary"

export type FolderSharingState = {
  status: "loading" | "ready" | "unavailable"
  sharing: FolderSharing | null
  error: unknown
  /**
   * Whether what is held answers the LATEST read asked for. False while a
   * re-read is out: the previous answer is still shown (no flicker), but
   * nothing may be decided on it — another manager may have changed it.
   */
  fresh: boolean
  /** A newer answer from the server, such as the one a save returns. */
  set: (sharing: FolderSharing) => void
  reload: () => void
  /** Read again and wait: the fresh answer, or null if it could not be read. */
  refresh: () => Promise<FolderSharing | null>
}

type Held = {
  key: string
  /** Which read this answers; a later one always wins over an earlier one. */
  seq: number
  status: "ready" | "unavailable"
  sharing: FolderSharing | null
  error: unknown
}

/**
 * A folder's setting for new items, read for the scope and folder on screen.
 *
 * Keyed by both: an answer for another folder or another organization is never
 * shown here, however late it arrives. Reads are sequenced, so an older answer
 * landing after a newer one is dropped, and `fresh` says whether the latest
 * read has come back. A failure is reported, not guessed past — nothing claims
 * "private" or "shared" without the server saying so.
 *
 * `refreshOnFocus` re-reads whenever the window regains focus: the folder page
 * states what new items get, and another manager may have changed that while
 * the reader was elsewhere.
 */
export function useFolderSharing(
  nodeId: number,
  { refreshOnFocus = false }: { refreshOnFocus?: boolean } = {}
): FolderSharingState {
  const auth = useNativeAuth()
  const key = `${auth.scopeKey}:${nodeId}`
  const seq = useRef(0)
  const live = useRef(new Set<AbortController>())
  const [requested, setRequested] = useState<{ key: string; seq: number }>()
  const [held, setHeld] = useState<Held | null>(null)
  const handleAuthorityError = auth.handleAuthorityError
  const keep = useCallback((next: Held) => {
    setHeld((prev) =>
      prev && prev.key === next.key && prev.seq > next.seq ? prev : next
    )
  }, [])
  const refresh = useCallback(async (): Promise<FolderSharing | null> => {
    const mine = ++seq.current
    setRequested({ key, seq: mine })
    const controller = new AbortController()
    live.current.add(controller)
    try {
      const sharing = await loadFolderSharing(nodeId, controller.signal)
      if (controller.signal.aborted) return null
      keep({ key, seq: mine, status: "ready", sharing, error: null })
      return sharing
    } catch (error) {
      if (controller.signal.aborted) return null
      keep({ key, seq: mine, status: "unavailable", sharing: null, error })
      if (error instanceof NativeClientError && error.status === 401)
        handleAuthorityError(error)
      return null
    } finally {
      live.current.delete(controller)
    }
  }, [key, nodeId, keep, handleAuthorityError])
  // A new folder or scope cancels every read for the old one.
  useEffect(() => {
    const reads = live.current
    void refresh()
    return () => {
      for (const controller of reads) controller.abort()
      reads.clear()
    }
  }, [refresh])
  useEffect(() => {
    if (!refreshOnFocus) return
    const onFocus = () => void refresh()
    window.addEventListener("focus", onFocus)
    return () => window.removeEventListener("focus", onFocus)
  }, [refresh, refreshOnFocus])
  const set = useCallback(
    (sharing: FolderSharing) => {
      const mine = ++seq.current
      setRequested({ key, seq: mine })
      keep({ key, seq: mine, status: "ready", sharing, error: null })
    },
    [key, keep]
  )
  const reload = useCallback(() => void refresh(), [refresh])
  const current = held?.key === key ? held : null
  const fresh =
    current !== null && (requested?.key !== key || requested.seq <= current.seq)
  return {
    status: current?.status ?? "loading",
    sharing: current?.sharing ?? null,
    error: current?.error ?? null,
    fresh,
    set,
    reload,
    refresh,
  }
}

type Phase = "idle" | "counting" | "applying" | "result" | "failed"
type PolicyOutcome = "saved" | "conflict" | "forbidden" | "failed" | null

/**
 * "Share…" for a folder. Two separate things, each said plainly:
 *
 * 1. **New items created here** — the folder's default, applied by the server
 *    when something is created here and stored on that item. The folder's
 *    creator or an admin may change it; everyone else sees it. Opening this
 *    dialog to share a folder preselects "shared", which is what the reader
 *    came to do. Existing items are not touched by it, and anything marked
 *    private stays private.
 * 2. **My existing private items** — optional and off by default: the same
 *    previewed, per-item sharing as before, reporting exactly what happened.
 *
 * They are separate requests and are reported separately. The default is
 * saved first; if that is refused or conflicts, nothing else is sent.
 */
export function FolderShare({
  node,
  folder,
  nodeName,
  onShared,
  onSharingChanged,
}: {
  node: NativeNode
  folder: FolderSharingState
  /** A folder's name, for "set on …" when the default is inherited. */
  nodeName: (id: number) => string | undefined
  /** Rows the server now reports shared, for the lists on screen. */
  onShared: (resources: NativeResource[]) => void
  /** The folder's new setting and revision, for the tree on screen. */
  onSharingChanged: (sharing: FolderSharing) => void
}) {
  const t = useTranslations("NativeContent"),
    sh = useTranslations("Sharing"),
    n = useTranslations("Native")
  const auth = useNativeAuth()
  /** The scope the dialog was opened in; it is open only while that holds. */
  const [openIn, setOpenIn] = useState<string | null>(null)
  const [choice, setChoice] = useState<FolderDefault>("organization")
  const [existing, setExisting] = useState(false)
  const [subfolders, setSubfolders] = useState(false)
  const [phase, setPhase] = useState<Phase>("idle")
  const [preview, setPreview] = useState<FolderSharePreview | null>(null)
  const [progress, setProgress] = useState(0)
  const [total, setTotal] = useState(0)
  const [result, setResult] = useState<FolderShareResult | null>(null)
  const [policyOutcome, setPolicyOutcome] = useState<PolicyOutcome>(null)
  const [savedChoice, setSavedChoice] = useState<FolderDefault | null>(null)
  const [error, setError] = useState<unknown>(null)
  /** The one request in flight, so closing or re-counting cancels it. */
  const running = useRef<AbortController | null>(null)
  const scope = auth.scopeKey
  const userId = auth.session?.user.id
  const org = auth.session?.organization
  // Only an answer to the latest read is shown as something to act on.
  const sharing =
    folder.status === "ready" && folder.fresh ? folder.sharing : null
  const managing = !!sharing?.canManage

  function cancel() {
    running.current?.abort()
    running.current = null
  }
  // Nothing outlives the dialog, the panel, or the scope it was started in.
  useEffect(() => cancel, [])
  // A new scope invalidates whatever was counted or is being sent.
  useEffect(() => cancel, [scope])
  const open = openIn !== null && openIn === scope

  function begin() {
    cancel()
    const controller = new AbortController()
    running.current = controller
    return controller
  }

  async function count(include: boolean) {
    if (userId === undefined || !org) return
    const controller = begin()
    setPhase("counting")
    setPreview(null)
    setError(null)
    try {
      const found = await previewFolderShare({
        orgId: org.id,
        nodeId: node.id,
        userId,
        includeSubfolders: include,
        signal: controller.signal,
      })
      if (controller.signal.aborted) return
      setPreview(found)
      setPhase("idle")
    } catch (cause) {
      if (controller.signal.aborted) return
      setError(cause)
      setPhase("failed")
      auth.handleAuthorityError(cause)
    }
  }

  /** The existing-items step: per item, exact, retryable. */
  async function shareItems(
    items: readonly NativeResource[],
    controller: AbortController,
    before: FolderShareResult | null
  ) {
    if (userId === undefined || !org || !auth.session || !preview) return
    setProgress(0)
    setTotal(items.length)
    const outcome = await applyFolderShare({
      orgId: org.id,
      userId,
      nodeIds: preview.nodeIds,
      items,
      csrf: auth.session.csrf,
      signal: controller.signal,
      onProgress: (done) => {
        if (!controller.signal.aborted) setProgress(done)
      },
    })
    // Whatever the server confirmed is true even if the dialog has gone.
    const confirmed = [...outcome.shared, ...outcome.already]
    if (confirmed.length) onShared(confirmed)
    const authority = outcome.failed.find((item) => isAuthority(item.error))
    if (authority) auth.handleAuthorityError(authority.error)
    if (controller.signal.aborted) return
    // A retry adds to what is already known; only its failures replace.
    setResult({
      shared: [...(before?.shared ?? []), ...outcome.shared],
      already: [...(before?.already ?? []), ...outcome.already],
      changed: [...(before?.changed ?? []), ...outcome.changed],
      failed: outcome.failed,
    })
  }

  const policyChange =
    managing && sharing !== null && choice !== sharing.defaultVisibility
  const items = existing ? (preview?.items ?? []) : []

  async function apply() {
    if (!auth.session) return
    const controller = begin()
    setPhase("applying")
    setPolicyOutcome(null)
    setResult(null)
    setError(null)
    if (policyChange && sharing) {
      try {
        const saved = await updateFolderSharing(
          node.id,
          {
            // Only the revision of the answer the reader is looking at. Apply
            // is not offered until that answer is the fresh one, so a change
            // somebody else made is either shown first or refused as a
            // conflict — never overwritten unseen by borrowing a newer
            // revision from somewhere else.
            expectedRevision: sharing.revision,
            defaultVisibility: choice,
          },
          auth.session.csrf,
          controller.signal
        )
        // True on the server whether or not this dialog is still here.
        folder.set(saved)
        onSharingChanged(saved)
        if (controller.signal.aborted) return
        setSavedChoice(choice)
        setPolicyOutcome("saved")
      } catch (cause) {
        if (controller.signal.aborted) return
        const kind = cause instanceof NativeClientError ? cause.kind : null
        setPolicyOutcome(
          kind === "conflict"
            ? "conflict"
            : kind === "forbidden"
              ? "forbidden"
              : "failed"
        )
        // A conflict means the setting moved: show what it is now.
        if (kind === "conflict") folder.reload()
        // 403 here is "not a manager of this folder", not a lost membership.
        if (kind === "unauthorized") auth.handleAuthorityError(cause)
        setPhase("result")
        return
      }
    }
    if (items.length) await shareItems(items, controller, null)
    if (controller.signal.aborted) return
    setPhase("result")
  }

  async function retryFailed() {
    if (!result) return
    const controller = begin()
    setPhase("applying")
    await shareItems(
      result.failed.map((item) => item.resource),
      controller,
      result
    )
    if (controller.signal.aborted) return
    setPhase("result")
  }

  function show() {
    // Read the setting afresh: it, or the folder's revision, may have moved
    // since the page first asked (a rename elsewhere, another manager).
    folder.reload()
    // The reader came to share this folder: new items shared is the default
    // choice, and saving it is still a separate, confirmed step.
    setChoice("organization")
    setExisting(false)
    setSubfolders(false)
    setPreview(null)
    setResult(null)
    setPolicyOutcome(null)
    setSavedChoice(null)
    setError(null)
    setPhase("idle")
    setOpenIn(scope)
  }
  function close() {
    cancel()
    setOpenIn(null)
  }

  const organization = org?.name ?? ""
  const subfolderCount = preview ? preview.nodeIds.length - 1 : 0
  const busy = phase === "applying"
  const nothingToDo =
    !policyChange && !(existing && phase === "idle" && items.length > 0)
  const source = sharing
    ? sharing.defaultVisibility !== "inherit"
      ? t("folderPolicyHere")
      : sharing.inheritedFrom === null
        ? t("folderPolicyFromNowhere")
        : t("folderPolicyFromParent", {
            name:
              nodeName(sharing.inheritedFrom) ?? `#${sharing.inheritedFrom}`,
          })
    : ""
  return (
    <>
      <Button
        variant="outline"
        size="sm"
        aria-label={t("folderShareLabel", { name: node.name })}
        title={t("folderShareLabel", { name: node.name })}
        onClick={show}
        disabled={userId === undefined || !org}
      >
        <Users aria-hidden className="size-4" />
        {t("folderShare")}
      </Button>
      <Dialog
        open={open}
        onOpenChange={(value) => {
          if (!value && !busy) close()
        }}
      >
        <DialogContent className="max-h-[90dvh] overflow-y-auto">
          <DialogTitle>
            {t("folderShareTitle", { name: node.name })}
          </DialogTitle>
          <DialogDescription>
            {t("folderShareStructure", { name: node.name, organization })}
          </DialogDescription>
          <div className="space-y-4 text-sm">
            {/* 1. New items created here. */}
            {(folder.status === "loading" ||
              (folder.status === "ready" && !folder.fresh)) && (
              <p role="status">{t("folderPolicyLoading")}</p>
            )}
            {folder.status === "unavailable" && (
              <NativeError error={folder.error} />
            )}
            {sharing && (
              <fieldset
                className="space-y-2"
                disabled={!managing || busy || phase === "result"}
              >
                <legend className="font-medium">
                  {t("folderPolicyLegend", { name: node.name })}
                </legend>
                <p>
                  {sharing.effectiveVisibility === "shared"
                    ? sh("folderPolicyNowFolderAccess")
                    : t(
                        sharing.effectiveVisibility === "organization"
                          ? "folderPolicyNowShared"
                          : "folderPolicyNowPrivate",
                        { organization }
                      )}{" "}
                  {source}
                </p>
                {(
                  [
                    ["organization", t("folderPolicyShared", { organization })],
                    // Deliberate, never preselected: new items open to the
                    // people and teams this folder is shared with.
                    ["shared", sh("folderPolicyFolderAccess")],
                    ["private", t("folderPolicyPrivate")],
                    ["inherit", t("folderPolicyInherit")],
                  ] as const
                ).map(([value, label]) => (
                  <label key={value} className="flex items-center gap-2">
                    <input
                      type="radio"
                      name={`folder-default-${node.id}`}
                      value={value}
                      checked={
                        (managing ? choice : sharing.defaultVisibility) ===
                        value
                      }
                      onChange={() => setChoice(value)}
                    />
                    {label}
                  </label>
                ))}
                {!managing && (
                  <p className="text-muted-foreground">
                    {t("folderPolicyManagers")}
                  </p>
                )}
                <p className="text-muted-foreground">
                  {t("folderPolicyExisting")}
                </p>
                <p className="text-muted-foreground">
                  {sh("folderGrantsApplyTo")}
                </p>
              </fieldset>
            )}

            {/* 2. My existing private items: optional, off by default. */}
            <div className="space-y-2 border-t pt-3">
              <label className="flex items-center gap-2 font-medium">
                <input
                  type="checkbox"
                  checked={existing}
                  disabled={busy || phase === "result"}
                  onChange={(event) => {
                    setExisting(event.target.checked)
                    if (event.target.checked) void count(subfolders)
                    else {
                      cancel()
                      setPhase("idle")
                    }
                  }}
                />
                {t("folderShareExistingToggle")}
              </label>
              {existing && (
                <div className="space-y-2 ps-6">
                  <p>{t("folderShareHistory", { organization })}</p>
                  <p className="text-muted-foreground">
                    {t("folderShareExcluded")}
                  </p>
                  <label className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      checked={subfolders}
                      disabled={busy || phase === "result"}
                      onChange={(event) => {
                        setSubfolders(event.target.checked)
                        void count(event.target.checked)
                      }}
                    />
                    {t("folderShareSubfolders")}
                  </label>
                  {phase === "counting" && (
                    <p role="status">{t("folderShareCounting")}</p>
                  )}
                  {preview && phase !== "counting" && (
                    <>
                      <p>
                        {subfolders
                          ? t("folderShareScopeTree", {
                              name: node.name,
                              number: subfolderCount,
                            })
                          : t("folderShareScopeDirect", { name: node.name })}
                      </p>
                      {preview.items.length === 0 ? (
                        <p role="status">{t("folderShareNone")}</p>
                      ) : (
                        <>
                          <p role="status">
                            {t("folderShareCount", {
                              number: preview.items.length,
                            })}
                          </p>
                          <ul
                            aria-label={t("folderShareExistingToggle")}
                            className="max-h-40 space-y-0.5 overflow-y-auto rounded-md border p-2"
                          >
                            {preview.items.map((item) => (
                              <li key={item.id} className="truncate">
                                {item.title}
                              </li>
                            ))}
                          </ul>
                        </>
                      )}
                    </>
                  )}
                  {phase === "failed" && <NativeError error={error} />}
                </div>
              )}
            </div>

            {busy && total > 0 && (
              <p role="status">
                {t("folderShareProgress", { number: progress, total })}
              </p>
            )}
            {phase === "result" && (
              <div role="status" className="space-y-1">
                {policyOutcome === "saved" && savedChoice && (
                  <p>
                    {savedChoice === "organization"
                      ? t("folderPolicySavedShared", {
                          name: node.name,
                          organization,
                        })
                      : savedChoice === "shared"
                        ? sh("folderPolicySavedFolderAccess", {
                            name: node.name,
                          })
                        : savedChoice === "private"
                          ? t("folderPolicySavedPrivate", { name: node.name })
                          : t("folderPolicySavedInherit", { name: node.name })}
                  </p>
                )}
                {policyOutcome === "conflict" && (
                  <p className="text-destructive">
                    {t("folderPolicyConflict")}
                  </p>
                )}
                {(policyOutcome === "failed" ||
                  policyOutcome === "forbidden") && (
                  <p className="text-destructive">{t("folderPolicyFailed")}</p>
                )}
                {result && (
                  <>
                    <p>
                      {t("folderShareShared", { number: result.shared.length })}
                    </p>
                    {result.already.length > 0 && (
                      <p>
                        {t("folderShareAlready", {
                          number: result.already.length,
                        })}
                      </p>
                    )}
                    {result.changed.length > 0 && (
                      <p>
                        {t("folderShareChanged", {
                          number: result.changed.length,
                        })}
                      </p>
                    )}
                    {result.failed.length > 0 && (
                      <p className="text-destructive">
                        {t("folderShareFailed", {
                          number: result.failed.length,
                        })}
                      </p>
                    )}
                  </>
                )}
              </div>
            )}
          </div>
          <div className="flex flex-wrap justify-end gap-2">
            {phase === "result" ? (
              <>
                {result && result.failed.length > 0 && (
                  <Button variant="outline" onClick={() => void retryFailed()}>
                    {t("folderShareRetry")}
                  </Button>
                )}
                <Button onClick={close}>{t("folderShareDone")}</Button>
              </>
            ) : (
              <>
                <Button variant="ghost" disabled={busy} onClick={close}>
                  {n("cancel")}
                </Button>
                <Button
                  disabled={
                    busy || phase === "counting" || nothingToDo || !folder.fresh
                  }
                  onClick={() => void apply()}
                >
                  {busy
                    ? n("working")
                    : !policyChange && items.length > 0
                      ? t("folderShareConfirm", { number: items.length })
                      : t("folderShareApply")}
                </Button>
              </>
            )}
          </div>
        </DialogContent>
      </Dialog>
    </>
  )
}
