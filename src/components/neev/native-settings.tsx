"use client"

import { useEffect, useRef, useState } from "react"
import Link from "next/link"
import { useTranslations } from "next-intl"
import { ArrowLeft } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogDescription, DialogTitle } from "@/components/ui/dialog"
import { cn } from "@/lib/utils"
import {
  loadMembers,
  loadMethods,
  loadOrganization,
  nativeMutation,
  parseOrganization,
  NativeClientError,
  type NativeMember,
  type NativeOrganizationDetails,
  type NativeProvider,
  type NativeRole,
} from "@/lib/neev-client"
import { NativeBoundary, NativeError } from "./native-boundary"
import { useNativeAuth } from "./native-auth-provider"
import { NativeDialogContent as DialogContent } from "./native-suspended-surface"
import { NativeSignInMethods } from "./native-login"
import { NativeAgentConnectionsPanel } from "./native-agent-connections"
import { NativeJoinCode } from "./native-join-code"

type Logo = { action: "keep" | "remove" } | { action: "upload"; data: string }

/**
 * The four things this page is actually for. Everything used to be stacked on
 * one scroll, which made the page read as a single undifferentiated list of
 * controls — organization branding sat two screens above the sign-in methods it
 * has nothing to do with.
 *
 * Section labels reuse existing `Native` messages, so this adds no translation
 * keys and reads correctly in all ten locales immediately.
 */
const SECTIONS = ["organization", "members", "agents", "profile"] as const
type Section = (typeof SECTIONS)[number]

// `as const` rather than `Record<Section, string>`: `useTranslations` only
// accepts a key it can see in the catalogue, so widening these to `string` would
// give up the compile-time check that each section's label actually exists.
const SECTION_LABEL = {
  organization: "organization",
  members: "members",
  agents: "personalAgents",
  profile: "yourProfile",
} as const satisfies Record<Section, string>

function SettingsContent() {
  const t = useTranslations("Native"),
    auth = useNativeAuth()
  const { handleError } = auth
  const organizationId = auth.session?.organization?.id
  const [org, setOrg] = useState<NativeOrganizationDetails | null>(null),
    [members, setMembers] = useState<NativeMember[]>([]),
    [methods, setMethods] = useState<NativeProvider[]>([])
  const [name, setName] = useState(""),
    [logo, setLogo] = useState<Logo>({ action: "keep" }),
    [hasMore, setHasMore] = useState(false)
  const [loading, setLoading] = useState(true),
    [busy, setBusy] = useState(false),
    [error, setError] = useState<unknown>(null),
    [notice, setNotice] = useState("")
  const [email, setEmail] = useState(""),
    [inviteRole, setInviteRole] = useState<NativeRole>("member"),
    [invites, setInvites] = useState<string[]>([]),
    [remove, setRemove] = useState<NativeMember | null>(null)
  const [section, setSection] = useState<Section>("organization")
  const opener = useRef<HTMLElement | null>(null),
    receipt = useRef<{ digest: string; id: string } | null>(null)
  const tabs = useRef(new Map<Section, HTMLButtonElement | null>())
  useEffect(() => {
    const controller = new AbortController()
    void Promise.all([
      loadOrganization(controller.signal),
      loadMembers(0, controller.signal),
      loadMethods(controller.signal),
    ])
      .then(([organization, people, linked]) => {
        if (controller.signal.aborted) return
        if (organization.id !== organizationId)
          throw new NativeClientError("invalid_response")
        setOrg(organization)
        setName(organization.name)
        setMembers(people)
        setHasMore(people.length === 100)
        setMethods(linked)
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
    return () => controller.abort()
  }, [handleError, organizationId])
  const admin = auth.session?.role === "admin",
    owner = org?.ownerUserId === auth.session?.user.id,
    emailAvailable = auth.capabilities?.providers.email === true
  async function action(work: () => Promise<void>) {
    if (busy) return
    setBusy(true)
    setError(null)
    setNotice("")
    try {
      await work()
    } catch (e) {
      if (!auth.signal?.aborted) {
        setError(e)
        auth.handleError(e)
      }
    } finally {
      if (!auth.signal?.aborted) setBusy(false)
    }
  }
  async function saveBranding() {
    if (!org || !auth.session) return
    const updated = parseOrganization(
      await nativeMutation(
        "/api/neev/organization",
        {
          expectedBrandingRevision: org.brandingRevision,
          name: name.trim(),
          logo,
        },
        auth.session.csrf,
        auth.signal,
        "PATCH"
      )
    )
    if (updated.id !== org.id) throw new NativeClientError("invalid_response")
    // Refresh the bootstrap as well so navigation never retains old branding.
    await auth.changed()
  }
  async function pickLogo(file?: File) {
    if (!file) return
    setError(null)
    if (
      file.size > 1024 * 1024 ||
      !["image/png", "image/jpeg", "image/webp"].includes(file.type)
    ) {
      setError(new NativeClientError("invalid_request"))
      return
    }
    const reader = new FileReader()
    reader.onload = () => {
      if (!auth.signal?.aborted && typeof reader.result === "string")
        setLogo({ action: "upload", data: reader.result })
    }
    reader.onerror = () => setError(new NativeClientError("invalid_request"))
    reader.readAsDataURL(file)
  }
  async function sendInvite() {
    if (!auth.session || !emailAvailable) return
    const digest = `${inviteRole}:${email.trim()}`
    if (receipt.current?.digest !== digest)
      receipt.current = { digest, id: crypto.randomUUID() }
    const result = await nativeMutation(
      "/api/neev/organization/invitations",
      { email: email.trim(), role: inviteRole, requestId: receipt.current.id },
      auth.session.csrf,
      auth.signal
    )
    if (
      !result ||
      typeof result !== "object" ||
      !("invitationId" in result) ||
      typeof result.invitationId !== "string" ||
      !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(
        result.invitationId
      )
    )
      throw new NativeClientError("invalid_response")
    setInvites((old) => [...new Set([...old, result.invitationId as string])])
    setEmail("")
    receipt.current = null
    setNotice(t("invitationCreated"))
  }
  async function changeRole(member: NativeMember, role: NativeRole) {
    if (!auth.session) return
    await nativeMutation(
      `/api/neev/organization/members/${member.id}/role`,
      { role },
      auth.session.csrf,
      auth.signal,
      "PATCH"
    )
    setMembers((old) =>
      old.map((row) => (row.id === member.id ? { ...row, role } : row))
    )
    setNotice(t("memberUpdated"))
  }
  async function removeMember() {
    if (!remove || !auth.session) return
    await nativeMutation(
      `/api/neev/organization/members/${remove.id}`,
      {},
      auth.session.csrf,
      auth.signal,
      "DELETE"
    )
    setMembers((old) => old.filter((row) => row.id !== remove.id))
    setRemove(null)
    setNotice(t("memberUpdated"))
  }
  async function more() {
    // Successful removals shift the server's active-membership offsets.
    const next = members.length
    const people = await loadMembers(next, auth.signal)
    setMembers((old) => [...old, ...people])
    setHasMore(people.length === 100 && next + people.length <= 100000)
  }
  const logoUrl =
    logo.action === "upload"
      ? logo.data
      : logo.action === "remove"
        ? null
        : org?.avatarUrl
  /**
   * Arrow keys move between sections, as a tablist is expected to. Both axes are
   * accepted because the nav is a column on a desktop and a row on a phone, and
   * which one the reader is looking at is a CSS breakpoint this cannot observe.
   */
  function onTabKey(event: React.KeyboardEvent, index: number) {
    const step =
      event.key === "ArrowRight" || event.key === "ArrowDown"
        ? 1
        : event.key === "ArrowLeft" || event.key === "ArrowUp"
          ? -1
          : 0
    let next = index
    if (step !== 0) next = (index + step + SECTIONS.length) % SECTIONS.length
    else if (event.key === "Home") next = 0
    else if (event.key === "End") next = SECTIONS.length - 1
    else return
    event.preventDefault()
    setSection(SECTIONS[next])
    tabs.current.get(SECTIONS[next])?.focus()
  }
  /**
   * Inactive sections are hidden rather than unmounted. The connections panel
   * polls a sign-in attempt it started, so discarding it on a tab switch would
   * abandon a login the user is part-way through; keeping every panel mounted
   * also means drafts and request timing are exactly what they were when this
   * page was one long scroll, which makes this a layout change and nothing more.
   */
  function panel(
    id: Section,
    children: React.ReactNode,
    /**
     * Off for a section whose content already carries its own heading — the
     * connections panel titles itself, and a second identical heading above it
     * reads as a rendering bug rather than as hierarchy.
     */
    heading = true
  ) {
    return (
      <div
        role="tabpanel"
        id={`neev-settings-${id}`}
        aria-labelledby={`neev-settings-tab-${id}`}
        hidden={section !== id}
        className="space-y-5"
      >
        {heading && (
          <h2 className="text-base font-medium">{t(SECTION_LABEL[id])}</h2>
        )}
        {children}
      </div>
    )
  }
  return (
    <main className="mx-auto w-full max-w-5xl space-y-5 p-5 pb-16 sm:p-8">
      <Link
        href="/workspace"
        className="inline-flex max-w-full items-center gap-2 rounded text-sm text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
      >
        <ArrowLeft aria-hidden className="size-4 shrink-0 rtl:rotate-180" />
        {t("backToWorkspace")}
      </Link>
      <header className="space-y-1">
        <p className="truncate text-sm text-muted-foreground" title={org?.name}>
          {org?.name ?? auth.session?.organization?.name}
        </p>
        <h1 className="text-2xl font-semibold tracking-tight">
          {t("settings")}
        </h1>
      </header>
      <div className="flex flex-col gap-5 sm:flex-row sm:gap-8">
        {/* A row that scrolls on a phone, a column beside the content on a
            desktop — the same destinations either way. */}
        <div
          role="tablist"
          aria-label={t("settings")}
          className="-mx-1 flex shrink-0 gap-1 overflow-x-auto px-1 pb-1 sm:mx-0 sm:w-52 sm:flex-col sm:overflow-x-visible sm:px-0 sm:pb-0"
        >
          {SECTIONS.map((id, index) => {
            const active = section === id
            return (
              <button
                key={id}
                type="button"
                role="tab"
                id={`neev-settings-tab-${id}`}
                ref={(node) => {
                  tabs.current.set(id, node)
                }}
                aria-selected={active}
                aria-controls={`neev-settings-${id}`}
                tabIndex={active ? 0 : -1}
                onClick={() => setSection(id)}
                onKeyDown={(event) => onTabKey(event, index)}
                className={cn(
                  "whitespace-nowrap rounded-lg px-3 py-2 text-start text-sm transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                  active
                    ? "bg-muted font-medium text-foreground"
                    : "text-muted-foreground hover:bg-muted/60 hover:text-foreground"
                )}
              >
                {t(SECTION_LABEL[id])}
              </button>
            )
          })}
        </div>
        <div className="min-w-0 flex-1 space-y-4">
          <p
            role="status"
            aria-live="polite"
            className="text-sm text-muted-foreground"
          >
            {loading ? t("loading") : busy ? t("working") : notice}
          </p>
          <NativeError error={error} />
          {panel(
            "organization",
            org && (
              <div className="space-y-5 rounded-xl border p-4">
                <div className="flex items-center gap-3">
                  {logoUrl?.startsWith("data:image/") ? (
                    <span
                      role="img"
                      aria-label={t("organizationLogo")}
                      className="size-12 shrink-0 rounded-lg border bg-contain bg-center bg-no-repeat"
                      style={{ backgroundImage: `url(${logoUrl})` }}
                    />
                  ) : (
                    <span
                      aria-hidden
                      className="flex size-12 shrink-0 items-center justify-center rounded-lg bg-muted text-xl"
                    >
                      {org.name.slice(0, 1)}
                    </span>
                  )}
                  <span className="min-w-0 break-words text-sm">
                    {org.name}
                  </span>
                </div>
                {admin ? (
                  <form
                    className="space-y-4"
                    onSubmit={(event) => {
                      event.preventDefault()
                      void action(saveBranding)
                    }}
                  >
                    <label className="block space-y-2">
                      <span className="text-sm">{t("organizationName")}</span>
                      <input
                        required
                        maxLength={100}
                        value={name}
                        disabled={busy}
                        onChange={(event) => setName(event.target.value)}
                        className="h-10 w-full rounded-lg border bg-background px-3 text-base md:text-sm"
                      />
                    </label>
                    <label className="block space-y-2">
                      <span className="text-sm">{t("organizationLogo")}</span>
                      <input
                        type="file"
                        accept="image/png,image/jpeg,image/webp"
                        disabled={busy}
                        onChange={(event) =>
                          void pickLogo(event.target.files?.[0])
                        }
                        className="block w-full min-w-0 text-sm file:me-3 file:rounded-md file:border file:bg-muted file:px-3 file:py-2"
                      />
                      <span className="block text-xs text-muted-foreground">
                        {t("logoHint")}
                      </span>
                    </label>
                    <div className="flex flex-wrap gap-2">
                      <Button
                        type="button"
                        variant="outline"
                        disabled={busy}
                        onClick={() => setLogo({ action: "remove" })}
                      >
                        {t("removeLogo")}
                      </Button>
                      <Button type="submit" disabled={busy || !name.trim()}>
                        {t("saveOrganization")}
                      </Button>
                    </div>
                  </form>
                ) : (
                  <p className="text-sm text-muted-foreground">
                    {t("adminOnly")}
                  </p>
                )}
              </div>
            )
          )}
          {panel(
            "members",
            <>
              {org && (
                <div className="space-y-4 rounded-xl border p-4">
                  <p className="text-sm text-muted-foreground">
                    {t("ownerProtection")}
                  </p>
                  <ul className="divide-y">
                    {members.map((member) => {
                      const isOwner = member.user.id === org.ownerUserId
                      const canRemove =
                        admin && !isOwner && (owner || member.role === "member")
                      return (
                        <li
                          key={member.id}
                          className="flex flex-wrap items-center gap-3 py-3"
                        >
                          <div className="min-w-0 flex-1">
                            <p
                              className="truncate text-sm"
                              title={member.user.displayName}
                            >
                              {member.user.displayName}
                            </p>
                            <p className="text-xs text-muted-foreground">
                              {t(isOwner ? "owner" : member.role)}
                            </p>
                          </div>
                          {owner && !isOwner && (
                            <label className="text-sm">
                              <span className="sr-only">
                                {t("roleFor", {
                                  name: member.user.displayName,
                                })}
                              </span>
                              <select
                                value={member.role}
                                disabled={busy}
                                onChange={(event) =>
                                  void action(() =>
                                    changeRole(
                                      member,
                                      event.target.value as NativeRole
                                    )
                                  )
                                }
                                className="max-w-full rounded-lg border bg-background p-2"
                              >
                                <option value="member">{t("member")}</option>
                                <option value="admin">{t("admin")}</option>
                              </select>
                            </label>
                          )}
                          {canRemove && (
                            <Button
                              variant="ghost"
                              size="sm"
                              disabled={busy}
                              aria-label={t("removePerson", {
                                name: member.user.displayName,
                              })}
                              onClick={() => {
                                opener.current =
                                  document.activeElement as HTMLElement
                                setRemove(member)
                              }}
                            >
                              {t("remove")}
                            </Button>
                          )}
                        </li>
                      )
                    })}
                  </ul>
                  {hasMore && (
                    <Button
                      variant="outline"
                      disabled={busy}
                      onClick={() => void action(more)}
                    >
                      {t("loadMore")}
                    </Button>
                  )}
                </div>
              )}
              {org && admin && (
                <form
                  className="space-y-3 rounded-xl border p-4"
                  onSubmit={(event) => {
                    event.preventDefault()
                    void action(sendInvite)
                  }}
                >
                  <label className="block space-y-2">
                    <span className="text-sm">{t("inviteEmail")}</span>
                    <input
                      type="email"
                      required
                      maxLength={254}
                      autoComplete="off"
                      value={email}
                      disabled={busy || !emailAvailable}
                      onChange={(event) => setEmail(event.target.value)}
                      className="h-10 w-full min-w-0 rounded-lg border bg-background px-3 text-base md:text-sm"
                    />
                  </label>
                  {owner && (
                    <label className="block space-y-2">
                      <span className="text-sm">{t("inviteRole")}</span>
                      <select
                        value={inviteRole}
                        disabled={busy || !emailAvailable}
                        onChange={(event) =>
                          setInviteRole(event.target.value as NativeRole)
                        }
                        className="h-10 w-full rounded-lg border bg-background px-3"
                      >
                        <option value="member">{t("member")}</option>
                        <option value="admin">{t("admin")}</option>
                      </select>
                    </label>
                  )}
                  {!emailAvailable && (
                    <p className="text-sm text-muted-foreground">
                      {t("invitationEmailUnavailable")}
                    </p>
                  )}
                  <Button
                    disabled={busy || !emailAvailable || !email.trim()}
                    type="submit"
                  >
                    {t("inviteMember")}
                  </Button>
                </form>
              )}
              {invites.length > 0 && (
                <div className="space-y-2 rounded-xl border p-4">
                  <p className="text-xs text-muted-foreground">
                    {t("invitationsThisVisit")}
                  </p>
                  {invites.map((invite, index) => (
                    <Button
                      key={invite}
                      variant="outline"
                      disabled={busy}
                      onClick={() =>
                        void action(async () => {
                          if (!auth.session) return
                          await nativeMutation(
                            `/api/neev/organization/invitations/${invite}`,
                            {},
                            auth.session.csrf,
                            auth.signal,
                            "DELETE"
                          )
                          setInvites((old) => old.filter((id) => id !== invite))
                          setNotice(t("invitationRevoked"))
                        })
                      }
                    >
                      {t("revokeInvitation", { number: index + 1 })}
                    </Button>
                  ))}
                </div>
              )}
              {/* Joining by code is how people get in, so it belongs with the
                  people rather than with the branding. */}
              <NativeJoinCode />
            </>
          )}
          {/* The caller's own provider connections, read live from the server
              (spec §5.4). Credentials never reach the browser. */}
          {panel(
            "agents",
            <NativeAgentConnectionsPanel
              heading={t("personalAgents")}
              showSettingsLink={false}
            />,
            false
          )}
          {panel(
            "profile",
            <div className="space-y-4 rounded-xl border p-4">
              <p className="break-words text-sm">
                {auth.session?.user.displayName}
              </p>
              <p className="text-sm text-muted-foreground">
                {t("profileReadOnly")}
              </p>
              <h3 className="text-sm font-medium">{t("signInMethods")}</h3>
              <p className="text-sm text-muted-foreground">
                {methods.map((method) => t(`provider_${method}`)).join(" · ")}
              </p>
              {!loading && <NativeSignInMethods linking linked={methods} />}
              <div className="border-t pt-4">
                <Button variant="outline" onClick={() => void auth.logout()}>
                  {t("signOut")}
                </Button>
              </div>
            </div>
          )}
        </div>
      </div>
      <Dialog
        open={remove !== null}
        onOpenChange={(open) => {
          if (!open && !busy) setRemove(null)
        }}
      >
        <DialogContent
          showCloseButton={false}
          onCloseAutoFocus={(event) => {
            event.preventDefault()
            opener.current?.focus()
          }}
        >
          <DialogTitle>
            {t("removePerson", { name: remove?.user.displayName ?? "" })}
          </DialogTitle>
          <DialogDescription>{t("removeDescription")}</DialogDescription>
          <div className="flex justify-end gap-2">
            <Button
              variant="ghost"
              disabled={busy}
              onClick={() => setRemove(null)}
            >
              {t("cancel")}
            </Button>
            <Button
              disabled={busy}
              variant="destructive"
              onClick={() => void action(removeMember)}
            >
              {t("confirmRemove")}
            </Button>
          </div>
          <NativeError error={error} />
        </DialogContent>
      </Dialog>
    </main>
  )
}
export function NativeSettings() {
  return (
    <NativeBoundary>
      <SettingsContent />
    </NativeBoundary>
  )
}
