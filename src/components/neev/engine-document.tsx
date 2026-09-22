"use client"

import { useEffect, useRef, useState } from "react"
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
 * Whether the frame is showing a failure rather than the editor.
 *
 * KNOWN GAP, stated rather than hidden: this only detects failures that carry
 * the marker above. Until the proxy emits it, an HTTP error page renders inside
 * the frame as itself and no retry is offered — the timeout only catches a
 * frame that never finishes loading at all. Neither branch has been exercised
 * against a genuinely unavailable upstream in a browser.
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

  /**
   * Re-read the document's metadata when focus comes back out of the frame.
   *
   * Deliberately **not** driven by `postMessage`. The frame could send any
   * title it liked, and believing it would let the content of a document
   * rewrite the name Neev shows for it. Instead this asks Neev's own API what
   * the title is, and only when there is reason to think it changed: focus was
   * inside the frame, and has now left it. That bounds it to at most one
   * request per visit to the editor rather than a poll.
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
    setState("loading")
    setAttempt((value) => value + 1)
  }

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
                No Rename. Upstream owns the title and writes it through its own
                save path; a Neev rename beside it would be a second writer for
                one name. Same reasoning as the absent title above.
              */}
              <DropdownMenuItem
                disabled={actions.disabled}
                onSelect={actions.onArchiveToggle}
              >
                {t(actions.archived ? "reopen" : "archive")}
              </DropdownMenuItem>
              <DropdownMenuItem
                disabled={actions.disabled}
                onSelect={actions.onMove}
              >
                {t("move")}
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
        {state === "loading" && (
          <p
            role="status"
            className="absolute inset-x-0 top-4 text-center text-sm text-muted-foreground"
          >
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
