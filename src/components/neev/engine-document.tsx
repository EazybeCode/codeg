"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { Lock, MoreHorizontal, Users } from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import {
  loadNativeResource,
  type NativeResource,
} from "@/lib/neev-content-client"
import { useNativeAuth } from "./native-auth-provider"
import { NativeDropdownMenuContent as DropdownMenuContent } from "./native-suspended-surface"
import type { ResourceActions } from "./native-document"

/**
 * A document whose engine is La Suite Docs, embedded.
 *
 * This is a deliberately thin component, and the thinness is the design. The
 * upstream editor inside the frame owns the document completely: the text, the
 * title, saving, and real-time collaboration between people editing at once.
 * Neev owns where the document lives and who may reach it.
 *
 * Two consequences worth stating, because both were tempting mistakes:
 *
 * - **Nothing here fetches document content.** No revisions, no autosave, no
 *   flush registration. Upstream persists through its own save path, and a
 *   second writer saving the same document from Neev is exactly the
 *   parallel-writer failure the evaluation warned about. The absence of those
 *   requests is asserted in the tests, not merely intended.
 * - **No title is rendered here.** Upstream draws the title inside the frame
 *   and edits it in place. Drawing a second one above the frame would put two
 *   editable titles on screen, backed by two different stores, free to
 *   disagree.
 *
 * The frame is same-origin and deliberately not sandboxed: it is first-party
 * content that needs its own cookies to authenticate the editor and its
 * collaboration socket, and `sandbox` without `allow-same-origin` would take
 * those away. The authorization boundary is the proxy in front of Docs, not
 * this element.
 */
type Props = {
  resource: NativeResource
  onBack: () => void
  actions?: ResourceActions
  /**
   * The document's metadata after someone renamed it inside the frame.
   *
   * Upstream owns the title, so a rename happens where Neev cannot see it, and
   * the tab and sidebar would keep the old name until a reload. This re-reads
   * the resource from Neev's own API once focus comes back out of the frame,
   * by which time the proxy has synced it.
   */
  onChanged?: (resource: NativeResource) => void
  /**
   * How long to wait for the frame before offering a retry. A frame that never
   * loads otherwise reads as a blank panel with nothing to press, which is the
   * one outcome worth ruling out. Shortened by tests; never set in the app.
   */
  loadTimeoutMs?: number
}

const LOAD_TIMEOUT_MS = 15_000

/**
 * The marker an error response must carry to be recognised as one.
 *
 * `onError` does not fire for an HTTP error: a 403 or a 503 is a response, and
 * the frame loads it like any other page. `onLoad` alone therefore means "some
 * document arrived", not "the editor is there" — which is why this looks
 * further before declaring the frame ready.
 *
 * It looks for one explicit marker and nothing else. Sniffing the body — for a
 * status code in the text, for the absence of an editor element, for a JSON
 * shape — would make an ordinary document that happened to say the wrong thing
 * indistinguishable from a failure. The frame is same-origin, so reading the
 * marker is possible; trusting anything the document *says* is not the same as
 * reading a marker the proxy put there.
 */
const ERROR_MARKER = 'meta[name="neev-document-engine-error"]'

/**
 * Asks the embedded editor to re-read this document's metadata.
 *
 * The counterpart of `neev-doc-metadata-updated`, which the editor posts out
 * when someone renames from inside it. This is the same nudge travelling the
 * other way, for a rename that happened in Neev's own chrome.
 */
export const FRAME_REFRESH_EVENT = "neev-doc-refresh-metadata"

/**
 * The editor saying it is listening.
 *
 * `load` fires before the editor has hydrated, so a refresh sent then arrives
 * at a window with no listener on it. A fixed delay was the first answer and
 * it is a guess: a slow machine hydrates after whatever number is chosen, and
 * the race stays open. So the frame says when it is ready instead, and
 * anything held is flushed on hearing it.
 */
export const FRAME_READY_EVENT = "neev-doc-frame-ready"

/**
 * Whether the frame is showing a failure rather than the editor.
 *
 * Detects only failures that carry the marker above. The proxy does emit it —
 * an unauthenticated editor route answers 401 with
 * `<meta name="neev-document-engine-error" content="unauthorized">` — so the
 * common cases are covered. Anything served without it still renders as itself
 * with no retry, and the timeout only catches a frame that never finishes
 * loading at all.
 *
 * Neither branch has been exercised against a genuinely unavailable upstream in
 * a browser; the acceptance harness covers the marker, the tests cover the
 * component's response to it.
 */
function failed(element: HTMLIFrameElement | null): boolean {
  if (!element) return false
  try {
    return !!element.contentDocument?.querySelector(ERROR_MARKER)
  } catch {
    // A cross-origin frame cannot be read. That should not happen here, and
    // guessing on the basis of it would be worse than carrying on.
    return false
  }
}

export function EngineDocument(props: Props) {
  const auth = useNativeAuth()
  // Same guard the native document uses: never render a resource belonging to
  // an organization other than the signed-in one, including across a switch.
  if (
    !auth.session?.organization ||
    props.resource.orgId !== auth.session.organization.id
  )
    return null
  return <Frame key={`${auth.scopeKey}:${props.resource.id}`} {...props} />
}

function Frame({
  resource,
  onBack,
  actions,
  onChanged,
  loadTimeoutMs = LOAD_TIMEOUT_MS,
}: Props) {
  const auth = useNativeAuth()
  const t = useTranslations("NativeContent")
  const n = useTranslations("Native")
  const [state, setState] = useState<"loading" | "ready" | "stalled">("loading")
  // Bumping this remounts the frame, which is what "try again" has to do: an
  // iframe will not re-request a src it already holds.
  const [attempt, setAttempt] = useState(0)
  const settled = useRef(false)
  const frame = useRef<HTMLIFrameElement>(null)
  // Refs, not effect locals: the blur and the focus that follows it are two
  // separate events, and anything re-subscribing the listeners between them
  // would otherwise forget that focus had ever been inside the frame — and the
  // refresh that the rename depends on would silently never happen.
  const wasInside = useRef(false)
  const busy = useRef(false)
  /**
   * A confirmed change that arrived while a read was already in flight.
   *
   * Dropping it would reintroduce the exact race it exists to close: the read
   * already running was started before the save landed, so it answers with the
   * OLD title, publishes it, and nothing ever asks again. One queued follow-up
   * is enough — it is triggered by a confirmed event rather than by a timer, so
   * it cannot become a poll.
   */
  const pending = useRef(false)
  /** The metadata revision last seen on the prop, to notice when it moves. */
  const lastRevision = useRef(resource.authRevision)
  /** A refresh held because the frame had not said it was listening yet. */
  const refreshPending = useRef(false)
  /** Whether the frame has announced its listener. Reset by a remount. */
  const frameReady = useRef(false)

  /**
   * Re-read the document's metadata when it may have changed.
   *
   * Two triggers, and the second exists because the first is not enough.
   * Focus leaving the frame is the cheap signal, but renaming and immediately
   * clicking something in Neev issues that read BEFORE the save lands — it
   * answers with the old title and the tab keeps it until a reload. So the
   * editor also confirms explicitly, once the server has accepted the change.
   *
   * **No title is ever read from the frame.** The message carries an id and a
   * type and nothing else; the value still comes from Neev's own API. That is
   * the difference between a nudge and a fact, and a frame that could name
   * itself could name itself anything.
   *
   * Both paths are event-driven, so neither can degrade into a poll.
   */
  const orgId = resource.orgId
  const resourceId = resource.id
  const handleError = auth.handleError
  useEffect(() => {
    if (!onChanged) return
    let cancelled = false
    const controller = new AbortController()

    function left() {
      // The parent window loses focus to its own iframe, so this is the moment
      // the editor has it.
      if (document.activeElement === frame.current) wasInside.current = true
    }
    async function reread({ force = false } = {}) {
      if (cancelled) return
      if (!force && !wasInside.current) return
      if (busy.current) {
        // A read is already running and may have been issued before the change
        // landed. Remember that it is now stale rather than dropping the news.
        if (force) pending.current = true
        return
      }
      wasInside.current = false
      busy.current = true
      try {
        const latest = await loadNativeResource(
          orgId,
          resourceId,
          controller.signal
        )
        if (!cancelled) onChanged?.(latest)
      } catch (error) {
        if (!cancelled) handleError(error)
      } finally {
        busy.current = false
        if (pending.current && !cancelled) {
          pending.current = false
          void reread({ force: true })
        }
      }
    }

    /**
     * The editor says "something about this document changed" once the server
     * has confirmed it.
     *
     * Focus alone is not enough. Renaming and then immediately clicking
     * something in Neev makes the focus-driven re-read race the save, and it
     * loses — the tab keeps the old title until a reload. That is the bug this
     * closes.
     *
     * Three things are checked before it counts, and all three matter:
     *
     * - `event.source` must be this frame's own window. Any other frame or
     *   window on the page can post here, and only this one is showing this
     *   document.
     * - `event.origin` must be our own origin. The embed is same-origin by
     *   design, so anything else is not the editor.
     * - the `resourceId` must be the document this component is showing,
     *   which stops a stale message from a previous document landing here.
     *
     * The message carries no title, and none is read from it. It says only
     * "go and look"; the answer comes from Neev's own API, as before. A frame
     * that could name itself could name itself anything.
     */
    function onMessage(event: MessageEvent) {
      if (event.source !== frame.current?.contentWindow) return
      if (event.origin !== window.location.origin) return
      const data = event.data as { type?: unknown; resourceId?: unknown } | null
      if (!data || data.type !== "neev-doc-metadata-updated") return
      if (data.resourceId !== resourceId) return
      void reread({ force: true })
    }

    // Named, because removeEventListener compares by reference — a fresh
    // arrow function here would register a listener that is never removed.
    const onFocus = () => void reread()

    window.addEventListener("blur", left)
    window.addEventListener("focus", onFocus)
    window.addEventListener("message", onMessage)
    return () => {
      cancelled = true
      controller.abort()
      window.removeEventListener("blur", left)
      window.removeEventListener("focus", onFocus)
      window.removeEventListener("message", onMessage)
    }
  }, [onChanged, orgId, resourceId, handleError])

  // Only the timer lives here. Going back to "loading" is the retry's doing,
  // where it is an event rather than a render cascade.
  useEffect(() => {
    const timer = setTimeout(() => {
      if (!settled.current) setState("stalled")
    }, loadTimeoutMs)
    return () => clearTimeout(timer)
  }, [attempt, loadTimeoutMs])

  function retry() {
    settled.current = false
    frameReady.current = false
    setState("loading")
    setAttempt((value) => value + 1)
  }

  /**
   * Tell the frame this document's metadata moved under it.
   *
   * The editor reads the title when it boots, so a rename done in Neev's own
   * chrome leaves it showing the old name. Invalidating one query fixes that
   * without reloading — a reload during an unsaved edit can cost a paragraph,
   * which is too much to spend on a name. The message carries an id and no
   * title: the frame re-reads its own API rather than believing a string.
   */
  const refreshFrame = useCallback(() => {
    frame.current?.contentWindow?.postMessage(
      { type: FRAME_REFRESH_EVENT, resourceId: resourceId },
      window.location.origin
    )
  }, [resourceId])

  /**
   * Any metadata write is worth a refresh, including the frame's own.
   *
   * Keyed on `authRevision` rather than the title, because the title is not
   * the only thing in here the editor renders: archiving makes the body
   * read-only upstream, and restoring makes it writable again. Every metadata
   * PATCH bumps this, so one dependency covers all of them.
   *
   * An earlier version tried to tell the two apart and suppress the frame's
   * own renames. It could not: the focus-driven re-read above picks up an
   * EXTERNAL rename just as readily, so "the frame told us this" was inferred
   * from evidence that did not support it, and a real rename went unsent. One
   * redundant GET after someone renames in place is the cheaper mistake.
   *
   * A change arriving before the frame says it is listening is held rather
   * than dropped, and flushed by the handshake below.
   */
  useEffect(() => {
    if (resource.authRevision === lastRevision.current) return
    lastRevision.current = resource.authRevision
    if (frameReady.current) refreshFrame()
    else refreshPending.current = true
  }, [resource.authRevision, refreshFrame])

  /**
   * The frame announcing its listener, and the flush that goes with it.
   *
   * Same three checks as every other message crossing this boundary: this
   * frame's own window, our own origin, this document's id.
   */
  useEffect(() => {
    const onReady = (event: MessageEvent) => {
      if (event.source !== frame.current?.contentWindow) return
      if (event.origin !== window.location.origin) return
      const data = event.data as { type?: unknown; resourceId?: unknown } | null
      if (!data || data.type !== FRAME_READY_EVENT) return
      if (data.resourceId !== resourceId) return
      frameReady.current = true
      if (!refreshPending.current) return
      refreshPending.current = false
      refreshFrame()
    }
    window.addEventListener("message", onReady)
    return () => window.removeEventListener("message", onReady)
  }, [resourceId, refreshFrame])

  /**
   * The resource id IS the upstream document id — there is no second id and no
   * mapping table. Same-origin and root-relative, so the one artifact works
   * unchanged on production HTTPS and on a loopback test.
   */
  const src = `/document-engine/docs/${resource.id}/`

  return (
    <section
      className="flex min-h-0 min-w-0 flex-1 flex-col"
      aria-label={resource.title}
    >
      {/*
        A thin band, and nothing in it that the frame already draws. No title:
        upstream renders and edits it inside. What is left is what Neev owns —
        getting out, who can read this, and the actions that move or retire it.
      */}
      <header className="flex shrink-0 items-center gap-2 px-6 py-2 sm:px-10">
        <Button variant="ghost" size="sm" onClick={onBack}>
          {t("back")}
        </Button>
        <span className="ml-auto flex items-center gap-1 text-xs text-muted-foreground">
          <span
            role="img"
            aria-label={t(resource.visibility)}
            title={t(resource.visibility)}
          >
            {resource.visibility === "private" ? (
              <Lock aria-hidden className="size-3.5" />
            ) : (
              <Users aria-hidden className="size-3.5" />
            )}
          </span>
          <span className="hidden sm:inline">
            {t(resource.visibility)}
            {resource.archived ? ` · ${t("archived")}` : ""}
          </span>
        </span>
        {actions?.isCreator && actions.shareable && (
          <Button
            variant="ghost"
            size="sm"
            disabled={actions.disabled}
            aria-label={t(
              actions.visibility === "private" ? "share" : "makePrivate"
            )}
            onClick={
              actions.visibility === "private"
                ? actions.onShare
                : actions.onMakePrivate
            }
          >
            {actions.visibility === "private" ? (
              <Users aria-hidden />
            ) : (
              <Lock aria-hidden />
            )}
          </Button>
        )}
        {/*
          Creator-only, exactly as the native document gates them. Archiving and
          moving are refused server-side for anyone else, so showing them to an
          organization member would be offering two controls that can only fail.
          With neither left there is no menu at all rather than an empty one.
        */}
        {actions?.isCreator && (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="ghost"
                size="sm"
                aria-label={t("resourceActions")}
                disabled={actions.disabled}
              >
                <MoreHorizontal aria-hidden />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              {/*
                Rename is here now, and the old reasoning for leaving it out no
                longer holds. It was omitted because upstream owns the title and
                a second writer for one name is a conflict — but Neev's PATCH is
                not a second writer, it is the same one: the backend projects a
                native title change into Docs and reconciles on open. The title
                inside the frame is still directly editable, and that remains
                the quickest way to do it; this exists because the reader
                looking for "rename" looks in a menu, and finding nothing there
                reads as "you cannot".
              */}
              <DropdownMenuItem
                disabled={actions.disabled}
                onSelect={actions.onRename}
              >
                {t("rename")}
              </DropdownMenuItem>
              <DropdownMenuItem
                disabled={actions.disabled}
                onSelect={actions.onMove}
              >
                {t("move")}
              </DropdownMenuItem>
              {/*
                The delete affordance, named for what it actually does. There is
                no hard delete behind it — the API archives, and archiving is
                reversible — so calling it "Delete" would promise a finality the
                system does not implement, and calling it "Archive" left readers
                hunting for a delete that was never coming. "Move to trash" is
                both true and the thing they were looking for, and it is paired
                with a restore in the same menu and a trash view in the sidebar.
              */}
              <DropdownMenuItem
                disabled={actions.disabled}
                onSelect={actions.onArchiveToggle}
              >
                {t(actions.archived ? "restoreFromTrash" : "moveToTrash")}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        )}
      </header>

      {state === "stalled" && (
        <div className="flex shrink-0 items-center gap-3 px-6 pb-2 sm:px-10">
          <p role="alert" className="text-sm text-destructive">
            {t("unavailable")}
          </p>
          <Button size="sm" variant="outline" onClick={retry}>
            {n("retry")}
          </Button>
        </div>
      )}

      <div className="relative min-h-0 flex-1">
        {/*
          The shape of the thing that is coming, rather than a word about it.
          This window is the editor's own boot — a large bundle and its first
          reads — and for that whole time the frame is blank. One line of
          centred grey text over a white rectangle read as a page that had
          failed; a title bar and a few lines of body read as a page that is
          arriving, which is what is happening. The wording still exists for
          anyone not looking at it.
        */}
        {state === "loading" && (
          <div
            aria-hidden
            className="pointer-events-none absolute inset-0 overflow-hidden bg-background px-6 pt-6 sm:px-10"
          >
            <div className="mx-auto w-full max-w-[760px] animate-pulse space-y-6">
              <div className="h-8 w-2/5 rounded-md bg-muted" />
              <div className="space-y-3">
                {["w-full", "w-11/12", "w-4/5", "w-full", "w-3/5"].map(
                  (width) => (
                    <div
                      key={width}
                      className={`h-3.5 rounded bg-muted/70 ${width}`}
                    />
                  )
                )}
              </div>
            </div>
          </div>
        )}
        {state === "loading" && (
          <p role="status" className="sr-only">
            {n("working")}
          </p>
        )}
        <iframe
          key={attempt}
          ref={frame}
          src={src}
          title={resource.title}
          className="h-full w-full border-0"
          onLoad={() => {
            settled.current = true
            setState(failed(frame.current) ? "stalled" : "ready")
          }}
          onError={() => {
            settled.current = true
            setState("stalled")
          }}
        />
      </div>
    </section>
  )
}
