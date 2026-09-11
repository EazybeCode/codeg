// Included exclusively by the cfg(test) browser fixture. Never an exported app asset.
const syntheticIssuer = "__NEEV_SYNTHETIC_ISSUER__"
let busy = false
async function beginSyntheticGoogle(linking) {
  if (busy) return
  busy = true
  const status = document.getElementById("status")
  status.textContent = "Starting synthetic sign-in…"
  const request = async (path, options = {}) => {
    const response = await fetch(path, {
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      referrerPolicy: "no-referrer",
      ...options,
    })
    if (!response.ok) throw new Error("synthetic request failed")
    return response.json()
  }
  try {
    const preauth = await request("/api/neev/preauth")
    const auth = linking ? await request("/api/neev/session") : preauth
    if (typeof auth.csrf !== "string" || !auth.csrf)
      throw new Error("missing csrf")
    const result = await request(
      linking ? "/api/neev/auth/google/link" : "/api/neev/auth/google/login",
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "x-neev-csrf": auth.csrf,
        },
        body: "{}",
      }
    )
    const target = new URL(result.authorizationUrl)
    if (
      target.origin !== syntheticIssuer ||
      target.pathname !== "/authorize" ||
      target.username ||
      target.password ||
      target.hash
    )
      throw new Error("untrusted target")
    window.location.assign(target.href)
  } catch {
    status.textContent = "Synthetic sign-in could not start."
    busy = false
  }
}
document
  .getElementById("begin-login")
  .addEventListener("click", () => beginSyntheticGoogle(false))
document
  .getElementById("begin-link")
  .addEventListener("click", () => beginSyntheticGoogle(true))
