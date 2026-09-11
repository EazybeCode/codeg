import { pathToFileURL } from "node:url"

// Read-only and credential-free. The fixture driver supplies a literal origin.
export async function preflight(raw) {
  let base
  try {
    base = new URL(raw)
  } catch {
    throw new Error("Use an explicitly isolated loopback HTTP origin with port")
  }
  if (
    (raw !== base.origin && raw !== `${base.origin}/`) ||
    base.protocol !== "http:" ||
    !["127.0.0.1", "[::1]"].includes(base.hostname) ||
    !base.port ||
    base.username ||
    base.password ||
    base.pathname !== "/" ||
    base.search ||
    base.hash
  ) {
    throw new Error("Use an explicitly isolated loopback HTTP origin with port")
  }
  const get = async (path) => {
    try {
      return await fetch(new URL(path, base), {
        redirect: "error",
        credentials: "omit",
        signal: AbortSignal.timeout(5000),
        headers: { accept: "application/json" },
      })
    } catch {
      throw new Error("HTTP boundary unavailable or redirect refused")
    }
  }
  const response = await get("/api/workos/capabilities")
  if (response.status !== 200)
    throw new Error("HTTP boundary capabilities unavailable")
  let capabilities
  try {
    capabilities = await response.json()
  } catch {
    throw new Error("HTTP boundary invalid capabilities")
  }
  if (
    capabilities?.identityMode !== "native" ||
    capabilities.runtimeEnabled !== false
  )
    throw new Error("Wrong server identity/runtime boundary")
  for (const route of [
    "/api/team/session",
    "/api/settings",
    "/api/chat/send",
  ]) {
    if ((await get(route)).status !== 404)
      throw new Error(`Legacy route exposed: ${route}`)
  }
  return {
    identityMode: "native",
    runtimeEnabled: false,
    legacyRoutesAbsent: true,
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    console.log(
      JSON.stringify(await preflight(process.env.NEEV_PREFLIGHT_ORIGIN))
    )
  } catch (error) {
    console.error(error.message)
    process.exitCode = 1
  }
}
