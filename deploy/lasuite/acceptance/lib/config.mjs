import { readFile, stat } from "node:fs/promises"

/**
 * Configuration for the acceptance run.
 *
 * Everything that identifies a person or authorises a request lives in files on
 * the canary host and is read at run time. Nothing here is committed, and
 * nothing read here is ever printed — see `redact`.
 */

export const BASE_URL = process.env.NEEV_ACCEPTANCE_BASE ?? "http://127.0.0.1:3100"

/**
 * Read at call time, not captured at import.
 *
 * Capturing it made the validation below untestable — every attempt to point it
 * at a crafted directory kept reading the real one and "failed" with ENOENT,
 * which looks exactly like the guard working. A guard that cannot be exercised
 * is a guard nobody knows the state of.
 */
export const canaryDir = () =>
  process.env.NEEV_ACCEPTANCE_DIR ?? "/opt/neev-docs-canary"

/** Where the pinned collaboration client lives. Never installed by this harness. */
export const MODULE_ROOT =
  process.env.NEEV_ACCEPTANCE_MODULES ??
  "/tmp/neev-lasuite-build/work/node_modules"

/**
 * The versions the WS cases are only meaningful against.
 *
 * The collaboration protocol is not stable across majors, and a convergence
 * result obtained with a different provider than the one the app ships proves
 * something about a client nobody runs. Asserted before any socket opens.
 */
export const REQUIRED_VERSIONS = {
  "@hocuspocus/provider": "3.4.4",
  yjs: "13.6.32",
}

/**
 * Strip anything secret from a value before it can reach a log.
 *
 * Deliberately a denylist of *keys* rather than a scan of values: a password
 * that happens to look like an ordinary word would survive a value scan, and
 * the point is that no secret is ever one formatting mistake from stdout.
 */
const SECRET_KEYS = new Set([
  "password",
  "secret",
  "token",
  "cookie",
  "cookies",
  "csrf",
  "sessionid",
  "docs_sessionid",
  "authorization",
  "apiKey",
  "api_key",
])

export function redact(value) {
  if (Array.isArray(value)) return value.map(redact)
  if (value && typeof value === "object") {
    const out = {}
    for (const [key, item] of Object.entries(value)) {
      out[key] = SECRET_KEYS.has(key.toLowerCase())
        ? "<redacted>"
        : redact(item)
    }
    return out
  }
  return value
}

async function readPrivateJson(path) {
  const info = await stat(path)
  // 0o077 is group+other. These files name people and carry their credentials;
  // if the host has left them world-readable that is worth stopping for, not
  // worth working around.
  if (info.mode & 0o077)
    throw new Error(
      `${path} is readable beyond its owner (mode ${(info.mode & 0o777).toString(8)}). Refusing to read it.`
    )
  return JSON.parse(await readFile(path, "utf8"))
}

/**
 * Actors and fixtures, as the canary host provides them.
 *
 * `actors.json` is expected to describe an owner and a member in one
 * organization, plus an outsider in another. `fixtures.json` is expected to
 * name a project, a folder, and a pre-existing legacy document.
 *
 * Shape is validated rather than assumed, because a missing actor silently
 * turning a permission test into a no-op is the failure mode that matters here:
 * it would report success for a boundary nobody checked.
 */
export async function loadEnvironment() {
  const dir = canaryDir()
  const actors = await readPrivateJson(`${dir}/actors.json`)
  const fixtures = await readPrivateJson(`${dir}/fixtures.json`)

  for (const role of ["owner", "member", "outsider"])
    if (!actors[role]) throw new Error(`actors.json has no "${role}"`)

  if (actors.owner.orgId !== actors.member.orgId)
    throw new Error("owner and member must share an organization")
  if (actors.outsider.orgId === actors.owner.orgId)
    throw new Error("outsider must be in a different organization")

  for (const key of ["projectNodeId", "folderNodeId", "legacyDocumentId"])
    if (fixtures[key] === undefined)
      throw new Error(`fixtures.json has no "${key}"`)

  return { actors, fixtures }
}
