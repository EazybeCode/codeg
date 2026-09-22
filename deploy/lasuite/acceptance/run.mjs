import { BASE_URL, loadEnvironment, redact } from "./lib/config.mjs"
import { Actor } from "./lib/http.mjs"
import { connect } from "./lib/collab.mjs"
import { loadClient } from "./lib/collab.mjs"
import { cases } from "./lib/cases.mjs"
import { randomUUID } from "node:crypto"

/** A document the owner certainly may edit, used only to see if sockets work. */
async function createProbeDocument({ owner, fixtures }) {
  const created = await owner.json(
    `/api/neev/nodes/${fixtures.projectNodeId}/documents`,
    {
      method: "POST",
      body: JSON.stringify({
        title: "Acceptance — socket probe",
        requestId: randomUUID(),
      }),
    }
  )
  if (!created.body?.id)
    return { ok: false, reason: `could not create a probe document (${created.status})` }
  const session = await connect(owner, created.body.id)
  session.destroy()
  return { ok: session.ok, reason: session.reason ?? null, id: created.body.id }
}

/**
 * The acceptance runner.
 *
 * Two rules it does not bend:
 *
 * - **It refuses to run against a canary that is still the baseline.** Every
 *   case would "fail" for the same uninteresting reason, and a wall of red that
 *   means "not deployed yet" is worse than no output — it trains everyone to
 *   ignore the harness.
 * - **It never prints a secret.** Everything that reaches stdout goes through
 *   `redact` first, including failures, where the temptation to dump the whole
 *   request is strongest.
 */

const json = process.argv.includes("--json")
const only = process.argv.find((a) => a.startsWith("--only="))?.slice(7)

function say(...args) {
  if (!json) console.log(...args)
}

/**
 * Is the candidate actually installed?
 *
 * Asked of the deployment rather than assumed from a flag, because the whole
 * point is to notice that nobody has deployed yet.
 */
async function preflight() {
  const reachable = await fetch(new URL("/api/neev/session", BASE_URL), {
    redirect: "manual",
  }).catch((error) => ({ error }))
  if (reachable.error)
    return { ready: false, why: `nothing answering on ${BASE_URL}: ${reachable.error.code ?? reachable.error.message}` }

  /**
   * Probe the engine's API, not the bare prefix.
   *
   * `/document-engine/` itself 404s on a perfectly healthy candidate — the
   * export has no page at the bare prefix and the proxy has no route for it.
   * Using that as the liveness check made this refuse to run against a live
   * engine, which is the worst way for a preflight to be wrong: it looks like
   * a considered decision not to run.
   *
   * The API path distinguishes the two states properly. A candidate answers it,
   * with 401 when unauthenticated — which is a *present* engine refusing, and
   * is exactly what a correctly gated deployment should say. Only a 404 there
   * means nothing is mounted.
   */
  const probe = await fetch(new URL("/document-engine/api/v1.0/config/", BASE_URL), {
    redirect: "manual",
  }).catch(() => null)
  if (!probe || probe.status === 404)
    return {
      ready: false,
      why: "/document-engine/api/v1.0/ is not mounted — this looks like the baseline, not the candidate",
    }
  return { ready: true, engineProbe: probe.status }
}

async function main() {
  const flight = await preflight()
  if (!flight.ready) {
    say(`PREFLIGHT: ${flight.why}`)
    say("Nothing was run. This is not a pass and not a failure.")
    if (json) console.log(JSON.stringify({ ran: false, reason: flight.why }))
    // 2, not 1: "could not run" must be distinguishable from "ran and failed"
    // by anything scripting this.
    process.exit(2)
  }

  const { actors, fixtures } = await loadEnvironment()
  await loadClient() // version assertions happen here, before any socket opens

  const context = { fixtures, engineStopped: process.env.NEEV_ACCEPTANCE_ENGINE_STOPPED === "1" }
  for (const role of ["owner", "member", "outsider"]) {
    context[role] = await new Actor(role, actors[role]).signIn()
  }
  if (context.owner.userId === context.member.userId)
    throw new Error(
      "owner and member resolved to the same user; a permission test between one person and themselves proves nothing"
    )

  /**
   * Can a socket be established at all?
   *
   * Several cases assert that a socket is REFUSED, and every one of them would
   * "pass" if collaboration were broken outright — refused and never-works look
   * identical from the outside. So establish once, against a document the owner
   * just made and plainly may edit, whether a socket can succeed. If it cannot,
   * those cases report skipped instead of claiming a boundary they never
   * reached.
   */
  const probe = await createProbeDocument(context)
  context.socketsWork = probe.ok
  context.socketProbe = probe
  say(
    probe.ok
      ? "sockets: a known-good connection succeeded"
      : `sockets: NO connection can be established (${probe.reason}) — refusal cases cannot be trusted and will skip`
  )

  const selected = only ? cases.filter((c) => c.name.includes(only)) : cases
  const results = []
  for (const test of selected) {
    let outcome
    try {
      outcome = await test.run(context)
    } catch (error) {
      outcome = { status: "fail", why: error.message, evidence: {} }
    }
    results.push({ name: test.name, ...outcome })
    const mark =
      outcome.status === "pass" ? "PASS" : outcome.status === "skipped" ? "SKIP" : "FAIL"
    say(`${mark}  ${test.name}`)
    if (outcome.why) say(`      ${outcome.why}`)
    if (outcome.evidence && Object.keys(outcome.evidence).length)
      say(`      ${JSON.stringify(redact(outcome.evidence))}`)
  }

  const failed = results.filter((r) => r.status === "fail")
  const skipped = results.filter((r) => r.status === "skipped")
  say(
    `\n${results.length - failed.length - skipped.length} passed, ${failed.length} failed, ${skipped.length} skipped`
  )
  if (skipped.length)
    say("Skipped cases proved nothing. They are not passes.")
  if (json) console.log(JSON.stringify({ ran: true, results: redact(results) }, null, 2))
  process.exit(failed.length ? 1 : 0)
}

main().catch((error) => {
  console.error(`harness error: ${error.message}`)
  process.exit(2)
})
