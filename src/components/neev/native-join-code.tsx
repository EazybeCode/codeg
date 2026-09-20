"use client"

import { useEffect, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { Button } from "@/components/ui/button"
import {
  loadJoinCode,
  rotateJoinCode,
  setJoinCodeEnabled,
  previewJoinCode,
  acceptJoinCode,
  selectOrganization,
  NativeClientError,
  type NativeJoinCode as JoinCode,
  type NativeJoinPreview,
} from "@/lib/neev-client"
import { NativeError } from "./native-boundary"
import { useNativeAuth } from "./native-auth-provider"

/** Organization join code: admins manage the org's code (generate/rotate/enable);
 *  any authenticated member can join another org by entering its code, previewing
 *  the organization, and confirming — landing immediately as a regular member. */
export function NativeJoinCode() {
  const t = useTranslations("Native")
  const auth = useNativeAuth()
  const csrf = auth.session?.csrf ?? ""
  const isAdmin = auth.session?.role === "admin"

  const [code, setCode] = useState<JoinCode | null>(null),
    [adminBusy, setAdminBusy] = useState(false),
    [adminError, setAdminError] = useState<unknown>(null),
    [copied, setCopied] = useState(false)
  const [input, setInput] = useState(""),
    [preview, setPreview] = useState<NativeJoinPreview | null>(null),
    [joinBusy, setJoinBusy] = useState(false),
    [joinError, setJoinError] = useState<unknown>(null),
    [joined, setJoined] = useState<NativeJoinPreview | null>(null)
  const alive = useRef(true)
  const { handleError } = auth

  useEffect(() => {
    alive.current = true
    if (!isAdmin) return
    const controller = new AbortController()
    void loadJoinCode(controller.signal)
      .then((value) => {
        if (!controller.signal.aborted) setCode(value)
      })
      .catch((e) => {
        if (!controller.signal.aborted) {
          setAdminError(e)
          handleError(e)
        }
      })
    return () => {
      alive.current = false
      controller.abort()
    }
  }, [isAdmin, handleError])

  async function rotate() {
    if (adminBusy || !csrf) return
    setAdminBusy(true)
    setAdminError(null)
    setCopied(false)
    try {
      const value = await rotateJoinCode(csrf, auth.signal)
      if (alive.current) setCode(value)
    } catch (e) {
      if (alive.current) {
        setAdminError(e)
        auth.handleError(e)
      }
    } finally {
      if (alive.current) setAdminBusy(false)
    }
  }

  async function toggleEnabled() {
    if (adminBusy || !csrf || !code) return
    setAdminBusy(true)
    setAdminError(null)
    try {
      await setJoinCodeEnabled(code.disabled, csrf, auth.signal)
      if (alive.current) setCode({ ...code, disabled: !code.disabled })
    } catch (e) {
      if (alive.current) {
        setAdminError(e)
        auth.handleError(e)
      }
    } finally {
      if (alive.current) setAdminBusy(false)
    }
  }

  async function runPreview() {
    if (joinBusy || !csrf || !input.trim()) return
    setJoinBusy(true)
    setJoinError(null)
    setJoined(null)
    try {
      const value = await previewJoinCode(input.trim(), csrf, auth.signal)
      if (alive.current) setPreview(value)
    } catch (e) {
      if (alive.current) {
        setJoinError(e)
        auth.handleError(e)
      }
    } finally {
      if (alive.current) setJoinBusy(false)
    }
  }

  async function confirmJoin() {
    if (joinBusy || !csrf || !preview) return
    setJoinBusy(true)
    setJoinError(null)
    try {
      await acceptJoinCode(input.trim(), preview.orgId, csrf, auth.signal)
      if (alive.current) {
        setJoined(preview)
        setPreview(null)
        setInput("")
      }
    } catch (e) {
      if (alive.current) {
        setJoinError(e)
        auth.handleError(e)
      }
    } finally {
      if (alive.current) setJoinBusy(false)
    }
  }

  async function switchTo(orgId: number) {
    if (!csrf) return
    try {
      await selectOrganization(orgId, csrf, auth.signal)
      await auth.refresh()
    } catch (e) {
      if (alive.current) {
        setJoinError(e)
        auth.handleError(e)
      }
    }
  }

  const joinErrorMessage =
    joinError instanceof NativeClientError &&
    (joinError.kind === "forbidden" || joinError.kind === "unavailable")
      ? t("invalidJoinCode")
      : null

  return (
    <div className="space-y-10">
      {isAdmin && (
        <section className="space-y-3" aria-label={t("joinCodeHeading")}>
          <div>
            <h2 className="text-sm font-medium">{t("joinCodeHeading")}</h2>
            <p className="text-xs leading-5 text-muted-foreground">
              {t("joinCodeHelp")}
            </p>
          </div>
          {code?.code ? (
            <div className="space-y-3 rounded-xl border p-4">
              <div className="flex flex-wrap items-center gap-3">
                <code className="rounded-md bg-muted px-3 py-1.5 text-base font-semibold tracking-widest">
                  {code.code}
                </code>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => {
                    void navigator.clipboard
                      .writeText(code.code ?? "")
                      .then(() => alive.current && setCopied(true))
                      .catch(() => {})
                  }}
                >
                  {copied ? t("joinCodeCopied") : t("copyJoinCode")}
                </Button>
                {code.disabled && (
                  <span className="text-xs text-muted-foreground">
                    {t("joiningDisabledNote")}
                  </span>
                )}
              </div>
              <div className="flex flex-wrap gap-2">
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={adminBusy}
                  onClick={() => void rotate()}
                >
                  {t("rotateJoinCode")}
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={adminBusy}
                  onClick={() => void toggleEnabled()}
                >
                  {t(code.disabled ? "enableJoining" : "disableJoining")}
                </Button>
              </div>
            </div>
          ) : (
            <div className="space-y-3 rounded-xl border p-4">
              <p className="text-sm text-muted-foreground">
                {t("noJoinCodeYet")}
              </p>
              <Button disabled={adminBusy} onClick={() => void rotate()}>
                {t("generateJoinCode")}
              </Button>
            </div>
          )}
          <NativeError error={adminError} />
        </section>
      )}

      <section className="space-y-3" aria-label={t("joinOrgHeading")}>
        <div>
          <h2 className="text-sm font-medium">{t("joinOrgHeading")}</h2>
          <p className="text-xs leading-5 text-muted-foreground">
            {t("joinOrgHelp")}
          </p>
        </div>
        {joined ? (
          <div className="space-y-3 rounded-xl border p-4">
            <p className="text-sm">
              {t("joinedOrgNotice", { name: joined.name })}
            </p>
            <div className="flex flex-wrap gap-2">
              <Button size="sm" onClick={() => void switchTo(joined.orgId)}>
                {t("switchToJoinedOrg", { name: joined.name })}
              </Button>
              <Button variant="ghost" size="sm" onClick={() => setJoined(null)}>
                {t("joinAnother")}
              </Button>
            </div>
          </div>
        ) : (
          <form
            className="space-y-3"
            onSubmit={(event) => {
              event.preventDefault()
              if (preview) void confirmJoin()
              else void runPreview()
            }}
          >
            <label className="block space-y-2">
              <span className="sr-only">{t("joinCodeInputLabel")}</span>
              <input
                value={input}
                disabled={joinBusy}
                placeholder={t("joinCodeInputLabel")}
                onChange={(event) => {
                  setInput(event.target.value)
                  setPreview(null)
                  setJoinError(null)
                }}
                className="h-10 w-full max-w-xs rounded-lg border bg-background px-3 tracking-widest"
              />
            </label>
            {preview ? (
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-sm">
                  {t("confirmJoinOrg", { name: preview.name })}
                </span>
                <Button type="submit" size="sm" disabled={joinBusy}>
                  {t("joinOrgConfirm")}
                </Button>
              </div>
            ) : (
              <Button
                type="submit"
                size="sm"
                disabled={joinBusy || !input.trim()}
              >
                {t("joinOrgPreview")}
              </Button>
            )}
            {joinErrorMessage ? (
              <p role="alert" className="text-sm text-destructive">
                {joinErrorMessage}
              </p>
            ) : (
              <NativeError error={joinError} />
            )}
          </form>
        )}
      </section>
    </div>
  )
}
