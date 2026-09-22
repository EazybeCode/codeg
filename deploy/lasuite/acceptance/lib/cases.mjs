import { randomUUID } from "node:crypto"
import { BASE_URL } from "./config.mjs"
import { engineApi, engineUrl, updateResource } from "./http.mjs"
import {
  connect,
  explainClose,
  loadClient,
  openWithForeignDocumentName,
  until,
} from "./collab.mjs"

/**
 * The acceptance cases.
 *
 * Each returns evidence — the statuses and values it actually saw — so a pass
 * can be read rather than believed. A case that cannot reach the state it is
 * about reports `skipped` with the reason; it never reports a pass for a
 * boundary it did not exercise, which is the only failure mode of a harness
 * that matters.
 */

const ok = (evidence) => ({ status: "pass", evidence })
const no = (why, evidence) => ({ status: "fail", why, evidence })
const skip = (why) => ({ status: "skipped", why })

/** Create a document through Neev and return the resource. */
async function createDocument(actor, nodeId, title, requestId = randomUUID()) {
  const { status, body } = await actor.json(`/api/neev/nodes/${nodeId}/documents`, {
    method: "POST",
    body: JSON.stringify({ title, requestId }),
  })
  return { status, resource: body, requestId }
}

/**
 * The title as each side holds it.
 *
 * Read from both stores every time rather than from one and assumed for the
 * other. A title is the one piece of state both systems display, and "they
 * disagree" is invisible unless something looks at both.
 */
async function titles(actor, id) {
  const native = await actor.json(`/api/neev/resources/${id}`)
  const docs = await engineApi(actor, `documents/${id}/`)
  return {
    native: native.body?.title ?? null,
    docs: docs.body?.title ?? null,
    nativeStatus: native.status,
    docsStatus: docs.status,
  }
}

/**
 * A request missing the CSRF token and nothing else.
 *
 * The Origin header stays, deliberately. Neev refuses an unsafe request with no
 * Origin as well, so dropping both would make this pass on the wrong one — a
 * green CSRF case that never exercised CSRF. One variable at a time is the whole
 * point of the case.
 */
function withoutCsrf(actor, path, body) {
  return fetch(new URL(path, BASE_URL), {
    method: "PATCH",
    headers: {
      cookie: actor.cookieHeader,
      "content-type": "application/json",
      origin: new URL(BASE_URL).origin,
    },
    body: JSON.stringify(body),
    redirect: "manual",
  })
}

export const cases = [
  {
    name: "new document in a project opens on the engine",
    async run({ owner, fixtures }) {
      const { status, resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — project"
      )
      if (status !== 201 && status !== 200)
        return no(`create returned ${status}`, { status })
      if (resource?.engine !== "lasuite")
        return no("new documents must be engine documents", {
          engine: resource?.engine ?? null,
        })

      const page = await owner.fetch(engineUrl(resource.id))
      if (page.status !== 200)
        return no(`editor page returned ${page.status}`, { page: page.status })

      // The framing headers live on the static export, not on Django. Without
      // them the iframe has no policy at all, which is quieter than being
      // blocked and so worth asserting explicitly.
      const csp = page.headers.get("content-security-policy") ?? ""
      const xfo = page.headers.get("x-frame-options") ?? ""
      if (!csp.includes("frame-ancestors"))
        return no("editor page carries no frame-ancestors", { csp, xfo })

      return ok({ id: resource.id, engine: resource.engine, csp, xfo })
    },
  },

  {
    name: "every asset the editor page references actually loads",
    async run({ owner, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — assets"
      )
      const id = resource?.id ?? fixtures.legacyDocumentId
      const page = await owner.fetch(engineUrl(id))
      if (page.status !== 200) {
        await page.arrayBuffer()
        return skip(`editor page is ${page.status}; nothing to check yet`)
      }
      const html = await page.text()

      /**
       * One missing chunk is enough to leave the editor as its prerendered
       * loader forever, and it is nearly invisible: the browser reports a
       * network 404, the application reports nothing, because the application
       * never starts. The page-specific chunk is the dangerous one — its name
       * contains the route's literal brackets, so it arrives percent-encoded
       * and any resolver that does not decode will miss the file that is
       * sitting right there on disk.
       */
      const urls = [
        ...new Set(
          [...html.matchAll(/(?:src|href)="(\/document-engine\/_next\/[^"]+)"/g)].map(
            (m) => m[1]
          )
        ),
      ]
      if (urls.length === 0) return no("the editor page references no assets at all", {})

      const broken = []
      for (const url of urls) {
        const { status, type, error } = await owner.head(url)
        const wanted = url.endsWith(".css")
          ? /css/
          : url.endsWith(".js")
            ? /javascript/
            : /./
        if (status !== 200 || !wanted.test(type))
          broken.push({ url: url.replace("/document-engine/_next/", "…/"), status, type, error })
      }

      return broken.length
        ? no(`${broken.length} of ${urls.length} assets do not load`, { broken })
        : ok({ checked: urls.length })
    },
  },

  {
    name: "the editor's boot content read succeeds",
    async run({ owner, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — boot content"
      )
      if (!resource?.id) return skip("could not create a document to read")

      /**
       * This single request decides whether the editor ever starts.
       *
       * `useCollaboration` refuses to create the collaboration provider while
       * `docContent === undefined`, and `useDocContent` fills that in with
       * `GET documents/<id>/content/`. If the read fails, the query stays in
       * error, the provider is never created, no socket is ever opened, and the
       * body sits on its skeleton forever — with nothing in the console to say
       * why, because nothing threw.
       *
       * It is sent with the browser's own Accept header, because that is part
       * of the request and a proxy may key on it.
       */
      const response = await owner.fetch(
        `/document-engine/api/v1.0/documents/${resource.id}/content/`,
        { headers: { accept: "text/plain,application/json" } }
      )
      const body = await response.text()
      if (response.status !== 200)
        return no(
          `the boot content read is ${response.status} — the editor will never leave its skeleton`,
          {
            status: response.status,
            type: response.headers.get("content-type"),
            body: body.slice(0, 120),
          }
        )
      /**
       * An EMPTY body with 200 is upstream's correct answer for a document
       * nobody has written yet, and it is what lets a new document open at all:
       * the provider is created when `docContent` is defined, and `""` is
       * defined. A 404 here is not "no content", it is "the editor never
       * starts" — so the assertion is on the status and on the body existing,
       * never on it being non-empty.
       */
      return ok({
        status: 200,
        type: response.headers.get("content-type"),
        bytes: body.length,
        note: body.length === 0 ? "empty, which is correct for a fresh document" : "has content",
      })
    },
  },

  {
    name: "written content comes back, byte for byte, and revalidates",
    async run({ owner, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — durability"
      )
      if (!resource?.id) return skip("could not create a document to write")

      /**
       * A real Yjs update, not invented bytes. Upstream parses what it is given
       * to extract attachment keys and rejects anything else with 400 "invalid
       * yjs document", so fabricated content would only ever prove that the
       * validator works.
       *
       * What this establishes is that the STORE round-trips: written, read back
       * identical, and revalidated. It is NOT the editor's save path — a
       * headless client never runs that — so it does not show that editing in a
       * browser persists. Those remain separate claims.
       */
      const { Y } = await loadClient()
      const doc = new Y.Doc()
      doc.getMap("acceptance").set("written", `durability-${randomUUID().slice(0, 8)}`)
      const content = Buffer.from(Y.encodeStateAsUpdate(doc)).toString("base64")

      const written = await engineApi(owner, `documents/${resource.id}/content/`, {
        method: "PATCH",
        body: JSON.stringify({ content, websocket: false }),
      })
      if (written.status >= 400)
        return no(`the content write was refused (${written.status})`, {
          write: written.status,
        })

      const read = await owner.fetch(
        `/document-engine/api/v1.0/documents/${resource.id}/content/`,
        { headers: { accept: "text/plain,application/json" } }
      )
      const back = await read.text()
      if (read.status !== 200)
        return no(`reading it back is ${read.status}`, { read: read.status })
      if (back !== content)
        return no("what came back is not what was written", {
          wrote: content.length,
          got: back.length,
          identical: false,
        })

      // It must also decode — identical bytes that are not a usable update
      // would still be a broken store.
      const round = new Y.Doc()
      Y.applyUpdate(round, Buffer.from(back, "base64"))
      const survived = round.getMap("acceptance").get("written")

      const etag = read.headers.get("etag")
      let revalidated = null
      if (etag) {
        const again = await owner.fetch(
          `/document-engine/api/v1.0/documents/${resource.id}/content/`,
          { headers: { accept: "text/plain,application/json", "if-none-match": etag } }
        )
        await again.arrayBuffer()
        revalidated = again.status
      }

      return ok({
        bytes: back.length,
        identical: true,
        decoded: survived ?? null,
        etag: etag ? "present" : "absent",
        ifNoneMatch: revalidated,
      })
    },
  },

  {
    name: "new document in a subfolder does the same",
    async run({ owner, fixtures }) {
      const { status, resource } = await createDocument(
        owner,
        fixtures.folderNodeId,
        "Acceptance — subfolder"
      )
      if (status !== 201 && status !== 200)
        return no(`create returned ${status}`, { status })
      if (resource?.engine !== "lasuite")
        return no("engine field missing in a subfolder", {
          engine: resource?.engine ?? null,
        })
      if (resource.nodeId !== fixtures.folderNodeId)
        return no("document landed in the wrong node", {
          got: resource.nodeId,
          want: fixtures.folderNodeId,
        })
      return ok({ id: resource.id, nodeId: resource.nodeId })
    },
  },

  {
    name: "a retried creation makes one document, not two",
    async run({ owner, fixtures }) {
      const requestId = randomUUID()
      const first = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — idempotent",
        requestId
      )
      const second = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — idempotent",
        requestId
      )
      if (!first.resource?.id || !second.resource?.id)
        return no("one of the attempts returned no resource", {
          first: first.status,
          second: second.status,
        })
      if (first.resource.id !== second.resource.id)
        return no("the retry created a second document", {
          first: first.resource.id,
          second: second.resource.id,
        })

      // And the engine side must not have been provisioned twice either — the
      // duplicate would be invisible in Neev and very visible in Docs.
      const engine = await engineApi(owner, `documents/${first.resource.id}/`)
      return engine.status === 200
        ? ok({ id: first.resource.id, engine: engine.status })
        : no(`engine does not know the document (${engine.status})`, {
            engine: engine.status,
          })
    },
  },

  {
    name: "CSRF is still enforced, on Neev and on Docs alike",
    async run({ owner, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — csrf"
      )
      if (!resource?.id) return skip("could not create a document to test with")

      // Neev: an unsafe request with no CSRF header.
      const native = await withoutCsrf(owner, `/api/neev/resources/${resource.id}`, {
        title: "no csrf",
      })
      if (native.status < 400)
        return no(`Neev accepted a mutation with no CSRF token (${native.status})`, {
          native: native.status,
        })

      // Docs: same, through the prefix. Upstream's CsrfViewMiddleware is meant
      // to be on; the proxy adds checks but must not have replaced it.
      const docs = await withoutCsrf(
        owner,
        `/document-engine/api/v1.0/documents/${resource.id}/`,
        { title: "no csrf" }
      )
      if (docs.status < 400)
        return no(`Docs accepted a mutation with no CSRF token (${docs.status})`, {
          docs: docs.status,
        })

      return ok({ native: native.status, docs: docs.status })
    },
  },

  {
    name: "a rename in the engine reaches Neev's metadata",
    async run({ owner, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — before rename"
      )
      if (!resource?.id) return skip("could not create a document to rename")

      const renamed = `Acceptance — renamed ${Date.now()}`
      const patch = await engineApi(owner, `documents/${resource.id}/`, {
        method: "PATCH",
        body: JSON.stringify({ title: renamed }),
      })
      if (patch.status >= 400)
        return no(`engine refused the rename (${patch.status})`, {
          patch: patch.status,
        })

      // The UI re-reads Neev's own resource after focus leaves the frame; this
      // asserts the thing that read depends on — that Neev's metadata follows.
      const synced = await until(async () => {
        const { body } = await owner.json(`/api/neev/resources/${resource.id}`)
        return body?.title === renamed
      })
      return synced
        ? ok({ id: resource.id, title: renamed })
        : no("Neev's title never caught up with the engine's", { want: renamed })
    },
  },

  {
    name: "a refused rename leaves both titles as they were",
    async run({ owner, fixtures }) {
      const before = `Acceptance — csrf title ${Date.now()}`
      const { resource } = await createDocument(owner, fixtures.projectNodeId, before)
      if (!resource?.id) return skip("could not create a document to rename")

      const attempt = await withoutCsrf(
        owner,
        `/document-engine/api/v1.0/documents/${resource.id}/`,
        { title: "renamed without a token" }
      )
      if (attempt.status < 400)
        return no(`Docs accepted a rename with no CSRF token (${attempt.status})`, {
          attempt: attempt.status,
        })

      // The refusal is only half of it. A rejected write that nevertheless
      // landed on one side would leave the two stores disagreeing, and the
      // status code alone would never show it.
      const after = await titles(owner, resource.id)
      if (after.native !== before || after.docs !== before)
        return no("a refused rename still changed a title", {
          expected: before,
          ...after,
        })
      return ok({ refused: attempt.status, native: after.native, docs: after.docs })
    },
  },

  {
    name: "a shared editor may rename, and both stores follow",
    async run({ owner, member, fixtures }) {
      const before = `Acceptance — shared editor ${Date.now()}`
      const { resource } = await createDocument(owner, fixtures.projectNodeId, before)
      if (!resource?.id) return skip("could not create a document to share")

      const shared = await updateResource(owner, resource.id, {
        visibility: "organization",
      })
      if (shared.status >= 400)
        return no(`sharing failed (${shared.status})`, { shared: shared.status })

      /**
       * Renaming is deliberately NOT creator-only.
       *
       * Neev's `resources::update` restricts visibility, archiving and moving to
       * the creator; the title is left to anyone who may edit the document. That
       * lines up with the Docs editor role, which includes `partial_update`, so
       * the two models agree rather than diverge — and a shared editor renaming
       * a document they may already rewrite is not a bypass of anything.
       *
       * What matters is that the two stores do not disagree afterwards, which is
       * what this asserts. An earlier version of this case asserted the opposite
       * on the assumption that renaming was creator-only; that assumption was
       * simply wrong about the product.
       */
      const renamed = `Acceptance — renamed by a member ${randomUUID().slice(0, 6)}`
      const viaNeev = await updateResource(member, resource.id, { title: renamed })
      if (viaNeev.status >= 400)
        return no(`a shared editor was refused a rename (${viaNeev.status})`, {
          viaNeev: viaNeev.status,
        })

      const settled = await until(async () => {
        const now = await titles(owner, resource.id)
        return now.native === renamed && now.docs === renamed
      })
      const after = await titles(owner, resource.id)
      if (!settled)
        return no("the rename landed on one store but not the other", {
          wanted: renamed,
          ...after,
        })

      return ok({
        viaNeev: viaNeev.status,
        native: after.native,
        docs: after.docs,
        agreed: after.native === after.docs,
      })
    },
  },

  {
    name: "archiving freezes the body, not the metadata",
    async run({ owner, fixtures }) {
      const before = `Acceptance — archived policy ${Date.now()}`
      const { resource } = await createDocument(owner, fixtures.projectNodeId, before)
      if (!resource?.id) return skip("could not create a document to archive")

      const archived = await updateResource(owner, resource.id, { archived: true })
      if (archived.status >= 400)
        return no(`archiving failed (${archived.status})`, {
          archived: archived.status,
        })

      /**
       * "Archived is read-only" is about the document BODY, and only that.
       *
       * Neev's `resources::update` deliberately still accepts metadata on an
       * archived resource — it has to, or nothing could ever be unarchived. So
       * a rename through Neev being accepted here is the existing policy
       * working, not a hole. An earlier version of this case expected a refusal
       * and was inventing a restriction the product does not have.
       *
       * The two things that must hold are narrower: the body cannot be written,
       * and whatever the metadata ends up as, both stores agree on it.
       */
      const renamed = `Acceptance — renamed while archived ${randomUUID().slice(0, 6)}`
      const viaNeev = await updateResource(owner, resource.id, { title: renamed })
      if (viaNeev.status >= 400)
        return no(
          `Neev refused a metadata change on an archived document (${viaNeev.status}) — unarchiving would be impossible`,
          { viaNeev: viaNeev.status }
        )

      // The body must be refused. Docs' own abilities are the authority here.
      const engine = await engineApi(owner, `documents/${resource.id}/`)
      const abilities = engine.body?.abilities ?? {}
      if (abilities.content_patch !== false)
        return no("an archived document still allows a content write", {
          content_patch: abilities.content_patch,
        })

      const settled = await until(async () => {
        const now = await titles(owner, resource.id)
        return now.native === renamed && now.docs === renamed
      })
      const after = await titles(owner, resource.id)
      if (!settled)
        return no("the archived rename landed on one store but not the other", {
          wanted: renamed,
          ...after,
        })

      return ok({
        metadataAccepted: viaNeev.status,
        bodyFrozen: abilities.content_patch === false,
        native: after.native,
        docs: after.docs,
      })
    },
  },

  {
    name: "two renames at once leave one title, the same on both sides",
    async run({ owner, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        `Acceptance — race ${Date.now()}`
      )
      if (!resource?.id) return skip("could not create a document to rename")

      const first = `Acceptance — race A ${randomUUID().slice(0, 6)}`
      const second = `Acceptance — race B ${randomUUID().slice(0, 6)}`

      // Issued together, not one after the other: the interesting state is the
      // one where both are in flight.
      const [a, b] = await Promise.all([
        engineApi(owner, `documents/${resource.id}/`, {
          method: "PATCH",
          body: JSON.stringify({ title: first }),
        }),
        engineApi(owner, `documents/${resource.id}/`, {
          method: "PATCH",
          body: JSON.stringify({ title: second }),
        }),
      ])

      // Either may lose — a conflict response is a perfectly good answer and is
      // not what this case is about. What matters is that the two stores agree
      // afterwards, and on a title somebody actually asked for.
      const settled = await until(async () => {
        const now = await titles(owner, resource.id)
        return now.native !== null && now.native === now.docs
      })
      const after = await titles(owner, resource.id)

      if (!settled)
        return no("Neev and the engine ended up with different titles", {
          a: a.status,
          b: b.status,
          ...after,
        })
      if (after.native !== first && after.native !== second)
        return no("the surviving title was one nobody asked for", {
          survived: after.native,
          asked: [first, second],
        })

      return ok({
        a: a.status,
        b: b.status,
        survived: after.native,
        agreed: after.native === after.docs,
      })
    },
  },

  {
    name: "comments are reachable, or the comment UI is a dead button",
    async run({ owner, outsider, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — comments"
      )
      if (!resource?.id) return skip("could not create a document to comment on")

      /**
       * Upstream decides whether commenting is offered, through
       * `abilities.comment`, and the editor shows the Add comment button on the
       * strength of it. So if the ability says yes and the API is unreachable,
       * the button is visibly broken rather than absent — the user clicks and
       * nothing happens. That is the state worth failing on, and it is why this
       * case checks the ability first: `comment: false` would make an
       * unreachable API perfectly consistent.
       */
      const doc = await engineApi(owner, `documents/${resource.id}/`)
      const offered = doc.body?.abilities?.comment
      if (offered !== true)
        return skip(`upstream does not offer commenting here (comment: ${offered})`)

      const list = await engineApi(owner, `documents/${resource.id}/threads/`)
      if (list.status !== 200)
        return no(
          `commenting is offered but GET threads/ is ${list.status} — the Add comment button will fail`,
          { comment: offered, threads: list.status, body: JSON.stringify(list.body).slice(0, 80) }
        )

      const created = await engineApi(owner, `documents/${resource.id}/threads/`, {
        method: "POST",
        body: JSON.stringify({ body: "acceptance comment" }),
      })
      if (created.status >= 400)
        return no(`creating a thread is ${created.status}`, {
          create: created.status,
        })

      // And the boundary: another organization must not reach them.
      const theirs = await engineApi(outsider, `documents/${resource.id}/threads/`)
      return theirs.status >= 400
        ? ok({ list: list.status, create: created.status, outsider: theirs.status })
        : no(`an outsider read the threads (${theirs.status})`, {
            outsider: theirs.status,
          })
    },
  },

  {
    name: "another organization is refused, on the page and on the API",
    async run({ owner, outsider, fixtures, socketsWork }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — other org"
      )
      if (!resource?.id) return skip("could not create a document to guard")

      const native = await outsider.json(`/api/neev/resources/${resource.id}`)
      const api = await engineApi(outsider, `documents/${resource.id}/`)
      const page = await outsider.fetch(engineUrl(resource.id))

      if (native.status < 400)
        return no(`outsider read Neev's resource (${native.status})`, {
          native: native.status,
        })
      if (api.status < 400)
        return no(`outsider read the engine document (${api.status})`, {
          api: api.status,
        })

      if (!socketsWork)
        return ok({
          native: native.status,
          api: api.status,
          page: page.status,
          socket: "not checked — no socket can be established at all",
        })
      const socket = await connect(outsider, resource.id)
      socket.destroy()
      if (socket.ok)
        return no("outsider authenticated on the collaboration socket", {
          native: native.status,
          api: api.status,
        })

      return ok({
        native: native.status,
        api: api.status,
        page: page.status,
        socket: explainClose(socket),
      })
    },
  },

  {
    name: "a second user in the organization can open and converge",
    async run({ owner, member, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — shared"
      )
      if (!resource?.id) return skip("could not create a document to share")

      const shared = await updateResource(owner, resource.id, {
        visibility: "organization",
      })
      if (shared.status >= 400)
        return no(`sharing failed (${shared.status})`, { shared: shared.status })

      const a = await connect(owner, resource.id)
      const b = await connect(member, resource.id)
      try {
        if (!a.ok) return no("owner could not connect", { reason: explainClose(a) })
        if (!b.ok) return no("member could not connect", { reason: explainClose(b) })

        // Both write without waiting for the other, which is the case that
        // distinguishes convergence from taking turns.
        const mark = randomUUID().slice(0, 8)
        a.doc.getMap("acceptance").set("owner", `owner-${mark}`)
        b.doc.getMap("acceptance").set("member", `member-${mark}`)

        const converged = await until(() => {
          const one = a.doc.getMap("acceptance")
          const two = b.doc.getMap("acceptance")
          return (
            one.get("member") === `member-${mark}` &&
            two.get("owner") === `owner-${mark}`
          )
        })
        return converged
          ? ok({ id: resource.id, mark })
          : no("the two sides never converged", {
              owner: a.doc.getMap("acceptance").toJSON(),
              member: b.doc.getMap("acceptance").toJSON(),
            })
      } finally {
        a.destroy()
        b.destroy()
      }
    },
  },

  {
    name: "withdrawing access stops the next connection",
    async run({ owner, member, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — revocation"
      )
      if (!resource?.id) return skip("could not create a document to revoke")

      await updateResource(owner, resource.id, { visibility: "organization" })
      const before = await connect(member, resource.id)
      before.destroy()
      if (!before.ok)
        return skip(
          `member could not connect even before revocation: ${explainClose(before)}`
        )

      await updateResource(owner, resource.id, { visibility: "private" })

      const after = await connect(member, resource.id)
      after.destroy()
      if (after.ok)
        return no("member still connected after access was withdrawn", {})

      // Stated precisely: this is about a NEW connection. Whether an already
      // open socket is dropped at the moment access is revoked is a different
      // question, and this case does not answer it.
      return ok({ before: "connected", after: after.reason, scope: "new connections only" })
    },
  },

  {
    name: "revoking a LIVE session stops its edits reaching the other side",
    async run({ owner, member, fixtures, socketsWork }) {
      if (!socketsWork)
        return skip("no socket can be established at all")

      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — live revocation"
      )
      if (!resource?.id) return skip("could not create a document to revoke")
      const shared = await updateResource(owner, resource.id, {
        visibility: "organization",
      })
      if (shared.status >= 400)
        return no(`sharing failed (${shared.status})`, { shared: shared.status })

      const a = await connect(owner, resource.id)
      const b = await connect(member, resource.id)
      try {
        if (!a.ok || !b.ok)
          return skip(
            `both sides must be connected first: owner ${a.ok}, member ${b.ok}`
          )

        /**
         * The preview left this open deliberately: it tested that a NEW
         * connection is refused after access is withdrawn, and said so, because
         * whether an ALREADY OPEN socket stops being able to write is a
         * different question. This is that question.
         *
         * The baseline below is what makes the answer mean anything. Proving
         * that an edit does not arrive is worthless unless an edit DID arrive a
         * moment earlier over the same pair of sockets — otherwise "nothing got
         * through" is just as consistent with the channel never having worked.
         */
        const before = randomUUID().slice(0, 8)
        b.doc.getMap("live").set("member", `before-${before}`)
        const baseline = await until(
          () => a.doc.getMap("live").get("member") === `before-${before}`
        )
        if (!baseline)
          return skip(
            "the member's edits were not reaching the owner even before revocation"
          )

        const revoked = await updateResource(owner, resource.id, {
          visibility: "private",
        })
        if (revoked.status >= 400)
          return no(`revocation failed (${revoked.status})`, {
            revoked: revoked.status,
          })

        // Now write as the member, who should no longer be allowed to.
        const after = randomUUID().slice(0, 8)
        b.doc.getMap("live").set("member", `after-${after}`)

        const leaked = await until(
          () => a.doc.getMap("live").get("member") === `after-${after}`,
          { timeoutMs: 8_000 }
        )
        if (leaked)
          return no(
            "an edit made after access was withdrawn still reached the owner",
            { baseline: `before-${before}`, leaked: `after-${after}` }
          )

        // And the member's own socket should not linger indefinitely.
        const closed = await until(() => b.live.closed, { timeoutMs: 20_000 })

        return closed
          ? ok({
              baseline: "member's edit reached the owner before revocation",
              afterRevocation: "not delivered",
              memberSocket: `closed, code ${b.live.code ?? "none"}`,
            })
          : ok({
              baseline: "member's edit reached the owner before revocation",
              afterRevocation: "not delivered",
              memberSocket:
                "STILL OPEN after 20s — the edit did not propagate, but the session was not cut",
            })
      } finally {
        a.destroy()
        b.destroy()
      }
    },
  },

  {
    name: "a frame naming a different document is refused",
    async run({ owner, member, fixtures, socketsWork }) {
      if (!socketsWork)
        return skip(
          "no socket can be established at all, so a refusal here would prove nothing"
        )

      const mine = await createDocument(owner, fixtures.projectNodeId, "Acceptance — mine")
      const theirs = await createDocument(owner, fixtures.projectNodeId, "Acceptance — private")
      if (!mine.resource?.id || !theirs.resource?.id)
        return skip("could not create the two documents this needs")

      await updateResource(owner, mine.resource.id, { visibility: "organization" })
      // `theirs` stays private to the owner.

      // The document name is a leading varstring on every frame, so a socket
      // authorised for one document can name another. Upstream rejects that in
      // onConnect (documentName !== room). This asserts the proxy has not lost
      // that defence.
      const attempt = await openWithForeignDocumentName(
        member,
        mine.resource.id,
        theirs.resource.id
      )
      attempt.destroy()

      return attempt.ok
        ? no("a socket carried a frame for a document it was not authorised for", {
            authorised: mine.resource.id,
            named: theirs.resource.id,
          })
        : ok({ refused: explainClose(attempt), code: attempt.code ?? null })
    },
  },

  {
    name: "an archived document is read-only, not writable",
    async run({ owner, fixtures, socketsWork }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — archived"
      )
      if (!resource?.id) return skip("could not create a document to archive")

      const archived = await updateResource(owner, resource.id, { archived: true })
      if (archived.status >= 400)
        return no(`archiving failed (${archived.status})`, { archived: archived.status })

      const session = await connect(owner, resource.id)
      try {
        if (!session.ok) {
          // Refusing outright is a defensible answer ONLY if sockets otherwise
          // work; if none do, this says nothing about archiving.
          if (!socketsWork)
            return skip("no socket can be established at all")
          return ok({ outcome: "socket refused", reason: explainClose(session) })
        }

        const writable = session.provider.configuration?.readOnly === false
        const engine = await engineApi(owner, `documents/${resource.id}/`)
        const abilities = engine.body?.abilities ?? {}
        return writable && abilities.update === true
          ? no("an archived document is still writable", { abilities })
          : ok({ readOnly: !writable, abilities })
      } finally {
        session.destroy()
      }
    },
  },

  {
    name: "the legacy document is untouched and still native",
    async run({ owner, fixtures }) {
      // Read-only throughout. There is no migration, and this fixture is the
      // baseline that proves it — writing to it would destroy the only evidence
      // that legacy documents were left alone.
      const { status, body } = await owner.json(
        `/api/neev/resources/${fixtures.legacyDocumentId}`
      )
      if (status !== 200)
        return no(`legacy document unreadable (${status})`, { status })
      if (body?.engine === "lasuite")
        return no("a legacy document was migrated onto the engine", {
          engine: body.engine,
        })

      const content = await owner.json(
        `/api/neev/documents/${fixtures.legacyDocumentId}/content`
      )
      return content.status === 200
        ? ok({
            engine: body?.engine ?? null,
            contentRevision: body?.contentRevision,
            native: content.status,
          })
        : no(`legacy content unreadable (${content.status})`, {
            content: content.status,
          })
    },
  },

  {
    name: "Neev's own save routes refuse an engine document",
    async run({ owner, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — native save"
      )
      if (!resource?.id) return skip("could not create a document to probe")

      // The UI never calls these for an engine document. If the server accepts
      // them anyway there are two writers for one document, which is the exact
      // failure the whole design avoids — so it has to be refused at the server,
      // not merely unused by the client.
      const save = await owner.json(`/api/neev/documents/${resource.id}/revisions`, {
        method: "POST",
        body: JSON.stringify({
          requestId: randomUUID(),
          expectedContentRevision: resource.contentRevision,
          expectedAuthRevision: resource.authRevision,
          markdown: "written by the wrong editor",
        }),
      })
      if (save.status < 400)
        return no(`a native save was accepted for an engine document (${save.status})`, {
          save: save.status,
        })

      const restore = await owner.json(`/api/neev/documents/${resource.id}/restore`, {
        method: "POST",
        body: JSON.stringify({
          requestId: randomUUID(),
          expectedContentRevision: resource.contentRevision,
          expectedAuthRevision: resource.authRevision,
          sourceRevision: "1",
        }),
      })
      return restore.status >= 400
        ? ok({ save: save.status, restore: restore.status })
        : no(`a native restore was accepted (${restore.status})`, {
            restore: restore.status,
          })
    },
  },

  {
    name: "an unavailable engine reads as retryable, not as a blank frame",
    tags: ["needs-engine-stopped"],
    async run({ owner, fixtures, engineStopped }) {
      if (!engineStopped)
        return skip(
          "needs the engine stopped; re-run with NEEV_ACCEPTANCE_ENGINE_STOPPED=1 while it is down"
        )

      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — unavailable"
      )
      const id = resource?.id ?? fixtures.legacyDocumentId

      const page = await owner.fetch(engineUrl(id))
      const body = await page.text()
      // The UI reads one explicit marker and refuses to sniff body text, so the
      // error page has to carry it. Without it the frame shows the error page
      // with no retry offered — which is the gap this case exists to catch.
      const marked = body.includes("neev-document-engine-error")
      return marked
        ? ok({ page: page.status, marker: true })
        : no(
            "the error page carries no neev-document-engine-error marker, so the UI cannot offer a retry",
            { page: page.status, marker: false }
          )
    },
  },
]
