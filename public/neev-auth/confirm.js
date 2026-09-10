"use strict"

const token = location.hash.startsWith("#token=") ? location.hash.slice(7) : ""
history.replaceState(null, "", location.pathname)

const form = document.querySelector("form")
const button = document.querySelector("button")
const alert = document.querySelector('[role="alert"]')
let submitting = false

function showFailure(message) {
  alert.textContent = message
}

form.addEventListener("submit", async (event) => {
  event.preventDefault()
  if (submitting) return
  if (!token) {
    showFailure("This link is missing or invalid. Request a new link.")
    return
  }

  submitting = true
  button.disabled = true
  alert.textContent = ""
  try {
    const bootstrap = await fetch("/api/neev/preauth", {
      credentials: "same-origin",
      cache: "no-store",
    })
    if (!bootstrap.ok) throw new Error("bootstrap")
    const contentType = bootstrap.headers.get("content-type") || ""
    if (!contentType.startsWith("application/json"))
      throw new Error("bootstrap")
    const bootstrapBody = await bootstrap.json()
    if (typeof bootstrapBody.csrf !== "string") throw new Error("bootstrap")

    const response = await fetch("/api/neev/email/confirm", {
      method: "POST",
      credentials: "same-origin",
      cache: "no-store",
      headers: {
        "Content-Type": "application/json",
        "x-neev-csrf": bootstrapBody.csrf,
      },
      body: JSON.stringify({ token }),
    })
    if (!response.ok) {
      showFailure(
        "This link is expired, already used, or open in a different browser. Request a new link."
      )
      return
    }
    const responseType = response.headers.get("content-type") || ""
    if (!responseType.startsWith("application/json"))
      throw new Error("confirmation")
    const result = await response.json()
    if (result.next !== "/onboarding" && result.next !== "/settings") {
      throw new Error("confirmation")
    }
    location.replace(result.next)
  } catch (_) {
    showFailure(
      "We could not confirm this link. Check your connection and try again."
    )
  } finally {
    submitting = false
    button.disabled = false
  }
})
