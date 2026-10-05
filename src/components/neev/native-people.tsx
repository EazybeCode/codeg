"use client"

// People & teams: the organization's directory, for every active member.
// Names only (no email or contact details), and only this organization.
// Teams are readable by everyone and managed by admins (`canManage`).

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
import {
  addTeamMember,
  createTeam,
  deleteTeam,
  listTeams,
  loadTeamMembers,
  removeTeamMember,
  renameTeam,
  searchDirectory,
  type DirectoryMember,
  type Team,
  type TeamMembers,
} from "@/lib/neev-sharing"
import { useNativeAuth } from "./native-auth-provider"
import { Avatar, SharingError } from "./native-sharing"

/** Directory search with "Load more": the API pages, it never caps. */
function useDirectory(query: string) {
  const [members, setMembers] = useState<DirectoryMember[]>([])
  const [nextAfter, setNextAfter] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<unknown>(null)
  const request = useRef(0)
  const load = useCallback(async (q: string, after: string | null) => {
    const id = ++request.current
    setLoading(true)
    try {
      const page = await searchDirectory({
        q: q || undefined,
        limit: 50,
        after,
      })
      if (id !== request.current) return
      setMembers((old) => (after ? [...old, ...page.members] : page.members))
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
  return {
    members,
    loading,
    error,
    more: nextAfter ? () => void load(query.trim(), nextAfter) : null,
  }
}

function PeopleList() {
  const t = useTranslations("Sharing")
  const [query, setQuery] = useState("")
  const { members, loading, error, more } = useDirectory(query)
  return (
    <section aria-label={t("people")} className="space-y-3">
      <input
        type="search"
        value={query}
        maxLength={100}
        aria-label={t("searchPeople")}
        placeholder={t("searchPeople")}
        onChange={(event) => setQuery(event.target.value)}
        className="h-9 w-full max-w-sm rounded-lg border bg-background px-3 text-sm"
      />
      <ul aria-label={t("people")} className="divide-y rounded-lg border">
        {members.map((member) => (
          <li
            key={member.userId}
            className="flex items-center gap-3 px-3 py-2 text-sm"
          >
            <Avatar name={member.displayName} url={member.avatarUrl} />
            <span className="min-w-0 flex-1 truncate">
              {member.displayName}
            </span>
            <span className="text-xs text-muted-foreground">
              {t(`role_${member.role}`)}
            </span>
          </li>
        ))}
        {!loading && members.length === 0 && (
          <li className="px-3 py-2 text-sm text-muted-foreground">
            {t("noMatches")}
          </li>
        )}
      </ul>
      {more && (
        <Button variant="outline" size="sm" disabled={loading} onClick={more}>
          {t("loadMorePeople")}
        </Button>
      )}
      <SharingError error={error} />
    </section>
  )
}

function TeamDetail({
  team,
  onChanged,
  onDeleted,
}: {
  team: Team
  onChanged: (team: Team) => void
  onDeleted: () => void
}) {
  const t = useTranslations("Sharing")
  const auth = useNativeAuth()
  const [detail, setDetail] = useState<TeamMembers | null>(null)
  const [name, setName] = useState(team.name)
  const [query, setQuery] = useState("")
  const [confirmDelete, setConfirmDelete] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>(null)
  const directory = useDirectory(team.canManage ? query : "")
  // The team's revision moves on every rename and membership change, and the
  // next write must carry the newest one whichever answer brought it.
  const [revision, setRevision] = useState(team.revision)

  useEffect(() => {
    const controller = new AbortController()
    loadTeamMembers(team.id, controller.signal)
      .then((next) => {
        if (!controller.signal.aborted) {
          setDetail(next)
          setRevision(next.revision)
        }
      })
      .catch((cause) => {
        if (!controller.signal.aborted) setError(cause)
      })
    return () => controller.abort()
  }, [team.id])

  async function run<T>(work: (csrf: string) => Promise<T>): Promise<T | null> {
    if (!auth.session || busy) return null
    setBusy(true)
    setError(null)
    try {
      return await work(auth.session.csrf)
    } catch (cause) {
      setError(cause)
      return null
    } finally {
      setBusy(false)
    }
  }

  const members = new Set((detail?.members ?? []).map((m) => m.userId))
  return (
    <section aria-label={team.name} className="space-y-3 rounded-lg border p-3">
      {team.canManage ? (
        <form
          className="flex gap-2"
          onSubmit={(event) => {
            event.preventDefault()
            const next = name.trim()
            if (!next || next === team.name) return
            void run((csrf) => renameTeam(team.id, next, revision, csrf)).then(
              (renamed) => {
                if (!renamed) return
                setRevision(renamed.revision)
                onChanged(renamed)
              }
            )
          }}
        >
          <input
            value={name}
            maxLength={80}
            required
            aria-label={t("teamName")}
            disabled={busy}
            onChange={(event) => setName(event.target.value)}
            className="h-9 flex-1 rounded-lg border bg-background px-3 text-sm"
          />
          <Button type="submit" size="sm" disabled={busy}>
            {t("rename")}
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            disabled={busy}
            onClick={() => setConfirmDelete(true)}
          >
            {t("deleteTeam")}
          </Button>
        </form>
      ) : (
        <h3 className="text-sm font-medium">{team.name}</h3>
      )}
      <ul aria-label={t("teamMemberList")} className="space-y-1">
        {(detail?.members ?? []).map((member) => (
          <li key={member.userId} className="flex items-center gap-2 text-sm">
            <Avatar name={member.displayName} url={member.avatarUrl} />
            <span className="min-w-0 flex-1 truncate">
              {member.displayName}
            </span>
            {team.canManage && (
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                aria-label={t("removeFromTeam", { name: member.displayName })}
                onClick={() =>
                  void run((csrf) =>
                    removeTeamMember(team.id, member.userId, revision, csrf)
                  ).then((next) => {
                    if (!next) return
                    setDetail(next)
                    setRevision(next.revision)
                    onChanged({
                      ...team,
                      revision: next.revision,
                      memberCount: next.members.length,
                    })
                  })
                }
              >
                {t("remove")}
              </Button>
            )}
          </li>
        ))}
        {detail && detail.members.length === 0 && (
          <li className="text-xs text-muted-foreground">{t("teamEmpty")}</li>
        )}
      </ul>
      {team.canManage && (
        <div className="space-y-2">
          <input
            type="search"
            value={query}
            maxLength={100}
            aria-label={t("addPeople")}
            placeholder={t("addPeople")}
            onChange={(event) => setQuery(event.target.value)}
            className="h-9 w-full rounded-lg border bg-background px-3 text-sm"
          />
          {query.trim() && (
            <ul aria-label={t("results")} className="space-y-0.5">
              {directory.members
                .filter((member) => !members.has(member.userId))
                .map((member) => (
                  <li key={member.userId}>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() =>
                        void run((csrf) =>
                          addTeamMember(team.id, member.userId, revision, csrf)
                        ).then((next) => {
                          if (!next) return
                          setDetail(next)
                          setRevision(next.revision)
                          setQuery("")
                          onChanged({
                            ...team,
                            revision: next.revision,
                            memberCount: next.members.length,
                          })
                        })
                      }
                      className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-start text-sm hover:bg-muted disabled:opacity-50"
                    >
                      <Avatar
                        name={member.displayName}
                        url={member.avatarUrl}
                      />
                      {t("addName", { name: member.displayName })}
                    </button>
                  </li>
                ))}
            </ul>
          )}
          {directory.more && (
            <Button size="sm" variant="ghost" onClick={directory.more}>
              {t("loadMorePeople")}
            </Button>
          )}
        </div>
      )}
      <SharingError error={error ?? directory.error} />
      <Dialog open={confirmDelete} onOpenChange={setConfirmDelete}>
        <DialogContent>
          <DialogTitle>{t("deleteTeamTitle", { name: team.name })}</DialogTitle>
          <DialogDescription>{t("deleteTeamWarning")}</DialogDescription>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setConfirmDelete(false)}>
              {t("cancel")}
            </Button>
            <Button
              variant="destructive"
              disabled={busy}
              onClick={() =>
                void run((csrf) => deleteTeam(team.id, revision, csrf)).then(
                  (done) => {
                    setConfirmDelete(false)
                    if (done !== null) onDeleted()
                  }
                )
              }
            >
              {t("deleteTeam")}
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </section>
  )
}

function TeamsList() {
  const t = useTranslations("Sharing")
  const auth = useNativeAuth()
  const admin = auth.session?.role === "admin"
  const [teams, setTeams] = useState<Team[] | null>(null)
  const [open, setOpen] = useState<string | null>(null)
  const [name, setName] = useState("")
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>(null)
  // One id per attempted name: a retry after a lost response is the same team.
  const pending = useRef<{ name: string; id: string } | null>(null)

  useEffect(() => {
    const controller = new AbortController()
    listTeams(controller.signal)
      .then((list) => {
        if (!controller.signal.aborted) setTeams(list)
      })
      .catch((cause) => {
        if (!controller.signal.aborted) setError(cause)
      })
    return () => controller.abort()
  }, [])

  async function create() {
    const next = name.trim()
    if (!next || !auth.session || busy) return
    if (pending.current?.name !== next)
      pending.current = { name: next, id: crypto.randomUUID() }
    setBusy(true)
    setError(null)
    try {
      const team = await createTeam(next, pending.current.id, auth.session.csrf)
      pending.current = null
      setName("")
      setTeams((old) =>
        [...(old ?? []).filter((item) => item.id !== team.id), team].sort(
          (a, b) => a.name.localeCompare(b.name)
        )
      )
      setOpen(team.id)
    } catch (cause) {
      setError(cause)
    } finally {
      setBusy(false)
    }
  }

  return (
    <section aria-label={t("teams")} className="space-y-3">
      {admin && (
        <form
          className="flex max-w-sm gap-2"
          onSubmit={(event) => {
            event.preventDefault()
            void create()
          }}
        >
          <input
            value={name}
            maxLength={80}
            aria-label={t("newTeamName")}
            placeholder={t("newTeamName")}
            disabled={busy}
            onChange={(event) => setName(event.target.value)}
            className="h-9 flex-1 rounded-lg border bg-background px-3 text-sm"
          />
          <Button type="submit" size="sm" disabled={busy || !name.trim()}>
            {t("createTeam")}
          </Button>
        </form>
      )}
      {teams && teams.length === 0 && (
        <p className="text-sm text-muted-foreground">
          {admin ? t("noTeamsAdmin") : t("noTeams")}
        </p>
      )}
      <ul aria-label={t("teams")} className="space-y-2">
        {(teams ?? []).map((team) => (
          <li key={team.id} className="space-y-2">
            <button
              type="button"
              aria-expanded={open === team.id}
              onClick={() => setOpen(open === team.id ? null : team.id)}
              className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-start text-sm hover:bg-muted"
            >
              <Users aria-hidden className="size-4 text-muted-foreground" />
              <span className="min-w-0 flex-1 truncate font-medium">
                {team.name}
              </span>
              <span className="text-xs text-muted-foreground">
                {t("teamMembers", { number: team.memberCount })}
              </span>
            </button>
            {open === team.id && (
              <TeamDetail
                team={team}
                onChanged={(next) =>
                  setTeams((old) =>
                    (old ?? []).map((item) =>
                      item.id === next.id ? next : item
                    )
                  )
                }
                onDeleted={() => {
                  setOpen(null)
                  setTeams((old) =>
                    (old ?? []).filter((item) => item.id !== team.id)
                  )
                }}
              />
            )}
          </li>
        ))}
      </ul>
      <SharingError error={error} />
    </section>
  )
}

/** The People & teams page body (a Settings section). */
export function NativePeopleDirectory() {
  const t = useTranslations("Sharing")
  const [tab, setTab] = useState<"people" | "teams">("people")
  return (
    <div className="space-y-4">
      <div
        role="tablist"
        aria-label={t("peopleAndTeams")}
        className="flex gap-1"
      >
        {(["people", "teams"] as const).map((value) => (
          <Button
            key={value}
            role="tab"
            aria-selected={tab === value}
            size="sm"
            variant={tab === value ? "secondary" : "ghost"}
            onClick={() => setTab(value)}
          >
            {t(value)}
          </Button>
        ))}
      </div>
      {tab === "people" ? <PeopleList /> : <TeamsList />}
    </div>
  )
}
