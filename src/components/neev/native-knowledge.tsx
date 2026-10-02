"use client"

import { useCallback, useEffect, useId, useRef, useState } from "react"
import { useRouter } from "next/navigation"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { NativeClientError } from "@/lib/neev-client"
import {
  batchesFor,
  controlKnowledgeJob,
  knowledgeComplete,
  KNOWLEDGE_PROVIDERS,
  loadKnowledgeSettings,
  loadKnowledgeStatus,
  resetKnowledgeUncovered,
  saveKnowledgeSettings,
  startKnowledgeJob,
  type KnowledgeJob,
  type KnowledgeMode,
  type KnowledgeSettings,
  type KnowledgeStatus,
} from "@/lib/neev-knowledge"
import {
  loadAgentCapabilities,
  loadAgentConnections,
  NEEV_DEFAULT_MODEL,
  type NeevAgentProviderCatalog,
} from "@/lib/neev-agent-sessions-client"
import {
  openTab,
  openTabInPlace,
  readTabs,
  writeTabs,
  type WorkspaceTab,
} from "@/lib/neev-workspace-tabs"
import { useNativeAuth } from "./native-auth-provider"
import { NativeError } from "./native-boundary"

/** How often a running job's status is re-read. */
const POLL_MS = 5_000
/**
 * How often status is re-read while capture is still catching up.
 *
 * Capture runs on a 2-minute server tick with a 2-tick debounce, so nothing
 * changes faster than this; it only has to stop "Captured 0" from standing
 * still after capture is switched on.
 */
const CAPTURE_POLL_MS = 30_000

/** Pause reasons with their own wording; anything newer reads "Paused". */
const PAUSE_REASONS = [
  "manual",
  "interrupted",
  "credential_missing",
  "source_revoked",
  "invalid_output",
  "run_failed",
  "busy",
  "not_admin",
  "timeout",
  "page_moved",
  "stopped",
] as const

/**
 * Open something in the workspace from Settings: the source a page cites, or a
 * compile chat. Uses the workspace's own saved tab strip, so it lands exactly
 * as if the reader had opened it there — and is authorized there, like any
 * restored tab: a reference confers nothing.
 */
export function useOpenInWorkspace() {
  const auth = useNativeAuth()
  const router = useRouter()
  const user = auth.session?.user.id
  const org = auth.session?.organization?.id
  return useCallback(
    (tab: WorkspaceTab) => {
      if (user === undefined || org === undefined) return
      const saved = readTabs(user, org)
      let next = openTab(saved.tabs, tab)
      // At the tab limit, replace the one in front rather than refusing.
      if (next.refused) next = openTabInPlace(saved.tabs, tab, saved.active)
      writeTabs(user, org, next.tabs, next.active)
      router.push("/workspace")
    },
    [user, org, router]
  )
}

function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`
  const units = ["KB", "MB", "GB"]
  let value = bytes / 1024
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`
}

/**
 * Settings → Knowledge (admins only): capture, status and compile.
 *
 * Saved only. The compiled pages stay here for admins; nothing in this
 * section publishes them to members or hands them to agents, and the copy
 * never says otherwise. What is incomplete, withheld or stale is stated rather
 * than smoothed over.
 */
/** The newer of two answers about the same job; a different job always wins. */
function newerJob(
  held: KnowledgeJob | null,
  incoming: KnowledgeJob | null
): KnowledgeJob | null {
  if (held && incoming && held.id === incoming.id)
    return incoming.updatedAt < held.updatedAt ? held : incoming
  return incoming
}

export function NativeKnowledge({
  onPagesChanged,
  refreshSignal,
}: {
  /** The job may have written or withheld pages: re-read them. */
  onPagesChanged?: () => void
  /** Changes when the reader asks for a refresh elsewhere in the section. */
  refreshSignal?: number
} = {}) {
  const t = useTranslations("NativeKnowledge")
  const auth = useNativeAuth()
  const { handleError } = auth
  const [status, setStatus] = useState<KnowledgeStatus | null>(null)
  /**
   * Orders every write of `status`. A poll records the ticket it started with
   * and is dropped if anything — another poll, a pause, a start — has taken a
   * newer one since: an answer that left before a Pause cannot undo it.
   */
  const ticket = useRef(0)
  const pagesChanged = useRef(onPagesChanged)
  useEffect(() => {
    pagesChanged.current = onPagesChanged
  }, [onPagesChanged])
  const [settings, setSettings] = useState<KnowledgeSettings | null>(null)
  const [error, setError] = useState<unknown>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  // This admin's own connected agents, and what each can run.
  const [agents, setAgents] = useState<string[] | null>(null)
  /** The connections could not be read: not the same as having none. */
  const [agentsFailed, setAgentsFailed] = useState(false)
  const [catalogs, setCatalogs] = useState<readonly NeevAgentProviderCatalog[]>(
    []
  )
  const [provider, setProvider] = useState("")
  const [model, setModel] = useState("")
  const [mode, setMode] = useState<KnowledgeMode>("incremental")
  const live = useRef(true)
  useEffect(() => {
    live.current = true
    return () => {
      live.current = false
    }
  }, [])
  const switchLabel = useId()

  const refresh = useCallback(
    async (pagesToo = true) => {
      const mine = ++ticket.current
      try {
        const next = await loadKnowledgeStatus()
        if (!live.current || mine !== ticket.current) return
        setStatus((old) => ({
          ...next,
          job: newerJob(old?.job ?? null, next.job),
        }))
        if (pagesToo) pagesChanged.current?.()
      } catch (cause) {
        if (live.current) {
          setError(cause)
          handleError(cause)
        }
      }
    },
    [handleError]
  )

  useEffect(() => {
    const controller = new AbortController()
    Promise.all([
      loadKnowledgeStatus(controller.signal),
      loadKnowledgeSettings(controller.signal),
    ]).then(
      ([nextStatus, nextSettings]) => {
        if (controller.signal.aborted) return
        setStatus(nextStatus)
        setSettings(nextSettings)
      },
      (cause: unknown) => {
        if (controller.signal.aborted) return
        setError(cause)
        handleError(cause)
      }
    )
    // Separately: an agent list that cannot be read must not hide the rest.
    loadAgentConnections(controller.signal)
      .then((wiring) => {
        if (controller.signal.aborted) return
        const connected = wiring.connections
          .filter(
            (item) =>
              item.state === "connected" &&
              KNOWLEDGE_PROVIDERS.includes(item.provider)
          )
          .map((item) => item.provider as string)
        setAgents(connected)
        setProvider((old) => old || connected[0] || "")
      })
      .catch(() => {
        if (controller.signal.aborted) return
        setAgents([])
        setAgentsFailed(true)
      })
    loadAgentCapabilities(controller.signal)
      .then((catalog) => {
        if (!controller.signal.aborted) setCatalogs(catalog.providers)
      })
      .catch(() => undefined)
    return () => controller.abort()
  }, [handleError])

  const job = status?.job ?? null
  const active = job?.state === "running" || job?.state === "paused"
  // Poll while a job runs; stop the moment it does not, or this unmounts.
  useEffect(() => {
    if (job?.state !== "running") return
    const timer = setInterval(() => void refresh(), POLL_MS)
    return () => clearInterval(timer)
  }, [job?.state, refresh])

  // Follow capture while it catches up — slowly, only while something is
  // still uncaptured, only when no job poll is already running, and not while
  // the tab is hidden. Pages are not touched: capture does not write them.
  // Sources that cannot be captured (too large, failed, no engine) are
  // accounted for: they do not keep this asking forever.
  const catchingUp =
    !!status &&
    status.captureEnabled &&
    status.captureBlocked === null &&
    status.captured +
      status.sourceProblems.tooLarge +
      status.sourceProblems.failed +
      status.sourceProblems.engineUnavailable <
      status.eligible &&
    job?.state !== "running"
  useEffect(() => {
    if (!catchingUp) return
    const timer = setInterval(() => {
      if (typeof document === "undefined" || !document.hidden)
        void refresh(false)
    }, CAPTURE_POLL_MS)
    return () => clearInterval(timer)
  }, [catchingUp, refresh])

  // An explicit Refresh elsewhere in the section (the pages' button).
  const lastSignal = useRef(refreshSignal)
  useEffect(() => {
    if (refreshSignal === lastSignal.current) return
    lastSignal.current = refreshSignal
    void refresh(false)
  }, [refreshSignal, refresh])

  const models = (() => {
    const catalog = catalogs.find((entry) => entry.provider === provider)
    const ids = catalog?.models.map((entry) => entry.id) ?? []
    return ids.length ? ids : [NEEV_DEFAULT_MODEL]
  })()
  const preferred =
    catalogs
      .find((entry) => entry.provider === provider)
      ?.models.find((entry) => entry.isDefault)?.id ?? models[0]
  const chosenModel = models.includes(model) ? model : preferred

  async function act(work: () => Promise<void>) {
    if (busy) return
    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      await work()
    } catch (cause) {
      if (live.current) {
        setError(cause)
        handleError(cause)
      }
    } finally {
      if (live.current) setBusy(false)
    }
  }

  function toggleCapture() {
    if (!settings || !auth.session) return
    const csrf = auth.session.csrf
    void act(async () => {
      try {
        const saved = await saveKnowledgeSettings(
          !settings.captureEnabled,
          settings.revision,
          csrf
        )
        if (!live.current) return
        setSettings(saved)
        setStatus((old) =>
          old ? { ...old, captureEnabled: saved.captureEnabled } : old
        )
        // What the server now reports, rather than only the flag we flipped.
        await refresh(false)
      } catch (cause) {
        // Someone else saved first: show what is true now, change nothing.
        if (cause instanceof NativeClientError && cause.kind === "conflict") {
          const current = await loadKnowledgeSettings()
          if (!live.current) return
          setSettings(current)
          setNotice(t("settingConflict"))
          return
        }
        throw cause
      }
    })
  }

  function start() {
    if (!auth.session || !provider) return
    const csrf = auth.session.csrf
    void act(async () => {
      ++ticket.current // any poll already out is now older than this
      try {
        const started = await startKnowledgeJob(
          mode,
          provider,
          chosenModel,
          csrf
        )
        ++ticket.current
        if (live.current)
          setStatus((old) => (old ? { ...old, job: started } : old))
      } catch (cause) {
        if (cause instanceof NativeClientError && cause.kind === "conflict") {
          setNotice(t("startConflict"))
          await refresh()
          return
        }
        throw cause
      }
    })
  }

  function control(target: KnowledgeJob, action: "pause" | "resume" | "stop") {
    if (!auth.session) return
    const csrf = auth.session.csrf
    void act(async () => {
      ++ticket.current // any poll already out is now older than this
      const next = await controlKnowledgeJob(target.id, action, csrf)
      ++ticket.current
      if (!live.current) return
      setStatus((old) => (old ? { ...old, job: newerJob(old.job, next) } : old))
      pagesChanged.current?.()
    })
  }

  function retryUncovered() {
    if (!auth.session) return
    const csrf = auth.session.csrf
    void act(async () => {
      await resetKnowledgeUncovered(csrf)
      await refresh()
    })
  }

  const enabled = settings?.captureEnabled ?? false
  const card = "space-y-3 rounded-xl border p-4"
  const muted = "text-sm text-muted-foreground"
  return (
    <div className="space-y-4">
      <p className={muted}>{t("savedOnly")}</p>
      <NativeError error={error} />
      {notice && (
        <p role="status" className="text-sm font-medium">
          {notice}
        </p>
      )}

      {/* 1. Capture: what is read, and what never is. */}
      <section className={card} aria-labelledby={switchLabel}>
        <div className="flex items-center justify-between gap-3">
          <h3 id={switchLabel} className="text-sm font-medium">
            {t("captureToggle")}
          </h3>
          <button
            type="button"
            role="switch"
            aria-checked={enabled}
            aria-labelledby={switchLabel}
            disabled={!settings || busy}
            onClick={toggleCapture}
            className={cn(
              "relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50",
              enabled ? "bg-primary" : "bg-muted-foreground/30"
            )}
          >
            <span
              aria-hidden
              className={cn(
                "inline-block size-4 rounded-full bg-background shadow transition-transform",
                enabled
                  ? "translate-x-4 rtl:-translate-x-4"
                  : "translate-x-0.5 rtl:-translate-x-0.5"
              )}
            />
          </button>
        </div>
        <p className={muted}>{t("captureScope")}</p>
        <p className={muted}>{t("filesByName")}</p>
        {status?.captureBlocked === "quota_exceeded" && (
          <p role="alert" className="text-sm font-medium text-destructive">
            {t("quotaExceeded")}
          </p>
        )}
        {!!status?.sourceProblems.engineUnavailable && (
          <p className={muted}>
            {t("engineUnavailable", {
              number: status.sourceProblems.engineUnavailable,
            })}
          </p>
        )}
      </section>

      {/* 2. Status: complete only when it is. */}
      {status && (
        <section className={card}>
          <h3 className="text-sm font-medium">{t("statusTitle")}</h3>
          <dl className="grid grid-cols-2 gap-3 sm:grid-cols-4">
            {(
              [
                ["statEligible", status.eligible],
                ["statCaptured", status.captured],
                ["statRepresented", status.represented],
                ["statPages", status.pages],
              ] as const
            ).map(([label, value]) => (
              <div key={label} className="rounded-lg bg-muted/40 px-3 py-2">
                <dt className="text-xs text-muted-foreground">{t(label)}</dt>
                <dd className="text-lg font-semibold tabular-nums">{value}</dd>
              </div>
            ))}
          </dl>
          {/* Complete only when nothing at all is outstanding (review I1). */}
          {status.eligible > 0 &&
            (knowledgeComplete(status) ? (
              <p className={muted}>{t("complete")}</p>
            ) : (
              <p className="text-sm font-medium">
                {status.represented < status.eligible
                  ? t("incomplete", {
                      represented: status.represented,
                      eligible: status.eligible,
                    })
                  : t("incompleteOther")}
              </p>
            ))}
          <ul className="space-y-1 text-sm">
            {status.coverage.uncovered > 0 && (
              <li>
                {t("uncoveredGaps", { number: status.coverage.uncovered })}
              </li>
            )}
            {status.coverage.pending > 0 && (
              <li>{t("pendingUnits", { number: status.coverage.pending })}</li>
            )}
            {status.withheldPages > 0 && (
              <li>{t("withheldCount", { number: status.withheldPages })}</li>
            )}
            {status.stale > 0 && (
              <li>{t("staleCount", { number: status.stale })}</li>
            )}
            {status.sourceProblems.tooLarge > 0 && (
              <li>
                {t("tooLarge", { number: status.sourceProblems.tooLarge })}
              </li>
            )}
            {status.sourceProblems.failed > 0 && (
              <li>
                {t("captureFailed", { number: status.sourceProblems.failed })}
              </li>
            )}
          </ul>
          <p className="text-xs text-muted-foreground">
            {t("storageUsed", { size: formatBytes(status.bytesUsed) })}
          </p>
          {/* Gaps can be retried whenever there are some and nothing runs. */}
          {status.coverage.uncovered > 0 && !active && (
            <Button
              size="sm"
              variant="outline"
              disabled={busy}
              onClick={retryUncovered}
            >
              {t("retryUncovered")}
            </Button>
          )}
        </section>
      )}

      {/* 3. Compile: on this admin's own connected agent only. */}
      <section className={card}>
        <h3 className="text-sm font-medium">{t("compileTitle")}</h3>
        <fieldset className="space-y-1" disabled={active || busy}>
          <legend className="sr-only">{t("modeLabel")}</legend>
          {(
            [
              ["incremental", "modeIncremental", "incrementalHelp"],
              ["rebuild", "modeRebuild", "rebuildHelp"],
            ] as const
          ).map(([value, label, help]) => (
            <label key={value} className="flex items-start gap-2 text-sm">
              <input
                type="radio"
                name="knowledge-mode"
                className="mt-1"
                checked={mode === value}
                onChange={() => setMode(value)}
              />
              <span>
                <span className="font-medium">{t(label)}</span>
                <span className="block text-muted-foreground">{t(help)}</span>
              </span>
            </label>
          ))}
        </fieldset>
        {agentsFailed ? (
          <p role="alert" className="text-sm text-destructive">
            {t("connectionsFailed")}
          </p>
        ) : agents !== null && agents.length === 0 ? (
          <p className={muted}>{t("noAgent")}</p>
        ) : (
          <div className="flex flex-wrap gap-3">
            <label className="space-y-1 text-sm">
              <span className="block">{t("agent")}</span>
              <select
                value={provider}
                disabled={active || busy || !agents}
                onChange={(event) => {
                  setProvider(event.target.value)
                  setModel("")
                }}
                className="h-9 rounded-lg border bg-background px-2"
              >
                {(agents ?? []).map((id) => (
                  <option key={id} value={id}>
                    {catalogs.find((entry) => entry.provider === id)?.label ??
                      id}
                  </option>
                ))}
              </select>
            </label>
            <label className="space-y-1 text-sm">
              <span className="block">{t("model")}</span>
              <select
                value={chosenModel}
                disabled={active || busy || !provider}
                onChange={(event) => setModel(event.target.value)}
                className="h-9 rounded-lg border bg-background px-2"
              >
                {models.map((id) => (
                  <option key={id} value={id}>
                    {catalogs
                      .find((entry) => entry.provider === provider)
                      ?.models.find((entry) => entry.id === id)?.label ?? id}
                  </option>
                ))}
              </select>
            </label>
          </div>
        )}
        <p className={muted}>{t("providerCopies")}</p>
        {status && !active && (
          <p className="text-sm">
            {status.coverage.pending > 0
              ? t("estimate", { number: batchesFor(status.coverage.pending) })
              : t("nothingPending")}
          </p>
        )}
        <Button
          size="sm"
          onClick={start}
          disabled={busy || active || !provider || agents === null}
        >
          {t("start")}
        </Button>
      </section>

      {/* 4. The job, as it stands. */}
      {job && (
        <section className={card} aria-live="polite">
          <div className="flex flex-wrap items-baseline justify-between gap-2">
            <h3 className="text-sm font-medium">{t("jobTitle")}</h3>
            <span className="text-sm">{t(`state_${job.state}`)}</span>
          </div>
          <p className="text-xs text-muted-foreground">
            {t(job.mode === "rebuild" ? "modeRebuild" : "modeIncremental")} ·{" "}
            {job.provider} · {job.model}
          </p>
          <p className="text-sm">
            {t("batchesDone", { number: job.batchesDone })}
          </p>
          {active && (
            <p className="text-sm">
              {t("remaining", { number: batchesFor(job.unitsRemaining) })}
            </p>
          )}
          {job.state === "running" && job.waitReason === "slot" && (
            <p className={muted}>{t("waitingForSlot")}</p>
          )}
          {job.state === "paused" && (
            <p className="text-sm font-medium">
              {job.pauseReason &&
              (PAUSE_REASONS as readonly string[]).includes(job.pauseReason)
                ? t(
                    `pause_${job.pauseReason as (typeof PAUSE_REASONS)[number]}`
                  )
                : t("pause_other")}
            </p>
          )}
          {job.state === "completed_with_gaps" && (
            <p className="text-sm font-medium">{t("completedWithGaps")}</p>
          )}
          <div className="flex flex-wrap gap-2">
            {job.state === "running" && (
              <Button
                size="sm"
                variant="outline"
                disabled={busy}
                onClick={() => control(job, "pause")}
              >
                {t("pause")}
              </Button>
            )}
            {job.state === "paused" && (
              <Button
                size="sm"
                variant="outline"
                disabled={busy}
                onClick={() => control(job, "resume")}
              >
                {t("resume")}
              </Button>
            )}
            {active && (
              <Button
                size="sm"
                variant="outline"
                disabled={busy}
                onClick={() => control(job, "stop")}
              >
                {t("stop")}
              </Button>
            )}
            {/* No compile-chat link yet: the job's session is hidden from
                lists (A1), and opening it by id is unconfirmed (review M3).
                See frontend-contract-notes.md. */}
          </div>
        </section>
      )}
    </div>
  )
}
