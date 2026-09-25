"use client"

/**
 * Being named, and finding out you were.
 *
 * Two things live here, and they are the same thing seen from either end:
 *
 *  - `MentionedText` draws a chip where a STORED span says one belongs. It
 *    never reads the text looking for `@`, so an `@` inside a code fence, a
 *    link, or somebody's actual sentence is the characters they wrote. A span
 *    that does not fit the body it arrived with decorates nothing at all.
 *  - `NativeMentionsIndicator` is the whole of the notification: one quiet mark
 *    with a count, a list under it, and a link that opens the conversation in
 *    the tab the reader is already in. Nothing is mailed, pushed or sent
 *    anywhere, and nothing is shared by being named.
 *
 * The inbox is re-proved by the server on every read — a notice only survives
 * while its conversation is still one this reader may open — so the list is
 * fetched rather than remembered, and everything held here is scoped to the
 * session that fetched it.
 */

import { AtSign } from "lucide-react"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import {
  DropdownMenu,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { NativeDropdownMenuContent as DropdownMenuContent } from "./native-suspended-surface"
import { NativeError } from "./native-boundary"
import { useNativeAuth } from "./native-auth-provider"
import { cn } from "@/lib/utils"
import { splitBodyParts, type MentionSpan } from "@/lib/neev-mentions"
import {
  loadNativeMentions,
  type NeevMentionInbox,
  type NeevMentionNotice,
} from "@/lib/neev-content-client"
import {
  dropMentionRead,
  stageMentionRead,
  subscribeMentionsRead,
} from "@/lib/neev-mention-reads"

/**
 * A run of a stored message, with its mentions drawn as chips.
 *
 * `offset` is where this run begins in the whole body, so a message already cut
 * into pieces by its attachment markers can be decorated a piece at a time
 * without anything re-deriving where anybody was named.
 */
export function MentionedText({
  text,
  spans,
  offset = 0,
}: {
  text: string
  spans: readonly MentionSpan[]
  offset?: number
}) {
  const parts = useMemo(
    () => splitBodyParts(text, spans, offset),
    [text, spans, offset]
  )
  return (
    <>
      {parts.map((part, index) =>
        part.kind === "text" ? (
          <span key={index}>{part.text}</span>
        ) : (
          <span
            key={index}
            data-mention-user={String(part.userId)}
            className="rounded bg-primary/10 px-0.5 font-medium text-primary"
          >
            {part.text}
          </span>
        )
      )}
    </>
  )
}

/** How often the indicator asks again while the workspace is open. */
const POLL_MS = 60_000

/**
 * The one discreet mark that says somebody named you.
 *
 * `onOpen` is the workspace's own way of opening a conversation, handed in
 * rather than guessed at: this component knows no routes and invents no
 * navigation. It answers whether it actually opened the chat, and a `false`
 * is reported to the reader instead of being swallowed — a link that silently
 * does nothing is worse than one that says why it cannot.
 */
export function NativeMentionsIndicator({
  onOpen,
}: {
  onOpen: (chatId: string) => boolean | Promise<boolean>
}) {
  const auth = useNativeAuth()
  // Everything below is scoped to one identity in one organization. Remounting
  // on `scopeKey` is what makes that true by construction: no cache, no count
  // and no open list can outlive the session it was fetched for, so a notice
  // from one organization can never be shown while another is selected.
  if (!auth.session?.organization || !auth.scopeKey) return null
  return (
    <Indicator key={auth.scopeKey} scopeKey={auth.scopeKey} onOpen={onOpen} />
  )
}

function Indicator({
  scopeKey,
  onOpen,
}: {
  scopeKey: string
  onOpen: (chatId: string) => boolean | Promise<boolean>
}) {
  const auth = useNativeAuth()
  const t = useTranslations("NativeContent")
  const [inbox, setInbox] = useState<NeevMentionInbox>({
    items: [],
    unread: 0,
  })
  const [error, setError] = useState<unknown>(null)
  // A notice whose conversation this workspace could not open. Kept apart from
  // a request failure because it is not one: the list is correct, the chat is
  // simply not reachable from here, and saying so is the only honest answer.
  const [unreachable, setUnreachable] = useState(false)
  const [open, setOpen] = useState(false)
  // Read by the poll, which is registered once and must not close over the
  // first render's session.
  const session = useRef(auth.session)
  useEffect(() => {
    session.current = auth.session
  })
  const live = useRef(true)
  useEffect(() => {
    live.current = true
    return () => {
      live.current = false
    }
  }, [])
  const refresh = useCallback(async (signal?: AbortSignal) => {
    if (!session.current) return
    try {
      const next = await loadNativeMentions(signal)
      if (!live.current || signal?.aborted) return
      setInbox(next)
      setError(null)
    } catch (cause) {
      if (!live.current || signal?.aborted) return
      // Said here and nowhere else. This is a secondary read: a refusal is
      // not proof that the session is finished, and escalating it would tear
      // down the workspace scope — taking the open list, and the reason it
      // is showing, with it. The surfaces that own real work still escalate.
      setError(cause)
    }
  }, [])
  useEffect(() => {
    const controller = new AbortController()
    // The count is the whole point of the mark, so it is read once on mount
    // rather than only after the first minute. The state it sets is this
    // component's own and lands after the request settles.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void refresh(controller.signal)
    const timer = window.setInterval(() => {
      // A tab nobody is looking at asks nothing. It reads on mount, and being
      // shown again remounts nothing — so the first poll after it comes back
      // is the one that catches up.
      if (document.visibilityState !== "visible") return
      void refresh(controller.signal)
    }, POLL_MS)
    return () => {
      controller.abort()
      window.clearInterval(timer)
    }
  }, [refresh])
  // A chat panel recorded a receipt: read the count again rather than guess at
  // what it became. The server is what decides whether anything else in that
  // conversation is still unread.
  useEffect(
    () => subscribeMentionsRead(() => void refresh(auth.signal)),
    [refresh, auth.signal]
  )
  /**
   * Follow a notice: ask to go there, and leave the receipt to the arrival.
   *
   * Nothing is marked read here, because clicking a link is not seeing a
   * conversation. The workspace asks any open panel first — an unsent message
   * is a question, and "stay" is one of its answers — and the chat may have
   * been archived or narrowed since the notice was written. So the receipt is
   * staged and claimed by the chat panel once it is actually showing this
   * conversation; a navigation that never happens leaves it unread, which is
   * the whole point of the count.
   */
  async function follow(notice: NeevMentionNotice) {
    if (!scopeKey) return
    if (!notice.read)
      stageMentionRead(scopeKey, notice.chatId, notice.messageId)
    if (!(await onOpen(notice.chatId))) {
      // Refused outright — gone, archived, or not this reader's to open. The
      // staged receipt goes with it rather than waiting for an arrival that
      // cannot happen.
      dropMentionRead(scopeKey, notice.chatId, notice.messageId)
      if (!live.current) return
      setUnreachable(true)
      return
    }
    if (!live.current) return
    setUnreachable(false)
    setOpen(false)
  }
  const unread = inbox.unread
  return (
    <DropdownMenu
      open={open}
      onOpenChange={(next) => {
        setOpen(next)
        // Opened deliberately: ask now rather than show what a minute-old poll
        // happened to leave behind.
        if (next) void refresh(auth.signal)
        else {
          setError(null)
          setUnreachable(false)
        }
      }}
    >
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label={
            unread > 0
              ? `${t("mentionsOpen")} — ${t("mentionsUnread", { number: unread })}`
              : t("mentionsOpen")
          }
          title={t("mentions")}
          className="relative flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring"
        >
          <AtSign aria-hidden className="size-3.5" />
          {unread > 0 && (
            <span
              aria-hidden
              className="absolute -end-0.5 -top-0.5 min-w-3 rounded-full bg-primary px-0.5 text-[9px] font-semibold leading-3 text-primary-foreground"
            >
              {unread > 9 ? "9+" : unread}
            </span>
          )}
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-80 max-w-[92vw]">
        <p className="px-2 py-1.5 text-[11px] font-medium uppercase leading-4 tracking-wide text-muted-foreground">
          {t("mentions")}
        </p>
        {error !== null && (
          <div className="px-2 pb-1">
            <NativeError error={error} />
          </div>
        )}
        {unreachable && (
          <p role="alert" className="px-2 pb-1 text-sm text-destructive">
            {t("mentionsUnreachable")}
          </p>
        )}
        {inbox.items.length === 0 && error === null && (
          <p className="px-2 pb-2 text-sm text-muted-foreground">
            {t("mentionsEmpty")}
          </p>
        )}
        <ul className="max-h-80 overflow-y-auto">
          {inbox.items.map((notice) => (
            <li key={notice.messageId}>
              <DropdownMenuItem
                // The whole notice is the link. Selecting it is the same
                // opening the sidebar performs, in the tab already in front of
                // the reader.
                onSelect={(event) => {
                  event.preventDefault()
                  void follow(notice)
                }}
                className="flex-col items-start gap-0.5"
              >
                <span className="flex w-full min-w-0 items-center gap-1.5">
                  {!notice.read && (
                    <span
                      aria-hidden
                      className="size-1.5 shrink-0 rounded-full bg-primary"
                    />
                  )}
                  <span
                    className={cn(
                      "min-w-0 flex-1 truncate text-sm",
                      !notice.read && "font-medium"
                    )}
                  >
                    {t("mentionedYou", { name: notice.authorName })}
                  </span>
                  <time
                    dateTime={new Date(notice.createdAt * 1000).toISOString()}
                    className="shrink-0 text-[11px] text-muted-foreground"
                  >
                    {new Date(notice.createdAt * 1000).toLocaleDateString()}
                  </time>
                </span>
                <span className="w-full truncate text-xs text-muted-foreground">
                  {notice.chatTitle}
                </span>
                {notice.excerpt !== "" && (
                  <span className="w-full truncate text-xs text-muted-foreground">
                    {notice.excerpt}
                  </span>
                )}
              </DropdownMenuItem>
            </li>
          ))}
        </ul>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
