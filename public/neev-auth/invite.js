"use strict"

const token = location.hash.startsWith("#token=") ? location.hash.slice(7) : ""
history.replaceState(null, "", location.pathname)

const company = document.querySelector("[data-company]")
const form = document.querySelector("form")
const button = document.querySelector("button")
const alert = document.querySelector('[role="alert"]')
let csrf = ""
let organizationId = 0
let submitting = false
let currentOrganizationId = null
let organizationName = ""

function fail(message) {
  company.textContent = "This invitation cannot be opened."
  alert.textContent = message
  form.hidden = true
}

async function json(response) {
  const contentType = response.headers.get("content-type") || ""
  if (!contentType.startsWith("application/json")) throw new Error("response")
  return response.json()
}

async function preview() {
  if (!token) {
    fail("The invitation link is missing or invalid.")
    return
  }
  try {
    const session = await fetch("/api/neev/session", {
      credentials: "same-origin",
      cache: "no-store",
    })
    if (!session.ok) {
      fail("Sign in, then reopen the original invitation email on this device.")
      return
    }
    const sessionBody = await json(session)
    if (
      typeof sessionBody.csrf !== "string" ||
      !sessionBody.csrf ||
      !sessionBody.user ||
      !Number.isInteger(sessionBody.user.id) ||
      sessionBody.user.id <= 0 ||
      (sessionBody.scope !== "identity" &&
        sessionBody.scope !== "organization") ||
      (sessionBody.scope === "identity" &&
        (sessionBody.organization !== null || sessionBody.role !== null)) ||
      (sessionBody.scope === "organization" &&
        (!sessionBody.organization ||
          !Number.isInteger(sessionBody.organization.id) ||
          sessionBody.organization.id <= 0 ||
          !["admin", "member"].includes(sessionBody.role)))
    )
      throw new Error("session")
    csrf = sessionBody.csrf
    currentOrganizationId = sessionBody.organization?.id ?? null
    const response = await fetch("/api/neev/invitations/preview", {
      method: "POST",
      credentials: "same-origin",
      cache: "no-store",
      headers: { "Content-Type": "application/json", "x-neev-csrf": csrf },
      body: JSON.stringify({ token }),
    })
    if (!response.ok) {
      fail(
        "This invitation is expired, revoked, already used, or addressed to another account."
      )
      return
    }
    const result = await json(response)
    if (
      !result.organization ||
      !Number.isInteger(result.organization.id) ||
      result.organization.id <= 0 ||
      typeof result.organization.name !== "string"
    ) {
      throw new Error("preview")
    }
    organizationId = result.organization.id
    organizationName = result.organization.name
    company.textContent = `Join ${result.organization.name}?`
    form.hidden = false
  } catch (_) {
    fail(
      "We could not verify this invitation. Check your connection and try again."
    )
  }
}

form.addEventListener("submit", async (event) => {
  event.preventDefault()
  if (submitting || !token || !csrf || organizationId <= 0) return
  submitting = true
  button.disabled = true
  alert.textContent = ""
  try {
    const response = await fetch("/api/neev/invitations/accept", {
      method: "POST",
      credentials: "same-origin",
      cache: "no-store",
      headers: { "Content-Type": "application/json", "x-neev-csrf": csrf },
      body: JSON.stringify({ token, confirmedOrgId: organizationId }),
    })
    if (!response.ok) {
      fail("This invitation can no longer be accepted.")
      return
    }
    const result = await json(response)
    if (result.organizationId !== organizationId) throw new Error("accept")
    if (
      currentOrganizationId !== null &&
      currentOrganizationId !== organizationId
    ) {
      company.textContent = `You joined ${organizationName}.`
      company.setAttribute("role", "status")
      form.hidden = true
      const hint = document.querySelector(".hint")
      if (hint)
        hint.textContent =
          "Your current workspace stays selected. Open Settings, sign out, then sign in again to choose the organization you just joined."
      const recovery = document.querySelector("[data-recovery]")
      if (recovery) {
        recovery.href = "/settings"
        recovery.textContent = "Open Settings to sign out"
      }
    } else {
      location.replace(
        currentOrganizationId === organizationId ? "/workspace" : "/onboarding"
      )
    }
  } catch (_) {
    fail(
      "We could not accept this invitation. Check your connection and try again."
    )
  } finally {
    submitting = false
    button.disabled = false
  }
})

preview()
