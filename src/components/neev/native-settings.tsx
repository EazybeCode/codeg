"use client"

import { useEffect, useRef, useState } from "react"
import Link from "next/link"
import { useTranslations } from "next-intl"
import { ArrowLeft } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogDescription, DialogTitle } from "@/components/ui/dialog"
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
import { NativeJoinCode } from "./native-join-code"

type Logo = { action: "keep" | "remove" } | { action: "upload"; data: string }
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
  const opener = useRef<HTMLElement | null>(null),
    receipt = useRef<{ digest: string; id: string } | null>(null)
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
  return (
    <main className="mx-auto w-full max-w-3xl space-y-8 p-5 pb-16 sm:p-10">
      <Link
        href="/workspace"
        className="inline-flex max-w-full items-center gap-2 rounded text-sm text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
      >
        <ArrowLeft aria-hidden className="size-4 shrink-0 rtl:rotate-180" />
        {t("backToWorkspace")}
      </Link>
      <header className="space-y-2">
        <p className="truncate text-sm text-muted-foreground" title={org?.name}>
          {org?.name ?? auth.session?.organization?.name}
        </p>
        <h1 className="text-3xl font-semibold tracking-tight">
          {t("settings")}
        </h1>
      </header>
      <p
        role="status"
        aria-live="polite"
        className="text-sm text-muted-foreground"
      >
        {loading ? t("loading") : busy ? t("working") : notice}
      </p>
      <NativeError error={error} />
      {org && (
        <>
          <section className="space-y-4 border-t pt-6">
            <h2 className="text-lg font-medium">{t("organization")}</h2>
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
              <span className="min-w-0 break-words text-sm">{org.name}</span>
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
                    onChange={(event) => void pickLogo(event.target.files?.[0])}
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
              <p className="text-sm text-muted-foreground">{t("adminOnly")}</p>
            )}
          </section>
          <section className="space-y-4 border-t pt-6">
            <h2 className="text-lg font-medium">{t("members")}</h2>
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
                          {t("roleFor", { name: member.user.displayName })}
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
                          opener.current = document.activeElement as HTMLElement
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
            {admin && (
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
              <div className="space-y-2">
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
          </section>
        </>
      )}
      <div className="border-t pt-6">
        <NativeJoinCode />
      </div>
      <section className="space-y-4 border-t pt-6">
        <h2 className="text-lg font-medium">{t("yourProfile")}</h2>
        <p className="break-words text-sm">{auth.session?.user.displayName}</p>
        <p className="text-sm text-muted-foreground">{t("profileReadOnly")}</p>
        <h3 className="text-sm font-medium">{t("signInMethods")}</h3>
        <p className="text-sm text-muted-foreground">
          {methods.map((method) => t(`provider_${method}`)).join(" · ")}
        </p>
        {!loading && <NativeSignInMethods linking linked={methods} />}
      </section>
      <section className="space-y-3 rounded-xl border p-5">
        <h2 className="font-medium">{t("personalAgents")}</h2>
        <p className="text-sm font-medium">{t("agentUnavailable")}</p>
        <p className="text-sm leading-6 text-muted-foreground">
          {t("agentDescription")}
        </p>
      </section>
      <Button variant="outline" onClick={() => void auth.logout()}>
        {t("signOut")}
      </Button>
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
