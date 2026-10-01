#!/usr/bin/env node
/**
 * Markdown fidelity of the bridge endpoint, on DISPOSABLE fixtures only.
 *
 *   NEEV_ACCEPTANCE_DISPOSABLE=1 \
 *   NEEV_ACCEPTANCE_STACK_DIR=/opt/neev-document-engine-canary \
 *     node markdown-fidelity.mjs
 *
 * It answers one question the unit tests cannot: does upstream's converter
 * (y-provider `POST /api/convert/`, reached through
 * `POST /_neev/v1/documents/<id>/markdown`) turn what the editor saves into
 * markdown an agent can use? It:
 *
 * 1. creates two NEW documents through Neev in the canary's fixture project —
 *    never an existing document, never a member's document;
 * 2. writes known blocks into the first one's `document-store` fragment (H1
 *    "Fidelity", a two-item bullet list, a 2×2 table, a paragraph carrying a
 *    comment mark, and an image block) and saves them through the editor's
 *    durable path, `PATCH documents/<id>/content/`, as `lib/cases.mjs` does;
 * 3. calls the bridge endpoint from INSIDE the backend container, so the bridge
 *    key never leaves it and the private network is not opened to the host;
 * 4. asserts on the markdown, then asserts the second (never written) fixture
 *    converts to "" — and archives both fixtures.
 *
 * It refuses ("could not run") when the stack directory, its resolved path or
 * its compose project name (`STACK_NAME` in `<stack>/.env`) says `prod`, and
 * when the bridge does not know a fixture Neev just created — that means the
 * stack and the Neev under test are not the same deployment, which is a
 * misconfigured run, not a fidelity result. Fixtures are archived on every
 * exit path after creation, including a failed second creation.
 *
 * Output is PASS/FAIL per assertion name and nothing else: no document text,
 * no key, no cookie. Exit 0 all passed, 1 an assertion failed, 2 could not run
 * (missing opt-in, actors, the pinned client or BlockNote) — "could not run" is
 * never reported as a pass.
 *
 * The blocks are built with `@blocknote/server-util` from the pinned build tree
 * (`NEEV_ACCEPTANCE_MODULES`), the same library y-provider converts with, so
 * the fixture has the editor's own Yjs shape rather than a hand-made imitation
 * of it. The comment mark is applied the way the editor's comment extension
 * stores one (`comment: {threadId, orphan}` on the text); if the converter's
 * schema drops commented text, the "commented paragraph" assertion fails, which
 * is exactly what this run exists to find out.
 */

import { execFile } from "node:child_process"
import { randomUUID } from "node:crypto"
import { readFile, realpath } from "node:fs/promises"
import { createRequire } from "node:module"
import { pathToFileURL } from "node:url"

const COULD_NOT_RUN = 2

/** A run that could not reach its question. Never reported as a pass or a fail. */
class CouldNotRun extends Error {}

/**
 * Why this stack must not be used, or null.
 *
 * Name-based, so it is a guard against a slip, not a proof of disposability:
 * the `NEEV_ACCEPTANCE_DISPOSABLE=1` opt-in remains the operator's assertion.
 */
export function stackRefusal({ stackDir, resolvedDir, stackName }) {
  if (!stackName) return "the stack has no STACK_NAME in .env; cannot tell it is not production"
  for (const [what, value] of [
    ["stack directory", stackDir],
    ["resolved stack directory", resolvedDir],
    ["compose project", stackName],
  ])
    if (/prod/i.test(value ?? "")) return `${what} looks like production; refusing`
  return null
}

async function inspectStack(stackDir) {
  const resolvedDir = await realpath(stackDir)
  const env = await readFile(`${resolvedDir}/.env`, "utf8")
  const stackName = env.match(/^STACK_NAME=(.*)$/m)?.[1]?.trim() || null
  return { stackDir, resolvedDir, stackName }
}

/** The assertions, separated from I/O so they can be read and exercised alone. */
export function fidelityAssertions(markdown, markers) {
  const has = (needle) => typeof markdown === "string" && markdown.includes(needle)
  return [
    ["heading converts to '# Fidelity'", /^# Fidelity\s*$/m.test(markdown ?? "")],
    ["first list item present", has(markers.itemOne)],
    ["second list item present", has(markers.itemTwo)],
    ["list items are bullets", new RegExp(`^\\s*[-*+] +${markers.itemOne}`, "m").test(markdown ?? "")],
    ["table cell texts present", markers.cells.every(has)],
    ["commented paragraph text present", has(markers.commented)],
    ["image replaced by [image]", has("[image]")],
    ["no media path or attachment key leaks", !has("/media/") && !has("attachments/")],
  ]
}

export function fixtureBlocks(markers, documentId) {
  return [
    { type: "heading", props: { level: 1 }, content: "Fidelity" },
    { type: "bulletListItem", content: markers.itemOne },
    { type: "bulletListItem", content: markers.itemTwo },
    {
      type: "table",
      content: {
        type: "tableContent",
        rows: [
          { cells: [markers.cells[0], markers.cells[1]] },
          { cells: [markers.cells[2], markers.cells[3]] },
        ],
      },
    },
    { type: "paragraph", content: markers.commented },
    {
      type: "image",
      props: {
        // Never uploaded: the point is that the URL must not survive conversion.
        url: `/media/${documentId}/attachments/${randomUUID()}.png`,
        caption: "fixture image",
      },
    },
  ]
}

function markersFor(run) {
  return {
    itemOne: `fidelity-item-one-${run}`,
    itemTwo: `fidelity-item-two-${run}`,
    cells: ["a1", "b1", "a2", "b2"].map((cell) => `fidelity-cell-${cell}-${run}`),
    commented: `fidelity-commented-${run}`,
  }
}

/** Find the paragraph holding `marker` and mark its text as commented. */
function markCommented(fragment, Y, marker) {
  const stack = [fragment]
  while (stack.length) {
    const node = stack.pop()
    if (node instanceof Y.XmlText && node.toString().includes(marker)) {
      node.format(0, node.length, { comment: { threadId: randomUUID(), orphan: false } })
      return true
    }
    if (node.toArray) stack.push(...node.toArray())
  }
  return false
}

async function loadBlockNote(moduleRoot) {
  const require = createRequire(`${moduleRoot}/`)
  const entry = require.resolve("@blocknote/server-util")
  return import(pathToFileURL(entry).href)
}

/**
 * Call the bridge from inside the backend container.
 *
 * The key is read from the container's own environment by the child process
 * and is never in this process, its argv or its output. Headers mirror Neev's
 * own bridge client (`engine/upstream.rs`): Host `backend`, forwarded https.
 */
const BRIDGE_CALL = `
import json, os, sys, urllib.error, urllib.request
doc, org, tag = sys.argv[1], int(sys.argv[2]), (sys.argv[3] or None)
req = urllib.request.Request(
    "http://127.0.0.1:8000/_neev/v1/documents/%s/markdown" % doc,
    data=json.dumps({"org_id": org, "if_none_match": tag}).encode(),
    method="POST",
    headers={"Content-Type": "application/json", "Host": "backend",
             "X-Forwarded-Proto": "https",
             "X-Neev-Bridge-Key": os.environ["NEEV_BRIDGE_KEY"]})
try:
    r = urllib.request.urlopen(req, timeout=60); status, body = r.status, r.read()
except urllib.error.HTTPError as e:
    status, body = e.code, e.read()
try:
    parsed = json.loads(body or b"null")
except ValueError:
    parsed = None
sys.stdout.write(json.dumps({"status": status, "body": parsed}))
`

function bridgeMarkdown(stackDir, documentId, orgId, tag = "") {
  return new Promise((resolve, reject) => {
    execFile(
      "docker",
      [
        "compose", "--project-directory", stackDir, "exec", "-T", "backend",
        "python", "-c", BRIDGE_CALL, documentId, String(orgId), tag,
      ],
      { maxBuffer: 8 * 1024 * 1024, timeout: 120_000 },
      (error, stdout) => {
        // stderr is deliberately not surfaced: it could carry a traceback with
        // request data in it.
        if (error) return reject(new Error(`bridge call failed (exit ${error.code ?? "?"})`))
        try {
          resolve(JSON.parse(stdout))
        } catch {
          reject(new Error("bridge call returned no JSON"))
        }
      }
    )
  })
}

async function main() {
  const results = []
  const say = (name, ok) => {
    results.push(ok)
    console.log(`${ok ? "PASS" : "FAIL"}  ${name}`)
  }
  const cannot = (why) => {
    console.log(`COULD NOT RUN: ${why}`)
    console.log("Nothing was asserted. This is not a pass.")
    process.exit(COULD_NOT_RUN)
  }

  if (process.env.NEEV_ACCEPTANCE_DISPOSABLE !== "1")
    cannot("set NEEV_ACCEPTANCE_DISPOSABLE=1 to confirm this targets a disposable canary")
  const stackDir = process.env.NEEV_ACCEPTANCE_STACK_DIR ?? "/opt/neev-document-engine-canary"
  let stack
  try {
    stack = await inspectStack(stackDir)
  } catch {
    cannot("the stack directory or its .env could not be read")
  }
  const refusal = stackRefusal(stack)
  if (refusal) cannot(refusal)

  const [{ loadEnvironment, MODULE_ROOT }, { Actor, engineApi, updateResource }, { loadClient }] =
    await Promise.all([
      import("./lib/config.mjs"),
      import("./lib/http.mjs"),
      import("./lib/collab.mjs"),
    ])

  let env, Y, ServerBlockNoteEditor
  try {
    env = await loadEnvironment()
    ;({ Y } = await loadClient())
    ;({ ServerBlockNoteEditor } = await loadBlockNote(MODULE_ROOT))
  } catch (error) {
    cannot(error.message)
  }
  const owner = await new Actor("owner", env.actors.owner).signIn()
  const orgId = Number(env.actors.owner.orgId)
  if (!Number.isInteger(orgId) || orgId <= 0) cannot("owner org_id is not a positive integer")

  // Every id lands here the moment it exists, so `finally` archives it whatever
  // happens next — including the second creation failing.
  const created = []
  const create = async (title) => {
    const { status, body } = await owner.json(
      `/api/neev/nodes/${env.fixtures.projectNodeId}/documents`,
      { method: "POST", body: JSON.stringify({ title, requestId: randomUUID() }) }
    )
    if (!body?.id) throw new CouldNotRun(`could not create a disposable fixture (${status})`)
    created.push(body.id)
    return body.id
  }

  let couldNotRun = null
  try {
    const run = randomUUID().slice(0, 8)
    const markers = markersFor(run)
    const written = await create(`Acceptance — markdown fidelity ${run} (disposable)`)
    const empty = await create(`Acceptance — markdown empty ${run} (disposable)`)

    const editor = ServerBlockNoteEditor.create()
    const ydoc = await editor.blocksToYDoc(fixtureBlocks(markers, written), "document-store")
    say("fixture comment mark applied", markCommented(ydoc.getXmlFragment("document-store"), Y, markers.commented))
    const content = Buffer.from(Y.encodeStateAsUpdate(ydoc)).toString("base64")

    const saved = await engineApi(owner, `documents/${written}/content/`, {
      method: "PATCH",
      body: JSON.stringify({ content, websocket: false }),
    })
    say("fixture saved through PATCH content/", saved.status < 400)

    const first = await bridgeMarkdown(stackDir, written, orgId)
    if (first.status === 404 || first.body?.error === "not_an_engine_document")
      throw new CouldNotRun(
        "the bridge does not know a fixture Neev just created — the stack and the Neev under test are different deployments"
      )
    say("bridge answers 200", first.status === 200)
    say("result is not 'unchanged'", first.body?.unchanged === false)
    for (const [name, ok] of fidelityAssertions(first.body?.markdown, markers)) say(name, ok)

    const again = await bridgeMarkdown(stackDir, written, orgId, first.body?.content_sha256 ?? "")
    say("same content is reported unchanged", again.status === 200 && again.body?.unchanged === true)

    const other = await bridgeMarkdown(stackDir, written, orgId + 1)
    say("another org is refused", other.status === 409 && other.body?.error === "organization_mismatch")

    const blank = await bridgeMarkdown(stackDir, empty, orgId)
    say("never-written fixture converts to ''", blank.status === 200 && blank.body?.markdown === "")
  } catch (error) {
    if (error instanceof CouldNotRun) couldNotRun = error.message
    // A fixed string: a future error could carry a response body, and this
    // run never prints document text.
    else say("run completed without an unexpected error", false)
  } finally {
    for (const id of created) {
      const archived = await updateResource(owner, id, { archived: true }).catch(() => ({ status: 0 }))
      if (archived.status >= 400 || archived.status === 0)
        console.log(`NOTE  could not archive a disposable fixture (${archived.status})`)
    }
  }

  if (couldNotRun) cannot(couldNotRun)
  const failed = results.filter((ok) => !ok).length
  console.log(`\n${results.length - failed} passed, ${failed} failed`)
  process.exit(failed ? 1 : 0)
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().catch(() => {
    console.log("COULD NOT RUN: harness error")
    process.exit(COULD_NOT_RUN)
  })
}
