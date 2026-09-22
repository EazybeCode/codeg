import { BASE_URL } from "./config.mjs"

/**
 * One actor's browser, as far as the server is concerned.
 *
 * Holds that actor's cookies and CSRF token and nothing else, so two actors in
 * the same run cannot accidentally borrow each other's session — which is the
 * mistake that turns a permission test into a test of nothing. Cookies are kept
 * in memory only; they are never written down and never logged.
 */
export class Actor {
  constructor(name, descriptor) {
    this.name = name
    this.descriptor = descriptor
    this.cookies = new Map()
    this.csrf = null
  }

  get cookieHeader() {
    return [...this.cookies].map(([k, v]) => `${k}=${v}`).join("; ")
  }

  absorb(response) {
    const raw = response.headers.getSetCookie?.() ?? []
    for (const line of raw) {
      const [pair] = line.split(";")
      const index = pair.indexOf("=")
      if (index > 0)
        this.cookies.set(pair.slice(0, index).trim(), pair.slice(index + 1))
    }
  }

  async fetch(path, init = {}) {
    const headers = new Headers(init.headers ?? {})
    if (this.cookies.size) headers.set("cookie", this.cookieHeader)
    if (init.body && !headers.has("content-type"))
      headers.set("content-type", "application/json")

    if (init.method && init.method !== "GET") {
      if (this.csrf && !headers.has("x-neev-csrf"))
        headers.set("x-neev-csrf", this.csrf)
      /**
       * Unsafe methods carry an Origin, because Neev checks it.
       *
       * Node's fetch sends none; a browser always does. Omitting it made every
       * creation come back 403 and looked convincingly like a product failure —
       * three cases failed and twelve skipped behind them. Verified on the
       * canary: identical request without Origin 403, with Origin 201.
       *
       * Sending it is not a way around the check. The check exists to stop
       * another site's page issuing this request, and this harness genuinely is
       * the origin it claims — it is driving the same API from the same host
       * the app is served from. Anything that made a cross-origin caller able
       * to set this would be a browser defect, not a gap here.
       */
      if (!headers.has("origin")) headers.set("origin", new URL(BASE_URL).origin)
    }

    const response = await fetch(new URL(path, BASE_URL), {
      ...init,
      headers,
      redirect: "manual",
    })
    this.absorb(response)
    return response
  }

  async json(path, init) {
    const response = await this.fetch(path, init)
    let body = null
    try {
      body = await response.json()
    } catch {
      body = null
    }
    return { status: response.status, body, response }
  }

  /**
   * Take up the session the host prepared, and check it is really this person.
   *
   * The descriptor carries a cookie jar rather than a password, so there is no
   * sign-in to perform — which suits these cases, since what is under test is
   * what a signed-in person can reach, not how they got there.
   *
   * The identity is then confirmed against the server rather than trusted from
   * the file. A stale jar would otherwise make every actor the same person, and
   * every permission case would pass while proving nothing.
   */
  async signIn() {
    for (const [name, value] of Object.entries(this.descriptor.cookies ?? {}))
      this.cookies.set(name, value)

    const { status, body } = await this.json("/api/neev/session")
    if (status !== 200)
      throw new Error(`${this.name}: session rejected (HTTP ${status})`)

    this.userId = body?.user?.id
    this.orgId = body?.organization?.id
    if (this.userId !== this.descriptor.userId)
      throw new Error(
        `${this.name}: the server says this session is user ${this.userId}, not ${this.descriptor.userId}`
      )

    // The CSRF token travels as a cookie and is echoed back in a header.
    this.csrf =
      body?.csrf ??
      [...this.cookies].find(([key]) => key.endsWith("csrf"))?.[1] ??
      null
    if (!this.csrf) throw new Error(`${this.name}: no CSRF token available`)
    return this
  }
}

/** A document's editor URL, exactly as the UI builds it. */
export const engineUrl = (id) => `/document-engine/docs/${id}/`

/**
 * Ask the engine's own API, carrying this actor's cookies.
 *
 * Docs runs its own CSRF protection, separate from Neev's and with its own
 * token: a `csrftoken` cookie echoed back in `X-CSRFToken`. Neev's
 * `x-neev-csrf` means nothing to it. Establishing that token needs one safe
 * request first, which is exactly the dance the browser performs.
 */
export async function engineApi(actor, path, init = {}) {
  const unsafe = init.method && init.method !== "GET"
  if (unsafe && !actor.cookies.has("csrftoken")) {
    await actor.fetch("/document-engine/api/v1.0/users/me/")
  }
  const headers = new Headers(init.headers ?? {})
  if (unsafe) {
    const token = actor.cookies.get("csrftoken")
    if (token) headers.set("x-csrftoken", token)
    headers.set("origin", new URL(BASE_URL).origin)
  }
  return actor.json(`/document-engine/api/v1.0/${path}`, { ...init, headers })
}

/**
 * Change one thing about a resource, sending the whole thing.
 *
 * Neev's update takes the full record — title, node, visibility, archived and
 * the revision it expects to be replacing — not a patch of the field you care
 * about. Sending only the changed key is a 400, and a 400 here reads exactly
 * like the product refusing the change.
 */
export async function updateResource(actor, resourceId, changes) {
  const current = await actor.json(`/api/neev/resources/${resourceId}`)
  if (current.status !== 200)
    return { status: current.status, body: current.body }
  const r = current.body
  return actor.json(`/api/neev/resources/${resourceId}`, {
    method: "PATCH",
    body: JSON.stringify({
      expectedAuthRevision: r.authRevision,
      title: r.title,
      nodeId: r.nodeId,
      visibility: r.visibility,
      archived: r.archived,
      ...changes,
    }),
  })
}
