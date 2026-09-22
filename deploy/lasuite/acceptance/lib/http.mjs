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
    if (this.csrf && init.method && init.method !== "GET")
      headers.set("x-neev-csrf", this.csrf)
    if (init.body && !headers.has("content-type"))
      headers.set("content-type", "application/json")

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
   * Establish this actor's session. How that happens is the canary's business —
   * the descriptor carries whatever the host set up — so the only thing asserted
   * here is that it worked and produced a distinct identity.
   */
  async signIn() {
    const { status, body } = await this.json("/api/neev/session")
    if (status !== 200)
      throw new Error(`${this.name}: no session (HTTP ${status})`)
    this.csrf = body?.csrf ?? null
    if (!this.csrf) throw new Error(`${this.name}: session carries no CSRF token`)
    this.userId = body?.user?.id
    this.orgId = body?.organization?.id
    return this
  }
}

/** A document's editor URL, exactly as the UI builds it. */
export const engineUrl = (id) => `/document-engine/docs/${id}/`

/** Ask the engine's own API directly, carrying this actor's cookies. */
export function engineApi(actor, path, init) {
  return actor.json(`/document-engine/api/v1.0/${path}`, init)
}
