import { pathToFileURL } from "node:url"

const failure = (message) => new Error(message)

function explicitLoopback(raw) {
  let base
  try {
    base = new URL(raw)
  } catch {
    throw failure("Use an explicitly isolated loopback HTTP origin with port")
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
    throw failure("Use an explicitly isolated loopback HTTP origin with port")
  }
  return base
}

function explicitFixtureSession(raw) {
  if (
    typeof raw !== "string" ||
    !/^n1\.session\.[A-Za-z0-9_-]{43}$/.test(raw)
  ) {
    throw failure("Use an explicit synthetic native fixture session")
  }
  return raw
}

export async function preflight(rawOrigin, rawSession) {
  const base = explicitLoopback(rawOrigin)
  const session = explicitFixtureSession(rawSession)
  const overall = AbortSignal.timeout(15000)
  const get = async (path) => {
    try {
      const response = await fetch(new URL(path, base), {
        redirect: "error",
        credentials: "omit",
        signal: AbortSignal.any([overall, AbortSignal.timeout(5000)]),
        headers: {
          accept: "application/json",
          cookie: `neev_preview_session=${session}`,
        },
      })
      if (response.status !== 200) throw failure("unexpected status")
      try {
        return await response.json()
      } catch {
        throw failure("invalid JSON")
      }
    } catch {
      throw failure(
        "Native content HTTP boundary unavailable or redirect refused"
      )
    }
  }

  const capabilities = await get("/api/workos/capabilities")
  if (
    capabilities?.identityMode !== "native" ||
    capabilities.runtimeEnabled !== false ||
    capabilities?.catalog?.chats !== true ||
    capabilities?.catalog?.documents !== true
  ) {
    throw failure("Wrong server identity/runtime/content boundary")
  }

  const nodePage = await get("/api/neev/nodes")
  if (!Array.isArray(nodePage?.nodes) || nodePage.nextAfter !== null) {
    throw failure("Native content preflight requires one bounded first page")
  }
  if (
    nodePage.nodes.length > 100 ||
    nodePage.nodes.some((node) => !Number.isInteger(node?.id) || node.id < 1)
  ) {
    throw failure("Native content preflight received invalid nodes")
  }

  let chats = 0
  let documents = 0
  for (const node of nodePage.nodes) {
    const [chatPage, documentPage] = await Promise.all([
      get(`/api/neev/nodes/${node.id}/chats`),
      get(`/api/neev/nodes/${node.id}/documents`),
    ])
    for (const [page, label] of [
      [chatPage, "chat"],
      [documentPage, "document"],
    ]) {
      if (
        !Array.isArray(page?.items) ||
        page.items.length > 100 ||
        page.nextAfter !== null
      ) {
        throw failure(
          `Native ${label} preflight requires one bounded first page`
        )
      }
    }
    chats += chatPage.items.length
    documents += documentPage.items.length
  }

  return {
    identityMode: "native",
    runtimeEnabled: false,
    organizationScoped: true,
    nodes: nodePage.nodes.length,
    chats,
    documents,
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    console.log(
      JSON.stringify(
        await preflight(
          process.env.NEEV_PREFLIGHT_ORIGIN,
          process.env.NEEV_PREFLIGHT_SESSION
        )
      )
    )
  } catch (error) {
    console.error(error.message)
    process.exitCode = 1
  }
}
