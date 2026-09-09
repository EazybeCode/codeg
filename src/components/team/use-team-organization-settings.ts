"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"

import {
  TeamClientError,
  loadTeamOrganizationSettings,
  updateTeamOrganizationSettings,
  type TeamOrganizationLogo,
  type TeamOrganizationSettings,
} from "@/lib/team-client"
import { notifyTeamAuthChanged, useTeamAuth } from "./team-auth-provider"

const MAX_LOGO_BYTES = 1024 * 1024
const LOGO_TYPES = new Set(["image/png", "image/jpeg", "image/webp"])
const SAFE_LOGO_DATA =
  /^data:image\/(?:png|jpeg|webp);base64,[A-Za-z0-9+/]*={0,2}$/

export type TeamOrganizationSettingsError = "load" | "save" | "file" | null

interface SettingsState {
  scopeKey: string | null
  organization: TeamOrganizationSettings | null
  name: string
  logo: TeamOrganizationLogo
  loading: boolean
  saving: boolean
  logoReading: boolean
  error: TeamOrganizationSettingsError
  saved: boolean
}

const emptyState: SettingsState = {
  scopeKey: null,
  organization: null,
  name: "",
  logo: { action: "keep" },
  loading: false,
  saving: false,
  logoReading: false,
  error: null,
  saved: false,
}

function validName(name: string): boolean {
  const normalized = name.trim()
  const characters = Array.from(normalized)
  return (
    characters.length > 0 &&
    characters.length <= 80 &&
    !characters.some((character) => /\p{Cc}/u.test(character))
  )
}

function safeLogo(url: string | null): string | null {
  return url && SAFE_LOGO_DATA.test(url) ? url : null
}

export function useTeamOrganizationSettings() {
  const auth = useTeamAuth()
  const { refresh, scopeKey: authScopeKey, session, status } = auth
  const [state, setReactState] = useState<SettingsState>(emptyState)
  const [retryGeneration, setRetryGeneration] = useState(0)
  const stateRef = useRef(state)
  const requestRef = useRef<AbortController | null>(null)
  const requestGenerationRef = useRef(0)
  const readerRef = useRef<FileReader | null>(null)
  const readerGenerationRef = useRef(0)
  const authorityRef = useRef({
    status,
    scopeKey: authScopeKey,
    organizationId: session?.organization.id ?? null,
  })
  authorityRef.current = {
    status,
    scopeKey: authScopeKey,
    organizationId: session?.organization.id ?? null,
  }

  const updateState = useCallback(
    (next: SettingsState | ((current: SettingsState) => SettingsState)) => {
      const value = typeof next === "function" ? next(stateRef.current) : next
      stateRef.current = value
      setReactState(value)
    },
    []
  )

  const cancelRequest = useCallback(() => {
    requestGenerationRef.current += 1
    requestRef.current?.abort()
    requestRef.current = null
  }, [])

  const cancelReader = useCallback(() => {
    readerGenerationRef.current += 1
    const reader = readerRef.current
    readerRef.current = null
    if (reader && reader.readyState === FileReader.LOADING) reader.abort()
  }, [])

  const authorityMatches = useCallback(
    (scopeKey: string, organizationId: number) => {
      const current = authorityRef.current
      return (
        current.status === "authenticated" &&
        current.scopeKey === scopeKey &&
        current.organizationId === organizationId
      )
    },
    []
  )

  useEffect(() => {
    if (status === "loading" || status === "unavailable") {
      cancelRequest()
      cancelReader()
      updateState((current) => ({
        ...current,
        loading: false,
        saving: false,
        logoReading: false,
      }))
      return
    }

    if (status !== "authenticated" || !session || !authScopeKey) {
      cancelRequest()
      cancelReader()
      updateState(emptyState)
      return
    }

    const scopeKey = authScopeKey
    const organizationId = session.organization.id
    const sameScope = stateRef.current.scopeKey === scopeKey
    cancelRequest()
    cancelReader()
    const controller = new AbortController()
    const generation = requestGenerationRef.current
    requestRef.current = controller
    updateState((current) =>
      sameScope
        ? {
            ...current,
            loading: true,
            saving: false,
            logoReading: false,
            error: null,
            saved: false,
          }
        : {
            ...emptyState,
            scopeKey,
            loading: true,
          }
    )

    void loadTeamOrganizationSettings(controller.signal)
      .then((organization) => {
        if (
          controller.signal.aborted ||
          generation !== requestGenerationRef.current ||
          !authorityMatches(scopeKey, organizationId)
        ) {
          return
        }
        if (organization.id !== organizationId) {
          throw new TeamClientError("invalid_response")
        }
        updateState((current) => {
          const retainedName =
            current.organization !== null &&
            current.name.trim() !== current.organization.name
          const retainedLogo = current.logo.action !== "keep"
          return {
            scopeKey,
            organization,
            name: retainedName ? current.name : organization.name,
            logo: retainedLogo ? current.logo : { action: "keep" },
            loading: false,
            saving: false,
            logoReading: false,
            error: null,
            saved: current.saved,
          }
        })
      })
      .catch((error: unknown) => {
        if (
          controller.signal.aborted ||
          generation !== requestGenerationRef.current ||
          !authorityMatches(scopeKey, organizationId)
        ) {
          return
        }
        if (error instanceof TeamClientError && error.kind === "unauthorized") {
          updateState(emptyState)
          void refresh()
          return
        }
        updateState((current) => ({
          ...current,
          loading: false,
          error: "load",
          saved: false,
        }))
      })
      .finally(() => {
        if (requestRef.current === controller) requestRef.current = null
      })
  }, [
    authScopeKey,
    authorityMatches,
    cancelReader,
    cancelRequest,
    retryGeneration,
    refresh,
    session,
    status,
    updateState,
  ])

  useEffect(
    () => () => {
      cancelRequest()
      cancelReader()
      stateRef.current = emptyState
    },
    [cancelReader, cancelRequest]
  )

  const setName = useCallback(
    (name: string) => {
      updateState((current) => ({
        ...current,
        name,
        error: current.error === "file" ? current.error : null,
        saved: false,
      }))
    },
    [updateState]
  )

  const uploadLogo = useCallback(
    (file: File | undefined) => {
      if (!file) return
      cancelReader()
      const current = stateRef.current
      const authority = authorityRef.current
      if (
        authority.status !== "authenticated" ||
        !authority.scopeKey ||
        authority.organizationId === null ||
        !current.organization?.canEdit
      ) {
        return
      }
      if (file.size > MAX_LOGO_BYTES || !LOGO_TYPES.has(file.type)) {
        updateState((value) => ({
          ...value,
          logoReading: false,
          error: "file",
          saved: false,
        }))
        return
      }

      const scopeKey = authority.scopeKey
      const organizationId = authority.organizationId
      const generation = readerGenerationRef.current
      const reader = new FileReader()
      readerRef.current = reader
      updateState((value) => ({
        ...value,
        logoReading: true,
        error: null,
        saved: false,
      }))
      reader.onload = () => {
        if (
          generation !== readerGenerationRef.current ||
          readerRef.current !== reader ||
          !authorityMatches(scopeKey, organizationId) ||
          typeof reader.result !== "string"
        ) {
          return
        }
        readerRef.current = null
        updateState((value) => ({
          ...value,
          logo: { action: "upload", data: reader.result as string },
          logoReading: false,
          error: null,
        }))
      }
      reader.onerror = () => {
        if (
          generation !== readerGenerationRef.current ||
          readerRef.current !== reader ||
          !authorityMatches(scopeKey, organizationId)
        ) {
          return
        }
        readerRef.current = null
        updateState((value) => ({
          ...value,
          logoReading: false,
          error: "file",
        }))
      }
      reader.readAsDataURL(file)
    },
    [authorityMatches, cancelReader, updateState]
  )

  const removeLogo = useCallback(() => {
    cancelReader()
    updateState((current) => ({
      ...current,
      logo: { action: "remove" },
      logoReading: false,
      error: null,
      saved: false,
    }))
  }, [cancelReader, updateState])

  const save = useCallback(async () => {
    const current = stateRef.current
    if (current.logoReading) {
      cancelReader()
      updateState((value) => ({ ...value, logoReading: false }))
      return
    }
    const authority = authorityRef.current
    const currentSession = session
    const normalizedName = current.name.trim()
    const dirty =
      current.organization !== null &&
      (normalizedName !== current.organization.name ||
        current.logo.action !== "keep")
    if (
      authority.status !== "authenticated" ||
      !authority.scopeKey ||
      authority.organizationId === null ||
      !currentSession ||
      current.scopeKey !== authority.scopeKey ||
      !current.organization?.canEdit ||
      !validName(current.name) ||
      !dirty ||
      current.saving
    ) {
      return
    }

    cancelRequest()
    cancelReader()
    const scopeKey = authority.scopeKey
    const organizationId = authority.organizationId
    const generation = requestGenerationRef.current
    const controller = new AbortController()
    requestRef.current = controller
    updateState((value) => ({
      ...value,
      saving: true,
      error: null,
      saved: false,
    }))
    try {
      const organization = await updateTeamOrganizationSettings(
        { name: normalizedName, logo: current.logo },
        currentSession.csrf,
        controller.signal
      )
      if (
        controller.signal.aborted ||
        generation !== requestGenerationRef.current ||
        !authorityMatches(scopeKey, organizationId)
      ) {
        return
      }
      if (organization.id !== organizationId) {
        throw new TeamClientError("invalid_response")
      }
      updateState({
        scopeKey,
        organization,
        name: organization.name,
        logo: { action: "keep" },
        loading: false,
        saving: false,
        logoReading: false,
        error: null,
        saved: true,
      })
      notifyTeamAuthChanged()
      void refresh()
    } catch (error) {
      if (
        controller.signal.aborted ||
        generation !== requestGenerationRef.current ||
        !authorityMatches(scopeKey, organizationId)
      ) {
        return
      }
      if (error instanceof TeamClientError && error.kind === "unauthorized") {
        updateState(emptyState)
        void refresh()
      } else {
        updateState((value) => ({
          ...value,
          saving: false,
          error: "save",
          saved: false,
        }))
      }
    } finally {
      if (requestRef.current === controller) requestRef.current = null
    }
  }, [
    authorityMatches,
    cancelReader,
    cancelRequest,
    refresh,
    session,
    updateState,
  ])

  const normalizedName = state.name.trim()
  const dirty =
    state.organization !== null &&
    (normalizedName !== state.organization.name || state.logo.action !== "keep")
  const preview = useMemo(() => {
    if (state.logo.action === "remove") return null
    if (state.logo.action === "upload") return safeLogo(state.logo.data)
    return safeLogo(state.organization?.avatarUrl ?? null)
  }, [state.logo, state.organization?.avatarUrl])
  const hasLogo =
    state.logo.action === "upload" ||
    (state.logo.action === "keep" &&
      state.organization !== null &&
      state.organization.avatarUrl !== null)

  return {
    ...state,
    preview,
    hasLogo,
    canSave:
      !!state.organization?.canEdit &&
      dirty &&
      validName(state.name) &&
      !state.loading &&
      !state.saving &&
      !state.logoReading,
    setName,
    uploadLogo,
    removeLogo,
    save,
    retry: () => setRetryGeneration((value) => value + 1),
  }
}
