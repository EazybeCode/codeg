"use client"

// Sharing with people and teams: one dialog for chats, documents, folders and
// projects, and standalone Agent Chats.
//
// Every read and write goes through `@/lib/neev-sharing` (the frozen v1
// contract). The server is the authority; this file only describes what it
// says, and never claims more access than it reports:
//  - "Private" is a private exception: folder access never applies to it.
//  - "Shared" means the creator, the people and teams named here, and anyone
//    with access to an enclosing folder ("Folder access" when nothing is named
//    here). Removing a name here never removes folder access, and the dialog
//    says so.
//  - "Organization" is unchanged: everyone in the organization can edit.

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { Lock, Users, X } from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog"
import { cn } from "@/lib/utils"
import type { NativeNode } from "@/lib/neev-client"
import type {
  NativeResource,
  NativeVisibility,
} from "@/lib/neev-content-client"
import type { NeevAgentSession } from "@/lib/neev-agent-sessions-client"
import {
  listSharedResources,
  listSharedSessions,
  listTeams,
  loadNodeAccess,
  loadResourceAccess,
  loadSessionAccess,
  removeNodeGrant,
  removeResourceGrant,
  removeSessionGrant,
  searchDirectory,
  setNodeGrant,
  setResourceGrant,
  setNodeRestriction,
  setSessionGrant,
  sharingRefusal,
  type DirectoryMember,
  type Grant,
  type InheritedGrant,
  type Level,
  type Principal,
  type PrincipalInput,
  type ResourceAccess,
  type SharedSession,
  type Team,
} from "@/lib/neev-sharing"
import { useNativeAuth } from "./native-auth-provider"
import { NativeError } from "./native-boundary"

export type ShareTarget =
  | {
      kind: "resource"
      resource: NativeResource
      /**
       * The existing metadata update. Resolves with the refusal (or null);
       * the dialog re-reads access once the item itself has changed.
       */
      onVisibility: (visibility: NativeVisibility) => Promise<unknown> | void
    }
  | { kind: "node"; node: NativeNode }
  | { kind: "session"; session: NeevAgentSession }

/** The contract's sharing refusals, named; anything else as usual. */
export function SharingError({ error }: { error: unknown }) {
  const t = useTranslations("Sharing")
  const refusal = sharingRefusal(error)
  if (refusal)
    return (
      <p role="alert" className="text-sm text-destructive">
        {t(`error_${refusal}`)}
      </p>
    )
  return <NativeError error={error} />
}

/** What the dialog shows, normalised over the three access shapes. */
type Access = {
  /** Which target this answers, so a different item's answer is never shown. */
  targetKey: string
  revision: string
  canManage: boolean
  visibility: NativeVisibility | null
  direct: Grant[]
  inherited: InheritedGrant[]
  boundTo: string | null
  /** Folders only: hidden from everyone not named here (v2). */
  restricted: boolean
  /**
   * Folders only: whether this reader may restrict or re-open it. In a
   * restricted region that is the nearest restricted root's creator alone.
   */
  canRestrict: boolean
  /** The caller's own level on this folder or item. */
  level: "owner" | "edit" | "view" | null
  /**
   * Items only: whether "organization" really means the whole organization.
   * In a restricted folder it means everyone who can see that folder; in a
   * folder hidden from the reader, it is not knowable from here.
   */
  folder: "open" | "restricted" | "hidden"
}

function targetKeyOf(target: ShareTarget): string {
  return target.kind === "resource"
    ? `resource:${target.resource.id}`
    : target.kind === "node"
      ? `node:${target.node.id}`
      : `session:${target.session.id}`
}

function sameTarget(access: Access, target: ShareTarget): boolean {
  return access.targetKey === targetKeyOf(target)
}

function principalKey(principal: Principal | PrincipalInput): string {
  return principal.kind === "user"
    ? `user:${principal.userId}`
    : `team:${principal.teamId}`
}

function inputOf(principal: Principal): PrincipalInput {
  return principal.kind === "user"
    ? { kind: "user", userId: principal.userId }
    : { kind: "team", teamId: principal.teamId }
}

function principalName(principal: Principal): string {
  return principal.kind === "user" ? principal.displayName : principal.name
}

async function readAccess(
  target: ShareTarget,
  signal?: AbortSignal
): Promise<Access> {
  if (target.kind === "resource") {
    const [access, folder] = await Promise.all([
      loadResourceAccess(target.resource.id, signal),
      loadNodeAccess(target.resource.nodeId, signal).then(
        (node) => (node.restricted ? "restricted" : "open") as Access["folder"],
        // A folder this reader cannot see answers 404: say nothing about it.
        () => "hidden" as const
      ),
    ])
    return {
      folder,
      targetKey: `resource:${target.resource.id}`,
      revision: access.authRevision,
      canManage: access.canManage,
      visibility: access.visibility,
      direct: access.direct,
      inherited: access.inherited,
      boundTo: access.boundTo,
      restricted: false,
      canRestrict: false,
      level: access.level,
    }
  }
  if (target.kind === "node") {
    const access = await loadNodeAccess(target.node.id, signal)
    return {
      targetKey: `node:${target.node.id}`,
      revision: access.revision,
      canManage: access.canManage,
      visibility: null,
      direct: access.direct,
      inherited: access.inherited,
      boundTo: null,
      restricted: access.restricted,
      canRestrict: access.canRestrict,
      level: access.level,
      folder: "open",
    }
  }
  const access = await loadSessionAccess(target.session.id, signal)
  return {
    targetKey: `session:${target.session.id}`,
    revision: access.revision,
    canManage: access.canManage,
    visibility: null,
    direct: access.direct,
    inherited: [],
    boundTo: null,
    restricted: false,
    canRestrict: false,
    level: access.level,
    folder: "open",
  }
}

async function writeGrant(
  target: ShareTarget,
  revision: string,
  principal: PrincipalInput,
  level: Level | null,
  csrf: string
): Promise<Access> {
  if (target.kind === "resource") {
    const access =
      level === null
        ? await removeResourceGrant(
            target.resource.id,
            { expectedAuthRevision: revision, principal },
            csrf
          )
        : await setResourceGrant(
            target.resource.id,
            { expectedAuthRevision: revision, principal, level },
            csrf
          )
    return {
      targetKey: `resource:${target.resource.id}`,
      revision: access.authRevision,
      canManage: access.canManage,
      visibility: access.visibility,
      direct: access.direct,
      inherited: access.inherited,
      boundTo: access.boundTo,
      restricted: false,
      canRestrict: false,
      level: access.level,
      folder: "open",
    }
  }
  if (target.kind === "node") {
    const access =
      level === null
        ? await removeNodeGrant(
            target.node.id,
            { expectedRevision: revision, principal },
            csrf
          )
        : await setNodeGrant(
            target.node.id,
            { expectedRevision: revision, principal, level },
            csrf
          )
    return {
      targetKey: `node:${target.node.id}`,
      revision: access.revision,
      canManage: access.canManage,
      visibility: null,
      direct: access.direct,
      inherited: access.inherited,
      boundTo: null,
      restricted: access.restricted,
      canRestrict: access.canRestrict,
      level: access.level,
      folder: "open",
    }
  }
  const access =
    level === null
      ? await removeSessionGrant(
          target.session.id,
          { expectedRevision: revision, principal },
          csrf
        )
      : await setSessionGrant(
          target.session.id,
          { expectedRevision: revision, principal, level: "view" },
          csrf
        )
  return {
    targetKey: `session:${target.session.id}`,
    revision: access.revision,
    canManage: access.canManage,
    visibility: null,
    direct: access.direct,
    inherited: [],
    boundTo: null,
    restricted: false,
    canRestrict: false,
    level: access.level,
    folder: "open",
  }
}

/**
 * The caller's own level on a resource, read ONCE when it is opened and only
 * when it could be less than edit: a creator, a private item (creator-only)
 * and an organization item (everyone edits) need no request at all, so the
 * owner's normal flow is unchanged and lists never ask per row.
 */
export function useResourceLevel(
  resource: Pick<
    NativeResource,
    "id" | "creatorId" | "visibility" | "authRevision"
  > | null
): ResourceAccess["level"] | "unknown" {
  const auth = useNativeAuth()
  const me = auth.session?.user.id
  // Nothing is asked before the session is known: without it every item
  // would look like someone else's.
  const needs =
    !!resource &&
    me !== undefined &&
    resource.visibility === "shared" &&
    resource.creatorId !== me
  const key = needs
    ? `${auth.scopeKey}:${resource!.id}:${resource!.authRevision}`
    : null
  const [held, setHeld] = useState<{
    key: string
    level: ResourceAccess["level"] | "unknown"
  } | null>(null)
  useEffect(() => {
    if (!key || !resource) return
    const controller = new AbortController()
    loadResourceAccess(resource.id, controller.signal)
      .then((access) => {
        if (!controller.signal.aborted) setHeld({ key, level: access.level })
      })
      .catch(() => {
        // Not readable any more, or unreachable: write controls stay off;
        // the views report the read failure themselves.
        if (!controller.signal.aborted) setHeld({ key, level: "view" })
      })
    return () => controller.abort()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key])
  if (!resource || me === undefined) return "unknown"
  if (!needs) return resource.creatorId === me ? "owner" : "edit"
  // Until the answer is in, nothing is offered for writing.
  return held?.key === key ? held.level : "unknown"
}

/** Search people (paged) and teams (filtered locally) to share with. */
function PrincipalPicker({
  exclude,
  disabled,
  onPick,
}: {
  exclude: ReadonlySet<string>
  disabled: boolean
  onPick: (principal: Principal) => void
}) {
  const t = useTranslations("Sharing")
  const [query, setQuery] = useState("")
  const [people, setPeople] = useState<DirectoryMember[]>([])
  const [nextAfter, setNextAfter] = useState<string | null>(null)
  const [teams, setTeams] = useState<Team[] | null>(null)
  const [error, setError] = useState<unknown>(null)
  const [loading, setLoading] = useState(false)
  const request = useRef(0)

  useEffect(() => {
    const controller = new AbortController()
    listTeams(controller.signal)
      .then((list) => {
        if (!controller.signal.aborted) setTeams(list)
      })
      .catch((cause) => {
        if (!controller.signal.aborted) {
          setTeams([])
          setError(cause)
        }
      })
    return () => controller.abort()
  }, [])

  const load = useCallback(async (q: string, after: string | null) => {
    const id = ++request.current
    setLoading(true)
    try {
      const page = await searchDirectory({
        q: q || undefined,
        limit: 20,
        after,
      })
      if (id !== request.current) return
      setPeople((old) => (after ? [...old, ...page.members] : page.members))
      setNextAfter(page.nextAfter)
      setError(null)
    } catch (cause) {
      if (id === request.current) setError(cause)
    } finally {
      if (id === request.current) setLoading(false)
    }
  }, [])

  useEffect(() => {
    const timer = setTimeout(() => void load(query.trim(), null), 200)
    return () => clearTimeout(timer)
  }, [query, load])

  const needle = query.trim().toLowerCase()
  const shownTeams = (teams ?? []).filter(
    (team) =>
      (!needle || team.name.toLowerCase().includes(needle)) &&
      !exclude.has(`team:${team.id}`)
  )
  const shownPeople = people.filter(
    (member) => !exclude.has(`user:${member.userId}`)
  )
  return (
    <div className="space-y-2">
      <input
        type="search"
        value={query}
        disabled={disabled}
        maxLength={100}
        aria-label={t("searchPeopleTeams")}
        placeholder={t("searchPeopleTeams")}
        onChange={(event) => setQuery(event.target.value)}
        className="h-9 w-full rounded-lg border bg-background px-3 text-sm"
      />
      <ul
        aria-label={t("results")}
        className="max-h-48 space-y-0.5 overflow-y-auto"
      >
        {shownTeams.map((team) => (
          <li key={`team:${team.id}`}>
            <button
              type="button"
              disabled={disabled}
              onClick={() =>
                onPick({
                  kind: "team",
                  teamId: team.id,
                  name: team.name,
                  memberCount: team.memberCount,
                })
              }
              className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-start text-sm hover:bg-muted disabled:opacity-50"
            >
              <Users aria-hidden className="size-4 text-muted-foreground" />
              <span className="truncate">{team.name}</span>
              <span className="ms-auto text-xs text-muted-foreground">
                {t("teamMembers", { number: team.memberCount })}
              </span>
            </button>
          </li>
        ))}
        {shownPeople.map((member) => (
          <li key={`user:${member.userId}`}>
            <button
              type="button"
              disabled={disabled}
              onClick={() =>
                onPick({
                  kind: "user",
                  userId: member.userId,
                  displayName: member.displayName,
                  avatarUrl: member.avatarUrl,
                })
              }
              className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-start text-sm hover:bg-muted disabled:opacity-50"
            >
              <Avatar name={member.displayName} url={member.avatarUrl} />
              <span className="truncate">{member.displayName}</span>
            </button>
          </li>
        ))}
        {!loading && shownTeams.length === 0 && shownPeople.length === 0 && (
          <li className="px-2 py-1.5 text-xs text-muted-foreground">
            {t("noMatches")}
          </li>
        )}
      </ul>
      {nextAfter && (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={loading || disabled}
          onClick={() => void load(needle, nextAfter)}
        >
          {t("loadMorePeople")}
        </Button>
      )}
      <SharingError error={error} />
    </div>
  )
}

export function Avatar({
  name,
  url,
  className,
}: {
  name: string
  url: string | null
  className?: string
}) {
  return url ? (
    // eslint-disable-next-line @next/next/no-img-element
    <img
      src={url}
      alt=""
      className={cn("size-6 shrink-0 rounded-full object-cover", className)}
    />
  ) : (
    <span
      aria-hidden
      className={cn(
        "flex size-6 shrink-0 items-center justify-center rounded-full bg-muted text-xs font-medium",
        className
      )}
    >
      {name.slice(0, 1).toUpperCase()}
    </span>
  )
}

function LevelSelect({
  value,
  viewOnly,
  disabled,
  label,
  onChange,
}: {
  value: Level
  viewOnly: boolean
  disabled: boolean
  label: string
  onChange: (level: Level) => void
}) {
  const t = useTranslations("Sharing")
  if (viewOnly)
    return <span className="text-xs text-muted-foreground">{t("view")}</span>
  return (
    <select
      aria-label={label}
      value={value}
      disabled={disabled}
      onChange={(event) => onChange(event.target.value as Level)}
      className="h-8 rounded-md border bg-background px-2 text-xs"
    >
      <option value="view">{t("view")}</option>
      <option value="edit">{t("edit")}</option>
    </select>
  )
}

/** The reusable Share dialog. */
export function ShareDialog({
  target,
  open,
  onOpenChange,
  title,
}: {
  target: ShareTarget
  open: boolean
  onOpenChange: (open: boolean) => void
  /** The item's name, for the heading. */
  title: string
}) {
  const t = useTranslations("Sharing"),
    c = useTranslations("NativeContent")
  const auth = useNativeAuth()
  const [access, setAccess] = useState<Access | null>(null)
  const [error, setError] = useState<unknown>(null)
  const [busy, setBusy] = useState(false)
  // A removal that left an inherited grant for the same person or team.
  const [stillHas, setStillHas] = useState<InheritedGrant | null>(null)
  const sessionView = target.kind === "session"
  // Re-read whenever the item itself moved (a visibility change bumps it), or
  // the account/organization did: nothing from another scope is ever shown.
  const scope = useRef(auth.scopeKey)
  useEffect(() => {
    scope.current = auth.scopeKey
  }, [auth.scopeKey])
  const version = `${String(auth.scopeKey)}|${
    target.kind === "resource"
      ? `${target.resource.id}:${target.resource.authRevision}:${target.resource.visibility}`
      : targetKeyOf(target)
  }`

  useEffect(() => {
    if (!open) return
    const controller = new AbortController()
    setError(null)
    // A different item or scope: never show the previous answer meanwhile.
    setAccess((old) => (old && sameTarget(old, target) ? old : null))
    readAccess(target, controller.signal)
      .then((next) => {
        if (!controller.signal.aborted) setAccess(next)
      })
      .catch((cause) => {
        if (!controller.signal.aborted) setError(cause)
      })
    return () => controller.abort()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, version])

  const exclude = useMemo(() => {
    const keys = new Set(
      (access?.direct ?? []).map((g) => principalKey(g.principal))
    )
    // The owner already has access and is never a grant.
    if (target.kind === "resource")
      keys.add(`user:${target.resource.creatorId}`)
    if (target.kind === "session") keys.add(`user:${target.session.ownerId}`)
    return keys
  }, [access, target])

  async function change(principal: Principal, level: Level | null) {
    if (!access || !auth.session || busy) return
    const asked = scope.current
    setBusy(true)
    setError(null)
    setStillHas(null)
    try {
      const next = await writeGrant(
        target,
        access.revision,
        inputOf(principal),
        level,
        auth.session.csrf
      )
      // An answer for a scope that has since changed is not this reader's.
      if (scope.current !== asked) return
      // A grant answer says nothing about the folder: keep what was read.
      setAccess({ ...next, folder: access.folder })
      if (level === null) {
        const inherited = next.inherited.find(
          (grant) => principalKey(grant.principal) === principalKey(principal)
        )
        if (inherited) setStillHas(inherited)
      }
    } catch (cause) {
      if (scope.current !== asked) return
      setError(cause)
      // A stale revision or a refusal: show what is true now.
      readAccess(target)
        .then(setAccess)
        .catch(() => {})
    } finally {
      setBusy(false)
    }
  }

  // Widening who can read this is a question first, in the words sharing has
  // always used: the full history becomes readable. Narrowing is immediate.
  const [confirming, setConfirming] = useState<NativeVisibility | null>(null)
  const rank = { private: 0, shared: 1, organization: 2 } as const
  function applyVisibility(visibility: NativeVisibility) {
    if (target.kind !== "resource") return
    setError(null)
    setStillHas(null)
    void Promise.resolve(target.onVisibility(visibility)).then((refusal) => {
      if (refusal) setError(refusal)
      else setConfirming(null)
    })
  }
  // Folders: restricting hides the folder and its contents from everyone not
  // named here, and happens at once. Re-opening shows it all to the whole
  // organization again, so it is asked first.
  const [confirmOpen, setConfirmOpen] = useState(false)
  async function setRestricted(restricted: boolean) {
    if (target.kind !== "node" || !access || !auth.session || busy) return
    const asked = scope.current
    setBusy(true)
    setError(null)
    try {
      const next = await setNodeRestriction(
        target.node.id,
        { expectedRevision: access.revision, restricted },
        auth.session.csrf
      )
      if (scope.current !== asked) return
      setConfirmOpen(false)
      setAccess({
        targetKey: `node:${target.node.id}`,
        revision: next.revision,
        canManage: next.canManage,
        visibility: null,
        direct: next.direct,
        inherited: next.inherited,
        boundTo: null,
        restricted: next.restricted,
        canRestrict: next.canRestrict,
        level: next.level,
        folder: "open",
      })
    } catch (cause) {
      if (scope.current !== asked) return
      setError(cause)
      readAccess(target)
        .then(setAccess)
        .catch(() => {})
    } finally {
      setBusy(false)
    }
  }
  function chooseVisibility(visibility: NativeVisibility) {
    if (busy || !access?.visibility) return
    if (rank[visibility] > rank[access.visibility]) setConfirming(visibility)
    else {
      setConfirming(null)
      applyVisibility(visibility)
    }
  }

  const manage = !!access?.canManage && !access.boundTo
  const visibility = access?.visibility ?? null
  const grantsApply = target.kind !== "resource" || visibility === "shared"
  const folderNames = [
    ...new Set((access?.inherited ?? []).map((grant) => grant.nodeName)),
  ]

  return (
    <Dialog open={open} onOpenChange={(next) => !busy && onOpenChange(next)}>
      <DialogContent className="max-w-lg">
        <DialogTitle>{t("shareTitle", { title })}</DialogTitle>
        <DialogDescription>
          {target.kind === "node"
            ? t("nodeExplain")
            : sessionView
              ? t("sessionExplain")
              : t("resourceExplain")}
        </DialogDescription>
        {!access && !error && (
          <p role="status" className="text-sm text-muted-foreground">
            {t("loading")}
          </p>
        )}
        {access?.boundTo && (
          <p role="status" className="text-sm text-muted-foreground">
            {t("boundTo")}
          </p>
        )}
        {access && target.kind === "resource" && !access.boundTo && (
          <fieldset className="space-y-1.5" disabled={!manage || busy}>
            <legend className="mb-1 text-sm font-medium">
              {t("audience")}
            </legend>
            {(["private", "shared", "organization"] as const).map((value) => (
              <label key={value} className="flex items-start gap-2 text-sm">
                <input
                  type="radio"
                  name="audience"
                  value={value}
                  checked={(confirming ?? visibility) === value}
                  onChange={() => chooseVisibility(value)}
                  className="mt-1"
                />
                <span>
                  <span className="font-medium">
                    {value === "shared"
                      ? access.direct.length === 0
                        ? t("audienceFolder")
                        : t("audiencePeople")
                      : value === "organization" && access.folder !== "open"
                        ? t("audienceOrganizationLimited")
                        : t(`audience_${value}`, {
                            organization:
                              auth.session?.organization?.name ?? "",
                          })}
                  </span>
                  <span className="block text-xs text-muted-foreground">
                    {value === "organization" && access.folder !== "open"
                      ? t("audienceHintOrganizationLimited")
                      : t(`audienceHint_${value}`)}
                  </span>
                </span>
              </label>
            ))}
          </fieldset>
        )}
        {access && target.kind === "node" && (
          <fieldset
            className="space-y-1.5"
            disabled={!access.canRestrict || busy}
          >
            <legend className="mb-1 text-sm font-medium">
              {t("folderAudience")}
            </legend>
            {([false, true] as const).map((restricted) => (
              <label
                key={String(restricted)}
                className="flex items-start gap-2 text-sm"
              >
                <input
                  type="radio"
                  name="folder-audience"
                  checked={
                    (confirmOpen ? false : access.restricted) === restricted
                  }
                  onChange={() =>
                    restricted
                      ? void setRestricted(true)
                      : access.restricted && setConfirmOpen(true)
                  }
                  className="mt-1"
                />
                <span>
                  <span className="font-medium">
                    {restricted
                      ? t("folderRestricted")
                      : t("folderOpen", {
                          organization: auth.session?.organization?.name ?? "",
                        })}
                  </span>
                  <span className="block text-xs text-muted-foreground">
                    {t(restricted ? "folderRestrictedHint" : "folderOpenHint")}
                  </span>
                </span>
              </label>
            ))}
          </fieldset>
        )}
        {confirmOpen && target.kind === "node" && (
          <div
            role="group"
            aria-label={t("folderReopenTitle")}
            className="space-y-2 rounded-lg border p-3 text-sm"
          >
            <p className="font-medium">{t("folderReopenTitle")}</p>
            <p className="text-muted-foreground">
              {t("folderReopenWarning", {
                organization: auth.session?.organization?.name ?? "",
              })}
            </p>
            <div className="flex justify-end gap-2">
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={() => setConfirmOpen(false)}
              >
                {t("cancel")}
              </Button>
              <Button
                type="button"
                size="sm"
                disabled={busy}
                onClick={() => void setRestricted(false)}
              >
                {t("folderReopen")}
              </Button>
            </div>
          </div>
        )}
        {access?.level && access.level !== "owner" && !access.canManage && (
          <p className="text-xs text-muted-foreground">
            {t("yourAccess", { level: t(access.level) })}
          </p>
        )}
        {confirming && target.kind === "resource" && (
          <div
            role="group"
            aria-label={c("shareTitle")}
            className="space-y-2 rounded-lg border p-3 text-sm"
          >
            <p className="font-medium">{c("shareTitle")}</p>
            <p className="text-muted-foreground">
              {confirming === "organization"
                ? c("shareDescription", {
                    organization: auth.session?.organization?.name ?? "",
                  })
                : t("shareSharedDescription")}
            </p>
            <div className="flex justify-end gap-2">
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={() => setConfirming(null)}
              >
                {t("cancel")}
              </Button>
              <Button
                type="button"
                size="sm"
                onClick={() => applyVisibility(confirming)}
              >
                {c("confirmShare")}
              </Button>
            </div>
          </div>
        )}
        {access && visibility === "private" && folderNames.length > 0 && (
          <p className="flex items-start gap-1.5 text-xs text-muted-foreground">
            <Lock aria-hidden className="mt-0.5 size-3.5 shrink-0" />
            {t("privateException", { folders: folderNames.join(", ") })}
          </p>
        )}
        {access && grantsApply && (
          <section aria-label={t("peopleWithAccess")} className="space-y-2">
            <h3 className="text-sm font-medium">{t("peopleWithAccess")}</h3>
            {access.direct.length === 0 && (
              <p className="text-xs text-muted-foreground">{t("noneNamed")}</p>
            )}
            <ul className="space-y-1">
              {access.direct.map((grant) => (
                <li
                  key={principalKey(grant.principal)}
                  className="flex items-center gap-2 text-sm"
                >
                  {grant.principal.kind === "team" ? (
                    <Users
                      aria-hidden
                      className="size-4 text-muted-foreground"
                    />
                  ) : (
                    <Avatar
                      name={grant.principal.displayName}
                      url={grant.principal.avatarUrl}
                    />
                  )}
                  <span className="min-w-0 flex-1 truncate">
                    {principalName(grant.principal)}
                  </span>
                  {manage ? (
                    <>
                      <LevelSelect
                        value={grant.level}
                        viewOnly={sessionView}
                        disabled={busy}
                        label={t("levelFor", {
                          name: principalName(grant.principal),
                        })}
                        onChange={(level) =>
                          void change(grant.principal, level)
                        }
                      />
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        disabled={busy}
                        aria-label={t("removeFor", {
                          name: principalName(grant.principal),
                        })}
                        onClick={() => void change(grant.principal, null)}
                        className="size-7"
                      >
                        <X aria-hidden className="size-4" />
                      </Button>
                    </>
                  ) : (
                    <span className="text-xs text-muted-foreground">
                      {t(grant.level)}
                    </span>
                  )}
                </li>
              ))}
            </ul>
            {stillHas && (
              <p role="status" className="text-xs text-muted-foreground">
                {t("stillInherited", {
                  name: principalName(stillHas.principal),
                  folder: stillHas.nodeName,
                })}
              </p>
            )}
            {manage && (
              <PrincipalPicker
                exclude={exclude}
                disabled={busy}
                onPick={(principal) => void change(principal, "view")}
              />
            )}
          </section>
        )}
        {access &&
          access.inherited.length > 0 &&
          visibility !== "private" &&
          visibility !== "organization" && (
            <section aria-label={t("inheritedAccess")} className="space-y-1">
              <h3 className="text-sm font-medium">{t("inheritedAccess")}</h3>
              <ul className="space-y-1">
                {access.inherited.map((grant) => (
                  <li
                    key={`${grant.nodeId}:${principalKey(grant.principal)}`}
                    className="flex items-center gap-2 text-sm text-muted-foreground"
                  >
                    <span className="min-w-0 flex-1 truncate">
                      {principalName(grant.principal)}
                    </span>
                    <span className="text-xs">
                      {t("inheritedFrom", {
                        level: t(grant.level),
                        folder: grant.nodeName,
                      })}
                    </span>
                  </li>
                ))}
              </ul>
            </section>
          )}
        {!manage && access && !access.boundTo && (
          <p className="text-xs text-muted-foreground">{t("onlyOwner")}</p>
        )}
        <SharingError error={error} />
        <div className="flex justify-end">
          <Button
            type="button"
            variant="secondary"
            disabled={busy}
            onClick={() => onOpenChange(false)}
          >
            {t("done")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  )
}

/**
 * Everything shared with the reader that sits in a place they cannot see:
 * documents and chats (opened by id; their folder stays unnamed) and Agent
 * Chats (View only). Read once per scope; tagged with that scope so another
 * account's list is never shown while this one's is read.
 */
export function SharedSessionsList({
  scopeKey,
  onOpen,
  onOpenResource,
}: {
  scopeKey: string | null
  onOpen: (sessionId: string) => void
  onOpenResource?: (resource: NativeResource) => void
}) {
  const t = useTranslations("Sharing")
  const [held, setHeld] = useState<{
    scope: string | null
    sessions: SharedSession[]
    resources: NativeResource[]
  } | null>(null)
  const [expanded, setExpanded] = useState(false)
  useEffect(() => {
    if (!scopeKey) return
    const controller = new AbortController()
    // Each list stands alone: one failing never hides the other.
    void Promise.all([
      listSharedSessions(controller.signal).catch(() => []),
      onOpenResource
        ? listSharedResources(controller.signal).catch(() => [])
        : Promise.resolve([] as NativeResource[]),
    ]).then(([sessions, resources]) => {
      if (!controller.signal.aborted)
        setHeld({ scope: scopeKey, sessions, resources })
    })
    return () => controller.abort()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scopeKey])
  const current = held?.scope === scopeKey ? held : null
  const total =
    (current?.sessions.length ?? 0) + (current?.resources.length ?? 0)
  if (!current || total === 0) return null
  return (
    <div>
      <button
        type="button"
        aria-expanded={expanded}
        onClick={() => setExpanded((value) => !value)}
        className="flex min-h-8 w-full items-center gap-2 rounded-lg px-2 text-start text-sm text-muted-foreground hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring"
      >
        <Users aria-hidden className="size-3.5 shrink-0" />
        <span className="truncate">{t("sharedWithMe", { number: total })}</span>
      </button>
      {expanded && (
        <ul aria-label={t("sharedItems")} className="ms-5 space-y-0.5">
          {current.resources.map((resource) => (
            <li key={`r:${resource.id}`}>
              <button
                type="button"
                onClick={() => onOpenResource?.(resource)}
                className="flex min-h-7 w-full min-w-0 flex-col rounded-md px-2 py-1 text-start text-sm hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring"
              >
                <span className="truncate">{resource.title}</span>
                <span className="truncate text-xs text-muted-foreground">
                  {t(
                    resource.kind === "chat" ? "sharedChat" : "sharedDocument"
                  )}
                </span>
              </button>
            </li>
          ))}
          {current.sessions.map(({ session, owner }) => (
            <li key={`s:${session.id}`}>
              <button
                type="button"
                onClick={() => onOpen(session.id)}
                className="flex min-h-7 w-full min-w-0 flex-col rounded-md px-2 py-1 text-start text-sm hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring"
              >
                <span className="truncate">{session.title}</span>
                <span className="truncate text-xs text-muted-foreground">
                  {t("sharedBy", { name: owner.displayName })}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

/**
 * The reader's own level on a folder page, read once when the page opens (not
 * per row). Placement needs Edit or Owner (contract v2): a View recipient is
 * not offered create controls. Until known, nothing is withheld — an open
 * folder is Edit for everyone, and the server is the authority either way.
 */
export function useNodeLevel(
  nodeId: number | null
): "owner" | "edit" | "view" | "unknown" {
  const auth = useNativeAuth()
  const key = nodeId === null ? null : `${auth.scopeKey}:${nodeId}`
  const [held, setHeld] = useState<{
    key: string
    level: "owner" | "edit" | "view"
  } | null>(null)
  useEffect(() => {
    if (key === null || nodeId === null) return
    const controller = new AbortController()
    loadNodeAccess(nodeId, controller.signal)
      .then((access) => {
        if (!controller.signal.aborted) setHeld({ key, level: access.level })
      })
      .catch(() => {
        // Unreadable: the page reports its own read failure.
      })
    return () => controller.abort()
  }, [key, nodeId])
  return held?.key === key ? held.level : "unknown"
}

// ---- Folder levels for sidebar menus, fetched lazily ----
//
// Never on the initial load (that would be one request per folder): a folder's
// level is read the first time its menu opens, and kept per scope, folder and
// folder revision — a scope change, or any change to the folder, reads again.
type MenuLevel = "owner" | "edit" | "view" | "pending"
const menuLevels = new Map<string, MenuLevel>()
// Bumped by a reset: an answer from before it is dropped, never cached.
let menuGeneration = 0
const menuListeners = new Set<() => void>()
function menuEmit() {
  for (const listener of menuListeners) listener()
}
function menuSubscribe(listener: () => void) {
  menuListeners.add(listener)
  return () => {
    menuListeners.delete(listener)
  }
}

/** Test seam: forget every cached folder level. */
export function resetMenuLevels() {
  menuGeneration += 1
  menuLevels.clear()
  menuEmit()
}

export function useNodeMenuLevels(scopeKey: string | null) {
  // Re-render whenever any cached level changes; reading is by key below.
  const [, setTick] = useState(0)
  useEffect(() => menuSubscribe(() => setTick((value) => value + 1)), [])
  const keyOf = (node: NativeNode) => `${scopeKey}:${node.id}:${node.revision}`
  return {
    /** "unknown" until read; "pending" while being read. */
    levelOf(node: NativeNode): MenuLevel | "unknown" {
      return menuLevels.get(keyOf(node)) ?? "unknown"
    },
    load(node: NativeNode) {
      if (!scopeKey) return
      const key = keyOf(node)
      if (menuLevels.has(key)) return
      const generation = menuGeneration
      menuLevels.set(key, "pending")
      menuEmit()
      loadNodeAccess(node.id)
        .then((access) => {
          if (generation === menuGeneration) menuLevels.set(key, access.level)
        })
        // Unreadable (hidden or revoked): offer nothing that writes.
        .catch(() => {
          if (generation === menuGeneration) menuLevels.set(key, "view")
        })
        .finally(menuEmit)
    },
  }
}

/** Whether a folder menu may offer writes: only once known to be Edit/Owner. */
export function canWriteFolder(level: MenuLevel | "unknown"): boolean {
  return level === "edit" || level === "owner"
}
